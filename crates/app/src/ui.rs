//! Drawing helpers shared by every view: a clipped canvas over ratatui's cell buffer, a
//! small style type that speaks the theme's colours, and text measuring and truncation.

use bakedit_core::Rgb;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// How a run of cells is drawn. `None` colours leave whatever is already in the cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub dim: bool,
    pub strike: bool,
}

impl Style {
    pub const fn new() -> Style {
        Style { fg: None, bg: None, bold: false, italic: false, underline: false, dim: false, strike: false }
    }

    pub const fn fg(fg: Rgb) -> Style {
        Style { fg: Some(fg), ..Style::new() }
    }

    pub const fn colors(fg: Rgb, bg: Rgb) -> Style {
        Style { fg: Some(fg), bg: Some(bg), ..Style::new() }
    }

    pub const fn on(mut self, bg: Rgb) -> Style {
        self.bg = Some(bg);
        self
    }

    pub const fn with_fg(mut self, fg: Rgb) -> Style {
        self.fg = Some(fg);
        self
    }

    pub const fn bold(mut self) -> Style {
        self.bold = true;
        self
    }

    pub const fn italic(mut self) -> Style {
        self.italic = true;
        self
    }

    pub const fn underline(mut self) -> Style {
        self.underline = true;
        self
    }

    pub const fn dim(mut self) -> Style {
        self.dim = true;
        self
    }

    fn modifier(self) -> Modifier {
        let mut modifier = Modifier::empty();
        if self.bold {
            modifier |= Modifier::BOLD;
        }
        if self.italic {
            modifier |= Modifier::ITALIC;
        }
        if self.underline {
            modifier |= Modifier::UNDERLINED;
        }
        if self.dim {
            modifier |= Modifier::DIM;
        }
        if self.strike {
            modifier |= Modifier::CROSSED_OUT;
        }
        modifier
    }
}

/// A theme colour as a ratatui colour.
pub fn color(rgb: Rgb) -> Color {
    Color::Rgb(rgb.r, rgb.g, rgb.b)
}

/// Columns a string occupies in a terminal.
pub fn width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// `text` cut to at most `max` columns, ending in an ellipsis when something was cut.
/// Never splits a grapheme cluster.
pub fn truncate_end(text: &str, max: usize) -> String {
    if width(text) <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        let w = width(grapheme);
        if used + w > max - 1 {
            break;
        }
        out.push_str(grapheme);
        used += w;
    }
    out.push('…');
    out
}

/// `text` cut to at most `max` columns keeping its end (for paths, where the file name
/// matters most), starting with an ellipsis when something was cut.
pub fn truncate_start(text: &str, max: usize) -> String {
    if width(text) <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut kept: Vec<&str> = Vec::new();
    let mut used = 0;
    for grapheme in text.graphemes(true).rev() {
        let w = width(grapheme);
        if used + w > max - 1 {
            break;
        }
        kept.push(grapheme);
        used += w;
    }
    let mut out = String::from("…");
    out.extend(kept.into_iter().rev());
    out
}

/// A drawing surface: ratatui's buffer plus the rectangle this view may draw in. Everything
/// is clipped to that rectangle, so a view cannot scribble on its neighbours.
pub struct Canvas<'a> {
    buf: &'a mut Buffer,
    clip: Rect,
}

impl<'a> Canvas<'a> {
    pub fn new(buf: &'a mut Buffer, clip: Rect) -> Canvas<'a> {
        let clip = clip.intersection(*buf.area());
        Canvas { buf, clip }
    }

    /// The rectangle this canvas may draw in.
    pub fn area(&self) -> Rect {
        self.clip
    }

    /// A canvas for part of this one.
    pub fn sub(&mut self, area: Rect) -> Canvas<'_> {
        let clip = area.intersection(self.clip);
        Canvas { buf: self.buf, clip }
    }

    fn contains(&self, x: u16, y: u16) -> bool {
        x >= self.clip.x && x < self.clip.right() && y >= self.clip.y && y < self.clip.bottom()
    }

