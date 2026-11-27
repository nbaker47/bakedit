//! Lightweight Markdown renderer for NAV mode in the editor and diff views.
//!
//! Renders headings, lists, task checkboxes, blockquotes, horizontal rules, GFM pipe
//! tables (with aligned columns, box-drawing borders, and in-cell wrapping), code-fence
//! dividers, inline code, bold, italic, strikethrough, and `[label](url)` links into
//! styled runs while leaving fenced code-block interiors untouched for tree-sitter.

use bakedit_core::{Rgb, Theme};

use crate::ui::{self, Canvas, Style};

/// One styled run of rendered Markdown text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

/// Column alignment parsed from a GFM table delimiter row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// Which kind of row a line occupies inside a detected GFM table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableRowKind {
    Header,
    Delimiter,
    Body,
}

/// Computed layout for a contiguous GFM pipe table covering `start..end`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableSpec {
    pub start: usize,
    pub end: usize,
    pub indent: usize,
    pub alignments: Vec<Align>,
    pub widths: Vec<usize>,
}

impl TableSpec {
    /// Classify a line index within `self.start..self.end`.
    pub fn row_kind(&self, line_idx: usize) -> TableRowKind {
        if line_idx == self.start {
            TableRowKind::Header
        } else if line_idx == self.start + 1 {
            TableRowKind::Delimiter
        } else {
            TableRowKind::Body
        }
    }
}

/// True if `line` opens or closes a fenced code block (``` or ~~~).
pub fn is_fence(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("```") || trimmed.starts_with("~~~")
}

/// Optional language label after an opening fence (e.g. ```` ```rust ```` -> `Some("rust")`).
pub fn fence_lang(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let rest = trimmed
        .strip_prefix("```")
        .or_else(|| trimmed.strip_prefix("~~~"))?
        .trim();
    if rest.is_empty() { None } else { Some(rest) }
}

/// Render a single Markdown line (outside a code fence, or a fence delimiter itself) into
/// styled runs.
pub fn render_line(line: &str, bg: Rgb, theme: &Theme, width: usize, is_closing_fence: bool) -> Vec<Span> {
    let trimmed = line.trim();
    if is_fence(line) {
        let rule_style = Style::colors(theme.border, bg);
        if is_closing_fence {
            return vec![Span { text: "─".repeat(width.min(48).max(8)), style: rule_style }];
        }
        if let Some(lang) = fence_lang(line) {
            let lead = "── ";
            let tail_len = width.min(48).saturating_sub(lead.len() + lang.len() + 1).max(4);
            return vec![
                Span { text: lead.to_string(), style: rule_style },
                Span { text: lang.to_string(), style: Style::colors(theme.text_muted, bg).italic() },
                Span { text: format!(" {}", "─".repeat(tail_len)), style: rule_style },
            ];
        }
        return vec![Span { text: "─".repeat(width.min(48).max(8)), style: rule_style }];
    }

    // Horizontal rule: ---, ***, ___ (3+ chars, optional spaces).
    if is_hr(trimmed) {
        return vec![Span { text: "─".repeat(width.max(4)), style: Style::colors(theme.border, bg) }];
    }

    // ATX Headings: # .. ######
    if let Some((level, rest)) = strip_heading(line) {
        let color = match level {
            1 => theme.panel_focused_border,
            2 => theme.ansi[4],
            3 => theme.ansi[5],
            _ => theme.text_accent,
        };
        let mut base = Style::colors(color, bg).bold();
        if level == 1 {
            base = base.underline();
        }
        return parse_inline(rest, base, bg, theme);
    }

    // Blockquote: > ...
    let leading_ws = line.len() - line.trim_start().len();
    let after_ws = &line[leading_ws..];
    if let Some(rest) = after_ws.strip_prefix("> ").or_else(|| if after_ws == ">" { Some("") } else { None }) {
        let mut out = Vec::new();
        if leading_ws > 0 {
            out.push(Span { text: line[..leading_ws].to_string(), style: Style::colors(theme.editor_fg, bg) });
        }
        out.push(Span { text: "▎ ".to_string(), style: Style::colors(theme.text_accent, bg).bold() });
        let base = Style::colors(theme.text_muted, bg).italic();
        out.extend(parse_inline(rest, base, bg, theme));
        return out;
    }

    // Task list & unordered bullet list: - [ ] / - [x] / - / * / +
    if let Some((prefix_text, prefix_color, rest)) = strip_list_marker(after_ws, theme) {
        let mut out = Vec::new();
        if leading_ws > 0 {
            out.push(Span { text: line[..leading_ws].to_string(), style: Style::colors(theme.editor_fg, bg) });
        }
        out.push(Span { text: prefix_text.to_string(), style: Style::colors(prefix_color, bg).bold() });
        let base = Style::colors(theme.editor_fg, bg);
        out.extend(parse_inline(rest, base, bg, theme));
        return out;
    }

    // Ordered list: "1. foo"
    if let Some((num_marker, rest)) = strip_ordered_marker(after_ws) {
        let mut out = Vec::new();
        if leading_ws > 0 {
            out.push(Span { text: line[..leading_ws].to_string(), style: Style::colors(theme.editor_fg, bg) });
        }
        out.push(Span { text: format!("{num_marker} "), style: Style::colors(theme.text_accent, bg).bold() });
        let base = Style::colors(theme.editor_fg, bg);
        out.extend(parse_inline(rest, base, bg, theme));
        return out;
    }

    parse_inline(line, Style::colors(theme.editor_fg, bg), bg, theme)
}

