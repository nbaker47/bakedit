//! Cursor motions: where each [`Motion`] takes a cursor.

use crate::buffer::Point;
use crate::chars;
use crate::command::Motion;
use crate::display;
use crate::indent;
use crate::pairs;
use crate::selection::{Selection, Selections};

use super::Editor;

impl Editor {
    /// Move (or, with `extend`, extend) every selection by `motion`.
    pub(crate) fn apply_motion(&mut self, motion: &Motion, extend: bool) {
        if matches!(motion, Motion::GoToLine { .. }) && !extend {
            // A jump leaves one cursor, wherever the others were.
            let (target, _) = self.motion_target(motion, self.selections.primary().head, None);
            self.selections = Selections::single(Selection::cursor(target));
            return;
        }
        let mut selections = self.selections.clone();
        selections.transform(|selection| self.moved(selection, motion, extend));
        self.selections = selections;
    }

    /// One selection after `motion`.
    fn moved(&self, selection: Selection, motion: &Motion, extend: bool) -> Selection {
        if extend {
            let (head, goal) = self.motion_target(motion, selection.head, selection.goal);
            return selection.with_head(head, goal);
        }
        let upwards = matches!(
            motion,
            Motion::Up | Motion::UpByLines { .. } | Motion::PageUp { .. }
        );
        let downwards = matches!(
            motion,
            Motion::Down | Motion::DownByLines { .. } | Motion::PageDown { .. }
        );
        let (from, goal) = if selection.is_empty() {
            (selection.head, selection.goal)
        } else {
            match motion {
                // Leaving a selection sideways lands on its edge.
                Motion::Left => return Selection::cursor(selection.start()),
                Motion::Right => return Selection::cursor(selection.end()),
                _ if upwards => (selection.start(), None),
                _ if downwards => (selection.end(), None),
                _ => (selection.head, selection.goal),
            }
        };
        let (target, goal) = self.motion_target(motion, from, goal);
        Selection::cursor(target).with_goal(goal)
    }

    /// Where `motion` takes a cursor at `from` whose goal column is `goal`: the new
    /// position and the new goal column.
    pub(crate) fn motion_target(
        &self,
        motion: &Motion,
        from: usize,
        goal: Option<usize>,
    ) -> (usize, Option<usize>) {
        let buffer = &self.buffer;
        let rope = buffer.rope();
        let words = self.word_chars();
        let from = from.min(buffer.len_chars());
        let target = match motion {
            Motion::Left => buffer.prev_grapheme(from),
            Motion::Right => buffer.next_grapheme(from),
            Motion::Up => return self.move_rows(from, true, 1, goal),
            Motion::Down => return self.move_rows(from, false, 1, goal),
            Motion::PreviousWordStart => chars::previous_word_start(rope, from, words),
            Motion::NextWordEnd => chars::next_word_end(rope, from, words),
            Motion::NextWordStart => chars::next_word_start(rope, from, words),
            Motion::PreviousSubwordStart => chars::previous_subword_start(rope, from, words),
            Motion::NextSubwordEnd => chars::next_subword_end(rope, from, words),
            Motion::BeginningOfLine {
                stop_at_soft_wraps,
                stop_at_indent,
            } => self.beginning_of_line(from, *stop_at_soft_wraps, *stop_at_indent),
            Motion::EndOfLine { stop_at_soft_wraps } => self.end_of_line(from, *stop_at_soft_wraps),
            Motion::FirstNonBlank => {
                let line = buffer.char_to_line(from);
                buffer.line_to_char(line)
                    + indent::indent_of_line(buffer, line, self.tab_size()).chars
            }
            Motion::Beginning => 0,
            Motion::End => buffer.len_chars(),
            Motion::StartOfParagraph => self.start_of_paragraph(from),
            Motion::EndOfParagraph => self.end_of_paragraph(from),
            Motion::PageUp { rows } => {
                return self.move_rows(from, true, self.page_rows(*rows), goal)
            }
            Motion::PageDown { rows } => {
                return self.move_rows(from, false, self.page_rows(*rows), goal)
            }
            Motion::UpByLines { lines } => return self.move_rows(from, true, *lines, goal),
            Motion::DownByLines { lines } => return self.move_rows(from, false, *lines, goal),
            Motion::EnclosingBracket => self.enclosing_bracket_target(from),
            Motion::GoToLine { line, column } => {
                let point = Point::new(line.saturating_sub(1), column.saturating_sub(1));
                buffer.snap_to_grapheme(buffer.point_to_char(point))
            }
        };
        (target, None)
    }

    fn page_rows(&self, rows: usize) -> usize {
        if rows == 0 {
            self.settings.page_rows.max(1)
        } else {
            rows
        }
    }

