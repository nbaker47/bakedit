//! Two small helpers around key bindings: the popup that shows how an unfinished chord can
//! be completed, and parsing what is typed into "go to line".

use bakedit_core::Theme;
use ratatui::layout::Rect;

use crate::ui::{self, Canvas, Style};

/// Draw the which-key popup: after `cmd-k` (or any other prefix) is held for a moment, the
/// keys that can follow and what each does, in columns at the bottom of `area`.
///
/// `entries` are (keys, title) pairs, e.g. ("cmd-s", "open keymap").
pub fn which_key(canvas: &mut Canvas<'_>, area: Rect, prefix: &str, entries: &[(String, String)], theme: &Theme) {
    if entries.is_empty() || area.width < 20 || area.height < 4 {
        return;
    }
    let bg = theme.elevated_surface;
    let key_width = entries.iter().map(|(keys, _)| ui::width(keys)).max().unwrap_or(1) as u16;
    let title_width = entries.iter().map(|(_, title)| ui::width(title)).max().unwrap_or(1).min(36) as u16;
    // A column is: two cells of margin, the keys, two cells of gap, the title.
    let column_width = 2 + key_width + 2 + title_width;
    let columns = ((area.width.saturating_sub(2)) / column_width).max(1) as usize;
    let rows = entries.len().div_ceil(columns) as u16;
    let height = (rows + 2).min(area.height);
    let panel = Rect::new(area.x, area.bottom() - height, area.width, height);
    canvas.fill(panel, bg);
    canvas.text(panel.x + 2, panel.y, &format!("{prefix} …"), Style::colors(theme.text_accent, bg).bold(), panel.width.saturating_sub(4));
    let visible_rows = height.saturating_sub(2) as usize;
    for (index, (keys, title)) in entries.iter().enumerate() {
        // Fill down each column, then move right, so related keys stay together.
        let column = index / rows.max(1) as usize;
        let row = index % rows.max(1) as usize;
        if column >= columns || row >= visible_rows {
            continue;
        }
        let x = panel.x + column as u16 * column_width + 2;
        let y = panel.y + 1 + row as u16;
        canvas.text(x, y, keys, Style::colors(theme.text, bg).bold(), key_width);
        canvas.text_truncated(x + key_width + 2, y, title, Style::colors(theme.text_muted, bg), title_width);
    }
}

/// What "go to line" was asked for: a 1-based line and optional 1-based column. A negative
/// line counts from the end of the file: -1 is the last line, -2 the one before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineTarget {
    pub line: i64,
    pub column: Option<usize>,
}

impl LineTarget {
    /// The 1-based line in a file of `line_count` lines, with negatives counted from the end
    /// and anything past either end clamped to it.
    pub fn resolve(&self, line_count: usize) -> usize {
        let count = line_count.max(1) as i64;
        let line = if self.line < 0 { count + 1 + self.line } else { self.line };
        line.clamp(1, count) as usize
    }
}

/// Parse `12`, `12:5`, `12,5`, `12 5` or `-1` (from the end). `None` for anything else
/// (including line 0).
pub fn parse_line_target(text: &str) -> Option<LineTarget> {
    let text = text.trim();
    let mut parts = text.split(|c: char| c == ':' || c == ',' || c.is_whitespace()).filter(|p| !p.is_empty());
    let line: i64 = parts.next()?.parse().ok().filter(|n| *n != 0)?;
    let column = match parts.next() {
        Some(column) => Some(column.parse::<usize>().ok().filter(|n| *n > 0)?),
        None => None,
    };
    if parts.next().is_some() {
        return None;
    }
    Some(LineTarget { line, column })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;

    #[test]
    fn line_targets() {
        assert_eq!(parse_line_target("12"), Some(LineTarget { line: 12, column: None }));
        assert_eq!(parse_line_target(" 12:5 "), Some(LineTarget { line: 12, column: Some(5) }));
        assert_eq!(parse_line_target("12,5"), Some(LineTarget { line: 12, column: Some(5) }));
        assert_eq!(parse_line_target("12 5"), Some(LineTarget { line: 12, column: Some(5) }));
        assert_eq!(parse_line_target("-1"), Some(LineTarget { line: -1, column: None }));
        assert_eq!(parse_line_target("-3:2"), Some(LineTarget { line: -3, column: Some(2) }));
        let at = |line| LineTarget { line, column: None };
        assert_eq!(at(-1).resolve(40), 40);
        assert_eq!(at(-2).resolve(40), 39);
        assert_eq!(at(-99).resolve(40), 1);
        assert_eq!(at(99).resolve(40), 40);
        assert_eq!(at(5).resolve(40), 5);
        assert_eq!(at(-1).resolve(0), 1);
        for bad in ["", "0", "abc", "12:", "12:0", "12:x", "1:2:3", "-0", "--1"] {
            let expected = if bad == "12:" { Some(LineTarget { line: 12, column: None }) } else { None };
            assert_eq!(parse_line_target(bad), expected, "{bad:?}");
        }
    }

    #[test]
    fn which_key_lists_completions_in_columns() {
        let area = Rect::new(0, 0, 60, 12);
        let mut buf = Buffer::empty(area);
        let theme = Theme::builtin();
        let entries: Vec<(String, String)> = [("cmd-s", "open keymap"), ("z", "toggle soft wrap"), ("s", "save without format"), ("0", "collapse all")]
            .iter()
            .map(|(k, t)| (k.to_string(), t.to_string()))
            .collect();
        {
            let mut canvas = Canvas::new(&mut buf, area);
            which_key(&mut canvas, area, "cmd-k", &entries, &theme);
            which_key(&mut canvas, Rect::new(0, 0, 5, 2), "x", &entries, &theme);
            which_key(&mut canvas, area, "x", &[], &theme);
        }
        let text = crate::dump::to_text(&buf);
        let lines: Vec<&str> = text.lines().collect();
        let header = lines.iter().position(|l| l.contains("cmd-k …")).expect("the prefix is shown");
        assert!(lines[header + 1].contains("cmd-s  open keymap"), "{:?}", lines[header + 1]);
        assert!(lines[header + 1].contains("s      save without format"), "keys are padded to one width: {:?}", lines[header + 1]);
        assert!(lines[header + 2].contains("z      toggle soft wrap"));
        assert_eq!(header, 12 - 4, "two rows of entries, the header and a blank row, at the bottom");
    }
}
