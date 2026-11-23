//! Read-only questions: which pane is focused, and which pane is under a screen cell.
//!
//! Nothing here changes herdr, so all of it is safe to call on the user's live session.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::client::{Client, HerdrError};
use crate::layout::nothing_focused;
use crate::types::{PaneInfo, Rect};

/// The size, in cells, of the terminal the herdr client is drawn in: the size of the pty the
/// IDE gave it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScreenSize {
    /// Columns.
    pub cols: u16,
    /// Rows.
    pub rows: u16,
}

/// Where one pane is drawn.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PaneRect {
    /// The pane's id.
    pub pane_id: String,
    /// Whether it is the focused pane of its tab.
    #[serde(default)]
    pub focused: bool,
    /// The cells it covers, borders included.
    pub rect: Rect,
}

/// Where the panes of the tab on screen are drawn.
///
/// The coordinates are herdr's: relative to the top-left corner of the *pane area*, the part
/// of the client's screen left over once the sidebar and the tab bar are taken away. Use
/// [`PaneLayout::origin_on`] to place the pane area on the screen.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PaneLayout {
    /// The workspace on screen.
    pub workspace_id: String,
    /// The tab on screen.
    pub tab_id: String,
    /// Whether the focused pane is zoomed. `panes` holds the unzoomed rectangles either way;
    /// on screen a zoomed pane covers the whole area.
    #[serde(default)]
    pub zoomed: bool,
    /// The pane area. herdr reports its corner as `(0, 0)`.
    pub area: Rect,
    /// The focused pane of the tab.
    pub focused_pane_id: String,
    /// One rectangle per pane.
    pub panes: Vec<PaneRect>,
}

impl PaneLayout {
    /// The pane drawn at cell `(col, row)` of the pane area, or `None` outside it. In a
    /// zoomed tab that is the focused pane, with the whole area as its rectangle.
    pub fn pane_at(&self, col: u16, row: u16) -> Option<PaneRect> {
        if !self.area.contains(col, row) {
            return None;
        }
        if self.zoomed {
            return Some(PaneRect {
                pane_id: self.focused_pane_id.clone(),
                focused: true,
                rect: self.area,
            });
        }
        self.panes
            .iter()
            .find(|pane| pane.rect.contains(col, row))
            .cloned()
    }

    /// The screen cell where the pane area starts, for a herdr client drawn on a `screen`
    /// sized terminal.
    ///
    /// herdr puts its sidebar on the left and its tab bar on top, and the pane area takes
    /// the rest, so the area starts as far in as the screen is larger than the area. That
    /// holds for the wide layout, the collapsed sidebar and the narrow "mobile" layout. It
    /// does not hold with `tab_bar_position = "bottom"` in herdr's config: there the row is 0
    /// and the caller has to say so itself.
    ///
    /// `None` when the area is larger than the screen, which means the layout belongs to
    /// some other client (or to none: a headless server lays out for 120x40).
    pub fn origin_on(&self, screen: ScreenSize) -> Option<(u16, u16)> {
        let left = screen
            .cols
            .checked_sub(self.area.x.saturating_add(self.area.width))?;
        let top = screen
            .rows
            .checked_sub(self.area.y.saturating_add(self.area.height))?;
        Some((left, top))
    }
}

/// The pane under a screen cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneAt {
    /// The pane, with its working directories.
    pub pane: PaneInfo,
    /// The screen cells the pane covers, borders included.
    pub rect: Rect,
}

/// The pane herdr sends keys to.
///
/// Fails with a [`HerdrError::Server`] whose code is `pane_not_found` when the session has
/// no pane at all.
pub fn focused_pane(client: &Client) -> Result<PaneInfo, HerdrError> {
    client.call_for("pane.current", Value::Null, "pane")
}

/// Pane `pane_id`, wherever it is. An unknown id is a [`HerdrError::Server`] with code
/// `pane_not_found`.
pub fn pane_info(client: &Client, pane_id: &str) -> Result<PaneInfo, HerdrError> {
    client.call_for("pane.get", json!({ "pane_id": pane_id }), "pane")
}

/// Where the panes of the tab on screen are drawn, in herdr's pane-area coordinates.
///
/// Fails with a [`HerdrError::Server`] whose code is `pane_not_found` when the session has
/// no pane at all.
pub fn pane_layout(client: &Client) -> Result<PaneLayout, HerdrError> {
    client.call_for("pane.layout", Value::Null, "layout")
}

