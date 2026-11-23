//! Turning a drawn frame into something that can be looked at outside a terminal: an HTML
//! page with one styled span per run of cells. Used by `--dump-frame` and by the visual
//! checks during development; a screenshot of the page is a faithful picture of the frame.

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};

fn css_color(color: Color, fallback: &str) -> String {
    match color {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        _ => fallback.to_string(),
    }
}

fn escape(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
}

/// The frame as a standalone HTML page. `font_size` is in CSS pixels; `cursor` draws a
/// marker at that cell so the picture shows where the text cursor is.
pub fn to_html(buf: &Buffer, default_fg: &str, default_bg: &str, font_size: u16, cursor: Option<(u16, u16)>) -> String {
    let area = *buf.area();
    let mut out = String::new();
    out.push_str("<!doctype html><meta charset=\"utf-8\"><style>");
    out.push_str(&format!(
        "body{{margin:0;background:{default_bg};}}\
         pre{{margin:0;padding:8px;font-family:'MesloLGSDZ Nerd Font Mono','MesloLGS NF','Menlo',monospace;\
         font-size:{font_size}px;line-height:1.25;color:{default_fg};background:{default_bg};\
         font-variant-ligatures:none;white-space:pre;}}\
         span{{display:inline-block;height:1.25em;vertical-align:top;}}\
         .cur{{outline:1px solid #ffffff;outline-offset:-1px;}}"
    ));
    out.push_str("</style><pre>");
    for y in area.y..area.bottom() {
        let mut x = area.x;
        while x < area.right() {
            let cell = &buf[(x, y)];
            let style = (cell.fg, cell.bg, cell.modifier);
            let is_cursor = cursor == Some((x, y));
            // Group a run of cells with one style into one span.
            let mut text = String::new();
            let mut run_end = x;
            while run_end < area.right() {
                let next = &buf[(run_end, y)];
                if (next.fg, next.bg, next.modifier) != style || (cursor == Some((run_end, y))) != is_cursor {
                    break;
                }
                let symbol = next.symbol();
                let width = unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
                escape(if symbol.is_empty() { " " } else { symbol }, &mut text);
                run_end += width;
                if is_cursor {
                    break;
                }
            }
            let (mut fg, mut bg) = (css_color(style.0, default_fg), css_color(style.1, default_bg));
            if style.2.contains(Modifier::REVERSED) {
                std::mem::swap(&mut fg, &mut bg);
            }
            let mut css = format!("color:{fg};background:{bg};");
            if style.2.contains(Modifier::BOLD) {
                css.push_str("font-weight:bold;");
            }
            if style.2.contains(Modifier::ITALIC) {
                css.push_str("font-style:italic;");
            }
            if style.2.contains(Modifier::UNDERLINED) {
                css.push_str("text-decoration:underline;");
            }
            if style.2.contains(Modifier::DIM) {
                css.push_str("opacity:0.6;");
            }
            let class = if is_cursor { " class=\"cur\"" } else { "" };
            out.push_str(&format!("<span{class} style=\"{css}\">{text}</span>"));
            x = run_end.max(x + 1);
        }
        out.push('\n');
    }
    out.push_str("</pre>");
    out
}

/// The frame as plain text, one line per row with trailing spaces removed.
pub fn to_text(buf: &Buffer) -> String {
    let area = *buf.area();
    let mut out = String::new();
    for y in area.y..area.bottom() {
        let mut line = String::new();
        let mut x = area.x;
        while x < area.right() {
            let symbol = buf[(x, y)].symbol();
            line.push_str(if symbol.is_empty() { " " } else { symbol });
            x += unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;
    use ratatui::style::Style;

    #[test]
    fn text_dump_trims_and_handles_wide_cells() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 8, 2));
        buf.set_string(0, 0, "ab 房c", Style::default());
        assert_eq!(to_text(&buf), "ab 房c\n\n");
    }

    #[test]
    fn html_groups_runs_and_escapes() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 6, 1));
        buf.set_string(0, 0, "a<b", Style::default().fg(Color::Rgb(255, 0, 0)));
        buf.set_string(3, 0, "c", Style::default().fg(Color::Rgb(0, 255, 0)).add_modifier(Modifier::BOLD));
        let html = to_html(&buf, "#eeeeee", "#000000", 13, Some((3, 0)));
        assert!(html.contains("<span style=\"color:#ff0000;background:#000000;\">a&lt;b</span>"), "{html}");
        assert!(html.contains("class=\"cur\""));
        assert!(html.contains("font-weight:bold;\">c</span>"));
        assert_eq!(html.matches("<span").count(), 3, "red run, the cursor cell, the blank tail: {html}");
    }
}