/// Draw a rendered Markdown line into `canvas` at `(x, y)` within `width` columns,
/// respecting horizontal scroll `left`.
pub fn draw_line(
    canvas: &mut Canvas<'_>,
    x: u16,
    y: u16,
    width: u16,
    left: usize,
    line: &str,
    bg: Rgb,
    theme: &Theme,
    is_closing_fence: bool,
) {
    if width == 0 {
        return;
    }
    let spans = render_line(line, bg, theme, width as usize, is_closing_fence);
    draw_spans(canvas, x, y, width, left, &spans, bg);
}

/// Detect whether `line_idx` belongs to a GFM pipe table and compute its column widths.
///
/// `line_at` returns `None` outside the document or across hard boundaries (such as diff
/// file/hunk headers). Scanning stops automatically at blank lines, code fences, headings,
/// blockquotes, and lines without an unescaped `|`.
pub fn detect_table_at<F>(
    line_idx: usize,
    line_count: usize,
    max_width: usize,
    theme: &Theme,
    line_at: F,
) -> Option<TableSpec>
where
    F: Fn(usize) -> Option<String>,
{
    const MAX_SCAN: usize = 512;
    if line_idx >= line_count {
        return None;
    }
    let cur = line_at(line_idx)?;
    if !is_table_line(&cur) {
        return None;
    }

    // Collect the contiguous run of pipe-bearing lines surrounding `line_idx`.
    let mut run_start = line_idx;
    while run_start > 0 && line_idx - run_start < MAX_SCAN {
        let Some(prev) = line_at(run_start - 1) else { break };
        if !is_table_line(&prev) {
            break;
        }
        run_start -= 1;
    }
    let mut run_end = line_idx + 1;
    while run_end < line_count && run_end - line_idx < MAX_SCAN {
        let Some(next) = line_at(run_end) else { break };
        if !is_table_line(&next) {
            break;
        }
        run_end += 1;
    }
    if run_end - run_start < 2 {
        return None;
    }

    let lines: Vec<String> = (run_start..run_end).filter_map(&line_at).collect();
    if lines.len() != run_end - run_start {
        return None;
    }

    // Find every delimiter row inside the run (must have a header row immediately before it).
    let mut delim_positions: Vec<(usize, Vec<Align>)> = Vec::new();
    for offset in 1..lines.len() {
        if let Some(aligns) = parse_delimiter_row(&lines[offset]) {
            if !split_table_cells(&lines[offset - 1]).is_empty() {
                delim_positions.push((run_start + offset, aligns));
            }
        }
    }
    if delim_positions.is_empty() {
        return None;
    }

    // Pick the table whose range [delim - 1, next_table_start) contains `line_idx`.
    let mut chosen: Option<(usize, usize, Vec<Align>)> = None;
    for (i, (delim_idx, aligns)) in delim_positions.iter().enumerate() {
        let t_start = delim_idx - 1;
        let t_end = if i + 1 < delim_positions.len() {
            delim_positions[i + 1].0 - 1
        } else {
            run_end
        };
        if line_idx >= t_start && line_idx < t_end {
            chosen = Some((t_start, t_end, aligns.clone()));
            break;
        }
    }
    let (start, end, mut alignments) = chosen?;

    let header_line = &lines[start - run_start];
    let indent = header_line.bytes().take_while(|&b| b == b' ').count();

    let mut num_cols = alignments.len();
    let mut parsed_rows: Vec<Vec<String>> = Vec::with_capacity(end - start);
    for idx in start..end {
        if idx == start + 1 {
            continue;
        }
        let cells = split_table_cells(&lines[idx - run_start]);
        num_cols = num_cols.max(cells.len());
        parsed_rows.push(cells);
    }
    if num_cols == 0 {
        return None;
    }
    while alignments.len() < num_cols {
        alignments.push(Align::Left);
    }

    // Measure natural display width of each column using rendered inline Markdown spans.
    let base = Style::colors(theme.editor_fg, theme.editor_bg);
    let mut widths = vec![3usize; num_cols];
    for row in &parsed_rows {
        for (c, cell) in row.iter().enumerate() {
            let spans = parse_inline(cell, base, theme.editor_bg, theme);
            let w = spans_display_width(&spans);
            if w > widths[c] {
                widths[c] = w;
            }
        }
    }

    // Fit columns inside `max_width` by shrinking only the widest columns (water-filling).
    // Total width = indent + sum(widths) + num_cols * 3 + 1.
    let chrome = indent + num_cols * 3 + 1;
    if max_width > chrome {
        let avail = max_width - chrome;
        let min_col = 3usize;
        if avail >= num_cols * min_col {
            let mut total: usize = widths.iter().sum();
            while total > avail {
                let max_w = widths.iter().copied().max().unwrap_or(0);
                if max_w <= min_col {
                    break;
                }
                for w in &mut widths {
                    if *w == max_w && total > avail {
                        *w -= 1;
                        total -= 1;
                    }
                }
            }
        }
    }

    Some(TableSpec { start, end, indent, alignments, widths })
}

