//! Lightweight, single-line Markdown renderer for NAV mode in the editor and diff views.
//!
//! Renders headings, lists, task checkboxes, blockquotes, horizontal rules, code-fence
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
}
