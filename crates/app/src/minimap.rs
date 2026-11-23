//! The minimap: a column at the right of the editor showing the shape of the file in
//! braille dots, with the part on screen highlighted. (The user has Zed's minimap set to
//! "always".)
//!
//! A terminal cell holds a 2x4 grid of braille dots, so one row of the minimap stands for
//! four lines of the file and one cell for a slice of columns. Each dot is lit when the
//! text it stands for is not blank. A cell has one colour: that of the text most of its
//! dots came from, so comments, strings and keywords keep their tint in miniature.

use bakedit_core::{Rgb, Theme};
use ratatui::layout::Rect;

use crate::ui::{Canvas, Style};

/// Lines of the file per minimap row.
pub const LINES_PER_ROW: usize = 4;
/// Dots per minimap cell, horizontally.
const DOTS_PER_CELL: usize = 2;

/// What the minimap needs to know about the file.
pub trait MinimapSource {
    fn line_count(&self) -> usize;

    /// Describe line `line` as coloured runs of non-blank text: `(start_col, end_col, colour)`
    /// in display columns, sorted and non-overlapping. Blank stretches are simply not listed.
    fn runs(&self, line: usize, out: &mut Vec<(usize, usize, Rgb)>);
}

/// Which file lines a minimap of `rows` rows shows when the editor's first visible line is
/// `viewport_top` and it shows `viewport_lines` lines: the first line shown.
///
/// A short file is shown from the top. A long one scrolls in proportion, so the highlighted
/// viewport travels from the top of the minimap to its bottom as the file is scrolled.
pub fn first_line(total_lines: usize, rows: usize, viewport_top: usize, viewport_lines: usize) -> usize {
    let capacity = rows * LINES_PER_ROW;
    if total_lines <= capacity || capacity == 0 {
        return 0;
    }
    let max_top = total_lines.saturating_sub(viewport_lines.max(1));
    let max_first = total_lines - capacity;
    if max_top == 0 {
        return 0;
    }
    let first = (viewport_top.min(max_top) as u128 * max_first as u128 / max_top as u128) as usize;
    // Keep whole rows aligned so the picture does not shimmer while scrolling.
    first - first % LINES_PER_ROW
}

/// The file line a click on minimap row `row` (0-based from its top) points at.
pub fn line_at_row(total_lines: usize, rows: usize, viewport_top: usize, viewport_lines: usize, row: usize) -> usize {
    let first = first_line(total_lines, rows, viewport_top, viewport_lines);
    (first + row * LINES_PER_ROW + LINES_PER_ROW / 2).min(total_lines.saturating_sub(1))
}

/// Braille dot bit for a dot at (column 0..2, row 0..4) within a cell.
fn dot_bit(col: usize, row: usize) -> u8 {
    match (col, row) {
        (0, 0) => 0x01,
        (0, 1) => 0x02,
        (0, 2) => 0x04,
        (0, 3) => 0x40,
        (1, 0) => 0x08,
        (1, 1) => 0x10,
        (1, 2) => 0x20,
        _ => 0x80,
    }
}

