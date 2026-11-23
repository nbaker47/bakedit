//! The frame around the views: the tab bar, the status bar, toasts, and the small
//! yes/no prompt. Each is a function of plain data so it can be drawn and tested alone.

use bakedit_core::{Rgb, Theme};
use ratatui::layout::Rect;

use crate::ui::{self, Canvas, Style};

// ---- tab bar ---------------------------------------------------------------------------

/// One tab as the tab bar shows it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Tab {
    pub title: String,
    /// Unsaved changes.
    pub dirty: bool,
    /// A preview tab (opened by a single selection in the tree; replaced by the next one).
    pub preview: bool,
    pub icon: Option<(String, Rgb)>,
}

/// Where each visible tab was drawn, for mouse clicks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TabHit {
    pub index: usize,
    pub rect: Rect,
}

fn tab_width(tab: &Tab) -> u16 {
    // " icon title ● " : a cell of padding each side, icon + gap, title, gap + marker.
    let icon = tab.icon.as_ref().map(|(glyph, _)| ui::width(glyph) + 1).unwrap_or(0);
    (1 + icon + ui::width(&tab.title) + 2 + 1) as u16
}

/// Draw the tab bar in `area` (one row). `first` is the index of the leftmost tab shown and
/// is adjusted so the active tab is always in view; keep it between frames.
pub fn tab_bar(
    canvas: &mut Canvas<'_>,
    area: Rect,
    tabs: &[Tab],
    active: usize,
    first: &mut usize,
    focused: bool,
    theme: &Theme,
) -> Vec<TabHit> {
    canvas.fill(area, theme.tab_bar_bg);
    let mut hits = Vec::new();
    if area.width == 0 || area.height == 0 || tabs.is_empty() {
        return hits;
    }
    let active = active.min(tabs.len() - 1);
    *first = (*first).min(active);
    // Move the window right until the active tab fits, leaving a cell for the overflow mark.
    loop {
        let needed: u16 = tabs[*first..=active].iter().map(tab_width).sum();
        let budget = area.width.saturating_sub(if *first > 0 { 1 } else { 0 });
        if needed <= budget || *first == active {
            break;
        }
        *first += 1;
    }

    let mut x = area.x;
    if *first > 0 {
        canvas.put(x, area.y, "‹", Style::colors(theme.text_muted, theme.tab_bar_bg));
        x += 1;
    }
    for (index, tab) in tabs.iter().enumerate().skip(*first) {
        let width = tab_width(tab);
        let is_last_visible = x + width > area.right();
        if is_last_visible {
            // Not enough room for the whole tab: say there are more and stop.
            if x < area.right() {
                canvas.put(area.right() - 1, area.y, "›", Style::colors(theme.text_muted, theme.tab_bar_bg));
            }
            break;
        }
        let is_active = index == active;
        let bg = if is_active { theme.tab_active_bg } else { theme.tab_inactive_bg };
        let fg = if is_active { theme.text } else { theme.text_muted };
        let rect = Rect::new(x, area.y, width, 1);
        canvas.fill(rect, bg);
        let mut cx = x + 1;
        if let Some((glyph, color)) = &tab.icon {
            let icon_color = if is_active { *color } else { color.lerp(theme.tab_bar_bg, 0.35) };
            cx += canvas.text(cx, area.y, glyph, Style::colors(icon_color, bg), 2);
            cx += 1;
        }
        let mut title = Style::colors(fg, bg);
        title.italic = tab.preview;
        title.bold = is_active && focused;
        cx += canvas.text(cx, area.y, &tab.title, title, width);
        if tab.dirty {
            canvas.put(cx + 1, area.y, "●", Style::colors(theme.modified, bg));
        }
        hits.push(TabHit { index, rect });
        x += width;
    }
    hits
}

// ---- status bar ------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeIndicator {
    Nav,
    Edit,
    Select,
    /// Modes are off (`"vim_mode": false`) or the focus is not in an editor.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavLayerIndicator {
    /// Switched off in settings or by the toggle.
    Off,
    /// On, waiting for the layer key.
    Ready,
    /// The layer key is held right now.
    Held,
    /// On, but this terminal cannot report key releases, so it cannot work here.
    Unavailable,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub mode: ModeIndicator,
    /// Branch name, or a short commit id when detached.
    pub branch: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    /// Number of changed files.
    pub changes: usize,
    /// A message for the user; errors are drawn in the error colour.
    pub message: Option<(String, bool)>,
    /// Keys typed so far of an unfinished chord, e.g. "cmd-k".
    pub pending_keys: Option<String>,
    /// 1-based cursor position and how many characters are selected.
    pub position: Option<(usize, usize, usize)>,
    pub cursors: usize,
    pub language: Option<String>,
    pub indent: Option<String>,
    pub line_ending: Option<&'static str>,
    pub nav_layer: NavLayerIndicator,
    pub nerd_font: bool,
}

