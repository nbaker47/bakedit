//! The picker: a query box over a filtered list, drawn as a panel near the top of the
//! screen. The file finder, command palette, tab switcher, outline, branch list and link
//! list are all this one widget with different rows.
//!
//! The picker owns the query text, the selection and the scrolling. It does not know where
//! rows come from: the app refills them with [`Picker::set_items`] whenever
//! [`PickerEvent::QueryChanged`] comes back.

use bakedit_core::{Action, Rgb, Theme};
use ratatui::layout::Rect;

use crate::line_input::{InputOutcome, LineInput};
use crate::ui::{self, Canvas, Style};

/// The widest a picker is drawn.
const MAX_WIDTH: u16 = 90;
/// The most rows of results shown at once.
const MAX_ROWS: u16 = 14;

/// One row of results.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PickerItem {
    /// Main text.
    pub label: String,
    /// Char positions in `label` that matched the query, drawn emphasised.
    pub matched: Vec<usize>,
    /// Quieter text after the label (a directory, a description).
    pub detail: Option<String>,
    /// Text at the right edge (a key binding, a line number).
    pub hint: Option<String>,
    /// A leading glyph and its colour.
    pub icon: Option<(String, Rgb)>,
}

impl PickerItem {
    pub fn new(label: impl Into<String>) -> PickerItem {
        PickerItem { label: label.into(), ..Default::default() }
    }

    pub fn detail(mut self, detail: impl Into<String>) -> PickerItem {
        self.detail = Some(detail.into());
        self
    }

    pub fn hint(mut self, hint: impl Into<String>) -> PickerItem {
        self.hint = Some(hint.into());
        self
    }

    pub fn icon(mut self, glyph: impl Into<String>, color: Rgb) -> PickerItem {
        self.icon = Some((glyph.into(), color));
        self
    }

    pub fn matched(mut self, matched: Vec<usize>) -> PickerItem {
        self.matched = matched;
        self
    }
}

/// What the picker wants the app to do after an input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerEvent {
    /// Handled, nothing for the app to do beyond redrawing.
    Handled,
    /// The query text changed: refill the rows.
    QueryChanged,
    /// Enter (or cmd-enter when `secondary`) on the selected row. With no rows the app
    /// decides what confirming the bare query means.
    Confirmed { secondary: bool },
    Cancelled,
    /// Copy this text.
    Copy,
    /// Paste the clipboard into the query.
    WantsPaste,
}

#[derive(Debug, Clone)]
pub struct Picker {
    pub input: LineInput,
    pub placeholder: String,
    /// Shown instead of the list when there are no rows.
    pub empty_message: String,
    items: Vec<PickerItem>,
    selected: usize,
    scroll: usize,
    /// Where the list was last drawn, for mouse hits.
    list_area: Rect,
    panel_area: Rect,
    copied: String,
}

impl Picker {
    pub fn new(placeholder: impl Into<String>) -> Picker {
        Picker {
            input: LineInput::new(),
            placeholder: placeholder.into(),
            empty_message: "No matches".to_string(),
            items: Vec::new(),
            selected: 0,
            scroll: 0,
            list_area: Rect::default(),
            panel_area: Rect::default(),
            copied: String::new(),
        }
    }

    pub fn query(&self) -> &str {
        self.input.text()
    }

    pub fn items(&self) -> &[PickerItem] {
        &self.items
    }

    /// Replace the rows. The selection goes back to the top, as in Zed: the best match for
    /// the new query is the one you most likely want.
    pub fn set_items(&mut self, items: Vec<PickerItem>) {
        self.items = items;
        self.selected = 0;
        self.scroll = 0;
    }

    /// Replace the rows but keep the selection where it was (clamped), for lists that
    /// refresh under the user without the query changing.
    pub fn refresh_items(&mut self, items: Vec<PickerItem>) {
        self.items = items;
        self.selected = self.selected.min(self.items.len().saturating_sub(1));
    }

    pub fn selected(&self) -> Option<usize> {
        if self.items.is_empty() {
            None
        } else {
            Some(self.selected)
        }
    }

    pub fn select(&mut self, index: usize) {
        self.selected = index.min(self.items.len().saturating_sub(1));
    }