/// Render one visual sub-row (`row_index` of `max_rows`) of a GFM table line into styled spans.
///
/// Returns an empty `Vec` for continuation rows (`row_index > 0`) when the table row has no
/// wrapped cell content remaining on that sub-row.
pub fn render_table_row(
    line: &str,
    spec: &TableSpec,
    kind: TableRowKind,
    row_index: usize,
    max_rows: usize,
    bg: Rgb,
    theme: &Theme,
) -> Vec<Span> {
    let border_style = Style::colors(theme.border, bg);
    let indent_span = (spec.indent > 0).then(|| Span {
        text: " ".repeat(spec.indent),
        style: Style::colors(theme.editor_fg, bg),
    });

    if kind == TableRowKind::Delimiter {
        if row_index > 0 {
            return Vec::new();
        }
        let mut out = Vec::new();
        if let Some(ind) = indent_span {
            out.push(ind);
        }
        let mut rule = String::from("├");
        for (c, &w) in spec.widths.iter().enumerate() {
            if c > 0 {
                rule.push('┼');
            }
            for _ in 0..(w + 2) {
                rule.push('─');
            }
        }
        rule.push('┤');
        out.push(Span { text: rule, style: border_style });
        return out;
    }

    let cells = split_table_cells(line);
    let cell_base = match kind {
        TableRowKind::Header => Style::colors(theme.text_accent, bg).bold(),
        TableRowKind::Body => Style::colors(theme.editor_fg, bg),
        TableRowKind::Delimiter => unreachable!(),
    };

    let wrapped_cells: Vec<Vec<Vec<Span>>> = (0..spec.widths.len())
        .map(|c| {
            let raw = cells.get(c).map(String::as_str).unwrap_or("");
            let spans = parse_inline(raw, cell_base, bg, theme);
            wrap_spans(&spans, spec.widths[c])
        })
        .collect();

    if row_index > 0 && wrapped_cells.iter().all(|lines| row_index >= lines.len()) {
        return Vec::new();
    }

    let mut out = Vec::new();
    if let Some(ind) = indent_span {
        out.push(ind);
    }

    let pad_style = Style::colors(theme.editor_fg, bg);
    let ellipsis_style = Style::colors(theme.text_muted, bg);
    let is_last_row = row_index + 1 >= max_rows.max(1);

    for (c, &col_w) in spec.widths.iter().enumerate() {
        out.push(Span { text: "│".to_string(), style: border_style });
        out.push(Span { text: " ".to_string(), style: pad_style });

        let cell_lines = &wrapped_cells[c];
        let has_more = is_last_row && cell_lines.len() > row_index + 1;
        let mut row_spans: Vec<Span> = cell_lines.get(row_index).cloned().unwrap_or_default();
        if has_more {
            let mut combined = row_spans;
            for extra in &cell_lines[row_index + 1..] {
                if !combined.is_empty() && !extra.is_empty() {
                    combined.push(Span { text: " ".to_string(), style: cell_base });
                }
                combined.extend(extra.iter().cloned());
            }
            row_spans = truncate_spans_with_ellipsis(&combined, col_w, ellipsis_style);
        }
        let used = spans_display_width(&row_spans);
        let pad = col_w.saturating_sub(used);
        let (left_pad, right_pad) = match spec.alignments.get(c).copied().unwrap_or(Align::Left) {
            Align::Left => (0, pad),
            Align::Right => (pad, 0),
            Align::Center => (pad / 2, pad - pad / 2),
        };
        if left_pad > 0 {
            out.push(Span { text: " ".repeat(left_pad), style: pad_style });
        }
        out.extend(row_spans);
        if right_pad > 0 {
            out.push(Span { text: " ".repeat(right_pad), style: pad_style });
        }
        out.push(Span { text: " ".to_string(), style: pad_style });
    }
    out.push(Span { text: "│".to_string(), style: border_style });
    out
}