    /// Fill a rectangle with blank cells of one background colour.
    pub fn fill(&mut self, area: Rect, bg: Rgb) {
        let area = area.intersection(self.clip);
        let bg = color(bg);
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                if let Some(cell) = self.buf.cell_mut((x, y)) {
                    cell.reset();
                    cell.set_bg(bg);
                }
            }
        }
    }

    /// Change the style of the cells in a rectangle without touching their text.
    pub fn restyle(&mut self, area: Rect, style: Style) {
        let area = area.intersection(self.clip);
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                if let Some(cell) = self.buf.cell_mut((x, y)) {
                    apply(cell, style);
                }
            }
        }
    }

    /// Put one grapheme at a cell. Returns the columns it took (0 if it was clipped).
    pub fn put(&mut self, x: u16, y: u16, grapheme: &str, style: Style) -> u16 {
        let w = width(grapheme).max(1) as u16;
        if !self.contains(x, y) || x + w > self.clip.right() {
            return 0;
        }
        if let Some(cell) = self.buf.cell_mut((x, y)) {
            cell.set_symbol(grapheme);
            apply(cell, style);
        }
        // The cells a wide character covers are part of it: blank them so stale text
        // cannot show through if the terminal draws the glyph narrower.
        for extra in 1..w {
            if let Some(cell) = self.buf.cell_mut((x + extra, y)) {
                cell.set_symbol(" ");
                apply(cell, style);
            }
        }
        w
    }

    /// Draw text starting at `(x, y)`, using at most `max_width` columns, and return the
    /// columns used. Control characters are drawn as a middle dot; tabs as one space.
    pub fn text(&mut self, x: u16, y: u16, text: &str, style: Style, max_width: u16) -> u16 {
        let mut used: u16 = 0;
        for grapheme in text.graphemes(true) {
            let shown = match grapheme {
                "\t" => " ",
                g if g.chars().all(|c| c.is_control()) => "·",
                g => g,
            };
            let w = width(shown).max(1) as u16;
            if used + w > max_width {
                break;
            }
            if self.put(x + used, y, shown, style) == 0 && x + used >= self.clip.right() {
                break;
            }
            used += w;
        }
        used
    }

    /// [`Canvas::text`] with an ellipsis when the text does not fit.
    pub fn text_truncated(&mut self, x: u16, y: u16, text: &str, style: Style, max_width: u16) -> u16 {
        let fitted = truncate_end(text, max_width as usize);
        self.text(x, y, &fitted, style, max_width)
    }

    /// Draw text right-aligned so that it ends at column `right` (exclusive).
    pub fn text_right(&mut self, right: u16, y: u16, text: &str, style: Style) -> u16 {
        let w = width(text) as u16;
        let x = right.saturating_sub(w);
        self.text(x, y, text, style, w)
    }

    /// A vertical line of `symbol` from `y` for `height` rows.
    pub fn vline(&mut self, x: u16, y: u16, height: u16, symbol: &str, style: Style) {
        for row in y..y.saturating_add(height) {
            self.put(x, row, symbol, style);
        }
    }

    /// A horizontal line of `symbol` from `x` for `width` columns.
    pub fn hline(&mut self, x: u16, y: u16, width: u16, symbol: &str, style: Style) {
        for col in x..x.saturating_add(width) {
            self.put(col, y, symbol, style);
        }
    }

    /// A crisp single-line box border around `area`.
    pub fn box_border(&mut self, area: Rect, style: Style) {
        if area.width < 2 || area.height < 2 {
            return;
        }
        let right = area.x + area.width - 1;
        let bottom = area.y + area.height - 1;
        self.put(area.x, area.y, "┌", style);
        self.put(right, area.y, "┐", style);
        self.put(area.x, bottom, "└", style);
        self.put(right, bottom, "┘", style);
        if area.width > 2 {
            self.hline(area.x + 1, area.y, area.width - 2, "─", style);
            self.hline(area.x + 1, bottom, area.width - 2, "─", style);
        }
        if area.height > 2 {
            self.vline(area.x, area.y + 1, area.height - 2, "│", style);
            self.vline(right, area.y + 1, area.height - 2, "│", style);
        }
    }

    /// Direct access for views that fill cells themselves (the terminal panel).
    pub fn cell_mut(&mut self, x: u16, y: u16) -> Option<&mut ratatui::buffer::Cell> {
        if self.contains(x, y) {
            self.buf.cell_mut((x, y))
        } else {
            None
        }
    }
}