    /// Text set aside by the last copy or cut in the query box.
    pub fn take_copied(&mut self) -> String {
        std::mem::take(&mut self.copied)
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.items.len();
        if len == 0 {
            return;
        }
        // Wrap around at both ends, like Zed's pickers.
        self.selected = (self.selected as isize + delta).rem_euclid(len as isize) as usize;
    }

    /// Offer an action to the picker. `None` means it is not the picker's to handle.
    pub fn handle_action(&mut self, action: &Action) -> Option<PickerEvent> {
        match action.name.as_str() {
            "menu::SelectNext" => self.move_selection(1),
            "menu::SelectPrevious" => self.move_selection(-1),
            "menu::SelectFirst" => self.selected = 0,
            "menu::SelectLast" => self.selected = self.items.len().saturating_sub(1),
            "menu::Confirm" => return Some(PickerEvent::Confirmed { secondary: false }),
            "menu::SecondaryConfirm" => return Some(PickerEvent::Confirmed { secondary: true }),
            "menu::Cancel" => return Some(PickerEvent::Cancelled),
            _ => {
                return match self.input.handle_action(action) {
                    InputOutcome::Ignored => None,
                    InputOutcome::Moved => Some(PickerEvent::Handled),
                    InputOutcome::Edited => Some(PickerEvent::QueryChanged),
                    InputOutcome::Copied { text, edited } => {
                        self.copied = text;
                        Some(if edited { PickerEvent::QueryChanged } else { PickerEvent::Copy })
                    }
                    InputOutcome::WantsPaste => Some(PickerEvent::WantsPaste),
                };
            }
        }
        Some(PickerEvent::Handled)
    }

    /// Typed or pasted text goes into the query.
    pub fn insert(&mut self, text: &str) -> PickerEvent {
        let before = self.input.text().len();
        self.input.insert(text);
        if self.input.text().len() == before && self.input.selection().is_none() && text.is_empty() {
            PickerEvent::Handled
        } else {
            PickerEvent::QueryChanged
        }
    }

    /// A click at a screen cell. A click on a row selects it and confirms; a click outside
    /// the picker cancels it.
    pub fn click(&mut self, col: u16, row: u16) -> PickerEvent {
        let inside = |area: Rect| col >= area.x && col < area.right() && row >= area.y && row < area.bottom();
        if inside(self.list_area) {
            let index = self.scroll + (row - self.list_area.y) as usize;
            if index < self.items.len() {
                self.selected = index;
                return PickerEvent::Confirmed { secondary: false };
            }
            PickerEvent::Handled
        } else if inside(self.panel_area) {
            PickerEvent::Handled
        } else {
            PickerEvent::Cancelled
        }
    }

    /// Mouse wheel over the list: move the selection.
    pub fn scroll_by(&mut self, delta: isize) {
        let len = self.items.len();
        if len > 0 {
            self.selected = (self.selected as isize + delta).clamp(0, len as isize - 1) as usize;
        }
    }

