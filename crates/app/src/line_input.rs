//! A one-line text field: the query box of every picker, the search bars, rename prompts.
//!
//! It understands the same `editor::` action names as the real editor for the subset that
//! makes sense on one line, so the keymap needs no special cases for it.

use bakedit_core::Action;
use ratatui::layout::Rect;
use unicode_segmentation::UnicodeSegmentation;

use crate::ui::{self, Canvas, Style};

/// What an action did to the field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputOutcome {
    /// The action is not one a line input handles.
    Ignored,
    /// Only the cursor or selection moved (or nothing needed doing).
    Moved,
    /// The text changed.
    Edited,
    /// The text to put on the clipboard (copy or cut; cut also edits).
    Copied { text: String, edited: bool },
    /// The field wants the clipboard's text: call [`LineInput::insert`] with it.
    WantsPaste,
}

#[derive(Debug, Clone, Default)]
pub struct LineInput {
    text: String,
    /// Byte offset of the cursor, always on a grapheme boundary.
    cursor: usize,
    /// The other end of the selection, if there is one.
    anchor: Option<usize>,
    /// First display column shown, when the text is wider than the field.
    scroll: usize,
    undo: Vec<(String, usize)>,
    redo: Vec<(String, usize)>,
}

impl LineInput {
    pub fn new() -> LineInput {
        LineInput::default()
    }

    pub fn with_text(text: &str) -> LineInput {
        let mut input = LineInput::new();
        input.set_text(text);
        input
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Replace the text, put the cursor at the end, forget the selection and history.
    pub fn set_text(&mut self, text: &str) {
        self.text = single_line(text);
        self.cursor = self.text.len();
        self.anchor = None;
        self.scroll = 0;
        self.undo.clear();
        self.redo.clear();
    }

    pub fn clear(&mut self) {
        self.set_text("");
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = self.text.len();
    }

    /// The selected byte range, if any text is selected.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let anchor = self.anchor?;
        if anchor == self.cursor {
            None
        } else {
            Some((anchor.min(self.cursor), anchor.max(self.cursor)))
        }
    }

    pub fn selected_text(&self) -> Option<&str> {
        self.selection().map(|(start, end)| &self.text[start..end])
    }

