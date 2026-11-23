//! The mouse: click, drag, double and triple click, alt-click for more cursors, the wheel,
//! the minimap, the scrollbar and the find bar.

use std::ops::Range;
use std::time::{Duration, Instant};

use bakedit_core::{Mods, MouseButton, MouseEvent, MouseKind};
use bakedit_text::DisplayPosition;
use ratatui::layout::Rect;

use super::layout::{Geometry, Reveal};
use super::EditorItem;
use crate::minimap;
use crate::view::Cx;

/// Lines a wheel step scrolls.
const WHEEL_LINES: isize = 3;
/// Columns a sideways wheel step scrolls.
const WHEEL_COLUMNS: usize = 4;
/// Clicks closer together than this are a double click.
const MULTI_CLICK: Duration = Duration::from_millis(450);

/// What a drag in progress does.
#[derive(Debug, Clone)]
enum Drag {
    /// Extend the selection by characters, words or lines from the range first selected.
    Select { unit: Unit, anchor: Range<usize> },
    Minimap,
    Scrollbar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unit {
    Char,
    Word,
    Line,
}

#[derive(Default)]
pub(super) struct MouseState {
    last_click: Option<(Instant, u16, u16)>,
    clicks: u8,
    drag: Option<Drag>,
}

impl EditorItem {
    /// The char under a screen cell; cells outside the text are clamped to its nearest row.
    fn position_at_cell(&self, col: u16, row: u16) -> usize {
        let text = self.geo.text;
        let Some(last) = self.geo.rows.len().checked_sub(1) else { return self.cursor() };
        let index = (row.saturating_sub(text.y) as usize).min(last);
        let shown = &self.geo.rows[index];
        let display_col = self.left + col.saturating_sub(text.x) as usize;
        self.editor.position_at(DisplayPosition { line: shown.line, row: shown.row, col: display_col })
    }

    fn line_range(&self, line: usize) -> Range<usize> {
        let buffer = self.editor.buffer();
        let start = buffer.line_to_char(line);
        let end = if line + 1 < buffer.len_lines() { buffer.line_to_char(line + 1) } else { buffer.len_chars() };
        start..end
    }

    fn select_unit(&mut self, unit: Unit, pos: usize) -> Range<usize> {
        let range = match unit {
            Unit::Char => pos..pos,
            Unit::Word => self.editor.word_at(pos).unwrap_or(pos..pos),
            Unit::Line => self.line_range(self.editor.buffer().char_to_line(pos)),
        };
        self.editor.select_range(range.start, range.end);
        range
    }

    fn extend_unit(&mut self, unit: Unit, anchor: &Range<usize>, pos: usize) {
        if unit == Unit::Char {
            self.editor.extend_primary_to(pos);
            return;
        }
        let target = match unit {
            Unit::Word => self.editor.word_at(pos).unwrap_or(pos..pos),
            _ => self.line_range(self.editor.buffer().char_to_line(pos)),
        };
        if target.start < anchor.start {
            self.editor.select_range(anchor.end, target.start);
        } else {
            self.editor.select_range(anchor.start, target.end.max(anchor.end));
        }
    }

    /// Scroll so the line a minimap or scrollbar click points at is in the middle.
    fn jump_to_line(&mut self, line: usize) {
        let rows = self.geo.text.height as usize;
        self.scroll_to_line(line.saturating_sub(rows / 2));
    }

    fn minimap_jump(&mut self, row: u16) {
        let area = self.geo.minimap;
        let total = self.editor.buffer().len_lines();
        let rel = row.saturating_sub(area.y).min(area.height.saturating_sub(1)) as usize;
        let line = minimap::line_at_row(total, area.height as usize, self.scroll.line, self.geo.viewport_lines, rel);
        self.jump_to_line(line);
    }

    fn scrollbar_jump(&mut self, row: u16) {
        let area = self.geo.scrollbar;
        let total = self.editor.buffer().len_lines();
        // The same scale the bar is drawn at: the file and one page past its end.
        let line = crate::paint::scrollbar_line_at(row.saturating_sub(area.y), area.height, total, area.height as usize);
        self.jump_to_line(line);
    }

