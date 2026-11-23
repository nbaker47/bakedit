//! Drawing the editor: breadcrumbs, the find bar, the gutter, the text rows, the minimap
//! and the scrollbar. Only the rows on screen are looked at.

use std::cell::RefCell;
use std::ops::Range;

use bakedit_core::{Rgb, Theme};
use bakedit_git::LineHunkKind;
use bakedit_syntax::{CaptureId, StyleCache, SyntaxResult};
use bakedit_text::Editor;
use ratatui::layout::Rect;

use super::layout::{Geometry, Scroll, ShownRow};
use super::shift::ShiftLog;
use super::EditorItem;
use crate::minimap::{self, MinimapSource};
use crate::paint::{self, GutterInfo, PaintOptions, Row, ScrollMarks};
use crate::settings::{MinimapShow, Settings};
use crate::ui::{self, Canvas, Style};
use crate::view::{Cursor, CursorShape, RenderCx};

/// How far below a blank line to look for the indentation its guides follow.
const GUIDE_LOOKAHEAD: usize = 64;
/// The narrowest text area that still gets a minimap.
const MINIMAP_MIN_TEXT: u16 = 50;
/// Scrollbar marks for search matches are thinned to this many.
const MATCH_MARKS: usize = 2000;

fn syntax_style(styles: &mut StyleCache, theme: &Theme, capture: CaptureId) -> Style {
    let mut style = Style::new();
    if let Some(syntax) = styles.style(theme, capture) {
        style.fg = syntax.color;
        style.italic = syntax.italic;
        style.bold = syntax.bold;
    }
    style
}

/// The highlight spans of `line` (`len` bytes) of the current text: the newest ones if they
/// are for this text, the older ones shifted into place otherwise.
fn spans_of(shifts: &ShiftLog, result: &SyntaxResult, line: usize, len: usize) -> Vec<(Range<usize>, CaptureId)> {
    if shifts.is_empty() {
        result.lines.spans_for_line(line).to_vec()
    } else {
        shifts.spans_for_line(line, len, &result.lines)
    }
}

/// What the minimap reads: the text, coloured by the (possibly older) highlights.
struct MapSource<'a> {
    editor: &'a Editor,
    result: Option<&'a SyntaxResult>,
    shifts: &'a ShiftLog,
    styles: RefCell<&'a mut StyleCache>,
    theme: &'a Theme,
    tab: usize,
}

impl MinimapSource for MapSource<'_> {
    fn line_count(&self) -> usize {
        self.editor.buffer().len_lines()
    }

    fn runs(&self, line: usize, out: &mut Vec<(usize, usize, Rgb)>) {
        let text = self.editor.buffer().line_text(line);
        let spans = self.result.map(|result| spans_of(self.shifts, result, line, text.len())).unwrap_or_default();
        let mut styles = self.styles.borrow_mut();
        let mut span = 0;
        let mut col = 0usize;
        let mut run: Option<(usize, Rgb)> = None;
        for (byte, ch) in text.char_indices() {
            while span < spans.len() && spans[span].0.end <= byte {
                span += 1;
            }
            let cells = if ch == '\t' { self.tab - col % self.tab } else { ui::width(ch.encode_utf8(&mut [0; 4])).max(1) };
            if ch.is_whitespace() {
                if let Some((start, color)) = run.take() {
                    out.push((start, col, color));
                }
            } else {
                let color = match spans.get(span) {
                    Some((range, capture)) if range.start <= byte => syntax_style(&mut styles, self.theme, *capture).fg,
                    _ => None,
                }
                .unwrap_or(self.theme.editor_fg);
                match run {
                    Some((_, current)) if current == color => {}
                    Some((start, current)) => {
                        out.push((start, col, current));
                        run = Some((col, color));
                    }
                    None => run = Some((col, color)),
                }
            }
            col += cells;
            if col > 400 {
                break;
            }
        }
        if let Some((start, color)) = run {
            out.push((start, col, color));
        }
    }
}

impl EditorItem {
    /// Whether the minimap is drawn in an editor `width` columns wide.
    pub(super) fn minimap_shown(&self, settings: &Settings, width: u16) -> bool {
        let wanted = match self.minimap_override {
            Some(wanted) => wanted,
            None => match settings.minimap {
                MinimapShow::Always => true,
                MinimapShow::Auto => width >= 100,
                MinimapShow::Never => false,
            },
        };
        wanted && width >= MINIMAP_MIN_TEXT + 12
    }

