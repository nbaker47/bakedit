//! Actions and typed keys: every `editor::` action through `bakedit_text::Command`, nav and
//! edit mode (`vim::*`), scrolling, toggles, and what typing does.

use std::cell::RefCell;

use bakedit_core::{Action, KeyEvent};
use bakedit_text::{Clipboard, ClipboardPiece, Command, Motion, Selection, Selections};

use super::layout::Reveal;
use super::{EditorItem, Visual};
use crate::view::{Context, Cx};

thread_local! {
    /// What this editor last cut or copied, in full (line-wise, one piece per cursor), to
    /// paste back when the system clipboard still holds the same text.
    static LAST_COPY: RefCell<Option<Clipboard>> = const { RefCell::new(None) };
}

/// Remember what was just put on the clipboard.
pub(super) fn remember_copy(clip: Clipboard) {
    LAST_COPY.with(|last| *last.borrow_mut() = Some(clip));
}

/// The clipboard as a paste should see it.
fn clipboard_for_paste(cx: &mut Cx<'_>) -> Option<Clipboard> {
    let clip = cx.clipboard.get()?;
    let remembered = LAST_COPY.with(|last| last.borrow().clone()).filter(|known| known.text == clip.text);
    Some(match remembered {
        Some(known) => known,
        None if clip.linewise => Clipboard { text: clip.text.clone(), linewise: true, pieces: vec![ClipboardPiece { text: clip.text, linewise: true }] },
        None => Clipboard::plain(clip.text),
    })
}

/// Whether a command only moves the cursor or selection.
fn is_motion(command: &Command) -> bool {
    matches!(command, Command::Move(_) | Command::Select(_))
}

impl EditorItem {
    pub(super) fn key_contexts(&self, out: &mut Vec<Context>) {
        if self.find_focused() {
            let in_replace = self.find.as_ref().is_some_and(|bar| bar.focus == super::find::FindFocus::Replace);
            out.push(Context::new("BufferSearchBar").flag_if(in_replace, "in_replace"));
            out.push(Context::new("Editor").value("mode", "single_line"));
            return;
        }
        let mut context = Context::new("Editor").value("mode", "full");
        if self.modal {
            context = context.value("vim_mode", if self.insert { "insert" } else { "normal" });
        }
        if let Some(extension) = self.editor.buffer().path().and_then(|p| p.extension()) {
            context = context.value("extension", extension.to_string_lossy().into_owned());
        }
        out.push(context);
    }

    /// Nav mode with no selection being made (`v`): what holds there is one cursor per
    /// place, never a selection.
    pub(super) fn nav_cursor_only(&self) -> bool {
        self.modal && !self.insert && self.selecting.is_none()
    }

    /// Nav mode: the cursor sits on a character, not past the end of the line. The column
    /// vertical motion is heading for is kept.
    pub(super) fn clamp_nav_cursor(&mut self) {
        if !self.modal || self.insert || self.selecting.is_some() {
            return;
        }
        let selections = self.editor.selections();
        if !selections.is_single_cursor() {
            return;
        }
        let selection = selections.primary();
        let buffer = self.editor.buffer();
        let line = buffer.char_to_line(selection.head);
        if buffer.line_len(line) == 0 || selection.head != buffer.line_end(line) {
            return;
        }
        let goal = selection.goal.unwrap_or_else(|| self.editor.display_position(selection.head).col);
        let back = buffer.prev_grapheme(selection.head);
        self.editor.set_selections(Selections::single(Selection::cursor(back).with_goal(Some(goal))));
    }

    fn enter_insert(&mut self) {
        self.insert = true;
        self.selecting = None;
        self.editor.break_undo_group();
    }