/// The herdr pane drawn at screen cell `(col, row)` of a herdr client shown on a `screen`
/// sized terminal, with its working directories, so that a relative path clicked there can
/// be resolved against the right directory ([`PaneInfo::working_dir`]).
///
/// `None` when the cell is on herdr's own chrome (sidebar, tab bar) or outside the screen,
/// when the session has no pane, or when herdr's layout does not fit `screen`, which means
/// it was laid out for a different client and cells cannot be told apart reliably. See
/// [`PaneLayout::origin_on`] for how the pane area is placed and when that goes wrong.
pub fn pane_at(
    client: &Client,
    screen: ScreenSize,
    col: u16,
    row: u16,
) -> Result<Option<PaneAt>, HerdrError> {
    if col >= screen.cols || row >= screen.rows {
        return Ok(None);
    }
    let layout = match pane_layout(client) {
        Ok(layout) => layout,
        Err(err) if nothing_focused(&err) => return Ok(None),
        Err(err) => return Err(err),
    };
    let Some((left, top)) = layout.origin_on(screen) else {
        return Ok(None);
    };
    let (Some(area_col), Some(area_row)) = (col.checked_sub(left), row.checked_sub(top)) else {
        return Ok(None);
    };
    let Some(hit) = layout.pane_at(area_col, area_row) else {
        return Ok(None);
    };
    let pane = pane_info(client, &hit.pane_id)?;
    Ok(Some(PaneAt {
        pane,
        rect: hit.rect.translated(left, top),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockServer;
    use std::path::Path;

    /// The layout herdr reports for two side-by-side panes with a client on a 100x30
    /// terminal: the sidebar takes 26 columns and the tab bar one row.
    fn two_panes(zoomed: bool, focused: &str) -> Value {
        json!({"type": "pane_layout", "layout": {
            "workspace_id": "w1", "tab_id": "w1:t1", "zoomed": zoomed,
            "area": {"x": 0, "y": 0, "width": 74, "height": 29},
            "focused_pane_id": focused,
            "panes": [
                {"pane_id": "w1:p1", "focused": focused == "w1:p1", "rect": {"x": 0, "y": 0, "width": 37, "height": 29}},
                {"pane_id": "w1:p2", "focused": focused == "w1:p2", "rect": {"x": 37, "y": 0, "width": 37, "height": 29}},
            ],
            "splits": [{"id": "split_0_root", "direction": "right", "ratio": 0.5, "rect": {"x": 0, "y": 0, "width": 74, "height": 29}}],
        }})
    }

    fn pane_reply(id: &str, cwd: &str) -> Value {
        json!({"type": "pane_info", "pane": {"pane_id": id, "tab_id": "w1:t1", "workspace_id": "w1", "cwd": cwd, "foreground_cwd": cwd}})
    }

    const SCREEN: ScreenSize = ScreenSize {
        cols: 100,
        rows: 30,
    };

    fn layout(value: Value) -> PaneLayout {
        serde_json::from_value(value["layout"].clone()).unwrap()
    }

    #[test]
    fn focused_pane_asks_for_the_current_pane() {
        let server = MockServer::start();
        server.ok("pane.current", json!({"type": "pane_current", "pane": {
            "pane_id": "w2:p1", "tab_id": "w2:t1", "workspace_id": "w2", "focused": true, "cwd": "/work/b",
        }}));
        let pane = focused_pane(&server.client()).unwrap();
        assert_eq!(pane.pane_id, "w2:p1");
        assert_eq!(pane.cwd.as_deref(), Some(Path::new("/work/b")));
        assert_eq!(server.requests()[0].params, json!({}));
    }

    #[test]
    fn focused_pane_in_an_empty_session_is_a_pane_not_found_error() {
        let server = MockServer::start();
        server.fail("pane.current", "pane_not_found", "pane not found");
        assert_eq!(
            focused_pane(&server.client()).unwrap_err().code(),
            Some("pane_not_found")
        );
    }

    #[test]
    fn pane_info_asks_for_one_pane() {
        let server = MockServer::start();
        server.ok("pane.get", pane_reply("w1:p2", "/work/b"));
        assert_eq!(
            pane_info(&server.client(), "w1:p2").unwrap().pane_id,
            "w1:p2"
        );
        assert_eq!(server.requests()[0].params, json!({"pane_id": "w1:p2"}));
    }

    #[test]
    fn queries_pass_the_mutation_guard() {
        let server = MockServer::start();
        server
            .ok("pane.current", pane_reply("w1:p1", "/work/a"))
            .ok("pane.layout", two_panes(false, "w1:p1"))
            .ok("pane.get", pane_reply("w1:p1", "/work/a"));
        let client = server.client().with_protected_socket(server.socket_path());
        focused_pane(&client).unwrap();
        assert!(pane_at(&client, SCREEN, 30, 5).unwrap().is_some());
        server.assert_finished();
    }

    #[test]
    fn the_pane_area_starts_after_the_sidebar_and_the_tab_bar() {
        let layout = layout(two_panes(false, "w1:p1"));
        assert_eq!(layout.origin_on(SCREEN), Some((26, 1)));
        // The narrow layout: no sidebar, two rows of header.
        let mut narrow = layout.clone();
        narrow.area = Rect::new(0, 0, 60, 22);
        assert_eq!(
            narrow.origin_on(ScreenSize { cols: 60, rows: 24 }),
            Some((0, 2))
        );
        // A screen exactly as large as the area: no chrome at all.
        assert_eq!(
            layout.origin_on(ScreenSize { cols: 74, rows: 29 }),
            Some((0, 0))
        );
    }

    #[test]
    fn a_layout_larger_than_the_screen_has_no_origin() {
        let layout = layout(two_panes(false, "w1:p1"));
        assert_eq!(layout.origin_on(ScreenSize { cols: 73, rows: 30 }), None);
        assert_eq!(
            layout.origin_on(ScreenSize {
                cols: 100,
                rows: 28
            }),
            None
        );
        assert_eq!(layout.origin_on(ScreenSize { cols: 0, rows: 0 }), None);
    }

    #[test]
    fn layout_cells_map_to_panes() {
        let layout = layout(two_panes(false, "w1:p1"));
        assert_eq!(layout.pane_at(0, 0).unwrap().pane_id, "w1:p1");
        assert_eq!(layout.pane_at(36, 28).unwrap().pane_id, "w1:p1");
        assert_eq!(layout.pane_at(37, 0).unwrap().pane_id, "w1:p2");
        assert_eq!(layout.pane_at(73, 28).unwrap().pane_id, "w1:p2");
        assert_eq!(layout.pane_at(74, 0), None);
        assert_eq!(layout.pane_at(0, 29), None);
        assert_eq!(
            layout.pane_at(37, 0).unwrap().rect,
            Rect::new(37, 0, 37, 29)
        );
    }

    #[test]
    fn a_zoomed_pane_covers_the_whole_area() {
        let layout = layout(two_panes(true, "w1:p2"));
        let hit = layout.pane_at(3, 3).unwrap();
        assert_eq!(hit.pane_id, "w1:p2");
        assert!(hit.focused);
        assert_eq!(hit.rect, Rect::new(0, 0, 74, 29));
        assert_eq!(layout.pane_at(74, 3), None);
    }

    #[test]
    fn a_gap_between_panes_belongs_to_no_pane() {
        let mut layout = layout(two_panes(false, "w1:p1"));
        layout.panes[1].rect = Rect::new(40, 0, 34, 29);
        assert_eq!(layout.pane_at(38, 4), None);
    }

    #[test]
    fn pane_at_translates_screen_cells_and_fetches_the_pane() {
        let server = MockServer::start();
        server
            .ok("pane.layout", two_panes(false, "w1:p1"))
            .ok("pane.get", pane_reply("w1:p2", "/work/b"));
        // Screen column 63 is area column 37: the first cell of the right pane.
        let hit = pane_at(&server.client(), SCREEN, 63, 10).unwrap().unwrap();
        assert_eq!(hit.pane.pane_id, "w1:p2");
        assert_eq!(hit.pane.working_dir(), Some(Path::new("/work/b")));
        assert_eq!(hit.rect, Rect::new(63, 1, 37, 29));
        let requests = server.requests();
        assert_eq!(requests[0].params, json!({}));
        assert_eq!(requests[1].params, json!({"pane_id": "w1:p2"}));
        server.assert_finished();
    }

    #[test]
    fn pane_at_finds_the_left_pane_up_to_its_last_column() {
        let server = MockServer::start();
        server
            .ok("pane.layout", two_panes(false, "w1:p1"))
            .ok("pane.get", pane_reply("w1:p1", "/work/a"));
        let hit = pane_at(&server.client(), SCREEN, 62, 29).unwrap().unwrap();
        assert_eq!(hit.pane.pane_id, "w1:p1");
        assert_eq!(hit.rect, Rect::new(26, 1, 37, 29));
    }

    #[test]
    fn cells_on_herdrs_chrome_are_no_pane() {
        // (col, row): sidebar, sidebar edge, tab bar, tab bar above the sidebar.
        for (col, row) in [(0, 5), (25, 5), (30, 0), (10, 0)] {
            let server = MockServer::start();
            server.ok("pane.layout", two_panes(false, "w1:p1"));
            assert_eq!(
                pane_at(&server.client(), SCREEN, col, row).unwrap(),
                None,
                "({col}, {row})"
            );
            server.assert_finished();
        }
    }

    #[test]
    fn cells_off_the_screen_are_no_pane_and_cost_no_call() {
        let server = MockServer::start();
        let client = server.client();
        assert_eq!(pane_at(&client, SCREEN, 100, 5).unwrap(), None);
        assert_eq!(pane_at(&client, SCREEN, 5, 30).unwrap(), None);
        assert_eq!(
            pane_at(&client, ScreenSize { cols: 0, rows: 0 }, 0, 0).unwrap(),
            None
        );
        assert!(server.requests().is_empty());
    }

    #[test]
    fn a_layout_for_another_client_is_no_pane() {
        // The server laid out for 120x40 (nobody attached) but the IDE's panel is 80x24.
        let server = MockServer::start();
        let mut reply = two_panes(false, "w1:p1");
        reply["layout"]["area"] = json!({"x": 0, "y": 0, "width": 120, "height": 40});
        server.ok("pane.layout", reply);
        assert_eq!(
            pane_at(&server.client(), ScreenSize { cols: 80, rows: 24 }, 10, 10).unwrap(),
            None
        );
        server.assert_finished();
    }

    #[test]
    fn pane_at_in_a_zoomed_tab_is_the_focused_pane() {
        let server = MockServer::start();
        server
            .ok("pane.layout", two_panes(true, "w1:p2"))
            .ok("pane.get", pane_reply("w1:p2", "/work/b"));
        // The cell is where the left pane would be if the tab were not zoomed.
        let hit = pane_at(&server.client(), SCREEN, 30, 5).unwrap().unwrap();
        assert_eq!(hit.pane.pane_id, "w1:p2");
        assert_eq!(hit.rect, Rect::new(26, 1, 74, 29));
    }

    #[test]
    fn an_empty_session_has_no_pane_anywhere() {
        let server = MockServer::start();
        server.fail("pane.layout", "pane_not_found", "pane not found");
        assert_eq!(pane_at(&server.client(), SCREEN, 30, 5).unwrap(), None);
    }

    #[test]
    fn pane_at_passes_on_other_failures() {
        let server = MockServer::start();
        server.fail(
            "pane.layout",
            "pane_layout_unavailable",
            "pane layout unavailable",
        );
        assert_eq!(
            pane_at(&server.client(), SCREEN, 30, 5).unwrap_err().code(),
            Some("pane_layout_unavailable")
        );

        let server = MockServer::start();
        server.ok("pane.layout", two_panes(false, "w1:p1")).fail(
            "pane.get",
            "pane_not_found",
            "pane w1:p1 not found",
        );
        assert_eq!(
            pane_at(&server.client(), SCREEN, 30, 5).unwrap_err().code(),
            Some("pane_not_found")
        );
    }

    #[test]
    fn pane_layout_reads_the_reply() {
        let server = MockServer::start();
        server.ok("pane.layout", two_panes(false, "w1:p2"));
        let layout = pane_layout(&server.client()).unwrap();
        assert_eq!(layout.tab_id, "w1:t1");
        assert_eq!(layout.workspace_id, "w1");
        assert_eq!(layout.area, Rect::new(0, 0, 74, 29));
        assert_eq!(layout.focused_pane_id, "w1:p2");
        assert_eq!(layout.panes.len(), 2);
        assert!(layout.panes[1].focused && !layout.panes[0].focused);
    }
}