    /// How many indent guides a line gets: its own indent level, or for a blank line the
    /// level of the code below it.
    fn guide_level(&self, line: usize, tab: usize) -> usize {
        let buffer = self.editor.buffer();
        let line_count = buffer.len_lines();
        let level = |line: usize| self.editor.indent_of_line(line) / tab.max(1);
        if !buffer.line_text(line).trim().is_empty() {
            return level(line);
        }
        (line + 1..line_count.min(line + GUIDE_LOOKAHEAD)).find(|&l| !buffer.line_text(l).trim().is_empty()).map(level).unwrap_or(0)
    }

    /// The indent guide to highlight and the lines it runs over: the block the cursor is in.
    fn active_guide(&self, tab: usize, rows: usize) -> Option<(usize, Range<usize>)> {
        let cursor_line = self.cursor_line();
        let level = self.guide_level(cursor_line, tab);
        let first_visible = self.scroll.line;
        if level == 0 || cursor_line < first_visible || cursor_line >= first_visible + rows {
            return None;
        }
        let mut start = cursor_line;
        while start > first_visible && self.guide_level(start - 1, tab) >= level {
            start -= 1;
        }
        let line_count = self.editor.buffer().len_lines();
        let mut end = cursor_line;
        while end + 1 < line_count && end + 1 < first_visible + rows && self.guide_level(end + 1, tab) >= level {
            end += 1;
        }
        Some((level - 1, start..end + 1))
    }

    fn breadcrumb_text(&self, root: &std::path::Path) -> Option<String> {
        if self.kind != "editor" {
            return None;
        }
        match self.file_path() {
            Some(path) => {
                let shown = path.strip_prefix(root).unwrap_or(path);
                Some(shown.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join(" › "))
            }
            None => Some(self.title.clone()),
        }
    }

    fn draw_breadcrumbs(&self, canvas: &mut Canvas<'_>, rect: Rect, text: &str, theme: &Theme) {
        canvas.fill(rect, theme.editor_bg);
        let room = rect.width.saturating_sub(2) as usize;
        let muted = Style::colors(theme.text_muted, theme.editor_bg);
        if ui::width(text) > room {
            canvas.text(rect.x + 1, rect.y, &ui::truncate_start(text, room), muted, rect.width.saturating_sub(2));
            return;
        }
        let mut x = rect.x + 1;
        let parts: Vec<&str> = text.split(" › ").collect();
        for (index, part) in parts.iter().enumerate() {
            let last = index + 1 == parts.len();
            let style = if last { Style::colors(theme.text, theme.editor_bg) } else { muted };
            x += canvas.text(x, rect.y, part, style, rect.right().saturating_sub(x)) ;
            if !last {
                x += canvas.text(x, rect.y, " › ", muted, rect.right().saturating_sub(x));
            }
        }
    }

