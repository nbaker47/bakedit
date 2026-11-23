//! The editing engine: a buffer, its selections, and every command that changes them.
//!
//! The app owns one [`Editor`] per open file and drives it with [`Editor::apply`]. The
//! editor never draws anything and never touches the clipboard or the screen: it returns an
//! [`Outcome`] saying what changed, and offers queries the view needs.

mod edit;
mod lines;
mod motion;
mod select;
#[cfg(test)]
mod tests;

use std::borrow::Cow;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::buffer::Buffer;
use crate::chars::{self, CharKind};
use crate::clock::Clock;
use crate::command::{Clipboard, Command};
use crate::display;
use crate::edit::EditSummary;
use crate::file::{BufferError, LineEnding, SaveOptions};
use crate::graphemes;
use crate::history::UndoKind;
use crate::indent::{self, IndentStyle};
use crate::pairs::{self, PairKind};
use crate::search::Search;
use crate::selection::{Selection, Selections};

/// How an editor edits: indentation, bracket handling, wrapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditSettings {
    /// Columns per indent level and per tab stop. 0 is treated as 1.
    pub tab_size: usize,
    /// Indent with tab characters instead of spaces.
    pub hard_tabs: bool,
    /// Keep the indentation of the current line on a new line, and add a level after a
    /// line that ends in one of `indent_after`.
    pub auto_indent: bool,
    /// Close brackets and quotes as they are typed, step over a closer that is already
    /// there, delete an empty pair together, and wrap a selection when an opener is typed.
    pub autoclose: bool,
    /// The pairs that are closed automatically, as (opener, closer).
    pub autoclose_pairs: Vec<(char, char)>,
    /// The pairs that wrap a selection when their opener is typed, as (opener, closer).
    pub surround_pairs: Vec<(char, char)>,
    /// A line ending in one of these characters indents the line after it.
    pub indent_after: String,
    /// The width, in cells, at which lines soft-wrap. `None` (or `Some(0)`) means no
    /// wrapping. Vertical motion then moves by wrapped rows, as the view draws them.
    pub wrap_width: Option<usize>,
    /// Characters that count as part of a word in addition to letters, digits and `_`.
    pub word_chars: String,
    /// How many rows a page motion moves when its `rows` is 0.
    pub page_rows: usize,
}

impl Default for EditSettings {
    fn default() -> EditSettings {
        let pairs = vec![
            ('(', ')'),
            ('[', ']'),
            ('{', '}'),
            ('"', '"'),
            ('\'', '\''),
            ('`', '`'),
        ];
        EditSettings {
            tab_size: 4,
            hard_tabs: false,
            auto_indent: true,
            autoclose: true,
            autoclose_pairs: pairs.clone(),
            surround_pairs: pairs,
            indent_after: "{[(:".to_string(),
            wrap_width: None,
            word_chars: String::new(),
            page_rows: 20,
        }
    }
}

/// What a command did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Outcome {
    /// The text changes, empty when the text did not change. Map anything that holds
    /// positions through it, and feed [`EditSummary::input_edits`] to the parser.
    pub edits: EditSummary,
    /// The selections are not what they were before the command.
    pub selections_changed: bool,
    /// Text for the system clipboard, produced by `Copy` and `Cut`.
    pub clipboard: Option<Clipboard>,
}

impl Outcome {
    /// True when the command changed the text.
    pub fn text_changed(&self) -> bool {
        !self.edits.is_empty()
    }

    /// True when the command did nothing at all.
    pub fn is_noop(&self) -> bool {
        !self.text_changed() && !self.selections_changed && self.clipboard.is_none()
    }
}

/// Where a position is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct DisplayPosition {
    /// 0-based buffer line.
    pub line: usize,
    /// 0-based soft-wrap row within the line (always 0 without wrapping).
    pub row: usize,
    /// Display column within the row.
    pub col: usize,
}

