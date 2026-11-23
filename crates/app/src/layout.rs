//! Where everything goes on screen: the docks, the centre, the dividers between them and
//! the status bar, worked out from the workspace model and the terminal size.

use ratatui::layout::Rect;

use crate::workspace::{DockPosition, PanelId, Workspace, Zoomed};

/// The centre never gets narrower than this while a side dock is showing.
pub const MIN_CENTER_WIDTH: u16 = 24;
/// Side docks at the width they start with (not one the user dragged them to) give way
/// until the centre has this share of the window, in fifths: their default widths suit a
/// wide window and would leave a narrow one an editor too small to read.
const CENTER_SHARE_FIFTHS: u16 = 2;
/// The centre never gets shorter than this while the bottom dock is showing.
pub const MIN_CENTER_HEIGHT: u16 = 4;
/// The narrowest a side dock is drawn.
pub const MIN_DOCK_WIDTH: u16 = 12;
/// The shortest the bottom dock is drawn.
pub const MIN_DOCK_HEIGHT: u16 = 4;
/// Height of the bottom dock when the user has not resized it.
pub const DEFAULT_DOCK_HEIGHT: u16 = 14;

/// A dock as drawn: which panel it shows and where.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DockArea {
    pub position: DockPosition,
    pub panel: PanelId,
    pub rect: Rect,
}

/// The one-cell line between a dock and the centre. It is drawn, and dragged to resize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Divider {
    pub position: DockPosition,
    pub rect: Rect,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Layout {
    pub status: Rect,
    /// The usage strip, the bottom-most row, when it is shown.
    pub usage: Option<Rect>,
    /// `None` while a panel is zoomed over it.
    pub center: Option<Rect>,
    pub docks: Vec<DockArea>,
    pub dividers: Vec<Divider>,
}

impl Layout {
    pub fn dock(&self, position: DockPosition) -> Option<&DockArea> {
        self.docks.iter().find(|d| d.position == position)
    }

    pub fn panel(&self, panel: PanelId) -> Option<Rect> {
        self.docks.iter().find(|d| d.panel == panel).map(|d| d.rect)
    }
}

/// Lay the workspace out on a screen of the given size. `default_width` is the width a side
/// panel takes until the user resizes it.
pub fn compute(ws: &Workspace, screen: Rect, default_width: impl Fn(PanelId) -> u16) -> Layout {
    compute_with(ws, screen, default_width, false)
}