/// Draw one visual sub-row of a GFM table line into `canvas`.
#[allow(clippy::too_many_arguments)]
pub fn draw_table_row(
    canvas: &mut Canvas<'_>,
    x: u16,
    y: u16,
    width: u16,
    left: usize,
    line: &str,
    spec: &TableSpec,
    kind: TableRowKind,
    row_index: usize,
    max_rows: usize,
    bg: Rgb,
    theme: &Theme,
) {
    if width == 0 {
        return;
    }
    let spans = render_table_row(line, spec, kind, row_index, max_rows, bg, theme);
    if !spans.is_empty() {
        draw_spans(canvas, x, y, width, left, &spans, bg);
    }
}

fn draw_spans(canvas: &mut Canvas<'_>, x: u16, y: u16, width: u16, left: usize, spans: &[Span], bg: Rgb) {
    let right = left + width as usize;
    let mut col = 0usize;
    for span in spans {
        for ch in span.text.chars() {
            if col >= right {
                return;
            }
            let mut buf = [0u8; 4];
            let s: &str = ch.encode_utf8(&mut buf);
            let cells = if ch == '\t' { 4 - (col % 4) } else { ui::width(s).max(1) };
            for c in 0..cells {
                let cell_col = col + c;
                if cell_col < left || cell_col >= right {
                    continue;
                }
                if c == 0 && col >= left && col + cells <= right {
                    let shown = if ch == '\t' { " " } else { s };
                    canvas.put(x + (cell_col - left) as u16, y, shown, span.style);
                } else {
                    canvas.put(x + (cell_col - left) as u16, y, " ", Style::new().on(bg));
                }
            }
            col += cells;
        }
    }
}