/// One line laid out under the current wrap settings.
pub(crate) struct LineLayout<'a> {
    pub text: Cow<'a, str>,
    pub rows: Vec<Range<usize>>,
}

/// What `SelectNext` and friends are looking for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SelectNextState {
    pub query: String,
    /// Only whole words match (the search started from a bare cursor).
    pub wordwise: bool,
}

/// The cursors `AddSelectionAbove`/`Below` have stacked up, so the opposite command can
/// take them away again.
#[derive(Debug, Clone)]
pub(crate) struct AddSelectionsState {
    pub above: bool,
    pub groups: Vec<AddSelectionsGroup>,
    /// The selections the last command left, to notice when something else changed them.
    pub result: Selections,
}

/// One original selection and the cursors added from it.
#[derive(Debug, Clone)]
pub(crate) struct AddSelectionsGroup {
    /// The selections of this group, the original first and the last added last.
    pub stack: Vec<Selection>,
    /// The display columns the group keeps to.
    pub columns: Range<usize>,
}

/// The total number of selections kept in the selection history before old entries go.
const SELECTION_HISTORY_BUDGET: usize = 20_000;

/// An editor: one buffer, its selections, and the settings that shape editing.
#[derive(Debug)]
pub struct Editor {
    buffer: Buffer,
    selections: Selections,
    settings: EditSettings,
    kill_ring: Option<Clipboard>,
    selection_undo: Vec<Selections>,
    selection_redo: Vec<Selections>,
    select_next: Option<SelectNextState>,
    add_selections: Option<AddSelectionsState>,
    /// Selections before each `SelectLargerSyntaxNode`, oldest first.
    expand_stack: Vec<Selections>,
    /// The selections the last expand or shrink left; the stack is only valid while the
    /// selections still equal this.
    expand_result: Option<Selections>,
    /// The last selection change was a drag step, so the next one replaces it in history.
    dragging: bool,
}

impl Default for Editor {
    fn default() -> Editor {
        Editor::new(Buffer::new())
    }
}

impl Editor {
    /// An editor for `buffer` with default settings and a single cursor at the start.
    pub fn new(buffer: Buffer) -> Editor {
        Editor::with_settings(buffer, EditSettings::default())
    }

    /// An editor for `buffer` with the given settings and a single cursor at the start.
    pub fn with_settings(buffer: Buffer, settings: EditSettings) -> Editor {
        Editor {
            buffer,
            selections: Selections::default(),
            settings,
            kill_ring: None,
            selection_undo: Vec::new(),
            selection_redo: Vec::new(),
            select_next: None,
            add_selections: None,
            expand_stack: Vec::new(),
            expand_result: None,
            dragging: false,
        }
    }