    fn remember(&mut self) {
        self.undo.push((self.text.clone(), self.cursor));
        if self.undo.len() > 200 {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn delete_selection(&mut self) -> bool {
        match self.selection() {
            Some((start, end)) => {
                self.text.replace_range(start..end, "");
                self.cursor = start;
                self.anchor = None;
                true
            }
            None => {
                self.anchor = None;
                false
            }
        }
    }

    /// Type or paste text at the cursor, replacing the selection. Line breaks become spaces.
    pub fn insert(&mut self, text: &str) {
        let text = single_line(text);
        if text.is_empty() && self.selection().is_none() {
            return;
        }
        self.remember();
        self.delete_selection();
        self.text.insert_str(self.cursor, &text);
        self.cursor += text.len();
    }

    fn prev_boundary(&self, from: usize) -> usize {
        self.text[..from].grapheme_indices(true).next_back().map(|(i, _)| i).unwrap_or(0)
    }

    fn next_boundary(&self, from: usize) -> usize {
        self.text[from..].graphemes(true).next().map(|g| from + g.len()).unwrap_or(self.text.len())
    }

    /// Start of the word before `from`: skip spaces and punctuation, then the word.
    fn prev_word(&self, from: usize) -> usize {
        let mut index = from;
        let mut seen_word = false;
        for (i, grapheme) in self.text[..from].grapheme_indices(true).rev() {
            let is_word = grapheme.chars().all(is_word_char);
            if is_word {
                seen_word = true;
            } else if seen_word {
                break;
            }
            index = i;
        }
        index
    }

    /// End of the word after `from`.
    fn next_word(&self, from: usize) -> usize {
        let mut index = from;
        let mut seen_word = false;
        for (i, grapheme) in self.text[from..].grapheme_indices(true) {
            let is_word = grapheme.chars().all(is_word_char);
            if is_word {
                seen_word = true;
            } else if seen_word {
                break;
            }
            index = from + i + grapheme.len();
        }
        index
    }

    fn move_to(&mut self, position: usize, extend: bool) {
        if extend {
            if self.anchor.is_none() {
                self.anchor = Some(self.cursor);
            }
        } else {
            self.anchor = None;
        }
        self.cursor = position;
    }

    fn delete_to(&mut self, position: usize) -> InputOutcome {
        if self.selection().is_some() {
            self.remember();
            self.delete_selection();
            return InputOutcome::Edited;
        }
        if position == self.cursor {
            return InputOutcome::Moved;
        }
        self.remember();
        let (start, end) = (position.min(self.cursor), position.max(self.cursor));
        self.text.replace_range(start..end, "");
        self.cursor = start;
        self.anchor = None;
        InputOutcome::Edited
    }

    /// Apply an `editor::` action. Anything this field has no use for is `Ignored`, so the
    /// caller can try the next handler.
    pub fn handle_action(&mut self, action: &Action) -> InputOutcome {
        let Some(name) = action.name.strip_prefix("editor::") else {
            return InputOutcome::Ignored;
        };
        let end = self.text.len();
        match name {
            "MoveLeft" | "SelectLeft" => {
                let extend = name == "SelectLeft";
                let target = match self.selection() {
                    Some((start, _)) if !extend => start,
                    _ => self.prev_boundary(self.cursor),
                };
                self.move_to(target, extend);
            }
            "MoveRight" | "SelectRight" => {
                let extend = name == "SelectRight";
                let target = match self.selection() {
                    Some((_, end)) if !extend => end,
                    _ => self.next_boundary(self.cursor),
                };
                self.move_to(target, extend);
            }
            "MoveToPreviousWordStart" | "MoveToPreviousSubwordStart" => self.move_to(self.prev_word(self.cursor), false),
            "SelectToPreviousWordStart" | "SelectToPreviousSubwordStart" => self.move_to(self.prev_word(self.cursor), true),
            "MoveToNextWordEnd" | "MoveToNextSubwordEnd" => self.move_to(self.next_word(self.cursor), false),
            "SelectToNextWordEnd" | "SelectToNextSubwordEnd" => self.move_to(self.next_word(self.cursor), true),
            "MoveToBeginningOfLine" | "MoveToBeginning" => self.move_to(0, false),
            "SelectToBeginningOfLine" | "SelectToBeginning" => self.move_to(0, true),
            "MoveToEndOfLine" | "MoveToEnd" => self.move_to(end, false),
            "SelectToEndOfLine" | "SelectToEnd" => self.move_to(end, true),
            "SelectAll" => self.select_all(),
            "Backspace" => return self.delete_to(self.prev_boundary(self.cursor)),
            "Delete" => return self.delete_to(self.next_boundary(self.cursor)),
            "DeleteToPreviousWordStart" | "DeleteToPreviousSubwordStart" => return self.delete_to(self.prev_word(self.cursor)),
            "DeleteToNextWordEnd" | "DeleteToNextSubwordEnd" => return self.delete_to(self.next_word(self.cursor)),
            "DeleteToBeginningOfLine" => return self.delete_to(0),
            "DeleteToEndOfLine" | "KillRingCut" => return self.delete_to(end),
            "Copy" => {
                return match self.selected_text() {
                    Some(text) => InputOutcome::Copied { text: text.to_string(), edited: false },
                    None => InputOutcome::Moved,
                };
            }
            "Cut" => {
                return match self.selected_text().map(str::to_string) {
                    Some(text) => {
                        self.remember();
                        self.delete_selection();
                        InputOutcome::Copied { text, edited: true }
                    }
                    None => InputOutcome::Moved,
                };
            }
            "Paste" => return InputOutcome::WantsPaste,
            "Undo" => {
                return match self.undo.pop() {
                    Some((text, cursor)) => {
                        self.redo.push((std::mem::replace(&mut self.text, text), self.cursor));
                        self.cursor = cursor.min(self.text.len());
                        self.anchor = None;
                        InputOutcome::Edited
                    }
                    None => InputOutcome::Moved,
                };
            }
            "Redo" => {
                return match self.redo.pop() {
                    Some((text, cursor)) => {
                        self.undo.push((std::mem::replace(&mut self.text, text), self.cursor));
                        self.cursor = cursor.min(self.text.len());
                        self.anchor = None;
                        InputOutcome::Edited
                    }
                    None => InputOutcome::Moved,
                };
            }
            "Cancel" if self.selection().is_some() => self.anchor = None,
            _ => return InputOutcome::Ignored,
        }
        InputOutcome::Moved
    }

    /// Draw the field in `area` (one row) and return the screen column of the cursor.
    /// An empty field shows `placeholder` in `placeholder_style`.
    pub fn render(
        &mut self,
        canvas: &mut Canvas<'_>,
        area: Rect,
        style: Style,
        selection: Style,
        placeholder: &str,
        placeholder_style: Style,
    ) -> u16 {
        if area.width == 0 || area.height == 0 {
            return area.x;
        }
        if self.text.is_empty() {
            canvas.text_truncated(area.x, area.y, placeholder, placeholder_style, area.width);
            return area.x;
        }
        // Keep the cursor inside the field, with the last column free for it at the end.
        let field = area.width as usize;
        let cursor_col = ui::width(&self.text[..self.cursor]);
        if cursor_col < self.scroll {
            self.scroll = cursor_col;
        } else if cursor_col >= self.scroll + field {
            self.scroll = cursor_col + 1 - field;
        }
        let total = ui::width(&self.text);
        if total < field {
            self.scroll = 0;
        }

        let selected = self.selection();
        let mut col = 0usize;
        for (index, grapheme) in self.text.grapheme_indices(true) {
            let w = ui::width(grapheme).max(1);
            if col >= self.scroll && col + w <= self.scroll + field {
                let in_selection = selected.is_some_and(|(start, end)| index >= start && index < end);
                let cell_style = if in_selection { selection } else { style };
                canvas.put(area.x + (col - self.scroll) as u16, area.y, grapheme, cell_style);
            }
            col += w;
            if col >= self.scroll + field {
                break;
            }
        }
        area.x + (cursor_col - self.scroll).min(field.saturating_sub(1)) as u16
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// One line: line breaks and tabs become spaces, other control characters are dropped.
fn single_line(text: &str) -> String {
    text.chars()
        .filter_map(|c| match c {
            '\n' | '\r' | '\t' => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;

    fn act(input: &mut LineInput, name: &str) -> InputOutcome {
        input.handle_action(&Action::new(format!("editor::{name}")))
    }

    #[test]
    fn typing_and_deleting() {
        let mut input = LineInput::new();
        input.insert("hello");
        input.insert(" world");
        assert_eq!(input.text(), "hello world");
        assert_eq!(act(&mut input, "Backspace"), InputOutcome::Edited);
        assert_eq!(input.text(), "hello worl");
        assert_eq!(act(&mut input, "DeleteToPreviousWordStart"), InputOutcome::Edited);
        assert_eq!(input.text(), "hello ");
        act(&mut input, "MoveToBeginningOfLine");
        assert_eq!(act(&mut input, "Delete"), InputOutcome::Edited);
        assert_eq!(input.text(), "ello ");
        assert_eq!(act(&mut input, "Backspace"), InputOutcome::Moved, "nothing before the cursor");
        assert_eq!(act(&mut input, "DeleteToEndOfLine"), InputOutcome::Edited);
        assert!(input.is_empty());
    }

    #[test]
    fn word_motions() {
        let mut input = LineInput::with_text("src/main.rs  foo_bar");
        act(&mut input, "MoveToPreviousWordStart");
        assert_eq!(input.cursor, "src/main.rs  ".len());
        act(&mut input, "MoveToPreviousWordStart");
        assert_eq!(input.cursor, "src/main.".len());
        act(&mut input, "MoveToBeginningOfLine");
        act(&mut input, "MoveToNextWordEnd");
        assert_eq!(input.cursor, 3);
        act(&mut input, "MoveToNextWordEnd");
        assert_eq!(input.cursor, "src/main".len());
    }

    #[test]
    fn selection_copy_cut_and_replace() {
        let mut input = LineInput::with_text("one two");
        act(&mut input, "SelectToPreviousWordStart");
        assert_eq!(input.selected_text(), Some("two"));
        assert_eq!(act(&mut input, "Copy"), InputOutcome::Copied { text: "two".into(), edited: false });
        input.insert("three");
        assert_eq!(input.text(), "one three");
        act(&mut input, "SelectAll");
        assert_eq!(act(&mut input, "Cut"), InputOutcome::Copied { text: "one three".into(), edited: true });
        assert!(input.is_empty());
        assert_eq!(act(&mut input, "Copy"), InputOutcome::Moved, "nothing selected");
        assert_eq!(act(&mut input, "Paste"), InputOutcome::WantsPaste);
    }

    #[test]
    fn arrows_collapse_a_selection_to_its_edge() {
        let mut input = LineInput::with_text("abcdef");
        act(&mut input, "MoveToBeginningOfLine");
        act(&mut input, "SelectRight");
        act(&mut input, "SelectRight");
        assert_eq!(input.selected_text(), Some("ab"));
        act(&mut input, "MoveRight");
        assert_eq!((input.cursor, input.selection()), (2, None));
        act(&mut input, "SelectLeft");
        act(&mut input, "MoveLeft");
        assert_eq!((input.cursor, input.selection()), (1, None));
    }

    #[test]
    fn graphemes_move_and_delete_as_one() {
        let mut input = LineInput::with_text("a👨‍👩‍👧e\u{301}");
        act(&mut input, "Backspace");
        assert_eq!(input.text(), "a👨‍👩‍👧");
        act(&mut input, "MoveLeft");
        assert_eq!(input.cursor, 1);
        act(&mut input, "Delete");
        assert_eq!(input.text(), "a");
    }

    #[test]
    fn pasted_line_breaks_become_spaces() {
        let mut input = LineInput::new();
        input.insert("a\nb\r\nc\td\u{7}");
        assert_eq!(input.text(), "a b  c d");
    }

    #[test]
    fn undo_and_redo() {
        let mut input = LineInput::new();
        input.insert("one");
        input.insert(" two");
        assert_eq!(act(&mut input, "Undo"), InputOutcome::Edited);
        assert_eq!(input.text(), "one");
        assert_eq!(act(&mut input, "Undo"), InputOutcome::Edited);
        assert_eq!(input.text(), "");
        assert_eq!(act(&mut input, "Undo"), InputOutcome::Moved);
        assert_eq!(act(&mut input, "Redo"), InputOutcome::Edited);
        assert_eq!(act(&mut input, "Redo"), InputOutcome::Edited);
        assert_eq!(input.text(), "one two");
        assert_eq!(act(&mut input, "Redo"), InputOutcome::Moved);
    }

    #[test]
    fn unknown_actions_are_ignored() {
        let mut input = LineInput::with_text("x");
        assert_eq!(act(&mut input, "MoveUp"), InputOutcome::Ignored);
        assert_eq!(act(&mut input, "Newline"), InputOutcome::Ignored);
        assert_eq!(input.handle_action(&Action::new("menu::Confirm")), InputOutcome::Ignored);
        assert_eq!(act(&mut input, "Cancel"), InputOutcome::Ignored, "cancel passes through with no selection");
        act(&mut input, "SelectAll");
        assert_eq!(act(&mut input, "Cancel"), InputOutcome::Moved);
    }

    fn rendered(input: &mut LineInput, width: u16) -> (String, u16) {
        let area = Rect::new(0, 0, width, 1);
        let mut buf = Buffer::empty(area);
        let mut canvas = Canvas::new(&mut buf, area);
        let cursor = input.render(&mut canvas, area, Style::new(), Style::new(), "type here", Style::new());
        let text = (0..width).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        (text, cursor)
    }

    #[test]
    fn renders_placeholder_text_and_cursor() {
        let mut input = LineInput::new();
        assert_eq!(rendered(&mut input, 12), ("type here   ".to_string(), 0));
        input.insert("abc");
        assert_eq!(rendered(&mut input, 12), ("abc         ".to_string(), 3));
        act(&mut input, "MoveLeft");
        assert_eq!(rendered(&mut input, 12).1, 2);
    }

    #[test]
    fn scrolls_to_keep_the_cursor_visible() {
        let mut input = LineInput::with_text("0123456789abcdef");
        let (text, cursor) = rendered(&mut input, 8);
        assert_eq!(text, "9abcdef ", "the end is shown with a free cell for the cursor");
        assert_eq!(cursor, 7);
        act(&mut input, "MoveToBeginningOfLine");
        let (text, cursor) = rendered(&mut input, 8);
        assert_eq!((text.as_str(), cursor), ("01234567", 0));
    }
}