/// [`compute`], optionally keeping the last row for the usage strip (with the status bar
/// just above it). The strip is dropped on screens too short to spare a row.
pub fn compute_with(ws: &Workspace, screen: Rect, default_width: impl Fn(PanelId) -> u16, usage_strip: bool) -> Layout {
    let mut layout = Layout::default();
    if screen.width == 0 || screen.height == 0 {
        return layout;
    }
    let mut screen = screen;
    if usage_strip && screen.height >= 8 {
        layout.usage = Some(Rect::new(screen.x, screen.bottom() - 1, screen.width, 1));
        screen.height -= 1;
    }
    let status_height = 1.min(screen.height);
    layout.status = Rect::new(screen.x, screen.bottom() - status_height, screen.width, status_height);
    let body = Rect::new(screen.x, screen.y, screen.width, screen.height - status_height);
    if body.height == 0 {
        return layout;
    }

    match ws.zoomed() {
        Some(Zoomed::Panel(panel)) => {
            if let Some(position) = ws.position_of(panel) {
                layout.docks.push(DockArea { position, panel, rect: body });
            }
            return layout;
        }
        Some(Zoomed::Center) => {
            layout.center = Some(body);
            return layout;
        }
        None => {}
    }

    let left = ws.visible_panel(DockPosition::Left);
    let right = ws.visible_panel(DockPosition::Right);
    let bottom = ws.visible_panel(DockPosition::Bottom);

    // Side docks: take what they ask for, then give back until the centre has its minimum.
    let wanted = |panel: Option<PanelId>| -> u16 {
        panel.map(|p| ws.panel_size(p).unwrap_or_else(|| default_width(p)).max(MIN_DOCK_WIDTH)).unwrap_or(0)
    };
    let at_default = |panel: Option<PanelId>| panel.is_some_and(|p| ws.panel_size(p).is_none());
    let (mut left_width, mut right_width) = (wanted(left), wanted(right));
    let dividers = |l: u16, r: u16| (l > 0) as u16 + (r > 0) as u16;
    let overflow_for = |l: u16, r: u16, center: u16| (l + r + dividers(l, r) + center).saturating_sub(body.width);
    let overflow = |l: u16, r: u16| overflow_for(l, r, MIN_CENTER_WIDTH);
    // Shrink the docks that may shrink in proportion to what each can give, down to the
    // minimum each, by up to `excess` columns.
    let shrink = |l: &mut u16, r: &mut u16, excess: u16, (left_may, right_may): (bool, bool)| {
        let shrinkable_left = if left_may { l.saturating_sub(MIN_DOCK_WIDTH) } else { 0 };
        let shrinkable_right = if right_may { r.saturating_sub(MIN_DOCK_WIDTH) } else { 0 };
        let shrinkable = shrinkable_left + shrinkable_right;
        if shrinkable > 0 {
            let take = excess.min(shrinkable);
            let from_left = ((take as u32 * shrinkable_left as u32).div_ceil(shrinkable as u32) as u16).min(shrinkable_left);
            let from_right = (take - from_left.min(take)).min(shrinkable_right);
            *l -= from_left;
            *r -= from_right;
        }
    };
    // First the docks at their default width make room for a fair share of the window.
    let share = (body.width / 5 * CENTER_SHARE_FIFTHS).max(MIN_CENTER_WIDTH);
    let fair_excess = overflow_for(left_width, right_width, share);
    if fair_excess > 0 {
        shrink(&mut left_width, &mut right_width, fair_excess, (at_default(left), at_default(right)));
    }
    // Then any dock gives way, down to the centre's absolute minimum.
    let mut excess = overflow(left_width, right_width);
    if excess > 0 {
        shrink(&mut left_width, &mut right_width, excess, (true, true));
        excess = overflow(left_width, right_width);
    }
    // Still too tight: drop the right dock, then the left, rather than squeeze the editor.
    if excess > 0 && right_width > 0 && left_width > 0 {
        right_width = 0;
        excess = overflow(left_width, right_width);
    }
    if excess > 0 {
        let available = body.width.saturating_sub(MIN_CENTER_WIDTH + 1);
        if left_width > 0 {
            left_width = if available >= MIN_DOCK_WIDTH { left_width.min(available) } else { 0 };
        } else if right_width > 0 {
            right_width = if available >= MIN_DOCK_WIDTH { right_width.min(available) } else { 0 };
        }
    }

    let mut x = body.x;
    let mut center_width = body.width;
    if let (Some(panel), true) = (left, left_width > 0) {
        layout.docks.push(DockArea {
            position: DockPosition::Left,
            panel,
            rect: Rect::new(x, body.y, left_width, body.height),
        });
        layout.dividers.push(Divider {
            position: DockPosition::Left,
            rect: Rect::new(x + left_width, body.y, 1, body.height),
        });
        x += left_width + 1;
        center_width -= left_width + 1;
    }
    if let (Some(panel), true) = (right, right_width > 0) {
        let dock_x = body.right() - right_width;
        layout.docks.push(DockArea {
            position: DockPosition::Right,
            panel,
            rect: Rect::new(dock_x, body.y, right_width, body.height),
        });
        layout.dividers.push(Divider {
            position: DockPosition::Right,
            rect: Rect::new(dock_x - 1, body.y, 1, body.height),
        });
        center_width -= right_width + 1;
    }

    // The bottom dock sits under the centre, between the side docks.
    let mut center_height = body.height;
    if let Some(panel) = bottom {
        let wanted = ws.panel_size(panel).unwrap_or(DEFAULT_DOCK_HEIGHT).max(MIN_DOCK_HEIGHT);
        let available = body.height.saturating_sub(MIN_CENTER_HEIGHT + 1);
        if available >= MIN_DOCK_HEIGHT {
            let height = wanted.min(available);
            let dock_y = body.bottom() - height;
            layout.docks.push(DockArea {
                position: DockPosition::Bottom,
                panel,
                rect: Rect::new(x, dock_y, center_width, height),
            });
            layout.dividers.push(Divider {
                position: DockPosition::Bottom,
                rect: Rect::new(x, dock_y - 1, center_width, 1),
            });
            center_height -= height + 1;
        }
    }

    layout.center = Some(Rect::new(x, body.y, center_width, center_height));
    layout
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::PanelId::{Git, Project, Terminal};
    use DockPosition::{Bottom, Left, Right};

    fn workspace() -> Workspace {
        Workspace::new(&[(Terminal, Left), (Git, Left), (Project, Right)])
    }

    fn widths(panel: PanelId) -> u16 {
        match panel {
            Terminal => 85,
            Git => 84,
            Project => 33,
        }
    }

    fn screen(width: u16, height: u16) -> Rect {
        Rect::new(0, 0, width, height)
    }

    #[test]
    fn nothing_open_is_all_centre() {
        let ws = workspace();
        let layout = compute(&ws, screen(200, 50), widths);
        assert_eq!(layout.status, Rect::new(0, 49, 200, 1));
        assert_eq!(layout.center, Some(Rect::new(0, 0, 200, 49)));
        assert!(layout.docks.is_empty());
        assert!(layout.dividers.is_empty());
    }

    #[test]
    fn three_columns() {
        let mut ws = workspace();
        ws.open_panel(Terminal);
        ws.open_panel(Project);
        let layout = compute(&ws, screen(200, 50), widths);
        assert_eq!(layout.panel(Terminal), Some(Rect::new(0, 0, 85, 49)));
        assert_eq!(layout.panel(Project), Some(Rect::new(167, 0, 33, 49)));
        assert_eq!(layout.center, Some(Rect::new(86, 0, 80, 49)));
        assert_eq!(layout.dividers.len(), 2);
        assert_eq!(layout.dividers[0].rect, Rect::new(85, 0, 1, 49));
        assert_eq!(layout.dividers[1].rect, Rect::new(166, 0, 1, 49));
        // Every column is accounted for exactly once.
        assert_eq!(85 + 1 + 80 + 1 + 33, 200);
    }

    #[test]
    fn zoomed_panel_takes_the_whole_body() {
        let mut ws = workspace();
        ws.open_panel(Project);
        ws.focus_panel(Terminal);
        ws.toggle_zoom();
        let layout = compute(&ws, screen(120, 40), widths);
        assert_eq!(layout.center, None);
        assert_eq!(layout.docks.len(), 1);
        assert_eq!(layout.panel(Terminal), Some(Rect::new(0, 0, 120, 39)));
        assert!(layout.dividers.is_empty());
        assert_eq!(layout.status, Rect::new(0, 39, 120, 1));
    }

    #[test]
    fn zoomed_centre_hides_the_docks() {
        let mut ws = workspace();
        ws.open_panel(Terminal);
        ws.toggle_zoom();
        let layout = compute(&ws, screen(120, 40), widths);
        assert_eq!(layout.center, Some(Rect::new(0, 0, 120, 39)));
        assert!(layout.docks.is_empty());
    }

    #[test]
    fn user_sizes_win_over_defaults() {
        let mut ws = workspace();
        ws.open_panel(Terminal);
        ws.set_panel_size(Terminal, Some(40));
        let layout = compute(&ws, screen(200, 50), widths);
        assert_eq!(layout.panel(Terminal).unwrap().width, 40);
        assert_eq!(layout.center.unwrap().width, 159);
    }

    #[test]
    fn docks_at_their_default_width_leave_the_centre_two_fifths() {
        let mut ws = workspace();
        ws.open_panel(Terminal);
        ws.open_panel(Project);
        // 85 + 33 + 2 dividers leave 80 of 200: two fifths already, nothing moves.
        let layout = compute(&ws, screen(200, 40), widths);
        assert_eq!(layout.center.unwrap().width, 80);
        for (width, center_width) in [(140, 56), (120, 48), (100, 40)] {
            let layout = compute(&ws, screen(width, 30), widths);
            let left = layout.panel(Terminal).unwrap();
            let right = layout.panel(Project).unwrap();
            let center = layout.center.unwrap();
            assert_eq!(center.width, center_width, "{width} columns");
            assert!(left.width < 85 && left.width >= MIN_DOCK_WIDTH, "{width}: {left:?}");
            assert!(right.width < 33 && right.width >= MIN_DOCK_WIDTH, "{width}: {right:?}");
            assert!(left.width > right.width, "they give way in proportion");
            assert_eq!(left.width + 1 + center.width + 1 + right.width, width);
            assert_eq!(center.x, left.width + 1);
            assert_eq!(right.x, center.right() + 1);
        }
    }

    #[test]
    fn a_dock_the_user_widened_keeps_its_width_down_to_the_centre_minimum() {
        let mut ws = workspace();
        ws.open_panel(Terminal);
        ws.open_panel(Project);
        ws.set_panel_size(Terminal, Some(90));
        // Only the tree (at its default) gives way for the share; the terminal stays 90.
        let layout = compute(&ws, screen(160, 30), widths);
        assert_eq!(layout.panel(Terminal).unwrap().width, 90);
        assert_eq!(layout.panel(Project).unwrap().width, MIN_DOCK_WIDTH);
        assert_eq!(layout.center.unwrap().width, 160 - 90 - MIN_DOCK_WIDTH - 2);
        // Narrower still: then it gives way too, to keep the centre's absolute minimum.
        let layout = compute(&ws, screen(120, 30), widths);
        let center = layout.center.unwrap();
        assert_eq!(center.width, MIN_CENTER_WIDTH);
        assert!(layout.panel(Terminal).unwrap().width < 90);
    }

    #[test]
    fn a_very_narrow_screen_drops_the_right_dock_then_the_left() {
        let mut ws = workspace();
        ws.open_panel(Terminal);
        ws.open_panel(Project);
        let layout = compute(&ws, screen(48, 30), widths);
        assert!(layout.panel(Project).is_none());
        let left = layout.panel(Terminal).unwrap();
        let center = layout.center.unwrap();
        assert!(center.width >= MIN_CENTER_WIDTH);
        assert_eq!(left.width + 1 + center.width, 48);

        let layout = compute(&ws, screen(30, 30), widths);
        assert!(layout.docks.is_empty());
        assert_eq!(layout.center, Some(Rect::new(0, 0, 30, 29)));
    }

    #[test]
    fn bottom_dock_sits_between_the_side_docks() {
        let mut ws = Workspace::new(&[(Terminal, Bottom), (Git, Left), (Project, Right)]);
        ws.open_panel(Terminal);
        ws.open_panel(Git);
        ws.open_panel(Project);
        let layout = compute(&ws, screen(200, 50), widths);
        let bottom = layout.panel(Terminal).unwrap();
        let center = layout.center.unwrap();
        assert_eq!(bottom.height, DEFAULT_DOCK_HEIGHT);
        assert_eq!(bottom.x, center.x);
        assert_eq!(bottom.width, center.width);
        assert_eq!(center.height + 1 + bottom.height, 49);
        assert_eq!(layout.panel(Git).unwrap().height, 49, "side docks run the full height");
        assert_eq!(layout.dock(Bottom).unwrap().panel, Terminal);
    }

    #[test]
    fn the_usage_strip_takes_the_last_row() {
        let mut ws = workspace();
        ws.open_panel(Terminal);
        let layout = compute_with(&ws, screen(120, 40), widths, true);
        assert_eq!(layout.usage, Some(Rect::new(0, 39, 120, 1)));
        assert_eq!(layout.status, Rect::new(0, 38, 120, 1));
        assert_eq!(layout.panel(Terminal).unwrap().height, 38);
        assert_eq!(compute_with(&ws, screen(120, 40), widths, false).usage, None);
        assert_eq!(compute_with(&ws, screen(120, 6), widths, true).usage, None, "too short to spare a row");
    }

    #[test]
    fn degenerate_screens_do_not_panic() {
        let mut ws = workspace();
        ws.open_panel(Terminal);
        ws.open_panel(Project);
        for (w, h) in [(0, 0), (1, 1), (5, 1), (1, 5), (10, 2), (300, 2)] {
            let layout = compute(&ws, screen(w, h), widths);
            for dock in &layout.docks {
                assert!(dock.rect.right() <= w && dock.rect.bottom() <= h);
            }
            if let Some(center) = layout.center {
                assert!(center.right() <= w && center.bottom() <= h);
            }
        }
    }
}