    /// Move `count` display rows up or down from `from`, aiming for the goal column.
    ///
    /// Rows are soft-wrapped rows when wrapping is on, buffer lines otherwise. Running out
    /// of rows stops at the first or last one; a cursor that is already there goes to the
    /// start or the end of the text instead, as in Zed. The goal column is kept either way.
    fn move_rows(
        &self,
        from: usize,
        up: bool,
        count: usize,
        goal: Option<usize>,
    ) -> (usize, Option<usize>) {
        if count == 0 {
            return (from, goal);
        }
        let buffer = &self.buffer;
        let tab_size = self.tab_size();
        let point = buffer.char_to_point(from);
        let last_line = buffer.len_lines() - 1;
        let mut line = point.line;
        let mut layout = self.layout(line);
        let (start_row, start_col) =
            display::char_col_to_row_col(&layout.text, &layout.rows, point.col, tab_size);
        let goal_col = goal.unwrap_or(start_col);
        let mut row = start_row;

        if self.wrap_width().is_none() {
            let target = if up {
                line.saturating_sub(count)
            } else {
                line.saturating_add(count).min(last_line)
            };
            if target != line {
                line = target;
                layout = self.layout(line);
            }
        } else {
            let mut remaining = count;
            while remaining > 0 {
                let last_row = layout.rows.len().saturating_sub(1);
                if up {
                    if row >= remaining {
                        row -= remaining;
                        remaining = 0;
                    } else if line == 0 {
                        row = 0;
                        remaining = 0;
                    } else {
                        remaining -= row + 1;
                        line -= 1;
                        layout = self.layout(line);
                        row = layout.rows.len().saturating_sub(1);
                    }
                } else if last_row - row >= remaining {
                    row += remaining;
                    remaining = 0;
                } else if line == last_line {
                    row = last_row;
                    remaining = 0;
                } else {
                    remaining -= last_row - row + 1;
                    line += 1;
                    layout = self.layout(line);
                    row = 0;
                }
            }
        }

        if line == point.line && row == start_row {
            let edge = if up { 0 } else { buffer.len_chars() };
            return (edge, Some(goal_col));
        }
        let col = display::row_col_to_char_col(&layout.text, &layout.rows, row, goal_col, tab_size);
        (buffer.line_to_char(line) + col, Some(goal_col))
    }

    /// Where "beginning of line" goes from `pos`, as Zed's `indented_line_beginning`.
    ///
    /// With `stop_at_soft_wraps`, a cursor on a continuation row goes to the start of that
    /// row first. With `stop_at_indent`, a cursor after the indentation, or at column 0,
    /// goes to the first non-blank character. Otherwise it goes to column 0.
    pub(crate) fn beginning_of_line(
        &self,
        pos: usize,
        stop_at_soft_wraps: bool,
        stop_at_indent: bool,
    ) -> usize {
        let buffer = &self.buffer;
        let point = buffer.char_to_point(pos);
        let line_start = buffer.line_to_char(point.line);
        let indent_start =
            line_start + indent::indent_of_line(buffer, point.line, self.tab_size()).chars;
        if stop_at_soft_wraps && self.wrap_width().is_some() {
            let layout = self.layout(point.line);
            let (row, _) = display::char_col_to_row_col(
                &layout.text,
                &layout.rows,
                point.col,
                self.tab_size(),
            );
            let soft_start = line_start + layout.rows.get(row).map_or(0, |range| range.start);
            if soft_start > indent_start && pos != soft_start {
                return soft_start;
            }
        }
        if stop_at_indent && (pos > indent_start || pos == line_start) {
            indent_start
        } else {
            line_start
        }
    }

    /// Where "end of line" goes from `pos`, as Zed's `line_end`.
    ///
    /// With `stop_at_soft_wraps`, a cursor on a row that wraps goes to the last position
    /// drawn on that row (before its final character) first, and to the end of the line
    /// from there.
    pub(crate) fn end_of_line(&self, pos: usize, stop_at_soft_wraps: bool) -> usize {
        let buffer = &self.buffer;
        let point = buffer.char_to_point(pos);
        if stop_at_soft_wraps && self.wrap_width().is_some() {
            let layout = self.layout(point.line);
            let (row, _) = display::char_col_to_row_col(
                &layout.text,
                &layout.rows,
                point.col,
                self.tab_size(),
            );
            if row + 1 < layout.rows.len() {
                let range = layout.rows[row].clone();
                let row_text = display::slice_chars(&layout.text, range.clone());
                let soft_end = buffer.line_to_char(point.line)
                    + range.start
                    + display::last_cluster_start(row_text);
                if pos != soft_end {
                    return soft_end;
                }
            }
        }
        buffer.line_end(point.line)
    }

    /// The start of the nearest blank line above the paragraph `pos` is in, or the start of
    /// the text.
    fn start_of_paragraph(&self, pos: usize) -> usize {
        let buffer = &self.buffer;
        let line = buffer.char_to_line(pos);
        let mut seen_text = false;
        for candidate in (0..=line).rev() {
            let blank = indent::is_blank_line(buffer, candidate);
            if seen_text && blank {
                return buffer.line_to_char(candidate);
            }
            seen_text |= !blank;
        }
        0
    }

    /// The start of the nearest blank line below the paragraph `pos` is in, or the end of
    /// the text.
    fn end_of_paragraph(&self, pos: usize) -> usize {
        let buffer = &self.buffer;
        let line = buffer.char_to_line(pos);
        let mut seen_text = false;
        for candidate in line..buffer.len_lines() {
            let blank = indent::is_blank_line(buffer, candidate);
            if seen_text && blank {
                return buffer.line_to_char(candidate);
            }
            seen_text |= !blank;
        }
        buffer.len_chars()
    }

    /// Where `MoveToEnclosingBracket` goes from `pos`.
    ///
    /// Next to a bracket, the cursor goes to the same side of its partner: from before an
    /// opening bracket to after the closing one, from after an opening bracket to before
    /// the closing one, and back. Elsewhere inside a pair it goes to just before the
    /// closing bracket. With no pair around, it stays.
    fn enclosing_bracket_target(&self, pos: usize) -> usize {
        let buffer = &self.buffer;
        if let Some((open, close)) = pairs::bracket_pair_at(buffer, pos) {
            return if pos == open { close + 1 } else { open + 1 };
        }
        if let Some(before) = pos.checked_sub(1) {
            if let Some((open, close)) = pairs::bracket_pair_at(buffer, before) {
                return if before == open { close } else { open };
            }
        }
        match pairs::enclosing_bracket(buffer, pos..pos) {
            Some((_, close)) => close,
            None => pos,
        }
    }
}