fn is_table_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() || is_fence(line) || is_hr(trimmed) || strip_heading(line).is_some() {
        return false;
    }
    if trimmed.starts_with("> ") || trimmed == ">" {
        return false;
    }
    has_unescaped_pipe(trimmed)
}

fn has_unescaped_pipe(s: &str) -> bool {
    let mut chars = s.chars().peekable();
    let mut in_code = false;
    while let Some(ch) = chars.next() {
        if ch == '`' {
            in_code = !in_code;
        } else if !in_code && ch == '\\' {
            let _ = chars.next();
        } else if !in_code && ch == '|' {
            return true;
        }
    }
    false
}

fn split_table_cells(line: &str) -> Vec<String> {
    let mut s = line.trim();
    if let Some(rest) = s.strip_prefix('|') {
        s = rest;
    }
    if s.ends_with('|') && !ends_with_escaped_pipe(s) {
        s = &s[..s.len() - 1];
    }

    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    let mut in_code = false;
    while let Some(ch) = chars.next() {
        if ch == '`' {
            in_code = !in_code;
            cur.push(ch);
        } else if !in_code && ch == '\\' && chars.peek() == Some(&'|') {
            chars.next();
            cur.push('|');
        } else if !in_code && ch == '|' {
            cells.push(cur.trim().to_string());
            cur.clear();
        } else {
            cur.push(ch);
        }
    }
    cells.push(cur.trim().to_string());
    cells
}

fn ends_with_escaped_pipe(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.last() != Some(&b'|') {
        return false;
    }
    let backslashes = bytes[..bytes.len() - 1].iter().rev().take_while(|&&b| b == b'\\').count();
    backslashes % 2 == 1
}

fn parse_delimiter_row(line: &str) -> Option<Vec<Align>> {
    if !has_unescaped_pipe(line.trim()) {
        return None;
    }
    let cells = split_table_cells(line);
    if cells.is_empty() {
        return None;
    }
    let mut aligns = Vec::with_capacity(cells.len());
    for cell in &cells {
        let t = cell.trim();
        if t.is_empty() {
            return None;
        }
        let left_colon = t.starts_with(':');
        let right_colon = t.ends_with(':');
        let mid = t.strip_prefix(':').unwrap_or(t);
        let mid = mid.strip_suffix(':').unwrap_or(mid);
        if mid.is_empty() || !mid.bytes().all(|b| b == b'-') {
            return None;
        }
        let align = match (left_colon, right_colon) {
            (true, true) => Align::Center,
            (false, true) => Align::Right,
            _ => Align::Left,
        };
        aligns.push(align);
    }
    Some(aligns)
}

fn char_display_width(ch: char) -> usize {
    let mut buf = [0u8; 4];
    ui::width(ch.encode_utf8(&mut buf)).max(1)
}

fn spans_display_width(spans: &[Span]) -> usize {
    spans.iter().flat_map(|s| s.text.chars()).map(char_display_width).sum()
}

/// Word-wrap styled spans to fit within `max_width` display columns.
fn wrap_spans(spans: &[Span], max_width: usize) -> Vec<Vec<Span>> {
    if max_width == 0 {
        return vec![Vec::new()];
    }
    let total = spans_display_width(spans);
    if total <= max_width {
        return vec![spans.to_vec()];
    }

    // Flatten into (char, style, width) tokens so we can break cleanly at spaces.
    let mut chars: Vec<(char, Style, usize)> = Vec::new();
    for span in spans {
        for ch in span.text.chars() {
            chars.push((ch, span.style, char_display_width(ch)));
        }
    }

    let mut lines: Vec<Vec<Span>> = Vec::new();
    let mut start = 0usize;
    while start < chars.len() {
        while start < chars.len() && !lines.is_empty() && chars[start].0 == ' ' {
            start += 1;
        }
        if start >= chars.len() {
            break;
        }
        let mut end = start;
        let mut col = 0usize;
        let mut last_space: Option<usize> = None;
        while end < chars.len() {
            let (ch, _, w) = chars[end];
            if col + w > max_width {
                break;
            }
            if ch == ' ' {
                last_space = Some(end);
            }
            col += w;
            end += 1;
        }
        if end == start {
            // Single character wider than max_width: force progress.
            end += 1;
        } else if end < chars.len() {
            if let Some(sp) = last_space {
                if sp > start {
                    end = sp;
                }
            }
        }
        // Trim trailing spaces on wrapped lines.
        let mut slice_end = end;
        while slice_end > start && chars[slice_end - 1].0 == ' ' {
            slice_end -= 1;
        }
        lines.push(coalesce_styled_chars(&chars[start..slice_end]));
        start = end;
    }
    if lines.is_empty() {
        lines.push(Vec::new());
    }
    lines
}