impl Default for Status {
    fn default() -> Status {
        Status {
            mode: ModeIndicator::None,
            branch: None,
            ahead: 0,
            behind: 0,
            changes: 0,
            message: None,
            pending_keys: None,
            position: None,
            cursors: 1,
            language: None,
            indent: None,
            line_ending: None,
            nav_layer: NavLayerIndicator::Off,
            nerd_font: true,
        }
    }
}

/// Draw the status bar in `area` (one row): mode and git on the left, the message in the
/// middle, the cursor and file facts on the right. On a narrow screen the right side gives
/// way first, least important item first, so the mode and branch never get cut. Returns
/// where the mode pill (NAV, EDIT, SELECT) was drawn, for clicks on it: `None` when there is
/// no mode to show or no room.
pub fn status_bar(canvas: &mut Canvas<'_>, area: Rect, status: &Status, theme: &Theme) -> Option<Rect> {
    let bg = theme.status_bar_bg;
    canvas.fill(area, bg);
    if area.width == 0 || area.height == 0 {
        return None;
    }
    let y = area.y;
    let muted = Style::colors(theme.text_muted, bg);
    let mut x = area.x;

    let pill = |label: &str, color: Rgb| -> (String, Style) {
        (format!(" {label} "), Style::colors(theme.background, color).bold())
    };
    let mode = match status.mode {
        ModeIndicator::Nav => Some(pill("NAV", theme.text_accent)),
        ModeIndicator::Edit => Some(pill("EDIT", theme.success)),
        ModeIndicator::Select => Some(pill("SELECT", theme.conflict)),
        ModeIndicator::None => None,
    };
    let mut pill = None;
    if let Some((label, style)) = mode {
        let width = canvas.text(x, y, &label, style, area.width);
        pill = (width > 0).then(|| Rect::new(x, y, width, 1));
        x += width;
        x += 1;
    } else {
        x += 1;
    }

    if let Some(branch) = &status.branch {
        let icon = if status.nerd_font { "\u{e0a0} " } else { "" };
        let mut text = format!("{icon}{branch}");
        if status.ahead > 0 {
            text.push_str(&format!(" ↑{}", status.ahead));
        }
        if status.behind > 0 {
            text.push_str(&format!(" ↓{}", status.behind));
        }
        let room = area.right().saturating_sub(x);
        x += canvas.text_truncated(x, y, &text, Style::colors(theme.text, bg), room.min(40));
        if status.changes > 0 {
            x += 2;
            let room = area.right().saturating_sub(x);
            x += canvas.text(x, y, &format!("±{}", status.changes), Style::colors(theme.modified, bg), room);
        }
        x += 2;
    }

    // Right side, built from the edge inwards.
    let mut right_items: Vec<(String, Style)> = Vec::new();
    match status.nav_layer {
        NavLayerIndicator::Off => {}
        NavLayerIndicator::Ready => right_items.push((";layer".into(), muted)),
        NavLayerIndicator::Held => right_items.push((";layer".into(), Style::colors(theme.background, theme.text_accent).bold())),
        NavLayerIndicator::Unavailable => right_items.push((";layer n/a".into(), Style::colors(theme.warning, bg))),
    }
    if let Some(line_ending) = status.line_ending {
        right_items.push((line_ending.to_string(), muted));
    }
    if let Some(indent) = &status.indent {
        right_items.push((indent.clone(), muted));
    }
    if let Some(language) = &status.language {
        right_items.push((language.clone(), muted));
    }
    if let Some((line, column, selected)) = status.position {
        let mut text = format!("{line}:{column}");
        if status.cursors > 1 {
            text.push_str(&format!(" ({} cursors)", status.cursors));
        } else if selected > 0 {
            text.push_str(&format!(" ({selected} selected)"));
        }
        right_items.push((text, Style::colors(theme.text, bg)));
    }

    // Items are separated by two spaces and the bar ends with one cell of padding. Drop
    // from the edge (the least important) until what is left fits beside the left side.
    let left_end = x;
    let total = |items: &[(String, Style)]| -> u16 {
        items.iter().map(|(text, _)| ui::width(text) as u16).sum::<u16>() + 2 * items.len().saturating_sub(1) as u16 + 1
    };
    let mut start = 0;
    while start < right_items.len() && left_end + total(&right_items[start..]) + 1 > area.right() {
        start += 1;
    }
    let shown = &right_items[start..];
    let mut right_start = area.right();
    if !shown.is_empty() {
        let mut rx = area.right() - 1;
        for (text, style) in shown {
            let w = ui::width(text) as u16;
            rx -= w;
            canvas.text(rx, y, text, *style, w);
            right_start = rx;
            rx = rx.saturating_sub(2);
        }
    }

    // The middle: an unfinished chord wins over a message, because it is what the next key
    // will be read against.
    let room = right_start.saturating_sub(x + 2);
    if room >= 4 {
        if let Some(keys) = &status.pending_keys {
            canvas.text_truncated(x, y, &format!("{keys} …"), Style::colors(theme.text_accent, bg).bold(), room);
        } else if let Some((message, is_error)) = &status.message {
            let color = if *is_error { theme.error } else { theme.text_muted };
            canvas.text_truncated(x, y, message, Style::colors(color, bg), room);
        }
    }
    pill
}