fn apply(cell: &mut ratatui::buffer::Cell, style: Style) {
    if let Some(fg) = style.fg {
        cell.set_fg(color(fg));
    }
    if let Some(bg) = style.bg {
        cell.set_bg(color(bg));
    }
    cell.modifier = style.modifier();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(buf: &Buffer, y: u16) -> String {
        (0..buf.area().width).map(|x| buf[(x, y)].symbol().to_string()).collect()
    }

    #[test]
    fn truncates_at_the_end_and_start() {
        assert_eq!(truncate_end("hello", 10), "hello");
        assert_eq!(truncate_end("hello world", 6), "hello…");
        assert_eq!(truncate_end("hello", 0), "");
        assert_eq!(truncate_end("hello", 1), "…");
        assert_eq!(truncate_start("crates/app/src/main.rs", 12), "…src/main.rs");
        assert_eq!(truncate_start("main.rs", 12), "main.rs");
    }

    #[test]
    fn truncation_respects_wide_characters() {
        // Each CJK character is two columns; four columns hold one of them plus the ellipsis.
        assert_eq!(truncate_end("房產金管家", 4), "房…");
        assert_eq!(width(&truncate_end("房產金管家", 5)), 5);
        assert_eq!(truncate_start("房產金管家", 5), "…管家");
    }

    #[test]
    fn text_is_clipped_to_the_canvas() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 2));
        let mut canvas = Canvas::new(&mut buf, Rect::new(2, 0, 5, 1));
        let used = canvas.text(2, 0, "abcdefghij", Style::new(), 20);
        assert_eq!(used, 5);
        // Outside the clip, including the other row, nothing is drawn.
        canvas.text(0, 0, "zz", Style::new(), 2);
        canvas.text(2, 1, "zz", Style::new(), 2);
        assert_eq!(row(&buf, 0), "  abcde   ");
        assert_eq!(row(&buf, 1), "          ");
    }

    #[test]
    fn max_width_limits_text() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 1));
        let mut canvas = Canvas::new(&mut buf, Rect::new(0, 0, 10, 1));
        assert_eq!(canvas.text(0, 0, "abcdef", Style::new(), 3), 3);
        assert_eq!(canvas.text_truncated(5, 0, "abcdef", Style::new(), 4), 4);
        assert_eq!(row(&buf, 0), "abc  abc… ");
    }

    #[test]
    fn a_wide_character_never_straddles_the_edge() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 5, 1));
        let mut canvas = Canvas::new(&mut buf, Rect::new(0, 0, 5, 1));
        // Five columns: two wide characters fit, the third would need a sixth column.
        let used = canvas.text(0, 0, "房產金", Style::new(), 5);
        assert_eq!(used, 4);
        assert_eq!(buf[(0, 0)].symbol(), "房");
        assert_eq!(buf[(2, 0)].symbol(), "產");
        assert_eq!(buf[(4, 0)].symbol(), " ");
    }

    #[test]
    fn styles_and_fill() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 2));
        let mut canvas = Canvas::new(&mut buf, Rect::new(0, 0, 4, 2));
        let red = Rgb::new(255, 0, 0);
        let blue = Rgb::new(0, 0, 255);
        canvas.fill(Rect::new(0, 0, 4, 2), blue);
        canvas.text(0, 0, "ab", Style::fg(red).bold(), 4);
        assert_eq!(buf[(0, 0)].fg, Color::Rgb(255, 0, 0));
        assert_eq!(buf[(0, 0)].bg, Color::Rgb(0, 0, 255), "text without a background keeps the fill");
        assert!(buf[(0, 0)].modifier.contains(Modifier::BOLD));
        assert_eq!(buf[(3, 1)].bg, Color::Rgb(0, 0, 255));
        let mut canvas = Canvas::new(&mut buf, Rect::new(0, 0, 4, 2));
        canvas.restyle(Rect::new(0, 0, 1, 1), Style::new().on(red));
        assert_eq!(buf[(0, 0)].bg, Color::Rgb(255, 0, 0));
        assert_eq!(buf[(0, 0)].symbol(), "a");
        assert!(!buf[(0, 0)].modifier.contains(Modifier::BOLD), "restyle replaces the modifiers");
    }

    #[test]
    fn control_characters_and_tabs_are_made_visible() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 6, 1));
        let mut canvas = Canvas::new(&mut buf, Rect::new(0, 0, 6, 1));
        canvas.text(0, 0, "a\tb\u{7}c", Style::new(), 6);
        assert_eq!(row(&buf, 0), "a b·c ");
    }

    #[test]
    fn right_aligned_text_and_lines() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 8, 3));
        let mut canvas = Canvas::new(&mut buf, Rect::new(0, 0, 8, 3));
        canvas.text_right(8, 0, "12", Style::new());
        canvas.vline(0, 0, 3, "│", Style::new());
        canvas.hline(1, 2, 3, "─", Style::new());
        assert_eq!(row(&buf, 0), "│     12");
        assert_eq!(row(&buf, 2), "│───    ");
    }
}