fn coalesce_styled_chars(chars: &[(char, Style, usize)]) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    for &(ch, style, _) in chars {
        if let Some(last) = out.last_mut() {
            if last.style == style {
                last.text.push(ch);
                continue;
            }
        }
        out.push(Span { text: ch.to_string(), style });
    }
    out
}

fn truncate_spans_with_ellipsis(spans: &[Span], max_width: usize, ellipsis_style: Style) -> Vec<Span> {
    if max_width == 0 {
        return Vec::new();
    }
    let limit = max_width.saturating_sub(1);
    let mut out: Vec<Span> = Vec::new();
    let mut col = 0usize;
    for span in spans {
        let mut buf = String::new();
        for ch in span.text.chars() {
            let w = char_display_width(ch);
            if col + w > limit {
                if !buf.is_empty() {
                    out.push(Span { text: buf, style: span.style });
                }
                out.push(Span { text: "…".to_string(), style: ellipsis_style });
                return out;
            }
            buf.push(ch);
            col += w;
        }
        if !buf.is_empty() {
            out.push(Span { text: buf, style: span.style });
        }
    }
    out.push(Span { text: "…".to_string(), style: ellipsis_style });
    out
}

fn is_hr(trimmed: &str) -> bool {
    if trimmed.len() < 3 {
        return false;
    }
    let first = trimmed.as_bytes()[0];
    if !matches!(first, b'-' | b'*' | b'_') {
        return false;
    }
    let count = trimmed.bytes().filter(|&b| b == first).count();
    count >= 3 && trimmed.bytes().all(|b| b == first || b == b' ')
}

fn strip_heading(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    let hashes = trimmed.bytes().take_while(|&b| b == b'#').count();
    if (1..=6).contains(&hashes) {
        let rest = &trimmed[hashes..];
        if let Some(after) = rest.strip_prefix(' ') {
            return Some((hashes, after));
        }
    }
    None
}

fn strip_list_marker<'a>(s: &'a str, theme: &Theme) -> Option<(&'static str, Rgb, &'a str)> {
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = s.strip_prefix(marker) {
            if let Some(task) = rest.strip_prefix("[ ] ") {
                return Some(("☐ ", theme.text_muted, task));
            }
            if let Some(task) = rest.strip_prefix("[x] ").or_else(|| rest.strip_prefix("[X] ")) {
                return Some(("☑ ", theme.created, task));
            }
            return Some(("• ", theme.text_accent, rest));
        }
    }
    None
}

fn strip_ordered_marker(s: &str) -> Option<(&str, &str)> {
    let digits = s.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || digits > 4 {
        return None;
    }
    let rest = &s[digits..];
    if let Some(after) = rest.strip_prefix(". ") {
        return Some((&s[..digits + 1], after));
    }
    None
}