    /// Handle every action this view owns. False lets the next candidate or the shell have it.
    pub(super) fn act(&mut self, action: &Action, cx: &mut Cx<'_>) -> bool {
        self.sync_engine(cx.settings);
        if let Some(handled) = self.search_action(action, cx) {
            return handled;
        }
        let name = action.name.as_str();
        if self.find_focused() && name.starts_with("editor::") {
            return self.find_input_action(action, cx);
        }
        let handled = match name {
            "workspace::SaveAsPath" => match action.arg_str("path") {
                Some(path) => self.save_as(path.into(), cx),
                None => false,
            },
            "editor::ReloadFromDisk" => self.reload_from_disk(cx),
            "editor::Format" => self.format_now(cx),
            "editor::CopyPermalinkToLine" => self.copy_permalink(cx),
            "editor::ToggleSoftWrap" => {
                self.wrap_override = Some(!self.wrapping());
                self.left = 0;
                self.reveal = Reveal::Fit;
                true
            }
            "editor::ToggleLineNumbers" => {
                self.numbers_override = Some(!self.numbers_override.unwrap_or(cx.settings.line_numbers));
                true
            }
            "editor::ToggleRelativeLineNumbers" => {
                self.relative_override = Some(!self.relative_override.unwrap_or(cx.settings.relative_line_numbers));
                true
            }
            "editor::ToggleMinimap" => {
                self.minimap_override = Some(!self.minimap_shown(cx.settings, self.geo.area.width));
                true
            }
            "editor::ScrollCursorCenter" => {
                self.reveal = Reveal::Center;
                true
            }
            "editor::LineDown" => {
                self.scroll_by(1);
                true
            }
            "editor::LineUp" => {
                self.scroll_by(-1);
                true
            }
            "editor::PageDown" => {
                self.scroll_by(self.geo.text.height.max(1) as isize);
                true
            }
            "editor::PageUp" => {
                self.scroll_by(-(self.geo.text.height.max(1) as isize));
                true
            }
            n if n.starts_with("vim::") => self.vim_action(n, cx),
            "editor::MoveToNextWordEnd" if self.nav_cursor_only() => self.nav_word_end(action, cx),
            _ => self.run_action_command(action, cx),
        };
        if handled && self.nav_cursor_only() && matches!(name, "editor::Undo" | "editor::Redo") {
            // As in vim: what came back is not left selected; the cursor goes to its start.
            let mut selections = self.editor.selections().clone();
            selections.transform(|selection| Selection::cursor(selection.start()));
            self.editor.set_selections(selections);
        }
        if handled {
            self.clamp_nav_cursor();
        }
        handled
    }

    /// `e` in nav mode, as in vim: the block lands on the last character of the word, and
    /// from there `e` goes on to the end of the next word. (The text engine's word end is the
    /// boundary after the word, where edit mode's bar cursor belongs.)
    fn nav_word_end(&mut self, action: &Action, cx: &mut Cx<'_>) -> bool {
        let before = self.cursor();
        self.run_command(Command::Move(Motion::Right), cx);
        self.run_action_command(action, cx);
        if self.cursor() > before + 1 {
            self.run_command(Command::Move(Motion::Left), cx);
        }
        true
    }

    fn vim_action(&mut self, name: &str, cx: &mut Cx<'_>) -> bool {
        match name {
            "vim::InsertBefore" => self.enter_insert(),
            "vim::InsertAfter" => {
                let buffer = self.editor.buffer();
                let head = self.cursor();
                if head < buffer.line_end(buffer.char_to_line(head)) {
                    self.run_command(Command::Move(Motion::Right), cx);
                }
                self.enter_insert();
            }
            "vim::InsertFirstNonWhitespace" => {
                self.run_command(Command::Move(Motion::FirstNonBlank), cx);
                self.enter_insert();
            }
            "vim::InsertEndOfLine" => {
                self.run_command(Command::Move(Motion::EndOfLine { stop_at_soft_wraps: false }), cx);
                self.enter_insert();
            }
            "vim::InsertLineBelow" => {
                if !self.run_command(Command::NewlineBelow, cx) {
                    return true;
                }
                self.enter_insert();
            }
            "vim::InsertLineAbove" => {
                if !self.run_command(Command::NewlineAbove, cx) {
                    return true;
                }
                self.enter_insert();
            }
            "vim::NormalBefore" => {
                self.insert = false;
                self.selecting = None;
                self.editor.break_undo_group();
                let buffer = self.editor.buffer();
                let head = self.cursor();
                if self.editor.selections().is_single_cursor() && head > buffer.line_to_char(buffer.char_to_line(head)) {
                    self.editor.set_cursor(buffer.prev_grapheme(head));
                }
            }
            "vim::ToggleVisual" => self.toggle_visual(false),
            "vim::ToggleVisualLine" => self.toggle_visual(true),
            "vim::ScrollDown" | "vim::ScrollUp" => {
                let lines = (self.geo.text.height as usize / 2).max(1);
                let motion = if name == "vim::ScrollDown" { Motion::DownByLines { lines } } else { Motion::UpByLines { lines } };
                self.run_command(Command::Move(motion), cx);
            }
            _ => return false,
        }
        true
    }

    fn toggle_visual(&mut self, line_wise: bool) {
        if !self.modal || self.insert {
            return;
        }
        let head = self.cursor();
        match (self.selecting, line_wise) {
            (Some(Visual::Char), false) | (Some(Visual::Line { .. }), true) => {
                self.selecting = None;
                self.editor.set_cursor(head);
            }
            (_, true) => {
                let line = self.cursor_line();
                self.selecting = Some(Visual::Line { anchor_line: line, cursor: head });
                self.select_lines();
            }
            (_, false) => {
                let buffer = self.editor.buffer();
                let end = if head < buffer.line_end(buffer.char_to_line(head)) { buffer.next_grapheme(head) } else { head };
                self.selecting = Some(Visual::Char);
                self.editor.select_range(head, end);
            }
        }
    }

