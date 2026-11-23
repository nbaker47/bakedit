//! Where things are on screen: the scroll position (a line and, with soft wrap, a row of
//! it), the wrap width, and the rectangles the last draw used (for the mouse).

use ratatui::layout::Rect;

use super::EditorItem;
use crate::settings::{Settings, SoftWrap};

/// Where the next draw should put the cursor, if it moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Reveal {
    /// Leave the scroll alone.
    No,
    /// Scroll just enough to show the cursor, keeping the scroll margin.
    Fit,
    /// Put the cursor line in the middle.
    Center,
    /// Leave the scroll alone if the cursor is on screen, otherwise centre it.
    Visible,
}

/// The first row shown: a buffer line and the soft-wrap row within it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(super) struct Scroll {
    pub line: usize,
    pub row: usize,
}

/// One screen row of text.
#[derive(Debug, Clone)]
pub(super) struct ShownRow {
    pub line: usize,
    /// Which soft-wrap row of the line this is.
    pub row: usize,
}

/// The rectangles and rows of the last draw.
#[derive(Debug, Clone, Default)]
pub(super) struct Geometry {
    pub area: Rect,
    pub breadcrumbs: Rect,
    pub find: Rect,
    pub gutter: Rect,
    pub text: Rect,
    pub minimap: Rect,
    pub scrollbar: Rect,
    pub rows: Vec<ShownRow>,
    /// How many buffer lines the rows show.
    pub viewport_lines: usize,
}

impl Geometry {
    pub fn contains(rect: Rect, col: u16, row: u16) -> bool {
        rect.width > 0 && rect.height > 0 && col >= rect.x && col < rect.right() && row >= rect.y && row < rect.bottom()
    }
}

impl EditorItem {
    /// Whether long lines wrap in the draw before this one.
    pub(super) fn wrapping(&self) -> bool {
        self.editor.settings().wrap_width.is_some_and(|width| width > 0)
    }

    /// How many soft-wrap rows `line` takes.
    pub(super) fn rows_in(&self, line: usize) -> usize {
        if self.wrapping() {
            self.editor.line_rows(line).len().max(1)
        } else {
            1
        }
    }

    /// The width to wrap at, from the settings, the language and the wrap toggle; `None`
    /// when lines do not wrap. One column is kept free for the cursor past the last char.
    pub(super) fn wrap_width(&self, settings: &Settings, text_width: usize) -> Option<usize> {
        let mode = match settings.language(self.language.name).and_then(|l| l.soft_wrap) {
            Some(mode) => mode,
            None if settings.soft_wrap != SoftWrap::None => settings.soft_wrap,
            None if self.language.soft_wrap => SoftWrap::EditorWidth,
            None => SoftWrap::None,
        };
        let mode = match (self.wrap_override, mode) {
            (Some(false), _) => SoftWrap::None,
            (Some(true), SoftWrap::None) => SoftWrap::EditorWidth,
            _ => mode,
        };
        let room = text_width.saturating_sub(1).max(1);
        match mode {
            SoftWrap::None => None,
            SoftWrap::EditorWidth => Some(room),
            SoftWrap::PreferredLineLength => Some(room.min(usize::from(settings.preferred_line_length).max(1))),
        }
    }

    /// `pos` moved down by `n` rows, stopping at the last row of the file.
    pub(super) fn advance(&self, pos: Scroll, n: usize) -> Scroll {
        let last = self.editor.buffer().len_lines().saturating_sub(1);
        if !self.wrapping() {
            return Scroll { line: (pos.line + n).min(last), row: 0 };
        }
        let mut pos = pos;
        for _ in 0..n {
            if pos.row + 1 < self.rows_in(pos.line) {
                pos.row += 1;
            } else if pos.line < last {
                pos = Scroll { line: pos.line + 1, row: 0 };
            } else {
                break;
            }
        }
        pos
    }

    /// `pos` moved up by `n` rows, stopping at the top of the file.
    pub(super) fn retreat(&self, pos: Scroll, n: usize) -> Scroll {
        if !self.wrapping() {
            return Scroll { line: pos.line.saturating_sub(n), row: 0 };
        }
        let mut pos = pos;
        for _ in 0..n {
            if pos.row > 0 {
                pos.row -= 1;
            } else if pos.line > 0 {
                let line = pos.line - 1;
                pos = Scroll { line, row: self.rows_in(line) - 1 };
            } else {
                break;
            }
        }
        pos
    }

    pub(super) fn scroll_by(&mut self, rows: isize) {
        let n = rows.unsigned_abs();
        self.scroll = if rows >= 0 { self.advance(self.scroll, n) } else { self.retreat(self.scroll, n) };
    }

    /// Scroll so that buffer line `line` is the first row shown.
    pub(super) fn scroll_to_line(&mut self, line: usize) {
        self.scroll = Scroll { line: line.min(self.editor.buffer().len_lines().saturating_sub(1)), row: 0 };
    }

    /// Where the cursor is as a scroll position: its line and soft-wrap row.
    pub(super) fn cursor_row(&self) -> Scroll {
        let position = self.editor.display_position(self.view_cursor());
        Scroll { line: position.line, row: position.row }
    }

    /// Scroll as the last move asked (see [`Reveal`]).
    pub(super) fn apply_reveal(&mut self, rows: usize, margin: usize) {
        let cursor = self.cursor_row();
        let margin = margin.min(rows.saturating_sub(1) / 2);
        match self.reveal {
            Reveal::No => {}
            Reveal::Center => self.scroll = self.retreat(cursor, rows / 2),
            Reveal::Visible => {
                if cursor < self.scroll || cursor > self.advance(self.scroll, rows.saturating_sub(1)) {
                    self.scroll = self.retreat(cursor, rows / 2);
                }
            }
            Reveal::Fit => {
                if cursor < self.advance(self.scroll, margin) {
                    self.scroll = self.retreat(cursor, margin);
                } else if cursor > self.advance(self.scroll, rows.saturating_sub(1 + margin)) {
                    self.scroll = self.retreat(cursor, rows.saturating_sub(1 + margin));
                }
            }
        }
        self.reveal = Reveal::No;
        let last = self.editor.buffer().len_lines().saturating_sub(1);
        if self.scroll.line > last {
            self.scroll = Scroll { line: last, row: 0 };
        }
    }
}
