//! The one-row header over the terminal: a tab per terminal, a button for a new one, and on
//! the right whether the panel is fullscreen.

use bakedit_core::Theme;
use ratatui::layout::Rect;

use crate::ui::{self, Canvas, Style};

/// Widest a tab's title gets before it is cut with an ellipsis.
const MAX_TITLE: usize = 22;
/// What the right edge says while the panel covers the whole window.
const ZOOMED_LABEL: &str = "fullscreen";

/// What the header shows of one terminal.
pub struct HeaderTab {
    pub title: String,
    pub exited: bool,
}

/// What a click in the header hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Tab(usize),
    New,
}

/// Where the header put each of its clickable parts: `(first column, width, what)`.
pub type Hits = Vec<(u16, u16, Hit)>;

/// The label of tab `index`: its number, its title, and a mark when its program has ended.
fn label(index: usize, tab: &HeaderTab) -> String {
    let title = ui::truncate_end(&tab.title, MAX_TITLE);
    let mark = if tab.exited { " (exited)" } else { "" };
    format!(" {} {title}{mark} ", index + 1)
}

/// Draw the header into the one-row `area` and say where its parts are. The active tab is
/// always visible: when the tabs do not all fit, the ones before it give way. `note` (why
/// the terminal runs something else than asked) takes whatever room the tabs leave.
pub fn draw(canvas: &mut Canvas<'_>, area: Rect, tabs: &[HeaderTab], active: usize, zoomed: bool, note: Option<&str>, theme: &Theme) -> Hits {
    let mut hits = Hits::new();
    if area.width == 0 || area.height == 0 {
        return hits;
    }
    canvas.fill(area, theme.tab_bar_bg);
    let bar = Style::colors(theme.text_muted, theme.tab_bar_bg);

    // The right edge: one space, the label, one space.
    let right_label = zoomed.then(|| format!(" {ZOOMED_LABEL} "));
    let reserved = right_label.as_ref().map_or(0, |text| ui::width(text) as u16 + 1);
    let room = area.width.saturating_sub(reserved);
    let plus = " + ";
    let plus_width = ui::width(plus) as u16;

    let labels: Vec<String> = tabs.iter().enumerate().map(|(i, tab)| label(i, tab)).collect();
    let widths: Vec<u16> = labels.iter().map(|text| ui::width(text) as u16).collect();
    let active = active.min(tabs.len().saturating_sub(1));
    // Drop tabs from the left until the active one fits.
    let mut first = 0;
    while first < active && widths[first..=active].iter().sum::<u16>() > room {
        first += 1;
    }

    let mut x = area.x;
    let limit = area.x + room;
    for index in first..tabs.len() {
        let width = widths[index];
        if x + width > limit {
            // The active tab is cut rather than dropped when even it alone does not fit.
            if index != active {
                break;
            }
        }
        let shown = width.min(limit.saturating_sub(x));
        if shown == 0 {
            break;
        }
        let (fg, bg) = if index == active { (theme.text, theme.tab_active_bg) } else { (theme.text_muted, theme.tab_inactive_bg) };
        let tab_area = Rect::new(x, area.y, shown, 1);
        canvas.fill(tab_area, bg);
        canvas.text_truncated(x, area.y, &labels[index], Style::colors(fg, bg), shown);
        hits.push((x, shown, Hit::Tab(index)));
        x += shown;
    }
    // Tabs that did not fit are said to exist: a dot-dot-dot before the button.
    let drawn_end = hits.last().map_or(first, |(_, _, hit)| match hit {
        Hit::Tab(index) => index + 1,
        Hit::New => first,
    });
    if drawn_end < tabs.len() && x < limit {
        canvas.text(x, area.y, "…", bar, 1);
        x += 1;
    }
    if x + plus_width <= limit {
        canvas.text(x, area.y, plus, bar, plus_width);
        hits.push((x, plus_width, Hit::New));
    }
    let mut edge = area.right();
    if let Some(text) = right_label {
        let style = Style::colors(theme.text_accent, theme.tab_bar_bg);
        edge = edge.saturating_sub(1);
        canvas.text_right(edge, area.y, &text, style);
        edge = edge.saturating_sub(ui::width(&text) as u16);
    }
    if let Some(note) = note {
        // At least a few words or nothing: a stub of three letters says nothing.
        let free = edge.saturating_sub(x + 1).saturating_sub(1) as usize;
        if free >= 16 {
            let shown = ui::truncate_end(note, free);
            canvas.text_right(edge.saturating_sub(1), area.y, &shown, bar);
        }
    }
    hits
}

/// What a click at screen column `col` hit.
pub fn hit(hits: &Hits, col: u16) -> Option<Hit> {
    hits.iter().find(|(x, width, _)| col >= *x && col < x + width).map(|(_, _, hit)| *hit)
}