/// Draw the minimap in `area`. `columns` is how many text columns the full width stands for
/// (Zed's `max_width_columns`, 80 by default): text beyond it is not shown.
pub fn render(
    canvas: &mut Canvas<'_>,
    area: Rect,
    source: &dyn MinimapSource,
    viewport_top: usize,
    viewport_lines: usize,
    columns: usize,
    theme: &Theme,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let rows = area.height as usize;
    let cells = area.width as usize;
    let dots = cells * DOTS_PER_CELL;
    let total = source.line_count();
    let first = first_line(total, rows, viewport_top, viewport_lines);
    let columns = columns.max(dots);

    let viewport_end = viewport_top + viewport_lines.max(1);
    let mut runs: Vec<(usize, usize, Rgb)> = Vec::new();
    // Per cell of the row being built: the dot bits, and votes for its colour.
    let mut bits = vec![0u8; cells];
    let mut votes: Vec<Vec<(Rgb, u32)>> = vec![Vec::new(); cells];

    for row in 0..rows {
        bits.iter_mut().for_each(|b| *b = 0);
        votes.iter_mut().for_each(Vec::clear);
        let row_first_line = first + row * LINES_PER_ROW;
        for sub in 0..LINES_PER_ROW {
            let line = row_first_line + sub;
            if line >= total {
                break;
            }
            runs.clear();
            source.runs(line, &mut runs);
            for &(start, end, color) in &runs {
                if start >= columns || end <= start {
                    continue;
                }
                // Map the run's columns onto dots; a run always lights at least one dot.
                let first_dot = start * dots / columns;
                let last_dot = ((end.min(columns) * dots).div_ceil(columns)).max(first_dot + 1).min(dots);
                for dot in first_dot..last_dot {
                    let cell = dot / DOTS_PER_CELL;
                    bits[cell] |= dot_bit(dot % DOTS_PER_CELL, sub);
                    match votes[cell].iter_mut().find(|(c, _)| *c == color) {
                        Some((_, count)) => *count += 1,
                        None => votes[cell].push((color, 1)),
                    }
                }
            }
        }

        // The rows that stand for what is on screen get the thumb background.
        let row_last_line = row_first_line + LINES_PER_ROW;
        let in_viewport = row_first_line < viewport_end && row_last_line > viewport_top && row_first_line < total.max(1);
        let bg = if in_viewport { theme.scrollbar_thumb } else { theme.editor_bg };
        let y = area.y + row as u16;
        for cell in 0..cells {
            let x = area.x + cell as u16;
            if bits[cell] == 0 {
                canvas.put(x, y, " ", Style::new().on(bg));
                continue;
            }
            let color = votes[cell].iter().max_by_key(|(_, count)| *count).map(|(c, _)| *c).unwrap_or(theme.text_muted);
            // Dimmed towards the background: the minimap is a hint, it must not compete
            // with the code beside it.
            let fg = color.lerp(bg, 0.35);
            let glyph = char::from_u32(0x2800 + bits[cell] as u32).unwrap_or(' ');
            let mut encoded = [0u8; 4];
            canvas.put(x, y, glyph.encode_utf8(&mut encoded), Style::colors(fg, bg));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui;
    use ratatui::buffer::Buffer;

    struct Lines(Vec<&'static str>);

    impl MinimapSource for Lines {
        fn line_count(&self) -> usize {
            self.0.len()
        }

        fn runs(&self, line: usize, out: &mut Vec<(usize, usize, Rgb)>) {
            let text = self.0[line];
            let mut start = None;
            for (i, c) in text.chars().enumerate() {
                match (c.is_whitespace(), start) {
                    (false, None) => start = Some(i),
                    (true, Some(s)) => {
                        out.push((s, i, Rgb::new(200, 200, 200)));
                        start = None;
                    }
                    _ => {}
                }
            }
            if let Some(s) = start {
                out.push((s, text.chars().count(), Rgb::new(200, 200, 200)));
            }
        }
    }

    fn draw(source: &Lines, width: u16, height: u16, top: usize, visible: usize, columns: usize) -> Buffer {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        let theme = Theme::builtin();
        let mut canvas = Canvas::new(&mut buf, area);
        render(&mut canvas, area, source, top, visible, columns, &theme);
        buf
    }

    fn symbol(buf: &Buffer, x: u16, y: u16) -> char {
        buf[(x, y)].symbol().chars().next().unwrap()
    }

    #[test]
    fn four_lines_fill_one_row_of_dots() {
        // 8 columns over 4 cells: 1 dot per column. Each line lights its own dot row.
        let source = Lines(vec!["xxxxxxxx", "xxxx", "", "    xxxx"]);
        let buf = draw(&source, 4, 1, 0, 4, 8);
        // Left two cells: dot rows 0 and 1 in both dot columns -> bits 0x01|0x08|0x02|0x10.
        assert_eq!(symbol(&buf, 0, 0), char::from_u32(0x2800 + 0x1b).unwrap());
        assert_eq!(symbol(&buf, 1, 0), char::from_u32(0x2800 + 0x1b).unwrap());
        // Right two cells: dot row 0 (line 0) and dot row 3 (line 3) -> 0x01|0x08|0x40|0x80.
        assert_eq!(symbol(&buf, 2, 0), char::from_u32(0x2800 + 0xc9).unwrap());
        assert_eq!(symbol(&buf, 3, 0), char::from_u32(0x2800 + 0xc9).unwrap());
    }

    #[test]
    fn blank_areas_stay_blank_and_short_runs_still_show() {
        let source = Lines(vec!["x", "", "", ""]);
        let buf = draw(&source, 4, 2, 0, 1, 80);
        assert_eq!(symbol(&buf, 0, 0), char::from_u32(0x2800 + 0x01).unwrap(), "one character still lights a dot");
        assert_eq!(symbol(&buf, 1, 0), ' ');
        assert_eq!(symbol(&buf, 0, 1), ' ', "rows past the end of the file are empty");
    }

    #[test]
    fn the_visible_part_is_highlighted() {
        let lines: Vec<&'static str> = vec!["xxxx"; 40];
        let source = Lines(lines);
        // 10 rows show all 40 lines; lines 8..16 are on screen -> rows 2 and 3.
        let buf = draw(&source, 4, 10, 8, 8, 8);
        let theme = Theme::builtin();
        let thumb = ui::color(theme.scrollbar_thumb);
        let plain = ui::color(theme.editor_bg);
        let backgrounds: Vec<bool> = (0..10).map(|y| buf[(0, y)].bg == thumb).collect();
        assert_eq!(backgrounds, vec![false, false, true, true, false, false, false, false, false, false]);
        assert_eq!(buf[(0, 0)].bg, plain);
    }

    #[test]
    fn long_files_scroll_in_proportion() {
        // 1000 lines, a 10-row minimap (40 lines), an editor showing 50 lines.
        assert_eq!(first_line(1000, 10, 0, 50), 0);
        assert_eq!(first_line(1000, 10, 950, 50), 960, "at the bottom the last 40 lines are shown");
        let middle = first_line(1000, 10, 475, 50);
        assert!(middle > 400 && middle < 560, "{middle}");
        assert_eq!(middle % LINES_PER_ROW, 0);
        // Short files never scroll.
        assert_eq!(first_line(30, 10, 20, 10), 0);
        assert_eq!(first_line(0, 10, 0, 10), 0);
        assert_eq!(first_line(1000, 0, 0, 10), 0);
        // Scrolled past the end (scroll_beyond_last_line) clamps.
        assert_eq!(first_line(1000, 10, 5000, 50), 960);
    }

    #[test]
    fn clicks_map_back_to_lines() {
        assert_eq!(line_at_row(40, 10, 0, 10, 0), 2);
        assert_eq!(line_at_row(40, 10, 0, 10, 9), 38);
        assert_eq!(line_at_row(3, 10, 0, 10, 9), 2, "clamped to the last line");
        assert_eq!(line_at_row(0, 10, 0, 10, 4), 0);
        assert_eq!(line_at_row(1000, 10, 950, 50, 0), 962);
    }

    #[test]
    fn text_beyond_the_width_is_ignored_and_tiny_areas_are_safe() {
        let source = Lines(vec!["                                                                                          far"]);
        let buf = draw(&source, 4, 1, 0, 1, 80);
        assert!((0..4).all(|x| symbol(&buf, x, 0) == ' '));
        draw(&source, 0, 0, 0, 1, 80);
        draw(&source, 1, 1, 0, 0, 0);
    }

    #[test]
    fn a_cell_takes_the_colour_of_most_of_its_text() {
        struct TwoColors;
        impl MinimapSource for TwoColors {
            fn line_count(&self) -> usize {
                4
            }
            fn runs(&self, line: usize, out: &mut Vec<(usize, usize, Rgb)>) {
                // Three lines of green and one of red in the same columns.
                let color = if line == 0 { Rgb::new(255, 0, 0) } else { Rgb::new(0, 255, 0) };
                out.push((0, 2, color));
            }
        }
        let area = Rect::new(0, 0, 1, 1);
        let mut buf = Buffer::empty(area);
        let theme = Theme::builtin();
        {
            let mut canvas = Canvas::new(&mut buf, area);
            render(&mut canvas, area, &TwoColors, 100, 1, 2, &theme);
        }
        let expected = Rgb::new(0, 255, 0).lerp(theme.editor_bg, 0.35);
        assert_eq!(buf[(0, 0)].fg, ui::color(expected));
    }
}
