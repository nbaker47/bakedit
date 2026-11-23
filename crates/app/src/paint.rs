//! Painting one row of the editor: the gutter, the text with its syntax colours, selections,
//! search matches, bracket colours, indent guides and extra cursors.
//!
//! This module knows nothing about buffers. The editor view gathers what a row needs into a
//! [`Row`] and this draws it, which keeps every layering rule in one place: selection over
//! search match over current-line highlight; bracket colour over syntax colour.

use std::ops::Range;

use bakedit_core::{Rgb, Theme};
use unicode_segmentation::UnicodeSegmentation;

use crate::ui::{self, Canvas, Style};

/// A mark in the gutter for a line that differs from git.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitMark {
    Added,
    Modified,
    /// Lines were deleted just above this line.
    DeletedAbove,
    /// Lines were deleted just below this line (used on the last line of the file).
    DeletedBelow,
}

/// What the gutter shows for a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GutterInfo {
    /// The number to print; `None` on the continuation rows of a wrapped line.
    pub number: Option<usize>,
    /// This row holds the primary cursor.
    pub current: bool,
    pub git: Option<GitMark>,
}

/// Columns the gutter needs for a file of `line_count` lines: a git mark column, the widest
/// line number (at least three digits, so the text does not jump as a file grows), a space.
pub fn gutter_width(line_count: usize, show_numbers: bool) -> u16 {
    if !show_numbers {
        return 2;
    }
    let digits = line_count.max(1).to_string().len().max(3);
    (1 + digits + 1) as u16
}

/// Draw the gutter cell for one row at `(x, y)`.
pub fn gutter(canvas: &mut Canvas<'_>, x: u16, y: u16, width: u16, info: GutterInfo, show_numbers: bool, theme: &Theme) {
    let bg = if info.current { theme.active_line_bg } else { theme.gutter_bg };
    canvas.fill(ratatui::layout::Rect::new(x, y, width, 1), bg);
    if width == 0 {
        return;
    }
    match info.git {
        Some(GitMark::Added) => {
            canvas.put(x, y, "▎", Style::colors(theme.created, bg));
        }
        Some(GitMark::Modified) => {
            canvas.put(x, y, "▎", Style::colors(theme.modified, bg));
        }
        Some(GitMark::DeletedAbove) => {
            canvas.put(x, y, "▔", Style::colors(theme.deleted, bg));
        }
        Some(GitMark::DeletedBelow) => {
            canvas.put(x, y, "▁", Style::colors(theme.deleted, bg));
        }
        None => {}
    }
    if show_numbers && width > 2 {
        if let Some(number) = info.number {
            let color = if info.current { theme.active_line_number } else { theme.line_number };
            canvas.text_right(x + width - 1, y, &number.to_string(), Style::colors(color, bg));
        }
    }
}

/// Everything needed to paint the text of one display row.
#[derive(Debug, Clone)]
pub struct Row<'a> {
    /// The whole line, without its line break.
    pub text: &'a str,
    /// The part of the line this row shows, in chars (the whole line unless soft-wrapped).
    pub segment: Range<usize>,
    /// Syntax styles as byte ranges into `text`, sorted, non-overlapping.
    pub syntax: &'a [(Range<usize>, Style)],
    /// Selected char ranges on this line, sorted. A range reaching `line_len + 1` means the
    /// line break itself is selected.
    pub selections: &'a [Range<usize>],
    /// Search matches on this line, as char ranges.
    pub matches: &'a [Range<usize>],
    /// The search match the cursor is on, drawn stronger.
    pub active_match: Option<Range<usize>>,
    /// Char positions of extra cursors to draw (the primary one is the terminal's cursor).
    pub cursors: &'a [usize],
    /// Rainbow brackets: (char position, nesting depth).
    pub brackets: &'a [(usize, u16)],
    /// Char positions to highlight as the bracket pair at the cursor.
    pub matching_brackets: &'a [usize],
    /// The primary cursor is on this line: draw the current-line background.
    pub current_line: bool,
    /// How many indent guides to draw: the line's indent level (for a blank line, that of
    /// the block around it). Guides sit at the start of each level, from column 0, as in Zed.
    pub indent_guides: usize,
    /// The indent level (0-based) whose guide is highlighted, if the cursor's block is here.
    pub active_guide: Option<usize>,
}