    pub(super) fn draw(&mut self, canvas: &mut Canvas<'_>, area: Rect, cx: &RenderCx<'_>) -> Option<Cursor> {
        let theme = cx.theme;
        let settings = cx.settings;
        canvas.fill(area, theme.editor_bg);
        self.geo = Geometry { area, ..Geometry::default() };
        if area.width == 0 || area.height == 0 {
            return None;
        }
        if self.styles_theme != theme.name {
            self.styles = StyleCache::new();
            self.styles_theme = theme.name.clone();
        }
        self.request_highlights(cx.project);
        self.refresh_git_marks(cx.project);
        self.find_refresh_matches(cx.project);
        let syntax = cx.project.syntax(self.doc_id);
        if let Some(result) = &syntax {
            self.shifts.prune(result.version);
        }
        self.shown_highlights = syntax.as_ref().map(|result| result.version);
        let tab = self.tab_size(settings);
        self.sync_engine(settings);

        // ---- layout -----------------------------------------------------------------
        let mut top = area.y;
        let mut left_rows = area.height;
        if let (true, Some(_)) = (settings.breadcrumbs && left_rows > 4, self.breadcrumb_text(cx.root)) {
            self.geo.breadcrumbs = Rect::new(area.x, top, area.width, 1);
            top += 1;
            left_rows -= 1;
        }
        let find_rows = self.find_rows().min(left_rows.saturating_sub(3));
        if find_rows > 0 {
            self.geo.find = Rect::new(area.x, top, area.width, find_rows);
            top += find_rows;
            left_rows -= find_rows;
        }
        let body = Rect::new(area.x, top, area.width, left_rows);
        let line_count = self.editor.buffer().len_lines();
        let numbers = self.numbers_override.unwrap_or(settings.line_numbers);
        let relative = self.relative_override.unwrap_or(settings.relative_line_numbers);
        let gutter_width = paint::gutter_width(line_count, numbers).min(body.width);
        let scrollbar = u16::from(body.width > gutter_width + 4);
        let free = body.width - gutter_width - scrollbar;
        let map_width = if self.minimap_shown(settings, free) { (free / 8).clamp(8, 16) } else { 0 };
        let text_width = free - map_width;
        self.geo.gutter = Rect::new(body.x, body.y, gutter_width, body.height);
        self.geo.text = Rect::new(body.x + gutter_width, body.y, text_width, body.height);
        self.geo.minimap = Rect::new(body.x + gutter_width + text_width, body.y, map_width, body.height);
        self.geo.scrollbar = Rect::new(body.right() - scrollbar, body.y, scrollbar, body.height);
        let rows = body.height as usize;

        let wrap = self.wrap_width(settings, text_width as usize);
        {
            let engine = self.editor.settings_mut();
            engine.wrap_width = wrap;
            engine.page_rows = rows.max(1);
        }
        if wrap.is_some() {
            self.left = 0;
        }
        self.apply_reveal(rows, settings.vertical_scroll_margin as usize);

        // Horizontal scroll: keep the cursor's column in view.
        let cursor_position = self.editor.display_position(self.cursor());
        if wrap.is_none() {
            let margin = (settings.horizontal_scroll_margin as usize).min((text_width as usize).saturating_sub(1) / 2);
            if cursor_position.col < self.left + margin {
                self.left = cursor_position.col.saturating_sub(margin);
            } else if text_width > 0 && cursor_position.col + margin + 1 > self.left + text_width as usize {
                self.left = cursor_position.col + margin + 1 - text_width as usize;
            }
        }

        // ---- what is the same for every row ----------------------------------------
        let cursor_line = self.cursor_line();
        let primary = self.editor.selections().primary_index();
        let selections: Vec<Range<usize>> = self.editor.selections().iter().filter(|s| !s.is_empty()).map(|s| s.range()).collect();
        let extra_cursors: Vec<usize> =
            self.editor.selections().iter().enumerate().filter(|(index, s)| *index != primary && s.is_empty()).map(|(_, s)| s.head).collect();
        let pair = self.editor.matching_bracket(self.cursor());
        let active_match = self.active_match();
        let active_guide = if settings.indent_guides { self.active_guide(tab, rows) } else { None };
        let options = PaintOptions {
            tab_size: tab,
            left: self.left,
            show_indent_guides: settings.indent_guides,
            colorize_brackets: settings.colorize_brackets,
            show_whitespace_in_selection: true,
        };

        // ---- rows ---------------------------------------------------------------------
        let mut shown: Vec<ShownRow> = Vec::with_capacity(rows);
        let mut scroll = self.scroll;
        scroll.row = scroll.row.min(self.rows_in(scroll.line).saturating_sub(1));
        let mut y = body.y;
        let current_line_on = settings.current_line_highlight;
        while y < body.bottom() {
            if scroll.line >= line_count {
                canvas.fill(Rect::new(body.x, y, gutter_width, 1), theme.gutter_bg);
                y += 1;
                continue;
            }
            let line = scroll.line;
            let buffer = self.editor.buffer();
            let text = buffer.line_text(line);
            let line_start = buffer.line_to_char(line);
            let line_len = buffer.line_len(line);
            let line_end = line_start + line_len;
            let ranges: Vec<Range<usize>> = if wrap.is_some() { self.editor.line_rows(line) } else { std::iter::once(0..line_len).collect() };

            let spans: Vec<(Range<usize>, Style)> = match &syntax {
                Some(result) => {
                    let styles = &mut self.styles;
                    spans_of(&self.shifts, result, line, text.len()).into_iter().map(|(range, capture)| (range, syntax_style(styles, theme, capture))).collect()
                }
                None => Vec::new(),
            };
            let mut brackets: Vec<(usize, u16)> = Vec::new();
            if let (Some(result), true) = (&syntax, settings.colorize_brackets) {
                let start_byte = buffer.char_to_byte(line_start);
                let end_byte = start_byte + text.len();
                let mut marks: Vec<(usize, u16)> = Vec::new();
                if self.shifts.is_empty() {
                    let first = result.brackets.partition_point(|mark| mark.byte < start_byte);
                    marks.extend(result.brackets[first..].iter().take_while(|mark| mark.byte < end_byte).map(|mark| (mark.byte, mark.depth)));
                } else {
                    self.shifts.brackets_in(start_byte..end_byte, &result.brackets, &mut marks);
                }
                brackets = marks.into_iter().map(|(byte, depth)| (buffer.byte_to_char(byte) - line_start, depth)).collect();
            }
            let first_sel = selections.partition_point(|range| range.end <= line_start);
            let line_selections: Vec<Range<usize>> = selections[first_sel..]
                .iter()
                .take_while(|range| range.start <= line_end)
                .map(|range| range.start.saturating_sub(line_start)..(range.end - line_start).min(line_len + 1))
                .collect();
            let first_cursor = extra_cursors.partition_point(|&pos| pos < line_start);
            let cursors: Vec<usize> = extra_cursors[first_cursor..].iter().take_while(|&&pos| pos <= line_end).map(|pos| pos - line_start).collect();
            let matching: Vec<usize> = pair
                .map(|(open, close)| [open, close].into_iter().filter(|&p| p >= line_start && p < line_end).map(|p| p - line_start).collect())
                .unwrap_or_default();
            let matches: Vec<Range<usize>> =
                self.matches_in(line_start..line_end + 1).iter().map(|m| m.start.saturating_sub(line_start)..(m.end - line_start).min(line_len)).collect();
            let active = active_match.as_ref().filter(|m| m.start >= line_start && m.start <= line_end).map(|m| m.start - line_start..m.end - line_start);
            let guides = if settings.indent_guides { self.guide_level(line, tab) } else { 0 };
            let active_level = active_guide.as_ref().filter(|(_, lines)| lines.contains(&line)).map(|(level, _)| *level);
            let current = line == cursor_line && current_line_on;
            let git = self.git.mark(line, line_count);

            for (row_index, range) in ranges.iter().enumerate().skip(if line == self.scroll.line { scroll.row } else { 0 }) {
                if y >= body.bottom() {
                    break;
                }
                let number = if row_index == 0 {
                    Some(if relative && line != cursor_line { line.abs_diff(cursor_line) } else { line + 1 })
                } else {
                    None
                };
                paint::gutter(canvas, body.x, y, gutter_width, GutterInfo { number, current, git }, numbers, theme);
                let mut row = Row::plain(&text);
                row.segment = range.clone();
                row.syntax = &spans;
                row.selections = &line_selections;
                row.matches = &matches;
                row.active_match = active.clone();
                row.cursors = &cursors;
                row.brackets = &brackets;
                row.matching_brackets = &matching;
                row.current_line = current;
                row.indent_guides = guides;
                row.active_guide = active_level;
                paint::row(canvas, self.geo.text.x, y, text_width, &row, &options, theme);
                shown.push(ShownRow { line, row: row_index });
                y += 1;
            }
            scroll = Scroll { line: line + 1, row: 0 };
        }
        let first_line = shown.first().map_or(0, |row| row.line);
        let last_line = shown.last().map_or(0, |row| row.line);
        self.geo.viewport_lines = (last_line + 1).saturating_sub(first_line).max(1);
        self.geo.rows = shown;

        // ---- minimap, scrollbar --------------------------------------------------------
        if map_width > 0 {
            let source = MapSource {
                editor: &self.editor,
                result: syntax.as_ref(),
                shifts: &self.shifts,
                styles: RefCell::new(&mut self.styles),
                theme,
                tab,
            };
            minimap::render(canvas, self.geo.minimap, &source, self.scroll.line, self.geo.viewport_lines, settings.minimap_max_columns as usize, theme);
        }
        if scrollbar == 1 {
            let mut marks: Vec<(usize, Rgb)> = self
                .git
                .hunks
                .iter()
                .map(|hunk| {
                    let color = match hunk.kind {
                        LineHunkKind::Added => theme.created,
                        LineHunkKind::Modified => theme.modified,
                        LineHunkKind::Deleted => theme.deleted,
                    };
                    (hunk.new_start, color)
                })
                .collect();
            let match_color = theme.search_match_bg.lerp(theme.text_accent, 0.45);
            marks.extend(self.match_lines(MATCH_MARKS).into_iter().map(|line| (line, match_color)));
            let marks = ScrollMarks { marks, cursor_line: Some(cursor_line) };
            // The track's scale is the view's height, so marks stay put while scrolling.
            paint::scrollbar(canvas, self.geo.scrollbar.x, body.y, body.height, line_count, self.scroll.line, body.height as usize, &marks, theme);
        }

        // ---- breadcrumbs and the find bar ---------------------------------------------
        if self.geo.breadcrumbs.height > 0 {
            if let Some(text) = self.breadcrumb_text(cx.root) {
                self.draw_breadcrumbs(canvas, self.geo.breadcrumbs, &text, theme);
            }
        }
        let find_cursor = if find_rows > 0 { self.draw_find(canvas, self.geo.find, cx) } else { None };
        if let Some(cursor) = find_cursor {
            return Some(cursor);
        }
        if self.find_focused() {
            return None;
        }

        let index = self.geo.rows.iter().position(|row| row.line == cursor_position.line && row.row == cursor_position.row)?;
        let col = cursor_position.col.checked_sub(self.left).filter(|col| *col < text_width as usize)?;
        let shape = if self.modal && !self.insert { CursorShape::Block } else { CursorShape::Bar };
        Some(Cursor { col: self.geo.text.x + col as u16, row: body.y + index as u16, shape })
    }
}