    /// Draw the picker over `screen` and return where the text cursor goes.
    pub fn render(&mut self, canvas: &mut Canvas<'_>, screen: Rect, theme: &Theme) -> (u16, u16) {
        let width = screen.width.saturating_sub(4).min(MAX_WIDTH).max(screen.width.min(20));
        let rows = (self.items.len() as u16).clamp(1, MAX_ROWS);
        // One row for the query, one blank row under it, then the list, one row of padding.
        let height = (rows + 3).min(screen.height);
        let x = screen.x + (screen.width.saturating_sub(width)) / 2;
        let y = screen.y + (screen.height / 8).min(screen.height.saturating_sub(height));
        let panel = Rect::new(x, y, width, height);
        self.panel_area = panel;

        let bg = theme.elevated_surface;
        canvas.fill(panel, bg);
        let inner_x = panel.x + 2;
        let inner_width = panel.width.saturating_sub(4);

        let text = Style::colors(theme.text, bg);
        let muted = Style::colors(theme.text_muted, bg);
        let query_area = Rect::new(inner_x, panel.y, inner_width, 1);
        let selection = Style::colors(theme.text, theme.selection_bg);
        let cursor_x = self.input.render(canvas, query_area, text, selection, &self.placeholder, muted);
        // A hairline under the query separates it from the results without a box.
        if panel.height > 1 {
            canvas.hline(panel.x, panel.y + 1, panel.width, "─", Style::colors(theme.border, bg));
        }

        let list_height = panel.height.saturating_sub(3);
        let list = Rect::new(panel.x, panel.y + 2, panel.width, list_height);
        self.list_area = list;
        if list_height == 0 {
            return (cursor_x, panel.y);
        }
        if self.items.is_empty() {
            canvas.text_truncated(inner_x, list.y, &self.empty_message, muted, inner_width);
            return (cursor_x, panel.y);
        }

        // Scroll just enough to keep the selection in view.
        let visible = list_height as usize;
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + visible {
            self.scroll = self.selected + 1 - visible;
        }
        self.scroll = self.scroll.min(self.items.len().saturating_sub(visible));

        for (offset, item) in self.items.iter().enumerate().skip(self.scroll).take(visible) {
            let row_y = list.y + (offset - self.scroll) as u16;
            let is_selected = offset == self.selected;
            let row_bg = if is_selected { theme.element_selected } else { bg };
            if is_selected {
                canvas.fill(Rect::new(panel.x, row_y, panel.width, 1), row_bg);
            }
            draw_item(canvas, item, inner_x, row_y, inner_width, row_bg, theme);
        }

        // A thin scroll marker on the right edge when the list is longer than the panel.
        if self.items.len() > visible {
            let track = list_height as usize;
            let thumb = (visible * track / self.items.len()).max(1);
            let start = self.scroll * track / self.items.len();
            for i in 0..thumb.min(track - start.min(track)) {
                canvas.put(panel.right() - 1, list.y + (start + i) as u16, "▐", Style::colors(theme.scrollbar_thumb, bg));
            }
        }
        (cursor_x, panel.y)
    }
}

