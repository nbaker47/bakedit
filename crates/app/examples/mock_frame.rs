//! Draws a made-up frame with every piece of chrome in it and writes it as HTML, so the
//! look can be checked in a browser screenshot without running the app in a terminal.
//!
//!   cargo run -p bakedit --example mock_frame -- /tmp/frame.html [width height] [--picker]

use std::ops::Range;

use bakedit::chrome::{self, ModeIndicator, NavLayerIndicator, Status, Tab};
use bakedit::layout;
use bakedit::minimap::{self, MinimapSource};
use bakedit::paint::{self, GitMark, GutterInfo, PaintOptions, Row};
use bakedit::picker::{Picker, PickerItem};
use bakedit::ui::{Canvas, Style};
use bakedit::workspace::{DockPosition, Focus, PanelId, Workspace};
use bakedit_core::{Rgb, Theme};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

const CODE: &str = r#"use std::collections::HashMap;

/// Counts how often each word appears.
pub fn word_counts(text: &str) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for word in text.split_whitespace() {
        let word = word.trim_matches(|c: char| !c.is_alphanumeric());
        if word.is_empty() {
            continue;
        }
        *counts.entry(word.to_lowercase()).or_insert(0) += 1;
    }
    counts
}

fn main() {
    let text = std::fs::read_to_string("notes.txt").expect("readable");
    let mut counts: Vec<(String, usize)> = word_counts(&text).into_iter().collect();
    counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    for (word, count) in counts.iter().take(10) {
        println!("{count:>6}  {word}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_words_case_insensitively() {
        let counts = word_counts("The cat and the hat.");
        assert_eq!(counts["the"], 2);
        assert_eq!(counts["hat"], 1);
    }
}
"#;

const KEYWORDS: &[&str] = &["use", "pub", "fn", "let", "mut", "for", "in", "if", "continue", "mod", "super"];

/// A crude highlighter, good enough to colour the sample.
fn highlight(line: &str, theme: &Theme) -> Vec<(Range<usize>, Style)> {
    let style = |name: &str| {
        let s = theme.syntax(name).unwrap_or_default();
        let mut out = Style::new();
        out.fg = s.color;
        out.italic = s.italic;
        out.bold = s.bold;
        out
    };
    let mut spans = Vec::new();
    if let Some(start) = line.find("//") {
        spans.push((start..line.len(), style("comment")));
        return spans;
    }
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == '"' {
            let end = line[i + 1..].find('"').map(|e| i + e + 2).unwrap_or(line.len());
            spans.push((i..end, style("string")));
            i = end;
        } else if c.is_ascii_digit() {
            let end = i + line[i..].bytes().take_while(|b| b.is_ascii_digit()).count();
            spans.push((i..end, style("number")));
            i = end;
        } else if c.is_ascii_alphabetic() || c == '_' {
            let end = i + line[i..].bytes().take_while(|b| b.is_ascii_alphanumeric() || *b == b'_').count();
            let word = &line[i..end];
            let next = line[end..].chars().next();
            if KEYWORDS.contains(&word) {
                spans.push((i..end, style("keyword")));
            } else if word.chars().next().is_some_and(char::is_uppercase) {
                spans.push((i..end, style("type")));
            } else if next == Some('(') || next == Some('!') {
                spans.push((i..end, style("function")));
            }
            i = end;
        } else {
            i += 1;
        }
    }
    spans
}

struct Source<'a> {
    lines: &'a [&'a str],
    theme: &'a Theme,
}

