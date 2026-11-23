//! Actions and typed keys: every `editor::` action through `bakedit_text::Command`, nav and
//! edit mode (`bakedit::ToggleMode`, `vim::NormalBefore`), scrolling, toggles, and what
//! typing does.

use std::cell::RefCell;

use bakedit_core::{Action, KeyEvent};
use bakedit_text::{Clipboard, ClipboardPiece, Command, Selection, Selections};

use super::layout::Reveal;
use super::EditorItem;
use crate::view::{Context, Cx, Effect};

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

    /// Nav mode.
    pub(super) fn in_nav_mode(&self) -> bool {
        self.modal && !self.insert
    }

    /// Nav mode: the cursor sits on a character, not past the end of the line. The column
    /// vertical motion is heading for is kept.
    pub(super) fn clamp_nav_cursor(&mut self) {
        if !self.in_nav_mode() {
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
        self.editor.break_undo_group();
    }

    /// Edit mode to nav mode, the cursor left where it is: what was typed becomes one undo
    /// step.
    fn leave_insert(&mut self) {
        self.insert = false;
        self.editor.break_undo_group();
        self.end_insert_run();
    }

    /// Switch nav and edit mode on or off for this editor (`vim_mode`). Either way it
    /// starts over in nav mode, or plain editing.
    pub(super) fn set_modal(&mut self, on: bool) {
        self.modal = on;
        self.end_insert_run();
        self.insert = false;
        self.clamp_nav_cursor();
    }

    /// `bakedit::ToggleMode` (cmd-i, or a click on the mode in the status bar): edit mode
    /// goes to nav mode as escape does, except that the cursor stays put; nav mode goes to
    /// edit mode, the cursor and any selection kept. With the modes off it switches them on
    /// for the session, which lands in nav mode.
    fn toggle_mode(&mut self, cx: &mut Cx<'_>) {
        if !self.modal {
            // The setting, not just this editor: the next tick would undo it otherwise.
            if !cx.settings.vim_mode {
                cx.push(Effect::Dispatch(Action::new("bakedit::ToggleEditMode")));
            }
            self.set_modal(true);
        } else if self.insert {
            self.leave_insert();
            // Nav mode starts with cursors, not a selection: each collapses onto its head
            // before the block is clamped onto a character.
            if !self.editor.selections().is_single_cursor() {
                let mut selections = self.editor.selections().clone();
                selections.transform(|selection| Selection::cursor(selection.head));
                self.editor.set_selections(selections);
            }
        } else {
            let mark = self.editor.undo_steps();
            self.enter_insert();
            self.insert_mark = Some(mark);
        }
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
            "bakedit::ToggleMode" => {
                self.toggle_mode(cx);
                true
            }
            "vim::NormalBefore" => {
                self.normal_before();
                true
            }
            _ => self.run_action_command(action, cx),
        };
        if handled && self.in_nav_mode() && matches!(name, "editor::Undo" | "editor::Redo") {
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

    /// Leave edit mode's undo run: everything typed since it began becomes one undo step.
    pub(super) fn end_insert_run(&mut self) {
        if let Some(mark) = self.insert_mark.take() {
            self.editor.squash_undo_steps(mark);
        }
    }

    /// Escape in edit mode: back to nav mode, the block stepping left onto the character
    /// before the bar, as vim does.
    fn normal_before(&mut self) {
        self.leave_insert();
        let buffer = self.editor.buffer();
        let head = self.cursor();
        if self.editor.selections().is_single_cursor() && head > buffer.line_to_char(buffer.char_to_line(head)) {
            self.editor.set_cursor(buffer.prev_grapheme(head));
        }
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
    /// in, a read-only buffer refused.
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
        let outcome = self.editor.apply(command);
        self.after_outcome(&outcome, cx);
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