fn draw_item(canvas: &mut Canvas<'_>, item: &PickerItem, x: u16, y: u16, width: u16, bg: Rgb, theme: &Theme) {
    let mut x = x;
    let mut remaining = width;
    if let Some((glyph, color)) = &item.icon {
        let used = canvas.text(x, y, glyph, Style::colors(*color, bg), remaining);
        // An icon and its label always have a gap.
        let gap = used + 1;
        x += gap.min(remaining);
        remaining = remaining.saturating_sub(gap);
    }

    // Reserve the right edge for the hint, with at least two cells between it and the text.
    let hint_width = item.hint.as_deref().map(ui::width).unwrap_or(0) as u16;
    let text_width = if hint_width > 0 && hint_width + 2 < remaining { remaining - hint_width - 2 } else { remaining };
    if hint_width > 0 && hint_width + 2 < remaining {
        if let Some(hint) = &item.hint {
            canvas.text_right(x + remaining, y, hint, Style::colors(theme.text_muted, bg));
        }
    }

    let label = ui::truncate_end(&item.label, text_width as usize);
    let plain = Style::colors(theme.text, bg);
    let emphasised = Style::colors(theme.text_accent, bg).bold();
    let mut used = 0u16;
    for (index, ch) in label.chars().enumerate() {
        let mut encoded = [0u8; 4];
        let style = if item.matched.contains(&index) { emphasised } else { plain };
        used += canvas.put(x + used, y, ch.encode_utf8(&mut encoded), style);
    }
    if let Some(detail) = &item.detail {
        let room = text_width.saturating_sub(used + 2);
        if room >= 4 {
            let detail = ui::truncate_start(detail, room as usize);
            canvas.text(x + used + 2, y, &detail, Style::colors(theme.text_muted, bg), room);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;

    fn act(picker: &mut Picker, name: &str) -> Option<PickerEvent> {
        picker.handle_action(&Action::new(name))
    }

    fn picker_with(labels: &[&str]) -> Picker {
        let mut picker = Picker::new("Search files");
        picker.set_items(labels.iter().map(|l| PickerItem::new(*l)).collect());
        picker
    }

    fn draw(picker: &mut Picker, width: u16, height: u16) -> (Vec<String>, (u16, u16), Buffer) {
        let screen = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(screen);
        let theme = Theme::builtin();
        let cursor = {
            let mut canvas = Canvas::new(&mut buf, screen);
            picker.render(&mut canvas, screen, &theme)
        };
        let rows = (0..height)
            .map(|y| (0..width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
            .collect();
        (rows, cursor, buf)
    }

    #[test]
    fn selection_moves_and_wraps() {
        let mut picker = picker_with(&["a", "b", "c"]);
        assert_eq!(picker.selected(), Some(0));
        act(&mut picker, "menu::SelectNext");
        act(&mut picker, "menu::SelectNext");
        assert_eq!(picker.selected(), Some(2));
        act(&mut picker, "menu::SelectNext");
        assert_eq!(picker.selected(), Some(0), "wraps to the top");
        act(&mut picker, "menu::SelectPrevious");
        assert_eq!(picker.selected(), Some(2), "and to the bottom");
        act(&mut picker, "menu::SelectFirst");
        assert_eq!(picker.selected(), Some(0));
        act(&mut picker, "menu::SelectLast");
        assert_eq!(picker.selected(), Some(2));
    }

    #[test]
    fn an_empty_picker_has_no_selection_and_still_confirms() {
        let mut picker = picker_with(&[]);
        assert_eq!(picker.selected(), None);
        assert_eq!(act(&mut picker, "menu::SelectNext"), Some(PickerEvent::Handled));
        assert_eq!(act(&mut picker, "menu::Confirm"), Some(PickerEvent::Confirmed { secondary: false }));
        assert_eq!(act(&mut picker, "menu::SecondaryConfirm"), Some(PickerEvent::Confirmed { secondary: true }));
        assert_eq!(act(&mut picker, "menu::Cancel"), Some(PickerEvent::Cancelled));
    }

    #[test]
    fn typing_reports_a_query_change_and_new_rows_reset_the_selection() {
        let mut picker = picker_with(&["a", "b", "c"]);
        act(&mut picker, "menu::SelectLast");
        assert_eq!(picker.insert("ma"), PickerEvent::QueryChanged);
        assert_eq!(picker.query(), "ma");
        picker.set_items(vec![PickerItem::new("main.rs"), PickerItem::new("Makefile")]);
        assert_eq!(picker.selected(), Some(0));
        assert_eq!(act(&mut picker, "editor::Backspace"), Some(PickerEvent::QueryChanged));
        assert_eq!(act(&mut picker, "editor::MoveLeft"), Some(PickerEvent::Handled));
        assert_eq!(act(&mut picker, "editor::MoveUp"), None, "not a picker action");
        assert_eq!(act(&mut picker, "workspace::Save"), None);
    }

    #[test]
    fn refresh_keeps_the_selection() {
        let mut picker = picker_with(&["a", "b", "c"]);
        act(&mut picker, "menu::SelectLast");
        picker.refresh_items(vec![PickerItem::new("a"), PickerItem::new("b")]);
        assert_eq!(picker.selected(), Some(1));
    }

    #[test]
    fn copy_and_paste_in_the_query() {
        let mut picker = picker_with(&[]);
        picker.insert("hello");
        act(&mut picker, "editor::SelectAll");
        assert_eq!(act(&mut picker, "editor::Copy"), Some(PickerEvent::Copy));
        assert_eq!(picker.take_copied(), "hello");
        assert_eq!(act(&mut picker, "editor::Cut"), Some(PickerEvent::QueryChanged));
        assert_eq!(picker.take_copied(), "hello");
        assert_eq!(act(&mut picker, "editor::Paste"), Some(PickerEvent::WantsPaste));
    }

    #[test]
    fn draws_query_rule_and_rows() {
        let mut picker = picker_with(&["main.rs", "lib.rs"]);
        picker.insert("rs");
        let (rows, cursor, _) = draw(&mut picker, 60, 20);
        // The panel starts an eighth of the way down and has a two-cell inner margin.
        assert_eq!(rows[2].trim_start(), "rs");
        assert!(rows[3].contains("────"));
        assert_eq!(rows[4].trim_start(), "main.rs");
        assert_eq!(rows[5].trim_start(), "lib.rs");
        assert_eq!(cursor.1, 2);
        let query_col = rows[2].find("rs").unwrap() as u16;
        assert_eq!(cursor.0, query_col + 2, "the cursor sits after the query");
        assert_eq!(rows[4].find("main.rs").unwrap() as u16, query_col, "rows line up with the query");
    }

    #[test]
    fn placeholder_and_empty_message() {
        let mut picker = picker_with(&[]);
        let (rows, _, _) = draw(&mut picker, 60, 20);
        assert_eq!(rows[2].trim_start(), "Search files");
        assert_eq!(rows[4].trim_start(), "No matches");
    }

    #[test]
    fn the_selected_row_is_highlighted_across_the_whole_panel() {
        let mut picker = picker_with(&["one", "two"]);
        act(&mut picker, "menu::SelectNext");
        let (rows, _, buf) = draw(&mut picker, 60, 20);
        let theme = Theme::builtin();
        let selected_bg = ui::color(theme.element_selected);
        let panel_bg = ui::color(theme.elevated_surface);
        let col = rows[5].find("two").unwrap() as u16;
        assert_eq!(buf[(col, 5)].bg, selected_bg);
        assert_eq!(buf[(col + 30, 5)].bg, selected_bg, "the highlight runs past the text");
        assert_eq!(buf[(col, 4)].bg, panel_bg);
    }

    #[test]
    fn long_lists_scroll_with_the_selection() {
        let labels: Vec<String> = (0..40).map(|i| format!("item {i}")).collect();
        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let mut picker = picker_with(&refs);
        for _ in 0..20 {
            act(&mut picker, "menu::SelectNext");
        }
        let (rows, _, _) = draw(&mut picker, 60, 30);
        let shown: Vec<&String> = rows.iter().filter(|r| r.contains("item ")).collect();
        assert_eq!(shown.len(), MAX_ROWS as usize);
        assert!(shown.last().unwrap().contains("item 20"), "the selection is the last visible row");
        act(&mut picker, "menu::SelectFirst");
        let (rows, _, _) = draw(&mut picker, 60, 30);
        assert!(rows.iter().any(|r| r.trim_start().starts_with("item 0")));
    }

    #[test]
    fn hints_details_icons_and_matches() {
        let mut picker = Picker::new("");
        picker.set_items(vec![PickerItem::new("Save File")
            .matched(vec![0, 5])
            .detail("workspace")
            .hint("⌘S")
            .icon("*", Rgb::new(1, 2, 3))]);
        let (rows, _, buf) = draw(&mut picker, 70, 12);
        let row = &rows[3];
        assert!(row.contains("* Save File"), "icon, a gap, then the label: {row:?}");
        assert!(row.contains("workspace"));
        assert!(row.trim_end().ends_with("⌘S"), "the hint is right-aligned: {row:?}");
        let theme = Theme::builtin();
        let label_col = (row.chars().position(|c| c == 'S').unwrap()) as u16;
        assert_eq!(buf[(label_col, 3)].fg, ui::color(theme.text_accent), "matched characters are emphasised");
        assert_eq!(buf[(label_col + 1, 3)].fg, ui::color(theme.text));
    }

    #[test]
    fn clicks_select_confirm_and_dismiss() {
        let mut picker = picker_with(&["one", "two", "three"]);
        let (rows, _, _) = draw(&mut picker, 60, 20);
        let col = rows[5].find("two").unwrap() as u16;
        assert_eq!(picker.click(col, 5), PickerEvent::Confirmed { secondary: false });
        assert_eq!(picker.selected(), Some(1));
        assert_eq!(picker.click(col, 2), PickerEvent::Handled, "a click in the query box does nothing");
        assert_eq!(picker.click(0, 19), PickerEvent::Cancelled, "a click outside dismisses");
    }

    #[test]
    fn tiny_screens_do_not_panic() {
        let mut picker = picker_with(&["one", "two"]);
        for (w, h) in [(1, 1), (3, 2), (10, 3), (20, 4), (200, 2)] {
            draw(&mut picker, w, h);
        }
    }
}