// ---- prompt ----------------------------------------------------------------------------

/// A small question with buttons: "Discard changes to x?  [Discard] [Cancel]".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub message: String,
    pub detail: Option<String>,
    pub buttons: Vec<String>,
    pub selected: usize,
    /// The button escape presses (usually "Cancel").
    pub cancel: usize,
    hits: Vec<Rect>,
    area: Rect,
}

impl Prompt {
    /// A prompt whose last button is the cancel button and starts selected: the safe choice
    /// is what a stray Enter gets.
    pub fn new(message: impl Into<String>, buttons: &[&str]) -> Prompt {
        let buttons: Vec<String> = buttons.iter().map(|b| b.to_string()).collect();
        let cancel = buttons.len().saturating_sub(1);
        Prompt { message: message.into(), detail: None, buttons, selected: cancel, cancel, hits: Vec::new(), area: Rect::default() }
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Prompt {
        self.detail = Some(detail.into());
        self
    }

    pub fn select_next(&mut self) {
        if !self.buttons.is_empty() {
            self.selected = (self.selected + 1) % self.buttons.len();
        }
    }

    pub fn select_previous(&mut self) {
        if !self.buttons.is_empty() {
            self.selected = (self.selected + self.buttons.len() - 1) % self.buttons.len();
        }
    }

    /// The button whose first letter is `c` (case-insensitive), if exactly one has it.
    pub fn button_for_key(&self, c: char) -> Option<usize> {
        let wanted = c.to_lowercase().next()?;
        let mut matches = self
            .buttons
            .iter()
            .enumerate()
            .filter(|(_, label)| label.chars().next().and_then(|f| f.to_lowercase().next()) == Some(wanted));
        let first = matches.next()?;
        if matches.next().is_some() {
            None
        } else {
            Some(first.0)
        }
    }

    /// The button at a screen cell; `Err(true)` when the click was outside the prompt.
    pub fn click(&self, col: u16, row: u16) -> Result<usize, bool> {
        let inside = |r: &Rect| col >= r.x && col < r.right() && row >= r.y && row < r.bottom();
        match self.hits.iter().position(inside) {
            Some(index) => Ok(index),
            None => Err(!inside(&self.area)),
        }
    }

    /// Draw the prompt centred in `screen`.
    pub fn render(&mut self, canvas: &mut Canvas<'_>, screen: Rect, theme: &Theme) {
        let bg = theme.elevated_surface;
        let buttons_width: u16 =
            self.buttons.iter().map(|b| ui::width(b) as u16 + 4).sum::<u16>() + 2 * self.buttons.len().saturating_sub(1) as u16;
        let detail_width = self.detail.as_deref().map(ui::width).unwrap_or(0) as u16;
        let content = (ui::width(&self.message) as u16).max(buttons_width).max(detail_width);
        let width = (content + 6).min(screen.width);
        let height = (if self.detail.is_some() { 6 } else { 5 }).min(screen.height);
        let x = screen.x + (screen.width - width) / 2;
        let y = screen.y + (screen.height - height) / 3;
        let panel = Rect::new(x, y, width, height);
        self.area = panel;
        canvas.fill(panel, bg);
        let inner_x = panel.x + 3;
        let inner_width = panel.width.saturating_sub(6);
        let mut row = panel.y + 1;
        canvas.text_truncated(inner_x, row, &self.message, Style::colors(theme.text, bg).bold(), inner_width);
        if let Some(detail) = &self.detail {
            row += 1;
            canvas.text_truncated(inner_x, row, detail, Style::colors(theme.text_muted, bg), inner_width);
        }
        row += 2;
        self.hits.clear();
        if row >= panel.bottom() {
            return;
        }
        // Buttons share one row, one height, and a two-cell gap, right-aligned like Zed's.
        let mut bx = panel.right().saturating_sub(3 + buttons_width).max(inner_x);
        for (index, label) in self.buttons.iter().enumerate() {
            let w = ui::width(label) as u16 + 4;
            let selected = index == self.selected;
            let style = if selected {
                Style::colors(theme.background, theme.text_accent).bold()
            } else {
                Style::colors(theme.text, theme.element_hover.lerp(theme.text, 0.12))
            };
            let rect = Rect::new(bx, row, w, 1);
            canvas.fill(rect, style.bg.unwrap_or(bg));
            canvas.text(bx + 2, row, label, style, w.saturating_sub(4));
            self.hits.push(rect);
            bx += w + 2;
        }
    }
}

// ---- toast -----------------------------------------------------------------------------

/// A short notice in the top-right corner of `area`.
pub fn toast(canvas: &mut Canvas<'_>, area: Rect, message: &str, is_error: bool, theme: &Theme) {
    if area.width < 12 || area.height < 3 {
        return;
    }
    let bg = theme.elevated_surface;
    let max = (area.width - 4).min(70) as usize;
    let text = ui::truncate_end(message, max.saturating_sub(4));
    let width = ui::width(&text) as u16 + 4;
    let x = area.right() - width - 1;
    let y = area.y + 1;
    canvas.fill(Rect::new(x, y, width, 1), bg);
    let color = if is_error { theme.error } else { theme.text };
    canvas.text(x + 2, y, &text, Style::colors(color, bg), width - 4);
    // A coloured bar on the left edge says what kind of notice it is at a glance.
    canvas.put(x, y, "▌", Style::colors(if is_error { theme.error } else { theme.text_accent }, bg));
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;

    fn row(buf: &Buffer, y: u16) -> String {
        (0..buf.area().width).map(|x| buf[(x, y)].symbol().to_string()).collect()
    }

    fn tabs(titles: &[&str]) -> Vec<Tab> {
        titles.iter().map(|t| Tab { title: t.to_string(), ..Default::default() }).collect()
    }

    fn draw_tabs(tabs: &[Tab], active: usize, first: &mut usize, width: u16) -> (String, Vec<TabHit>, Buffer) {
        let area = Rect::new(0, 0, width, 1);
        let mut buf = Buffer::empty(area);
        let theme = Theme::builtin();
        let hits = {
            let mut canvas = Canvas::new(&mut buf, area);
            tab_bar(&mut canvas, area, tabs, active, first, true, &theme)
        };
        (row(&buf, 0), hits, buf)
    }

    #[test]
    fn tabs_have_padding_and_a_dirty_marker() {
        let mut list = tabs(&["main.rs", "lib.rs"]);
        list[1].dirty = true;
        let (text, hits, buf) = draw_tabs(&list, 0, &mut 0, 40);
        assert_eq!(text.trim_end(), " main.rs    lib.rs ●");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].rect, Rect::new(0, 0, 11, 1));
        assert_eq!(hits[1].rect, Rect::new(11, 0, 10, 1));
        let theme = Theme::builtin();
        assert_eq!(buf[(1, 0)].bg, ui::color(theme.tab_active_bg));
        assert_eq!(buf[(12, 0)].bg, ui::color(theme.tab_inactive_bg));
        assert_eq!(buf[(1, 0)].fg, ui::color(theme.text));
        assert_eq!(buf[(12, 0)].fg, ui::color(theme.text_muted));
    }

    #[test]
    fn icons_get_a_gap_before_the_title() {
        let list = vec![Tab { title: "a.rs".into(), icon: Some(("R".into(), Rgb::new(1, 2, 3))), ..Default::default() }];
        let (text, _, _) = draw_tabs(&list, 0, &mut 0, 20);
        assert_eq!(text.trim_end(), " R a.rs");
    }

    #[test]
    fn the_active_tab_is_scrolled_into_view() {
        let list = tabs(&["alpha.rs", "beta.rs", "gamma.rs", "delta.rs", "epsilon.rs"]);
        let mut first = 0;
        let (text, hits, _) = draw_tabs(&list, 4, &mut first, 30);
        assert!(first > 0, "the window moved right");
        assert!(text.starts_with('‹'));
        assert!(text.contains("epsilon.rs"));
        assert_eq!(hits.last().unwrap().index, 4);
        // Going back to the first tab scrolls back and marks the overflow on the right.
        let (text, hits, _) = draw_tabs(&list, 0, &mut first, 30);
        assert_eq!(first, 0);
        assert!(text.starts_with(" alpha.rs"));
        assert!(text.trim_end().ends_with('›'));
        assert_eq!(hits[0].index, 0);
    }

    #[test]
    fn tab_bar_edge_cases() {
        let (text, hits, _) = draw_tabs(&[], 0, &mut 0, 20);
        assert_eq!(text.trim(), "");
        assert!(hits.is_empty());
        let list = tabs(&["a-very-long-file-name-that-does-not-fit.rs"]);
        let mut first = 3;
        let (_, hits, _) = draw_tabs(&list, 7, &mut first, 10);
        assert_eq!(first, 0, "out-of-range values are clamped");
        assert!(hits.is_empty(), "a tab that cannot fit is not half drawn");
    }

    fn draw_status(status: &Status, width: u16) -> String {
        let area = Rect::new(0, 0, width, 1);
        let mut buf = Buffer::empty(area);
        let theme = Theme::builtin();
        {
            let mut canvas = Canvas::new(&mut buf, area);
            status_bar(&mut canvas, area, status, &theme);
        }
        row(&buf, 0)
    }

    fn full_status() -> Status {
        Status {
            mode: ModeIndicator::Nav,
            branch: Some("main".into()),
            ahead: 1,
            behind: 2,
            changes: 3,
            message: Some(("Saved src/main.rs".into(), false)),
            position: Some((12, 5, 0)),
            language: Some("Rust".into()),
            indent: Some("Spaces: 4".into()),
            line_ending: Some("LF"),
            nav_layer: NavLayerIndicator::Ready,
            nerd_font: false,
            ..Default::default()
        }
    }

    #[test]
    fn status_bar_lays_out_left_middle_right() {
        let text = draw_status(&full_status(), 100);
        assert!(text.starts_with(" NAV  main ↑1 ↓2  ±3  Saved src/main.rs"), "{text:?}");
        assert!(text.ends_with("12:5  Rust  Spaces: 4  LF  ;layer "), "{text:?}");
    }

    #[test]
    fn status_bar_drops_the_right_side_first_when_narrow() {
        let text = draw_status(&full_status(), 44);
        assert!(text.starts_with(" NAV  main ↑1 ↓2  ±3"), "{text:?}");
        assert!(text.contains("12:5"), "the cursor position is the last to go: {text:?}");
        assert!(!text.contains(";layer"), "{text:?}");
        let text = draw_status(&full_status(), 24);
        assert!(text.starts_with(" NAV  main ↑1 ↓2  ±3"), "{text:?}");
        assert!(!text.contains("Rust"));
        for width in [0, 1, 3, 8] {
            draw_status(&full_status(), width);
        }
    }

    #[test]
    fn status_bar_variants() {
        let mut status = full_status();
        status.pending_keys = Some("cmd-k".into());
        assert!(draw_status(&status, 100).contains("cmd-k …"));
        assert!(!draw_status(&status, 100).contains("Saved"), "a pending chord replaces the message");
        status.pending_keys = None;
        status.position = Some((3, 1, 10));
        assert!(draw_status(&status, 100).contains("3:1 (10 selected)"));
        status.cursors = 4;
        assert!(draw_status(&status, 100).contains("3:1 (4 cursors)"));
        status.mode = ModeIndicator::Edit;
        assert!(draw_status(&status, 100).starts_with(" EDIT "));
        status.mode = ModeIndicator::None;
        status.branch = None;
        assert!(draw_status(&status, 100).starts_with(" Saved"), "no mode and no branch: the message starts the bar");
        status.nav_layer = NavLayerIndicator::Unavailable;
        assert!(draw_status(&status, 100).contains(";layer n/a"));
    }

    #[test]
    fn the_status_bar_says_where_it_drew_the_mode() {
        let pill = |mode: ModeIndicator, area: Rect| {
            let screen = Rect::new(0, 0, 80, 5);
            let mut buf = Buffer::empty(screen);
            let mut canvas = Canvas::new(&mut buf, screen);
            status_bar(&mut canvas, area, &Status { mode, ..full_status() }, &Theme::builtin())
        };
        assert_eq!(pill(ModeIndicator::Nav, Rect::new(0, 3, 60, 1)), Some(Rect::new(0, 3, 5, 1)), "\" NAV \"");
        assert_eq!(pill(ModeIndicator::Select, Rect::new(10, 3, 60, 1)), Some(Rect::new(10, 3, 8, 1)));
        assert_eq!(pill(ModeIndicator::None, Rect::new(0, 3, 60, 1)), None);
        assert_eq!(pill(ModeIndicator::Edit, Rect::new(0, 3, 3, 1)), Some(Rect::new(0, 3, 3, 1)), "only what fits");
        assert_eq!(pill(ModeIndicator::Nav, Rect::new(0, 3, 0, 1)), None);
    }

    fn draw_prompt(prompt: &mut Prompt, width: u16, height: u16) -> Vec<String> {
        let screen = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(screen);
        let theme = Theme::builtin();
        {
            let mut canvas = Canvas::new(&mut buf, screen);
            prompt.render(&mut canvas, screen, &theme);
        }
        (0..height).map(|y| row(&buf, y).trim_end().to_string()).collect()
    }

    #[test]
    fn prompt_defaults_to_the_safe_button() {
        let mut prompt = Prompt::new("Discard changes to main.rs?", &["Discard", "Cancel"]);
        assert_eq!(prompt.selected, 1);
        assert_eq!(prompt.cancel, 1);
        prompt.select_next();
        assert_eq!(prompt.selected, 0);
        prompt.select_previous();
        assert_eq!(prompt.selected, 1);
        assert_eq!(prompt.button_for_key('d'), Some(0));
        assert_eq!(prompt.button_for_key('C'), Some(1));
        assert_eq!(prompt.button_for_key('x'), None);
        let ambiguous = Prompt::new("?", &["Save", "Skip", "Cancel"]);
        assert_eq!(ambiguous.button_for_key('s'), None, "two buttons start with s");
    }

    #[test]
    fn prompt_draws_message_and_aligned_buttons() {
        let mut prompt = Prompt::new("Discard changes to main.rs?", &["Discard", "Cancel"]).detail("This cannot be undone.");
        let rows = draw_prompt(&mut prompt, 80, 24);
        let message_row = rows.iter().position(|r| r.contains("Discard changes to main.rs?")).unwrap();
        assert!(rows[message_row + 1].contains("This cannot be undone."));
        let button_row = &rows[message_row + 3];
        let discard = button_row.find("Discard").unwrap();
        let cancel = button_row.find("Cancel").unwrap();
        assert_eq!(cancel - (discard + "Discard".len()), 6, "two cells of padding each plus a two-cell gap");
        // Clicking the buttons, the panel, and outside it.
        assert_eq!(prompt.click(discard as u16, (message_row + 3) as u16), Ok(0));
        assert_eq!(prompt.click(cancel as u16, (message_row + 3) as u16), Ok(1));
        assert_eq!(prompt.click(discard as u16, message_row as u16), Err(false));
        assert_eq!(prompt.click(0, 0), Err(true));
        for (w, h) in [(1, 1), (10, 3), (30, 5)] {
            draw_prompt(&mut prompt, w, h);
        }
    }

    #[test]
    fn toast_sits_in_the_top_right() {
        let area = Rect::new(0, 0, 60, 10);
        let mut buf = Buffer::empty(area);
        let theme = Theme::builtin();
        {
            let mut canvas = Canvas::new(&mut buf, area);
            toast(&mut canvas, area, "Saved", false, &theme);
            toast(&mut canvas, Rect::new(0, 0, 5, 1), "too small to draw", true, &theme);
        }
        let text = row(&buf, 1);
        assert!(text.trim_end().ends_with("▌ Saved"), "{text:?}");
        assert_eq!(text.len() - text.trim_end().len(), 3, "padding inside plus a cell from the edge");
    }
}