    /// Select whole lines from the anchor line to the one the moving end is on.
    fn select_lines(&mut self) {
        let Some(Visual::Line { anchor_line, cursor }) = self.selecting else { return };
        let buffer = self.editor.buffer();
        let line = buffer.char_to_line(cursor);
        let (low, high) = (anchor_line.min(line), anchor_line.max(line));
        let start = buffer.line_to_char(low);
        let end = if high + 1 < buffer.len_lines() { buffer.line_to_char(high + 1) } else { buffer.len_chars() };
        if line < anchor_line {
            self.editor.select_range(end, start);
        } else {
            self.editor.select_range(start, end);
        }
        self.reveal = Reveal::Fit;
    }

    /// An `editor::` action that is a text command.
    fn run_action_command(&mut self, action: &Action, cx: &mut Cx<'_>) -> bool {
        let name = action.name.as_str();
        let Some(mut command) = Command::from_action(name, action.args.as_ref()) else { return false };
        if name == "editor::Paste" {
            let Some(clipboard) = clipboard_for_paste(cx) else { return true };
            command = Command::Paste(clipboard);
        }
        let center = action.arg_bool("center_cursor").unwrap_or(false);
        let ran = self.run_command(command, cx);
        if ran && center {
            self.reveal = Reveal::Center;
        }
        true
    }

    /// Run one command on the text: the language's comment markers and the page size filled
    /// in, motions turned into selections while selecting, a read-only buffer refused.
    pub(super) fn run_command(&mut self, mut command: Command, cx: &mut Cx<'_>) -> bool {
        if command.modifies_text() && self.read_only_blocks(cx) {
            return false;
        }
        let language = self.language;
        command = command.with_page_rows(self.geo.text.height.max(1) as usize);
        match (language.line_comment, language.block_comment) {
            (Some(prefix), _) => command = command.with_line_comment(prefix),
            (None, Some((start, end))) => {
                if matches!(&command, Command::ToggleComments { prefix, .. } if prefix.is_empty()) {
                    command = Command::ToggleBlockComments { start: start.to_string(), end: end.to_string() };
                }
            }
            (None, None) => {}
        }
        if let Command::ToggleBlockComments { .. } = &command {
            if let Some((start, end)) = language.block_comment {
                command = command.with_block_comment(start, end);
            }
        }
        if is_motion(&command) {
            self.editor.break_undo_group();
        }
        let selecting = self.selecting;
        if let (Some(visual), true) = (selecting, matches!(command, Command::Move(_))) {
            return self.visual_motion(visual, command, cx);
        }
        let ends_selecting = selecting.is_some() && !is_motion(&command);
        let collapses = matches!(command, Command::Copy | Command::Cancel);
        let start = self.editor.selections().primary().start();
        let outcome = self.editor.apply(command);
        self.after_outcome(&outcome, cx);
        if ends_selecting {
            self.selecting = None;
            if collapses {
                self.editor.set_cursor(start);
            }
        }
        true
    }

    fn visual_motion(&mut self, visual: Visual, command: Command, cx: &mut Cx<'_>) -> bool {
        match visual {
            Visual::Char => {
                let outcome = self.editor.apply(command.extending());
                self.after_outcome(&outcome, cx);
            }
            Visual::Line { anchor_line, cursor } => {
                self.editor.set_cursor(cursor);
                let outcome = self.editor.apply(command);
                let cursor = self.cursor();
                self.after_outcome(&outcome, cx);
                self.selecting = Some(Visual::Line { anchor_line, cursor });
                self.select_lines();
            }
        }
        true
    }

    /// A key no binding took: typed text goes into the text (edit mode) or the find bar.
    pub(super) fn type_key(&mut self, key: &KeyEvent, cx: &mut Cx<'_>) -> bool {
        let Some(text) = key.text.as_deref().filter(|_| key.is_down()) else { return false };
        if text.chars().all(char::is_control) {
            return false;
        }
        if self.find_focused() {
            self.find_insert(text, cx);
            return true;
        }
        if self.modal && !self.insert {
            // Nav mode types nothing, and a letter that means nothing here is not an error.
            return true;
        }
        self.sync_engine(cx.settings);
        self.run_command(Command::Insert(text.to_string()), cx);
        true
    }

    /// Pasted text from the host terminal.
    pub(super) fn paste_text(&mut self, text: &str, cx: &mut Cx<'_>) -> bool {
        if self.find_focused() {
            self.find_insert(text, cx);
            return true;
        }
        if self.modal && !self.insert {
            return true;
        }
        self.run_command(Command::Paste(Clipboard::plain(text)), cx);
        true
    }
}