/// Parse inline Markdown (`code`, **bold**, *italic*, ~~strike~~, [label](url)).
fn parse_inline(text: &str, base: Style, bg: Rgb, theme: &Theme) -> Vec<Span> {
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut out: Vec<Span> = Vec::new();
    let mut plain = String::new();
    let mut i = 0usize;

    let flush = |plain: &mut String, out: &mut Vec<Span>, style: Style| {
        if !plain.is_empty() {
            out.push(Span { text: std::mem::take(plain), style });
        }
    };

    while i < len {
        // Inline code: `...`
        if bytes[i] == b'`' {
            if let Some(rel_end) = text[i + 1..].find('`') {
                let code = &text[i + 1..i + 1 + rel_end];
                if !code.is_empty() {
                    flush(&mut plain, &mut out, base);
                    let code_bg = theme.elevated_surface.lerp(bg, 0.35);
                    out.push(Span {
                        text: code.to_string(),
                        style: Style::colors(theme.ansi[2], code_bg),
                    });
                    i += rel_end + 2;
                    continue;
                }
            }
        }
        // Markdown link: [label](url)
        if bytes[i] == b'[' {
            if let Some(close_bracket) = text[i + 1..].find(']') {
                let after_bracket = i + 1 + close_bracket + 1;
                if after_bracket < len && bytes[after_bracket] == b'(' {
                    if let Some(close_paren) = text[after_bracket + 1..].find(')') {
                        let label = &text[i + 1..i + 1 + close_bracket];
                        if !label.is_empty() {
                            flush(&mut plain, &mut out, base);
                            out.push(Span {
                                text: label.to_string(),
                                style: Style::colors(theme.text_accent, bg).underline(),
                            });
                            i = after_bracket + 1 + close_paren + 1;
                            continue;
                        }
                    }
                }
            }
        }
        // Bold: **...** or __...__
        if i + 1 < len && ((bytes[i] == b'*' && bytes[i + 1] == b'*') || (bytes[i] == b'_' && bytes[i + 1] == b'_')) {
            let delim = &text[i..i + 2];
            if let Some(rel_end) = text[i + 2..].find(delim) {
                let inner = &text[i + 2..i + 2 + rel_end];
                if !inner.is_empty() && !inner.starts_with(' ') && !inner.ends_with(' ') {
                    flush(&mut plain, &mut out, base);
                    out.extend(parse_inline(inner, base.bold(), bg, theme));
                    i += rel_end + 4;
                    continue;
                }
            }
        }
        // Strikethrough: ~~...~~
        if i + 1 < len && bytes[i] == b'~' && bytes[i + 1] == b'~' {
            if let Some(rel_end) = text[i + 2..].find("~~") {
                let inner = &text[i + 2..i + 2 + rel_end];
                if !inner.is_empty() {
                    flush(&mut plain, &mut out, base);
                    let mut st = Style::colors(theme.text_muted, bg);
                    st.strike = true;
                    out.push(Span { text: inner.to_string(), style: st });
                    i += rel_end + 4;
                    continue;
                }
            }
        }
        // Italic: *...* (single asterisk, not followed by space)
        if bytes[i] == b'*' && i + 1 < len && bytes[i + 1] != b'*' && bytes[i + 1] != b' ' {
            if let Some(rel_end) = text[i + 1..].find('*') {
                let inner = &text[i + 1..i + 1 + rel_end];
                if !inner.is_empty() && !inner.ends_with(' ') {
                    flush(&mut plain, &mut out, base);
                    out.push(Span { text: inner.to_string(), style: base.italic() });
                    i += rel_end + 2;
                    continue;
                }
            }
        }
        let ch = text[i..].chars().next().unwrap();
        plain.push(ch);
        i += ch.len_utf8();
    }
    flush(&mut plain, &mut out, base);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_headings_lists_links_and_inline_styles() {
        let theme = Theme::builtin();
        let bg = theme.editor_bg;

        let h1 = render_line("# Hello **world**", bg, &theme, 80, false);
        assert_eq!(h1.iter().map(|s| s.text.as_str()).collect::<String>(), "Hello world");
        assert!(h1[0].style.bold && h1[0].style.underline);

        let bullet = render_line("  - [x] Ship `bakedit` via [docs](https://example.com)", bg, &theme, 80, false);
        let joined: String = bullet.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "  ☑ Ship bakedit via docs");
    }

    fn text_of(spans: &[Span]) -> String {
        spans.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn renders_aligned_gfm_table_with_inline_markdown() {
        let theme = Theme::builtin();
        let bg = theme.editor_bg;
        let lines = [
            "| Crate | Count | Status |",
            "| :--- | :---: | ---: |",
            "| `core` | **12** | [ok](https://example.com/very/long/url) |",
            "| `app` | 3 | ready |",
        ];

        let spec = detect_table_at(0, lines.len(), 80, &theme, |i| lines.get(i).map(|s| (*s).to_string()))
            .expect("table detected");
        assert_eq!(spec.start, 0);
        assert_eq!(spec.end, 4);
        assert_eq!(spec.alignments, vec![Align::Left, Align::Center, Align::Right]);
        // Rendered widths: Crate(5)/core(4)/app(3) -> 5; Count(5)/12(2)/3(1) -> 5; Status(6)/ok(2)/ready(5) -> 6.
        assert_eq!(spec.widths, vec![5, 5, 6]);

        let r0 = render_table_row(lines[0], &spec, spec.row_kind(0), 0, 1, bg, &theme);
        let r1 = render_table_row(lines[1], &spec, spec.row_kind(1), 0, 1, bg, &theme);
        let r2 = render_table_row(lines[2], &spec, spec.row_kind(2), 0, 1, bg, &theme);
        let r3 = render_table_row(lines[3], &spec, spec.row_kind(3), 0, 1, bg, &theme);

        assert_eq!(text_of(&r0), "│ Crate │ Count │ Status │");
        assert_eq!(text_of(&r1), "├───────┼───────┼────────┤");
        assert_eq!(text_of(&r2), "│ core  │  12   │     ok │");
        assert_eq!(text_of(&r3), "│ app   │   3   │  ready │");
    }

    #[test]
    fn handles_escaped_and_code_pipes_and_outer_pipeless_rows() {
        let theme = Theme::builtin();
        let bg = theme.editor_bg;
        let lines = [
            "Expr | Meaning",
            "---|---",
            "`a | b` | bitwise \\| or",
        ];

        let spec = detect_table_at(2, lines.len(), 80, &theme, |i| lines.get(i).map(|s| (*s).to_string()))
            .expect("table detected");
        assert_eq!(spec.widths, vec![5, 12]);
        let r2 = render_table_row(lines[2], &spec, spec.row_kind(2), 0, 1, bg, &theme);
        assert_eq!(text_of(&r2), "│ a | b │ bitwise | or │");
    }

    #[test]
    fn does_not_misclassify_horizontal_rule_as_table() {
        let theme = Theme::builtin();
        let lines = ["Heading text", "---", "Paragraph"];
        assert!(detect_table_at(1, lines.len(), 80, &theme, |i| lines.get(i).map(|s| (*s).to_string())).is_none());
    }

    #[test]
    fn shrinks_widest_column_and_wraps_or_truncates_cells() {
        let theme = Theme::builtin();
        let bg = theme.editor_bg;
        let lines = [
            "| Key | Description |",
            "|---|---|",
            "| `a` | alpha beta gamma delta |",
        ];
        // Total chrome for 2 cols = 2 * 3 + 1 = 7. With max_width = 24, avail = 17.
        // Key stays at 3, Description shrinks from 22 to 14.
        let spec = detect_table_at(0, lines.len(), 24, &theme, |i| lines.get(i).map(|s| (*s).to_string()))
            .expect("table detected");
        assert_eq!(spec.widths, vec![3, 14]);

        // Single-row mode (e.g. diff view) truncates with ellipsis.
        let single = render_table_row(lines[2], &spec, TableRowKind::Body, 0, 1, bg, &theme);
        assert_eq!(text_of(&single), "│ a   │ alpha beta ga… │");

        // Multi-row mode (soft-wrapped editor) wraps cleanly inside the column borders.
        let sub0 = render_table_row(lines[2], &spec, TableRowKind::Body, 0, 2, bg, &theme);
        let sub1 = render_table_row(lines[2], &spec, TableRowKind::Body, 1, 2, bg, &theme);
        assert_eq!(text_of(&sub0), "│ a   │ alpha beta     │");
        assert_eq!(text_of(&sub1), "│     │ gamma delta    │");
    }
}