impl<'a> Row<'a> {
    /// A row with just text: the whole line, nothing highlighted.
    pub fn plain(text: &'a str) -> Row<'a> {
        Row {
            text,
            segment: 0..text.chars().count(),
            syntax: &[],
            selections: &[],
            matches: &[],
            active_match: None,
            cursors: &[],
            brackets: &[],
            matching_brackets: &[],
            current_line: false,
            indent_guides: 0,
            active_guide: None,
        }
    }
}

/// Options that are the same for every row of a frame.
#[derive(Debug, Clone, Copy)]
pub struct PaintOptions {
    pub tab_size: usize,
    /// First display column shown (horizontal scroll).
    pub left: usize,
    pub show_indent_guides: bool,
    pub colorize_brackets: bool,
    /// Draw spaces and tabs inside selections as faint marks (Zed's default).
    pub show_whitespace_in_selection: bool,
}

fn contains(ranges: &[Range<usize>], index: usize) -> bool {
    ranges.iter().any(|r| r.start <= index && index < r.end)
}

/// Paint the text of a row into `width` columns starting at `(x, y)`.
pub fn row(canvas: &mut Canvas<'_>, x: u16, y: u16, width: u16, row: &Row<'_>, options: &PaintOptions, theme: &Theme) {
    let line_bg = if row.current_line { theme.active_line_bg } else { theme.editor_bg };
    canvas.fill(ratatui::layout::Rect::new(x, y, width, 1), line_bg);
    if width == 0 {
        return;
    }
    let tab = options.tab_size.max(1);
    let line_len = row.text.chars().count();
    let indent_chars = row.text.chars().take_while(|c| *c == ' ' || *c == '\t').count();

    let mut syntax = row.syntax.iter().peekable();
    let mut char_index = 0usize;
    let mut col = 0usize;
    let right = options.left + width as usize;

    for (byte_index, grapheme) in row.text.grapheme_indices(true) {
        let chars = grapheme.chars().count();
        if char_index >= row.segment.end {
            break;
        }
        if char_index < row.segment.start {
            char_index += chars;
            continue;
        }
        let is_tab = grapheme == "\t";
        let cells = if is_tab { tab - col % tab } else { ui::width(grapheme).max(1) };

        // Background, weakest first.
        let selected = contains(row.selections, char_index);
        let mut bg = line_bg;
        if contains(row.matches, char_index) {
            bg = theme.search_match_bg;
        }
        if row.active_match.as_ref().is_some_and(|m| m.start <= char_index && char_index < m.end) {
            bg = theme.search_match_bg.lerp(theme.text_accent, 0.45);
        }
        if row.matching_brackets.contains(&char_index) {
            bg = theme.matching_bracket_bg;
        }
        if selected {
            bg = theme.selection_bg;
        }

        // Foreground: syntax, then rainbow brackets on top.
        while syntax.peek().is_some_and(|(range, _)| range.end <= byte_index) {
            syntax.next();
        }
        let mut style = match syntax.peek() {
            Some((range, style)) if range.start <= byte_index => *style,
            _ => Style::fg(theme.editor_fg),
        };
        if style.fg.is_none() {
            style.fg = Some(theme.editor_fg);
        }
        if options.colorize_brackets && !theme.accents.is_empty() {
            if let Some((_, depth)) = row.brackets.iter().find(|(position, _)| *position == char_index) {
                style.fg = Some(theme.accents[*depth as usize % theme.accents.len()]);
            }
        }
        style.bg = Some(bg);

        let is_extra_cursor = row.cursors.contains(&char_index);
        if is_extra_cursor {
            style = Style::colors(theme.editor_bg, theme.cursor);
        }

        // What to draw in the cell(s).
        let is_space = grapheme == " ";
        let in_indent = char_index < indent_chars;
        let guide_level = if options.show_indent_guides && in_indent && col.is_multiple_of(tab) { Some(col / tab) } else { None };
        let draw_guide = guide_level.is_some_and(|level| level < row.indent_guides);
        for cell in 0..cells {
            let cell_col = col + cell;
            if cell_col < options.left {
                continue;
            }
            if cell_col >= right {
                break;
            }
            let sx = x + (cell_col - options.left) as u16;
            if is_tab || is_space {
                let (symbol, mut cell_style) = if selected && options.show_whitespace_in_selection {
                    let mark = if is_tab && cell == 0 { "→" } else if is_space { "·" } else { " " };
                    (mark, Style::colors(theme.text_muted.lerp(theme.selection_bg, 0.3), bg))
                } else if draw_guide && cell == 0 && !is_extra_cursor {
                    let active = row.active_guide == guide_level;
                    let color = if active { theme.indent_guide_active } else { theme.indent_guide };
                    ("│", Style::colors(color, bg))
                } else {
                    (" ", Style::new().on(bg))
                };
                if is_extra_cursor && cell == 0 {
                    cell_style = style;
                }
                canvas.put(sx, y, symbol, cell_style);
            } else if cell == 0 {
                // A wide character that would cross the right edge is not drawn at all.
                if cell_col + cells <= right {
                    let shown = if grapheme.chars().all(|c| c.is_control()) { "·" } else { grapheme };
                    canvas.put(sx, y, shown, style);
                }
            }
        }
        col += cells;
        char_index += chars;
    }

    // Past the end of the text: the selected line break, an extra cursor at end of line, and
    // indent guides on blank lines.
    let at_line_end = row.segment.end >= line_len;
    if at_line_end && col >= options.left && col < right {
        let sx = x + (col - options.left) as u16;
        if row.cursors.contains(&line_len) {
            canvas.put(sx, y, " ", Style::colors(theme.editor_bg, theme.cursor));
        } else if contains(row.selections, line_len) {
            canvas.put(sx, y, " ", Style::new().on(theme.selection_bg));
        }
    }
    if options.show_indent_guides && line_len == 0 {
        for level in 0..row.indent_guides {
            let guide_col = level * tab;
            if guide_col >= options.left && guide_col < right && !contains(row.selections, 0) {
                let active = row.active_guide == Some(level);
                let color = if active { theme.indent_guide_active } else { theme.indent_guide };
                canvas.put(x + (guide_col - options.left) as u16, y, "│", Style::colors(color, line_bg));
            }
        }
    }
}

/// The display column, counted from the start of the row's segment, of the char at
/// `char_index` on the line (tabs expanded, wide characters counted as two).
pub fn column_of(text: &str, segment_start: usize, char_index: usize, tab_size: usize) -> usize {
    let tab = tab_size.max(1);
    let mut col = 0;
    let mut index = 0;
    for grapheme in text.graphemes(true) {
        if index >= char_index {
            break;
        }
        let chars = grapheme.chars().count();
        if index >= segment_start {
            col += if grapheme == "\t" { tab - col % tab } else { ui::width(grapheme).max(1) };
        }
        index += chars;
    }
    col
}

/// The char on the line at display column `col` of the row whose segment is `segment` (the
/// inverse of [`column_of`]): the char whose cells cover `col`, or the segment's end when
/// `col` is past the text. With `round`, a click on the right half of a character lands
/// after it, which is where a text cursor should go.
pub fn char_at_column(text: &str, segment: Range<usize>, col: usize, tab_size: usize, round: bool) -> usize {
    let tab = tab_size.max(1);
    let mut current = 0;
    let mut index = 0;
    for grapheme in text.graphemes(true) {
        let chars = grapheme.chars().count();
        if index >= segment.end {
            break;
        }
        if index >= segment.start {
            let cells = if grapheme == "\t" { tab - current % tab } else { ui::width(grapheme).max(1) };
            if col < current + cells {
                let past_middle = (col - current) * 2 >= cells;
                return if round && past_middle { index + chars } else { index };
            }
            current += cells;
        }
        index += chars;
    }
    segment.end.min(index.max(segment.start))
}

/// The vertical scrollbar: a thumb for the visible part plus marks for things worth
/// jumping to. One column wide.
#[derive(Debug, Clone, Default)]
pub struct ScrollMarks {
    /// (line, colour) marks: git changes, search matches.
    pub marks: Vec<(usize, Rgb)>,
    /// The primary cursor's line.
    pub cursor_line: Option<usize>,
}

/// How many lines the scrollbar's track stands for: the file plus the empty page you can
/// scroll into past its end (`visible` is how many lines the view holds).
fn scrollbar_span(total_lines: usize, visible: usize) -> usize {
    (total_lines.max(1) + visible.saturating_sub(1)).max(1)
}

/// The line a click on row `row` of a scrollbar `height` rows tall points at: the inverse of
/// where [`scrollbar`] draws a line's marks, clamped to the file.
pub fn scrollbar_line_at(row: u16, height: u16, total_lines: usize, visible: usize) -> usize {
    let track = height.max(1) as usize;
    let span = scrollbar_span(total_lines, visible);
    let row = (row as usize).min(track - 1);
    // The first line drawn on this row.
    (row * span).div_ceil(track).min(total_lines.saturating_sub(1))
}

// A drawing call: position, extent and scroll state are all plain numbers. `visible` is how
// many lines the view holds (its height), not how many the file has on screen: the track
// keeps its scale when a short file, or the end of a long one, leaves the view part empty.
#[allow(clippy::too_many_arguments)]
pub fn scrollbar(
    canvas: &mut Canvas<'_>,
    x: u16,
    y: u16,
    height: u16,
    total_lines: usize,
    top: usize,
    visible: usize,
    marks: &ScrollMarks,
    theme: &Theme,
) {
    if height == 0 {
        return;
    }
    let track = height as usize;
    let total = total_lines.max(1);
    let span = scrollbar_span(total_lines, visible);
    let row_of = |line: usize| -> usize { (line.min(span - 1) * track / span).min(track - 1) };
    let thumb_start = row_of(top);
    let thumb_len = (visible.max(1) * track).div_ceil(span).clamp(1, track - thumb_start);
    let needs_thumb = total > visible;

    for row in 0..track {
        let in_thumb = needs_thumb && row >= thumb_start && row < thumb_start + thumb_len;
        let bg = if in_thumb { theme.scrollbar_thumb } else { theme.scrollbar_track };
        canvas.put(x, y + row as u16, " ", Style::new().on(bg));
    }
    for &(line, color) in &marks.marks {
        let row = row_of(line);
        let in_thumb = needs_thumb && row >= thumb_start && row < thumb_start + thumb_len;
        let bg = if in_thumb { theme.scrollbar_thumb } else { theme.scrollbar_track };
        canvas.put(x, y + row as u16, "▐", Style::colors(color, bg));
    }
    if let Some(line) = marks.cursor_line {
        let row = row_of(line);
        let in_thumb = needs_thumb && row >= thumb_start && row < thumb_start + thumb_len;
        let bg = if in_thumb { theme.scrollbar_thumb } else { theme.scrollbar_track };
        canvas.put(x, y + row as u16, "━", Style::colors(theme.text_muted, bg));
    }
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    const OPTIONS: PaintOptions = PaintOptions {
        tab_size: 4,
        left: 0,
        show_indent_guides: true,
        colorize_brackets: true,
        show_whitespace_in_selection: true,
    };

    fn paint(r: &Row<'_>, width: u16, options: &PaintOptions) -> (String, Buffer) {
        let area = Rect::new(0, 0, width, 1);
        let mut buf = Buffer::empty(area);
        let theme = Theme::builtin();
        {
            let mut canvas = Canvas::new(&mut buf, area);
            row(&mut canvas, 0, 0, width, r, options, &theme);
        }
        let text = (0..width).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        (text, buf)
    }

    #[test]
    fn plain_text_and_tabs() {
        let (text, buf) = paint(&Row::plain("let x = 1;"), 14, &OPTIONS);
        assert_eq!(text, "let x = 1;    ");
        let theme = Theme::builtin();
        assert_eq!(buf[(0, 0)].fg, ui::color(theme.editor_fg));
        assert_eq!(buf[(12, 0)].bg, ui::color(theme.editor_bg));
        let (text, _) = paint(&Row::plain("\tx\ty"), 12, &OPTIONS);
        assert_eq!(text, "    x   y   ", "tabs advance to the next multiple of four");
    }

    #[test]
    fn syntax_spans_use_byte_ranges() {
        let theme = Theme::builtin();
        let keyword = Style::fg(Rgb::new(255, 0, 255));
        let string = Style::fg(Rgb::new(80, 250, 123));
        // "é" is two bytes: the string span starts at byte 9.
        let spans = [(0..3, keyword), (9..13, string)];
        let mut r = Row::plain("let é = \"hi\";");
        r.syntax = &spans;
        let (_, buf) = paint(&r, 20, &OPTIONS);
        assert_eq!(buf[(0, 0)].fg, ui::color(Rgb::new(255, 0, 255)));
        assert_eq!(buf[(2, 0)].fg, ui::color(Rgb::new(255, 0, 255)));
        assert_eq!(buf[(4, 0)].fg, ui::color(theme.editor_fg), "é is not in a span");
        assert_eq!(buf[(8, 0)].fg, ui::color(Rgb::new(80, 250, 123)), "the opening quote, char 8, byte 9");
        assert_eq!(buf[(11, 0)].fg, ui::color(Rgb::new(80, 250, 123)));
        assert_eq!(buf[(12, 0)].fg, ui::color(theme.editor_fg), "the semicolon");
    }

    #[test]
    fn selection_covers_text_and_the_line_break() {
        let theme = Theme::builtin();
        let selections = [2..6];
        let mut r = Row::plain("abcde");
        r.selections = &selections;
        let (_, buf) = paint(&r, 10, &OPTIONS);
        let selected = ui::color(theme.selection_bg);
        assert_ne!(buf[(1, 0)].bg, selected);
        assert_eq!(buf[(2, 0)].bg, selected);
        assert_eq!(buf[(4, 0)].bg, selected);
        assert_eq!(buf[(5, 0)].bg, selected, "the cell after the text shows the selected line break");
        assert_ne!(buf[(6, 0)].bg, selected);
    }

    #[test]
    fn whitespace_is_marked_inside_a_selection_only() {
        let selections = [0..3];
        let mut r = Row::plain("a b c");
        r.selections = &selections;
        let (text, _) = paint(&r, 8, &OPTIONS);
        assert_eq!(text, "a·b c   ");
        let mut options = OPTIONS;
        options.show_whitespace_in_selection = false;
        let (text, _) = paint(&r, 8, &options);
        assert_eq!(text, "a b c   ");
    }

    #[test]
    fn layering_selection_beats_match_beats_current_line() {
        let theme = Theme::builtin();
        let matches = [0..2, 4..6];
        let selections = [4..5];
        let mut r = Row::plain("ab  cd  ");
        r.matches = &matches;
        r.selections = &selections;
        r.current_line = true;
        r.active_match = Some(0..2);
        let (_, buf) = paint(&r, 10, &OPTIONS);
        assert_eq!(buf[(4, 0)].bg, ui::color(theme.selection_bg));
        assert_eq!(buf[(5, 0)].bg, ui::color(theme.search_match_bg));
        assert_eq!(buf[(3, 0)].bg, ui::color(theme.active_line_bg));
        assert_eq!(buf[(9, 0)].bg, ui::color(theme.active_line_bg), "the highlight runs to the edge");
        assert_ne!(buf[(0, 0)].bg, ui::color(theme.search_match_bg), "the active match is drawn stronger");
    }

    #[test]
    fn rainbow_brackets_and_the_matching_pair() {
        let theme = Theme::builtin();
        let brackets = [(1, 0u16), (2, 1), (3, 1), (4, 0)];
        let pair = [1, 4];
        let mut r = Row::plain("f(())");
        r.brackets = &brackets;
        r.matching_brackets = &pair;
        let (_, buf) = paint(&r, 8, &OPTIONS);
        assert_eq!(buf[(1, 0)].fg, ui::color(theme.accents[0]));
        assert_eq!(buf[(2, 0)].fg, ui::color(theme.accents[1]));
        assert_eq!(buf[(4, 0)].fg, ui::color(theme.accents[0]));
        assert_eq!(buf[(1, 0)].bg, ui::color(theme.matching_bracket_bg));
        assert_eq!(buf[(2, 0)].bg, ui::color(theme.editor_bg));
        let mut options = OPTIONS;
        options.colorize_brackets = false;
        let (_, buf) = paint(&r, 8, &options);
        assert_eq!(buf[(1, 0)].fg, ui::color(theme.editor_fg));
    }

    #[test]
    fn indent_guides_in_leading_whitespace() {
        let mut r = Row::plain("        deep");
        r.indent_guides = 2;
        let (text, _) = paint(&r, 14, &OPTIONS);
        assert_eq!(text, "│   │   deep  ", "a guide at the start of each indent level");
        // A blank line inside a block still shows the block's guides.
        let mut blank = Row::plain("");
        blank.indent_guides = 3;
        let (text, _) = paint(&blank, 14, &OPTIONS);
        assert_eq!(text, "│   │   │     ");
        let mut options = OPTIONS;
        options.show_indent_guides = false;
        let (text, _) = paint(&r, 14, &options);
        assert_eq!(text, "        deep  ");
    }

    #[test]
    fn extra_cursors_are_drawn_as_blocks() {
        let theme = Theme::builtin();
        let cursors = [1, 3];
        let mut r = Row::plain("abc");
        r.cursors = &cursors;
        let (_, buf) = paint(&r, 6, &OPTIONS);
        assert_eq!(buf[(1, 0)].bg, ui::color(theme.cursor));
        assert_eq!(buf[(1, 0)].symbol(), "b");
        assert_eq!(buf[(3, 0)].bg, ui::color(theme.cursor), "a cursor at the end of the line");
        assert_eq!(buf[(0, 0)].bg, ui::color(theme.editor_bg));
    }

    #[test]
    fn horizontal_scroll_and_wide_characters() {
        let mut options = OPTIONS;
        options.left = 3;
        let (text, _) = paint(&Row::plain("0123456789"), 4, &options);
        assert_eq!(text, "3456");
        // Scrolled so that half of a wide character would show: it is left blank.
        options.left = 1;
        let (text, _) = paint(&Row::plain("房產x"), 6, &options);
        assert_eq!(text.chars().next(), Some(' '));
        assert!(text.contains('產'));
        // A wide character that does not fit at the right edge is not half drawn.
        let (text, _) = paint(&Row::plain("ab房"), 3, &OPTIONS);
        assert_eq!(text, "ab ");
    }

    #[test]
    fn wrapped_segments_paint_only_their_part() {
        let mut r = Row::plain("hello world");
        r.segment = 6..11;
        let (text, _) = paint(&r, 8, &OPTIONS);
        assert_eq!(text, "world   ");
        r.segment = 0..6;
        let selections = [0..12];
        r.selections = &selections;
        let (_, buf) = paint(&r, 8, &OPTIONS);
        let theme = Theme::builtin();
        assert_ne!(buf[(6, 0)].bg, ui::color(theme.selection_bg), "no line-break cell on a non-final segment");
    }

    #[test]
    fn columns_and_chars_convert_both_ways() {
        assert_eq!(column_of("\tab", 0, 0, 4), 0);
        assert_eq!(column_of("\tab", 0, 1, 4), 4);
        assert_eq!(column_of("\tab", 0, 3, 4), 6);
        assert_eq!(column_of("房產x", 0, 2, 4), 4);
        assert_eq!(column_of("hello world", 6, 8, 4), 2, "columns restart on a wrapped row");
        assert_eq!(char_at_column("\tab", 0..3, 0, 4, false), 0);
        assert_eq!(char_at_column("\tab", 0..3, 3, 4, false), 0, "anywhere in the tab is the tab");
        assert_eq!(char_at_column("\tab", 0..3, 3, 4, true), 1, "the right half of the tab rounds past it");
        assert_eq!(char_at_column("\tab", 0..3, 4, 4, false), 1);
        assert_eq!(char_at_column("\tab", 0..3, 99, 4, false), 3, "past the end is the end");
        assert_eq!(char_at_column("房產x", 0..3, 3, 4, true), 2);
        assert_eq!(char_at_column("hello world", 6..11, 1, 4, false), 7);
        assert_eq!(char_at_column("", 0..0, 5, 4, true), 0);
    }

    #[test]
    fn gutter_numbers_marks_and_width() {
        assert_eq!(gutter_width(7, true), 5);
        assert_eq!(gutter_width(999, true), 5);
        assert_eq!(gutter_width(1000, true), 6);
        assert_eq!(gutter_width(1000, false), 2);
        let area = Rect::new(0, 0, 6, 3);
        let mut buf = Buffer::empty(area);
        let theme = Theme::builtin();
        {
            let mut canvas = Canvas::new(&mut buf, area);
            gutter(&mut canvas, 0, 0, 5, GutterInfo { number: Some(42), current: true, git: Some(GitMark::Added) }, true, &theme);
            gutter(&mut canvas, 0, 1, 5, GutterInfo { number: None, current: false, git: None }, true, &theme);
            gutter(&mut canvas, 0, 2, 5, GutterInfo { number: Some(7), current: false, git: Some(GitMark::DeletedAbove) }, true, &theme);
        }
        let text = |y: u16| (0..5).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>();
        assert_eq!(text(0), "▎ 42 ");
        assert_eq!(text(1), "     ", "a wrapped row has no number");
        assert_eq!(text(2), "▔  7 ");
        assert_eq!(buf[(2, 0)].fg, ui::color(theme.active_line_number));
        assert_eq!(buf[(3, 2)].fg, ui::color(theme.line_number));
        assert_eq!(buf[(0, 0)].fg, ui::color(theme.created));
    }

    #[test]
    fn scrollbar_thumb_and_marks() {
        let area = Rect::new(0, 0, 1, 10);
        let mut buf = Buffer::empty(area);
        let theme = Theme::builtin();
        let marks = ScrollMarks { marks: vec![(0, Rgb::new(1, 2, 3)), (99, Rgb::new(4, 5, 6))], cursor_line: Some(50) };
        {
            let mut canvas = Canvas::new(&mut buf, area);
            scrollbar(&mut canvas, 0, 0, 10, 100, 0, 20, &marks, &theme);
        }
        let thumb = ui::color(theme.scrollbar_thumb);
        assert_eq!(buf[(0, 0)].bg, thumb);
        assert_ne!(buf[(0, 9)].bg, thumb);
        assert_eq!(buf[(0, 0)].symbol(), "▐");
        assert_eq!(buf[(0, 0)].fg, ui::color(Rgb::new(1, 2, 3)));
        assert_eq!(buf[(0, 8)].symbol(), "▐", "line 99 of 100 sits near the bottom");
        assert_eq!(buf[(0, 4)].symbol(), "━");
        // A file that fits has no thumb.
        let mut buf = Buffer::empty(area);
        {
            let mut canvas = Canvas::new(&mut buf, area);
            scrollbar(&mut canvas, 0, 0, 10, 5, 0, 20, &ScrollMarks::default(), &theme);
        }
        assert!((0..10).all(|y| buf[(0, y)].bg != thumb));
        // A click on a row lands on the line whose marks are drawn there.
        for line in [0, 37, 50, 99] {
            let span = 100 + 20 - 1;
            let row = (line * 10 / span) as u16;
            let back = scrollbar_line_at(row, 10, 100, 20);
            assert_eq!((back * 10 / span) as u16, row, "line {line}: row {row}, back to line {back}");
        }
        assert_eq!(scrollbar_line_at(0, 10, 100, 20), 0);
        assert_eq!(scrollbar_line_at(99, 10, 100, 20), 99, "past the end: the last line");
        assert_eq!(scrollbar_line_at(3, 0, 0, 0), 0, "odd sizes");
        // Odd sizes do not panic.
        let mut buf = Buffer::empty(area);
        let mut canvas = Canvas::new(&mut buf, area);
        scrollbar(&mut canvas, 0, 0, 0, 5, 0, 20, &ScrollMarks::default(), &theme);
        scrollbar(&mut canvas, 0, 0, 1, 0, 9, 0, &ScrollMarks::default(), &theme);
        scrollbar(&mut canvas, 0, 0, 3, 1_000_000, 999_999, 1, &marks, &theme);
    }
}