    pub(super) fn mouse_event(&mut self, event: &MouseEvent, area: Rect, cx: &mut Cx<'_>) -> bool {
        let inside = Geometry::contains(area, event.col, event.row);
        match event.kind {
            MouseKind::ScrollDown | MouseKind::ScrollUp if inside => {
                self.scroll_by(if event.kind == MouseKind::ScrollDown { WHEEL_LINES } else { -WHEEL_LINES });
                true
            }
            MouseKind::ScrollRight | MouseKind::ScrollLeft if inside && !self.wrapping() => {
                self.left = if event.kind == MouseKind::ScrollRight { self.left + WHEEL_COLUMNS } else { self.left.saturating_sub(WHEEL_COLUMNS) };
                true
            }
            MouseKind::Press(MouseButton::Left) if inside => self.press(event, cx),
            MouseKind::Drag(MouseButton::Left) => self.drag(event),
            MouseKind::Release(MouseButton::Left) => self.mouse.drag.take().is_some(),
            _ => false,
        }
    }

    fn press(&mut self, event: &MouseEvent, cx: &mut Cx<'_>) -> bool {
        let (col, row) = (event.col, event.row);
        if Geometry::contains(self.geo.find, col, row) {
            return self.find_click(col, row, cx);
        }
        if Geometry::contains(self.geo.breadcrumbs, col, row) {
            return true;
        }
        if Geometry::contains(self.geo.minimap, col, row) {
            self.mouse.drag = Some(Drag::Minimap);
            self.minimap_jump(row);
            return true;
        }
        if Geometry::contains(self.geo.scrollbar, col, row) {
            self.mouse.drag = Some(Drag::Scrollbar);
            self.scrollbar_jump(row);
            return true;
        }
        if self.geo.rows.is_empty() {
            return true;
        }
        self.find_focus_editor();
        let pos = self.position_at_cell(col, row);
        let in_gutter = col < self.geo.text.x;
        self.editor.break_undo_group();

        let clicks = match self.mouse.last_click {
            Some((at, c, r)) if cx.now.duration_since(at) <= MULTI_CLICK && (c, r) == (col, row) => self.mouse.clicks % 3 + 1,
            _ => 1,
        };
        self.mouse.clicks = clicks;
        self.mouse.last_click = Some((cx.now, col, row));

        let unit = if in_gutter {
            Unit::Line
        } else {
            match clicks {
                1 => Unit::Char,
                2 => Unit::Word,
                _ => Unit::Line,
            }
        };
        if event.mods.contains(Mods::SHIFT) && !in_gutter {
            let anchor = self.editor.selections().primary().range();
            self.editor.extend_primary_to(pos);
            self.mouse.drag = Some(Drag::Select { unit: Unit::Char, anchor });
        } else if event.mods.contains(Mods::ALT) && !in_gutter {
            self.editor.add_selection(pos, pos);
            self.mouse.drag = Some(Drag::Select { unit: Unit::Char, anchor: pos..pos });
        } else {
            let anchor = self.select_unit(unit, pos);
            self.mouse.drag = Some(Drag::Select { unit, anchor });
        }
        self.reveal = Reveal::No;
        self.clamp_nav_cursor();
        true
    }

    fn drag(&mut self, event: &MouseEvent) -> bool {
        let Some(drag) = self.mouse.drag.clone() else { return false };
        match drag {
            Drag::Minimap => self.minimap_jump(event.row),
            Drag::Scrollbar => self.scrollbar_jump(event.row),
            Drag::Select { unit, anchor } => {
                let text = self.geo.text;
                // Past the top or bottom edge, keep scrolling.
                if event.row >= text.bottom() {
                    self.scroll_by(1);
                } else if event.row < text.y {
                    self.scroll_by(-1);
                }
                let pos = self.position_at_cell(event.col, event.row);
                self.extend_unit(unit, &anchor, pos);
            }
        }
        true
    }
}