    /// An editor holding `text`, with no file.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(text: &str) -> Editor {
        Editor::new(Buffer::from_str(text))
    }

    /// An editor for the file at `path` (see [`Buffer::from_file`]).
    pub fn open(path: impl AsRef<Path>) -> Result<Editor, BufferError> {
        Ok(Editor::new(Buffer::from_file(path)?))
    }

    /// The buffer. Read-only: every change goes through the editor so the selections stay
    /// in step with the text.
    pub fn buffer(&self) -> &Buffer {
        &self.buffer
    }

    /// Give up the editor and keep its buffer.
    pub fn into_buffer(self) -> Buffer {
        self.buffer
    }

    /// The selections.
    pub fn selections(&self) -> &Selections {
        &self.selections
    }

    /// The settings.
    pub fn settings(&self) -> &EditSettings {
        &self.settings
    }

    /// The settings, to change them. Changes apply from the next command on.
    pub fn settings_mut(&mut self) -> &mut EditSettings {
        &mut self.settings
    }

    /// What `KillRingCut` took last, if anything. The app may copy it to another editor's
    /// kill ring to share one between editors.
    pub fn kill_ring(&self) -> Option<&Clipboard> {
        self.kill_ring.as_ref()
    }

    /// Replace the kill ring's contents.
    pub fn set_kill_ring(&mut self, kill_ring: Option<Clipboard>) {
        self.kill_ring = kill_ring;
    }

    // ----- small helpers -------------------------------------------------------------

    pub(crate) fn tab_size(&self) -> usize {
        self.settings.tab_size.max(1)
    }

    /// The wrap width when soft wrap is on.
    pub(crate) fn wrap_width(&self) -> Option<usize> {
        self.settings
            .wrap_width
            .filter(|&width| width > 0 && width < usize::MAX)
    }

    /// One level of indentation as text.
    pub(crate) fn indent_unit(&self) -> String {
        if self.settings.hard_tabs {
            "\t".to_string()
        } else {
            " ".repeat(self.tab_size())
        }
    }

    pub(crate) fn word_chars(&self) -> &str {
        &self.settings.word_chars
    }

    /// Line `line` with its soft-wrap rows.
    pub(crate) fn layout(&self, line: usize) -> LineLayout<'_> {
        let text = self.buffer.line_text(line);
        let rows = match self.wrap_width() {
            Some(width) => display::wrap_line(&text, width, self.tab_size()),
            None => std::iter::once(0..self.buffer.line_len(line)).collect(),
        };
        LineLayout { text, rows }
    }

    /// Clamp the selections to the text and pull any end that sits inside a grapheme
    /// cluster back to the start of the cluster.
    fn normalize_selections(&mut self) {
        let rope = self.buffer.rope();
        let len = rope.len_chars();
        let fine = self.selections.iter().all(|selection| {
            selection.end() <= len
                && graphemes::is_boundary(rope, selection.anchor)
                && graphemes::is_boundary(rope, selection.head)
        });
        if fine {
            return;
        }
        let mut selections = self.selections.clone();
        selections.transform(|selection| Selection {
            anchor: graphemes::floor_boundary(rope, selection.anchor),
            head: graphemes::floor_boundary(rope, selection.head),
            goal: selection.goal,
        });
        self.selections = selections;
    }

    /// Run `f` as one undo step that remembers the selections before and after.
    pub(crate) fn transact(
        &mut self,
        kind: UndoKind,
        f: impl FnOnce(&mut Editor) -> EditSummary,
    ) -> EditSummary {
        self.buffer.begin_transaction(kind, Some(&self.selections));
        let summary = f(self);
        self.normalize_selections();
        self.buffer.end_transaction(Some(&self.selections));
        summary
    }

    /// Forget the state that only makes sense while one kind of command is repeated.
    fn reset_transient_state(&mut self, command: &Command) {
        let continues_select_next = matches!(
            command,
            Command::SelectNext { .. } | Command::SelectPrevious { .. } | Command::SelectAllMatches
        );
        if !continues_select_next {
            self.select_next = None;
        }
        if !matches!(
            command,
            Command::AddSelectionAbove { .. } | Command::AddSelectionBelow { .. }
        ) {
            self.add_selections = None;
        }
        if !matches!(
            command,
            Command::SelectLargerSyntaxNode | Command::SelectSmallerSyntaxNode
        ) {
            self.expand_stack.clear();
            self.expand_result = None;
        }
        // Typed characters and single-step deletions may merge into the previous undo step;
        // anything else ends the run.
        if !matches!(
            command,
            Command::Insert(_) | Command::Backspace | Command::Delete
        ) {
            self.buffer.break_undo_group();
        }
        self.dragging = false;
    }

    /// Record `before` in the selection history when the selections changed.
    fn record_selection_change(&mut self, before: Selections) -> bool {
        if before == self.selections {
            return false;
        }
        self.selection_undo.push(before);
        self.selection_redo.clear();
        self.trim_selection_history();
        true
    }

    fn trim_selection_history(&mut self) {
        let mut total: usize = self.selection_undo.iter().map(Selections::len).sum();
        let mut drop = 0;
        while total > SELECTION_HISTORY_BUDGET && drop < self.selection_undo.len() {
            total -= self.selection_undo[drop].len();
            drop += 1;
        }
        self.selection_undo.drain(..drop);
    }

    /// Keep the remembered selections pointing at the same text after an edit.
    fn map_selection_history(&mut self, summary: &EditSummary) {
        if summary.is_empty() {
            return;
        }
        for selections in self
            .selection_undo
            .iter_mut()
            .chain(self.selection_redo.iter_mut())
        {
            selections.map_through(summary);
        }
    }

    /// Finish a command or an outside change: tidy the selections, keep the selection
    /// history, and fill in the outcome.
    fn finish(
        &mut self,
        before: Selections,
        edits: EditSummary,
        clipboard: Option<Clipboard>,
    ) -> Outcome {
        self.normalize_selections();
        let selections_changed = before != self.selections;
        self.map_selection_history(&edits);
        // Selections that only moved along with the text around them (typing pushes the
        // cursor forward) are not worth a history entry of their own.
        let mut carried = before;
        carried.map_through(&edits);
        self.record_selection_change(carried);
        Outcome {
            edits,
            selections_changed,
            clipboard,
        }
    }

    // ----- commands ------------------------------------------------------------------

    /// Run one command and report what it changed.
    ///
    /// Every text change is an undo step that remembers the selections around it.
    /// Consecutive `Insert`s merge into one step while they arrive within 300 ms of each
    /// other and continue where the last one stopped; so do consecutive `Backspace`s and
    /// consecutive `Delete`s. Any other command starts a new step.
    pub fn apply(&mut self, command: Command) -> Outcome {
        let before = self.selections.clone();
        self.reset_transient_state(&command);
        let mut clipboard = None;
        let edits = match command {
            Command::Move(motion) => {
                self.apply_motion(&motion, false);
                EditSummary::new()
            }
            Command::Select(motion) => {
                self.apply_motion(&motion, true);
                EditSummary::new()
            }
            Command::SelectAll => {
                self.selections = Selections::single(Selection::new(0, self.buffer.len_chars()));
                EditSummary::new()
            }
            Command::SelectLine => self.without_edit(Editor::select_line),
            Command::SelectNext { replace_newest } => {
                self.select_next_match(replace_newest, false);
                EditSummary::new()
            }
            Command::SelectPrevious { replace_newest } => {
                self.select_next_match(replace_newest, true);
                EditSummary::new()
            }
            Command::SelectAllMatches => self.without_edit(Editor::select_all_matches),
            Command::AddSelectionAbove { skip_soft_wrap } => {
                self.add_selection_in_direction(true, skip_soft_wrap);
                EditSummary::new()
            }
            Command::AddSelectionBelow { skip_soft_wrap } => {
                self.add_selection_in_direction(false, skip_soft_wrap);
                EditSummary::new()
            }
            Command::UndoSelection => return self.undo_selection(),
            Command::RedoSelection => return self.redo_selection(),
            Command::Cancel => self.without_edit(Editor::cancel),
            Command::SelectLargerSyntaxNode => self.without_edit(Editor::select_larger),
            Command::SelectSmallerSyntaxNode => self.without_edit(Editor::select_smaller),
            Command::SelectBetween(kind) => {
                self.select_between_primary(kind);
                EditSummary::new()
            }
            Command::Insert(text) => self.transact(UndoKind::Typing, |editor| editor.insert(&text)),
            Command::Newline => self.transact(UndoKind::Other, Editor::newline),
            Command::NewlineAbove => {
                self.transact(UndoKind::Other, |editor| editor.newline_beside(true))
            }
            Command::NewlineBelow => {
                self.transact(UndoKind::Other, |editor| editor.newline_beside(false))
            }
            Command::Backspace => self.transact(UndoKind::Backspace, Editor::backspace),
            Command::Delete => self.transact(UndoKind::Delete, Editor::delete_forward),
            Command::DeleteToPreviousWordStart {
                ignore_newlines,
                ignore_brackets,
            } => self.transact(UndoKind::Other, |editor| {
                editor.delete_word(false, false, ignore_newlines, ignore_brackets)
            }),
            Command::DeleteToNextWordEnd {
                ignore_newlines,
                ignore_brackets,
            } => self.transact(UndoKind::Other, |editor| {
                editor.delete_word(true, false, ignore_newlines, ignore_brackets)
            }),
            Command::DeleteToPreviousSubwordStart {
                ignore_newlines,
                ignore_brackets,
            } => self.transact(UndoKind::Other, |editor| {
                editor.delete_word(false, true, ignore_newlines, ignore_brackets)
            }),
            Command::DeleteToNextSubwordEnd {
                ignore_newlines,
                ignore_brackets,
            } => self.transact(UndoKind::Other, |editor| {
                editor.delete_word(true, true, ignore_newlines, ignore_brackets)
            }),
            Command::DeleteToBeginningOfLine { stop_at_indent } => self
                .transact(UndoKind::Other, |editor| {
                    editor.delete_to_beginning_of_line(stop_at_indent)
                }),
            Command::DeleteToEndOfLine => {
                self.transact(UndoKind::Other, Editor::delete_to_end_of_line)
            }
            Command::DeleteLine => self.transact(UndoKind::Other, Editor::delete_line),
            Command::Tab => self.transact(UndoKind::Other, Editor::tab),
            Command::Indent => self.transact(UndoKind::Other, Editor::indent_lines),
            Command::Backtab | Command::Outdent => {
                self.transact(UndoKind::Other, Editor::outdent_lines)
            }
            Command::MoveLineUp => self.transact(UndoKind::Other, |editor| editor.move_lines(true)),
            Command::MoveLineDown => {
                self.transact(UndoKind::Other, |editor| editor.move_lines(false))
            }
            Command::DuplicateLineUp => {
                self.transact(UndoKind::Other, |editor| editor.duplicate_lines(true))
            }
            Command::DuplicateLineDown => {
                self.transact(UndoKind::Other, |editor| editor.duplicate_lines(false))
            }
            Command::JoinLines => self.transact(UndoKind::Other, Editor::join_lines),
            Command::Transpose => self.transact(UndoKind::Other, Editor::transpose),
            Command::ToggleComments {
                prefix,
                comment_empty_lines,
                advance_downwards,
            } => self.transact(UndoKind::Other, |editor| {
                editor.toggle_comments(&prefix, comment_empty_lines, advance_downwards)
            }),
            Command::ToggleBlockComments { start, end } => self
                .transact(UndoKind::Other, |editor| {
                    editor.toggle_block_comments(&start, &end)
                }),
            Command::Copy => {
                clipboard = Some(self.copy_selections().0).filter(|clip| !clip.is_empty());
                EditSummary::new()
            }
            Command::Cut => {
                let (clip, ranges) = self.copy_selections();
                clipboard = Some(clip).filter(|clip| !clip.is_empty());
                self.transact(UndoKind::Other, |editor| editor.delete_ranges(ranges))
            }
            Command::Paste(clip) => self.transact(UndoKind::Other, |editor| editor.paste(&clip)),
            Command::KillRingCut => self.transact(UndoKind::Other, Editor::kill_ring_cut),
            Command::KillRingYank => match self.kill_ring.clone() {
                Some(clip) => self.transact(UndoKind::Other, |editor| editor.paste(&clip)),
                None => EditSummary::new(),
            },
            Command::Undo => self.undo(),
            Command::Redo => self.redo(),
        };
        self.finish(before, edits, clipboard)
    }

    /// Run a command that only changes selections.
    fn without_edit(&mut self, f: impl FnOnce(&mut Editor)) -> EditSummary {
        f(self);
        EditSummary::new()
    }

    fn undo(&mut self) -> EditSummary {
        match self.buffer.undo() {
            Some(step) => {
                match step.selections {
                    Some(selections) => self.selections = selections,
                    None => self.selections.map_through(&step.edits),
                }
                step.edits
            }
            None => EditSummary::new(),
        }
    }

    fn redo(&mut self) -> EditSummary {
        match self.buffer.redo() {
            Some(step) => {
                match step.selections {
                    Some(selections) => self.selections = selections,
                    None => self.selections.map_through(&step.edits),
                }
                step.edits
            }
            None => EditSummary::new(),
        }
    }

    /// Go back to the newest remembered selections that differ from the current ones.
    fn undo_selection(&mut self) -> Outcome {
        let original = self.selections.clone();
        while let Some(previous) = self.selection_undo.pop() {
            self.selections = previous;
            self.normalize_selections();
            if self.selections != original {
                self.selection_redo.push(original);
                return Outcome {
                    selections_changed: true,
                    ..Outcome::default()
                };
            }
        }
        self.selections = original;
        Outcome::default()
    }

    /// Go forward again to the newest undone selections that differ from the current ones.
    fn redo_selection(&mut self) -> Outcome {
        let original = self.selections.clone();
        while let Some(next) = self.selection_redo.pop() {
            self.selections = next;
            self.normalize_selections();
            if self.selections != original {
                self.selection_undo.push(original);
                return Outcome {
                    selections_changed: true,
                    ..Outcome::default()
                };
            }
        }
        self.selections = original;
        Outcome::default()
    }

    /// Select the text between the nearest pair of delimiters of `kind` around the primary
    /// cursor; when exactly that is already selected, grow to the next enclosing pair; when
    /// the cursor is in no such pair, jump just inside the first pair of the text. The same
    /// as applying [`Command::SelectBetween`].
    pub fn select_between(&mut self, kind: PairKind) -> Outcome {
        self.apply(Command::SelectBetween(kind))
    }

    // ----- selections set from outside (mouse, pickers) ------------------------------

    /// Replace the selections, for a click or a jump. Positions are clamped to the text and
    /// moved off the inside of grapheme clusters. The change goes into the selection
    /// history and starts a new undo step for whatever is typed next.
    pub fn set_selections(&mut self, selections: Selections) -> Outcome {
        let before = self.selections.clone();
        self.reset_after_outside_change();
        self.selections = selections;
        self.finish(before, EditSummary::new(), None)
    }

    /// Put a single cursor at `pos`.
    pub fn set_cursor(&mut self, pos: usize) -> Outcome {
        self.set_selections(Selections::single(Selection::cursor(pos)))
    }

    /// Select from `anchor` to `head` as the only selection.
    pub fn select_range(&mut self, anchor: usize, head: usize) -> Outcome {
        self.set_selections(Selections::single(Selection::new(anchor, head)))
    }

    /// Add a selection from `anchor` to `head` (a cursor when they are equal) to the ones
    /// there are. It becomes the primary selection.
    pub fn add_selection(&mut self, anchor: usize, head: usize) -> Outcome {
        let mut selections = self.selections.clone();
        selections.push(Selection::new(anchor, head));
        self.set_selections(selections)
    }

    /// Move the head of the primary selection to `head`, keeping its anchor: one step of a
    /// mouse drag or a shift-click. A run of these calls counts as a single entry in the
    /// selection history.
    pub fn extend_primary_to(&mut self, head: usize) -> Outcome {
        let before = self.selections.clone();
        let was_dragging = self.dragging;
        self.reset_after_outside_change();
        let mut selections = self.selections.clone();
        let primary = selections.primary();
        selections.replace_primary(primary.with_head(head, None));
        self.selections = selections;
        self.normalize_selections();
        let changed = before != self.selections;
        if changed && !was_dragging {
            self.record_selection_change(before);
        }
        self.dragging = true;
        Outcome {
            selections_changed: changed,
            ..Outcome::default()
        }
    }

    fn reset_after_outside_change(&mut self) {
        self.select_next = None;
        self.add_selections = None;
        self.expand_stack.clear();
        self.expand_result = None;
        self.dragging = false;
        self.buffer.break_undo_group();
    }

    // ----- changes from outside commands ---------------------------------------------

    /// Run an outside change to the buffer as one undo step and move the selections
    /// through it.
    fn outside_edit(&mut self, f: impl FnOnce(&mut Buffer) -> EditSummary) -> Outcome {
        let before = self.selections.clone();
        self.reset_after_outside_change();
        self.buffer
            .begin_transaction(UndoKind::Other, Some(&before));
        let edits = f(&mut self.buffer);
        self.selections.map_through(&edits);
        self.normalize_selections();
        self.buffer.end_transaction(Some(&self.selections));
        self.finish(before, edits, None)
    }

    /// Replace several ranges at once (see [`Buffer::edit`]), as one undo step. The
    /// selections move with the text around them.
    pub fn edit<S: AsRef<str>>(&mut self, edits: &[(Range<usize>, S)]) -> Outcome {
        self.outside_edit(|buffer| buffer.edit(edits))
    }

    /// Replace every match of `search` in the buffer by `template` (see
    /// [`Search::replacement`]), as one undo step.
    pub fn replace_all(&mut self, search: &Search, template: &str) -> Outcome {
        self.outside_edit(|buffer| search.replace_all(buffer, 0..usize::MAX, template))
    }

    /// Replace the first match of `search` at or after `from` (wrapping around when `wrap`
    /// is set) by `template`, as one undo step. Returns the range the replacement text
    /// occupies, or `None` when nothing matched.
    pub fn replace_next(
        &mut self,
        search: &Search,
        from: usize,
        wrap: bool,
        template: &str,
    ) -> (Option<Range<usize>>, Outcome) {
        let mut replaced = None;
        let outcome =
            self.outside_edit(
                |buffer| match search.replace_next(buffer, from, wrap, template) {
                    Some((range, summary)) => {
                        replaced = Some(range);
                        summary
                    }
                    None => EditSummary::new(),
                },
            );
        (replaced, outcome)
    }

    /// Save the buffer to its path (see [`Buffer::save`]). The selections follow the
    /// clean-up edits (trailing whitespace, final newline) when there are any.
    pub fn save(&mut self, options: SaveOptions) -> Result<Outcome, BufferError> {
        let before = self.selections.clone();
        self.reset_after_outside_change();
        let (edits, selections) = self.buffer.save_with_selections(options, Some(&before))?;
        if let Some(selections) = selections {
            self.selections = selections;
        }
        Ok(self.finish(before, edits, None))
    }

    /// Save the buffer to `path` and make that its path (see [`Buffer::save_as`]).
    pub fn save_as(
        &mut self,
        path: impl Into<PathBuf>,
        options: SaveOptions,
    ) -> Result<Outcome, BufferError> {
        let before = self.selections.clone();
        self.reset_after_outside_change();
        let (edits, selections) =
            self.buffer
                .save_as_with_selections(path.into(), options, Some(&before))?;
        if let Some(selections) = selections {
            self.selections = selections;
        }
        Ok(self.finish(before, edits, None))
    }

    /// Reload the buffer from disk (see [`Buffer::reload`]). Only the changed parts of the
    /// text are edited, and the selections stay with the text they were in. The reload is
    /// one undo step; undoing it also restores the selections.
    pub fn reload(&mut self) -> Result<Outcome, BufferError> {
        let before = self.selections.clone();
        self.reset_after_outside_change();
        let (edits, selections) = self.buffer.reload_with_selections(Some(&before))?;
        if let Some(selections) = selections {
            self.selections = selections;
        }
        Ok(self.finish(before, edits, None))
    }

    /// Point the buffer at a different file without reading or writing anything.
    pub fn set_path(&mut self, path: Option<PathBuf>) {
        self.buffer.set_path(path);
    }

    /// The file was moved or renamed to `path`: see [`Buffer::follow_move`].
    pub fn follow_move(&mut self, path: PathBuf) {
        self.buffer.follow_move(path);
    }

    /// Change the line ending written on save.
    pub fn set_line_ending(&mut self, line_ending: LineEnding) {
        self.buffer.set_line_ending(line_ending);
    }

    /// Choose whether a UTF-8 byte order mark is written on save.
    pub fn set_bom(&mut self, has_bom: bool) {
        self.buffer.set_bom(has_bom);
    }

    /// Replace the time source used to group typed characters into undo steps.
    pub fn set_clock(&mut self, clock: Arc<dyn Clock>) {
        self.buffer.set_clock(clock);
    }

    /// How many steps there are to undo (see [`Editor::squash_undo_steps`]).
    pub fn undo_steps(&self) -> usize {
        self.buffer.undo_steps()
    }

    /// Make the undo steps above the first `keep` one step: remember
    /// [`Editor::undo_steps`] when a run of editing starts and squash at its end, and one
    /// undo takes the whole run back. Returns false when there was nothing to fuse.
    pub fn squash_undo_steps(&mut self, keep: usize) -> bool {
        self.buffer.squash_undo_steps(keep)
    }

    /// Make the next edit start a new undo step even if it would otherwise merge with the
    /// previous one.
    pub fn break_undo_group(&mut self) {
        self.buffer.break_undo_group();
    }

    // ----- queries for the view ------------------------------------------------------

    /// The bracket pair to highlight for a cursor at `pos`: the offsets of the opening and
    /// the closing bracket of the pair that has a bracket right after or right before the
    /// cursor (see [`pairs::matching_bracket`]).
    pub fn matching_bracket(&self, pos: usize) -> Option<(usize, usize)> {
        pairs::matching_bracket(&self.buffer, pos)
    }

    /// The word at `pos`: the run of word characters the position is in or next to. `None`
    /// when there is no word character on either side.
    pub fn word_at(&self, pos: usize) -> Option<Range<usize>> {
        let (range, kind) = chars::surrounding_run(self.buffer.rope(), pos, self.word_chars());
        (kind == CharKind::Word && !range.is_empty()).then_some(range)
    }

    /// The display width of line `line`'s leading whitespace, with the editor's tab size.
    pub fn indent_of_line(&self, line: usize) -> usize {
        indent::indent_of_line(&self.buffer, line, self.tab_size()).width
    }

    /// Guess the buffer's indentation style from its text (see [`indent::detect_indent`]).
    pub fn detect_indent(&self) -> Option<IndentStyle> {
        indent::detect_indent(&self.buffer)
    }

    /// The line ending the buffer is saved with.
    pub fn line_ending(&self) -> LineEnding {
        self.buffer.line_ending()
    }

    /// The soft-wrap rows of line `line` under the current settings, as char ranges of the
    /// line (see [`display::wrap_line`]). One row covering the line when wrapping is off.
    pub fn line_rows(&self, line: usize) -> Vec<Range<usize>> {
        self.layout(line).rows
    }

    /// Where `pos` is drawn: its line, soft-wrap row within the line, and display column.
    pub fn display_position(&self, pos: usize) -> DisplayPosition {
        let point = self.buffer.char_to_point(pos);
        let layout = self.layout(point.line);
        let (row, col) =
            display::char_col_to_row_col(&layout.text, &layout.rows, point.col, self.tab_size());
        DisplayPosition {
            line: point.line,
            row,
            col,
        }
    }

    /// The char offset drawn at a line, soft-wrap row and display column: the inverse of
    /// [`Editor::display_position`], for turning a mouse click into a position. Everything
    /// is clamped: a line past the end is the last line, a row past the end the last row,
    /// and a column past the end of a row the last position on that row.
    pub fn position_at(&self, position: DisplayPosition) -> usize {
        let line = position.line.min(self.buffer.len_lines() - 1);
        let layout = self.layout(line);
        let col = display::row_col_to_char_col(
            &layout.text,
            &layout.rows,
            position.row,
            position.col,
            self.tab_size(),
        );
        self.buffer.line_to_char(line) + col
    }
}