impl MinimapSource for Source<'_> {
    fn line_count(&self) -> usize {
        self.lines.len()
    }

    fn runs(&self, line: usize, out: &mut Vec<(usize, usize, Rgb)>) {
        let text = self.lines[line];
        let spans = highlight(text, self.theme);
        let mut start: Option<usize> = None;
        let color_at = |byte: usize| {
            spans.iter().find(|(r, _)| r.contains(&byte)).and_then(|(_, s)| s.fg).unwrap_or(self.theme.editor_fg)
        };
        for (i, c) in text.char_indices() {
            match (c.is_whitespace(), start) {
                (false, None) => start = Some(i),
                (true, Some(s)) => {
                    out.push((s, i, color_at(s)));
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(s) = start {
            out.push((s, text.len(), color_at(s)));
        }
    }
}

fn panel_header(canvas: &mut Canvas<'_>, area: Rect, title: &str, detail: &str, focused: bool, theme: &Theme) {
    let bg = theme.panel_bg;
    canvas.fill(Rect::new(area.x, area.y, area.width, 1), bg);
    let title_style = if focused { Style::colors(theme.text_accent, bg).bold() } else { Style::colors(theme.text_muted, bg) };
    let used = canvas.text(area.x + 1, area.y, title, title_style, area.width.saturating_sub(2));
    canvas.text_truncated(
        area.x + 1 + used + 2,
        area.y,
        detail,
        Style::colors(theme.text_muted, bg),
        area.width.saturating_sub(used + 4),
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = args.first().cloned().unwrap_or_else(|| "frame.html".into());
    let width: u16 = args.get(1).and_then(|w| w.parse().ok()).unwrap_or(170);
    let height: u16 = args.get(2).and_then(|h| h.parse().ok()).unwrap_or(46);
    let with_picker = args.iter().any(|a| a == "--picker");
    let focus_tree = args.iter().any(|a| a == "--tree");

    let theme = Theme::builtin();
    let screen = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(screen);

    let mut ws = Workspace::new(&[
        (PanelId::Terminal, DockPosition::Left),
        (PanelId::Git, DockPosition::Left),
        (PanelId::Project, DockPosition::Right),
    ]);
    ws.open_panel(PanelId::Terminal);
    ws.open_panel(PanelId::Project);
    if focus_tree {
        ws.focus_panel(PanelId::Project);
    }
    let default_width = |panel: PanelId| match panel {
        PanelId::Terminal => 70,
        PanelId::Git => 70,
        PanelId::Project => 33,
    };
    let layout = layout::compute(&ws, screen, default_width);
    let mut cursor = None;

    {
        let mut canvas = Canvas::new(&mut buf, screen);
        canvas.fill(screen, theme.background);

        // Dividers take the accent colour beside the focused panel.
        for divider in &layout.dividers {
            let focused = matches!(ws.focus(), Focus::Panel(p) if ws.position_of(p) == Some(divider.position));
            let color = if focused { theme.panel_focused_border } else { theme.border };
            canvas.vline(divider.rect.x, divider.rect.y, divider.rect.height, "│", Style::colors(color, theme.background));
        }

        // Terminal panel (left): a header and stand-in content.
        if let Some(area) = layout.panel(PanelId::Terminal) {
            panel_header(&mut canvas, area, "Terminal", "herdr", ws.focus() == Focus::Panel(PanelId::Terminal), &theme);
            let lines = [
                " w1 bakedit   w2 notes   w3 dotfiles",
                "╭─ claude ──────────────────────────────╮",
                "│ ⏺ I updated the parser. The change is │",
                "│   in crates/input/src/parser.rs:214   │",
                "│   and the test at tests/kitty.rs:88   │",
                "│                                       │",
                "│ ✻ Cogitated for 12s                   │",
                "╰───────────────────────────────────────╯",
                " > ",
            ];
            for (i, line) in lines.iter().enumerate() {
                canvas.text(area.x, area.y + 1 + i as u16, line, Style::colors(theme.terminal_fg, theme.terminal_bg), area.width);
            }
        }

        // Project panel (right).
        if let Some(area) = layout.panel(PanelId::Project) {
            let focused = ws.focus() == Focus::Panel(PanelId::Project);
            panel_header(&mut canvas, area, "bakedit", "", focused, &theme);
            let rows: &[(u16, &str, &str, Rgb, Option<Rgb>)] = &[
                (0, "\u{eab4}", "crates", theme.text_muted, None),
                (1, "\u{eab4}", "app", theme.text_muted, None),
                (2, "\u{eab4}", "src", theme.text_muted, None),
                (3, "\u{e7a8}", "chrome.rs", Rgb::new(0xef, 0x9f, 0x76), Some(theme.modified)),
                (3, "\u{e7a8}", "main.rs", Rgb::new(0xef, 0x9f, 0x76), None),
                (3, "\u{e7a8}", "words.rs", Rgb::new(0xef, 0x9f, 0x76), Some(theme.created)),
                (2, "\u{e6b2}", "Cargo.toml", Rgb::new(0xe7, 0x82, 0x84), None),
                (1, "\u{eab6}", "core", theme.text_muted, None),
                (1, "\u{eab6}", "term", theme.text_muted, None),
                (0, "\u{eab6}", "assets", theme.text_muted, None),
                (0, "\u{eab6}", "target", theme.text_muted, Some(theme.ignored)),
                (0, "\u{e6b2}", "Cargo.toml", Rgb::new(0xe7, 0x82, 0x84), None),
                (0, "\u{e73e}", "README.md", Rgb::new(0x8c, 0xaa, 0xee), None),
            ];
            for (i, (depth, icon, name, icon_color, status)) in rows.iter().enumerate() {
                let y = area.y + 1 + i as u16;
                let selected = i == 5;
                let bg = if selected { theme.element_selected } else { theme.panel_bg };
                if selected {
                    canvas.fill(Rect::new(area.x, y, area.width, 1), bg);
                }
                let x = area.x + 1 + depth * 2;
                canvas.text(x, y, icon, Style::colors(*icon_color, bg), 2);
                let name_color = status.unwrap_or(theme.text);
                canvas.text_truncated(x + 2, y, name, Style::colors(name_color, bg), area.right().saturating_sub(x + 3));
            }
        }

        // Centre: tabs, breadcrumbs, editor.
        if let Some(center) = layout.center {
            let tabs = vec![
                Tab { title: "words.rs".into(), dirty: true, icon: Some(("\u{e7a8}".into(), Rgb::new(0xef, 0x9f, 0x76))), ..Default::default() },
                Tab { title: "Cargo.toml".into(), icon: Some(("\u{e6b2}".into(), Rgb::new(0xe7, 0x82, 0x84))), ..Default::default() },
                Tab { title: "README.md".into(), preview: true, icon: Some(("\u{e73e}".into(), Rgb::new(0x8c, 0xaa, 0xee))), ..Default::default() },
            ];
            let mut first = 0;
            let focused = ws.focus() == Focus::Center;
            chrome::tab_bar(&mut canvas, Rect::new(center.x, center.y, center.width, 1), &tabs, 0, &mut first, focused, &theme);
            let crumbs = Rect::new(center.x, center.y + 1, center.width, 1);
            canvas.fill(crumbs, theme.editor_bg);
            canvas.text(crumbs.x + 1, crumbs.y, "crates › app › src › words.rs", Style::colors(theme.text_muted, theme.editor_bg), center.width);

            let editor = Rect::new(center.x, center.y + 2, center.width, center.height.saturating_sub(2));
            let lines: Vec<&str> = CODE.lines().collect();
            let gutter_width = paint::gutter_width(lines.len(), true);
            let minimap_width = 10;
            let text_x = editor.x + gutter_width;
            let text_width = editor.width.saturating_sub(gutter_width + minimap_width + 3);
            let options = PaintOptions {
                tab_size: 4,
                left: 0,
                show_indent_guides: true,
                colorize_brackets: true,
                show_whitespace_in_selection: true,
            };
            let cursor_line = 10;
            for row in 0..editor.height as usize {
                let y = editor.y + row as u16;
                if row >= lines.len() {
                    canvas.fill(Rect::new(editor.x, y, gutter_width + text_width, 1), theme.editor_bg);
                    continue;
                }
                let line = lines[row];
                let git = match row {
                    4..=6 => Some(GitMark::Modified),
                    17..=18 => Some(GitMark::Added),
                    21 => Some(GitMark::DeletedAbove),
                    _ => None,
                };
                paint::gutter(
                    &mut canvas,
                    editor.x,
                    y,
                    gutter_width,
                    GutterInfo { number: Some(row + 1), current: row == cursor_line, git },
                    true,
                    &theme,
                );
                let spans = highlight(line, &theme);
                let indent = line.chars().take_while(|c| *c == ' ').count() / 4;
                let mut brackets = Vec::new();
                let mut depth: u16 = 0;
                for (i, c) in line.chars().enumerate() {
                    match c {
                        '(' | '[' | '{' => {
                            brackets.push((i, depth));
                            depth += 1;
                        }
                        ')' | ']' | '}' => {
                            depth = depth.saturating_sub(1);
                            brackets.push((i, depth));
                        }
                        _ => {}
                    }
                }
                let selections = if row == 6 { vec![19..49] } else { Vec::new() };
                let matches = if line.contains("counts") {
                    let start = line.find("counts").unwrap();
                    let start = line[..start].chars().count();
                    vec![start..start + 6]
                } else {
                    Vec::new()
                };
                let mut r = Row::plain(line);
                r.syntax = &spans;
                r.brackets = &brackets;
                r.selections = &selections;
                r.matches = &matches;
                r.current_line = row == cursor_line;
                r.indent_guides = indent;
                paint::row(&mut canvas, text_x, y, text_width, &r, &options, &theme);
            }
            cursor = Some((text_x + 12, editor.y + cursor_line as u16));

            let source = Source { lines: &lines, theme: &theme };
            let map = Rect::new(editor.right() - minimap_width - 2, editor.y, minimap_width, editor.height);
            minimap::render(&mut canvas, map, &source, 0, editor.height as usize, 80, &theme);
            let marks = paint::ScrollMarks {
                marks: vec![(4, theme.modified), (17, theme.created), (21, theme.deleted)],
                cursor_line: Some(cursor_line),
            };
            paint::scrollbar(&mut canvas, editor.right() - 1, editor.y, editor.height, lines.len(), 0, editor.height as usize, &marks, &theme);
        }

        let status = Status {
            mode: ModeIndicator::Nav,
            branch: Some("feat/app-shell".into()),
            ahead: 2,
            changes: 3,
            message: Some(("Saved crates/app/src/words.rs".into(), false)),
            position: Some((11, 13, 0)),
            language: Some("Rust".into()),
            indent: Some("Spaces: 4".into()),
            line_ending: Some("LF"),
            nav_layer: NavLayerIndicator::Ready,
            ..Default::default()
        };
        chrome::status_bar(&mut canvas, layout.status, &status, &theme);

        if with_picker {
            let mut picker = Picker::new("Search files");
            picker.insert("wor");
            picker.set_items(vec![
                PickerItem::new("words.rs").matched(vec![0, 1, 2]).detail("crates/app/src").icon("\u{e7a8}", Rgb::new(0xef, 0x9f, 0x76)),
                PickerItem::new("workspace.rs").matched(vec![0, 1, 2]).detail("crates/app/src").icon("\u{e7a8}", Rgb::new(0xef, 0x9f, 0x76)),
                PickerItem::new("word_wrap.rs").matched(vec![0, 1, 2]).detail("crates/text/src").icon("\u{e7a8}", Rgb::new(0xef, 0x9f, 0x76)),
                PickerItem::new("Cargo.toml").matched(vec![3, 6, 7]).detail("crates/keymap").icon("\u{e6b2}", Rgb::new(0xe7, 0x82, 0x84)),
            ]);
            cursor = Some(picker.render(&mut canvas, screen, &theme));
        }
    }

    let html = bakedit::dump::to_html(&buf, &theme.text.to_hex(), &theme.background.to_hex(), 14, cursor);
    std::fs::write(&out, html).expect("write the frame");
    println!("wrote {out} ({width}x{height})");
}
