//! A new pane that spans the whole tab: a port of the `herdr-root-split` script.

use std::path::PathBuf;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::client::{Client, HerdrError};
use crate::layout::{self, Node, TabLayout};
use crate::query::pane_info;
use crate::types::SplitDirection;

/// The share of the tab the new pane gets unless told otherwise.
pub const DEFAULT_ROOT_SPLIT_SHARE: f64 = 0.4;

/// The label of the short-lived tab the other panes wait in.
const PARKING_TAB_LABEL: &str = "herdr-root-split";

#[derive(Deserialize)]
struct Move {
    pane: Moved,
}

#[derive(Deserialize)]
struct Moved {
    tab_id: String,
}

#[derive(Deserialize)]
struct NewPane {
    pane_id: String,
}

/// Add a pane along the whole right side (`Right`) or the whole bottom (`Down`) of the tab
/// on screen, giving it [`DEFAULT_ROOT_SPLIT_SHARE`] of the tab, and focus it. Returns the
/// new pane's id. See [`root_split_with_share`].
pub fn root_split(client: &Client, direction: SplitDirection) -> Result<String, HerdrError> {
    root_split_with_share(client, direction, DEFAULT_ROOT_SPLIT_SHARE)
}

/// [`root_split`] with the new pane taking `share` of the tab. `share` is kept between 0.1
/// and 0.9, and anything that is not a number counts as the default.
///
/// herdr only ever splits a pane, never a tab, so its own split always leaves the new pane
/// sharing an edge with just one other. Here the existing layout is kept exactly as it is
/// and sits beside the new pane: every pane but the first is parked in a temporary tab
/// named `herdr-root-split`, the new pane is split off the one left behind, and the others
/// are moved back into that pane's place in their old arrangement, ratios included. Moving
/// a pane keeps its terminal and whatever runs in it. The new pane starts in the directory
/// of the focused pane's foreground program. A zoomed tab is unzoomed first.
///
/// If anything fails part-way, the panes still parked are moved back into the tab (stacked,
/// not in their old arrangement) so that none is lost, and the error is returned. An empty
/// session is an error too, since there is no tab to split.
pub fn root_split_with_share(
    client: &Client,
    direction: SplitDirection,
    share: f64,
) -> Result<String, HerdrError> {
    let share = if share.is_nan() {
        DEFAULT_ROOT_SPLIT_SHARE
    } else {
        share.clamp(0.1, 0.9)
    };
    let layout = layout::export(client, None)?;
    let focused = layout.focused_pane_id.as_str();
    if layout.zoomed {
        client.call("pane.zoom", json!({ "pane_id": focused, "mode": "off" }))?;
    }
    let cwd: Option<PathBuf> = pane_info(client, focused)?.working_dir().map(PathBuf::from);

    // The first pane stays in the tab; everything else is rebuilt inside its slot.
    let anchor = layout.root.first_leaf();
    let mut job = Job {
        client,
        layout: &layout,
        anchor,
        parked: Vec::new(),
    };
    match job.run(direction, share, cwd) {
        Ok(new_pane) => Ok(new_pane),
        Err(err) => {
            job.unpark_everything();
            Err(err)
        }
    }
}

struct Job<'a> {
    client: &'a Client,
    layout: &'a TabLayout,
    anchor: &'a str,
    /// Panes currently waiting in the temporary tab.
    parked: Vec<&'a str>,
}

impl<'a> Job<'a> {
    fn run(
        &mut self,
        direction: SplitDirection,
        share: f64,
        cwd: Option<PathBuf>,
    ) -> Result<String, HerdrError> {
        self.park_others()?;
        let split = json!({
            "target_pane_id": self.anchor,
            "direction": direction,
            "ratio": 1.0 - share,
            "cwd": cwd,
            "focus": false,
        });
        let new_pane: NewPane = self.client.call_for("pane.split", split, "pane")?;
        let layout = self.layout;
        self.rebuild(&layout.root, self.anchor)?;
        layout::focus_pane(self.client, &new_pane.pane_id)?;
        Ok(new_pane.pane_id)
    }

    fn move_pane(&self, pane_id: &str, destination: Value) -> Result<Move, HerdrError> {
        self.client.call_for(
            "pane.move",
            json!({ "pane_id": pane_id, "destination": destination }),
            "move_result",
        )
    }

    /// Move every pane but the anchor into a temporary tab, the first one creating it.
    fn park_others(&mut self) -> Result<(), HerdrError> {
        let layout = self.layout;
        let others: Vec<&'a str> = layout
            .root
            .leaves()
            .into_iter()
            .filter(|pane| *pane != self.anchor)
            .collect();
        let Some((&first, rest)) = others.split_first() else {
            return Ok(());
        };
        let new_tab = json!({ "type": "new_tab", "workspace_id": layout.workspace_id, "label": PARKING_TAB_LABEL });
        let parking_tab = self.move_pane(first, new_tab)?.pane.tab_id;
        self.parked.push(first);
        for &pane in rest {
            let beside_first = json!({ "type": "tab", "tab_id": parking_tab, "split": "down", "target_pane_id": first, "ratio": 0.5 });
            self.move_pane(pane, beside_first)?;
            self.parked.push(pane);
        }
        Ok(())
    }

    /// Replay the old tree inside the slot of `stand_in`, the one pane of `node` that is
    /// already in place. Every split brings back the first pane of its second child, as the
    /// second child of a new split of the stand-in; that pane then stands in for the second
    /// subtree.
    fn rebuild(&mut self, node: &'a Node, stand_in: &'a str) -> Result<(), HerdrError> {
        let Some(split) = node.as_split() else {
            return Ok(());
        };
        let returning = split.second.first_leaf();
        let destination = json!({
            "type": "tab",
            "tab_id": self.layout.tab_id,
            "split": split.direction,
            "target_pane_id": stand_in,
            "ratio": split.ratio,
        });
        self.move_pane(returning, destination)?;
        self.parked.retain(|pane| *pane != returning);
        self.rebuild(split.first, stand_in)?;
        self.rebuild(split.second, returning)
    }

    /// Bring back whatever is still parked, any way that works.
    fn unpark_everything(&mut self) {
        for pane in std::mem::take(&mut self.parked) {
            let below_anchor = json!({
                "type": "tab",
                "tab_id": self.layout.tab_id,
                "split": "down",
                "target_pane_id": self.anchor,
                "ratio": 0.5,
            });
            if let Err(err) = self.move_pane(pane, below_anchor) {
                log::warn!("could not move pane {pane} back out of the temporary tab: {err}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::testing::{down, export_result, pane, right};
    use crate::mock::MockServer;

    fn pane_reply(id: &str, cwd: Option<&str>, foreground: Option<&str>) -> Value {
        json!({"type": "pane_info", "pane": {
            "pane_id": id, "tab_id": "w1:t1", "workspace_id": "w1", "focused": true,
            "cwd": cwd, "foreground_cwd": foreground,
        }})
    }

    fn moved(tab: &str) -> Value {
        json!({"type": "pane_move", "move_result": {"changed": true, "pane": {"pane_id": "x", "tab_id": tab}}})
    }

    fn new_pane(id: &str) -> Value {
        json!({"type": "pane_info", "pane": {"pane_id": id, "tab_id": "w1:t1", "workspace_id": "w1"}})
    }

    fn move_request(pane: &str, destination: Value) -> (String, Value) {
        (
            "pane.move".to_string(),
            json!({"pane_id": pane, "destination": destination}),
        )
    }

    fn sent(server: &MockServer) -> Vec<(String, Value)> {
        server
            .requests()
            .into_iter()
            .map(|request| (request.method, request.params))
            .collect()
    }

    fn back(split: &str, target: &str, ratio: f64) -> Value {
        json!({"type": "tab", "tab_id": "w1:t1", "split": split, "target_pane_id": target, "ratio": ratio})
    }

    #[test]
    fn a_single_pane_is_simply_split() {
        let server = MockServer::start();
        server
            .ok("layout.export", export_result("a", false, pane("a")))
            .ok("pane.get", pane_reply("a", Some("/work/a"), None))
            .ok("pane.split", new_pane("n"))
            .ok("pane.focus", json!({"type": "pane_info"}));
        let id = root_split(&server.client(), SplitDirection::Right).unwrap();
        assert_eq!(id, "n");
        assert_eq!(
            sent(&server),
            vec![
                ("layout.export".to_string(), json!({})),
                ("pane.get".to_string(), json!({"pane_id": "a"})),
                (
                    "pane.split".to_string(),
                    json!({"target_pane_id": "a", "direction": "right", "ratio": 0.6, "cwd": "/work/a", "focus": false})
                ),
                ("pane.focus".to_string(), json!({"pane_id": "n"})),
            ]
        );
        server.assert_finished();
    }

    #[test]
    fn two_panes_are_parked_split_and_put_back() {
        // a | b with b focused: b waits in the temporary tab while a is split, then returns
        // to the right of a at the old ratio.
        let server = MockServer::start();
        server
            .ok(
                "layout.export",
                export_result("b", false, right(0.3, pane("a"), pane("b"))),
            )
            .ok(
                "pane.get",
                pane_reply("b", Some("/work/b"), Some("/work/b/deep")),
            )
            .ok("pane.move", moved("w1:t9"))
            .ok("pane.split", new_pane("n"))
            .ok("pane.move", moved("w1:t1"))
            .ok("pane.focus", json!({"type": "pane_info"}));
        let id = root_split(&server.client(), SplitDirection::Down).unwrap();
        assert_eq!(id, "n");
        let requests = sent(&server);
        assert_eq!(
            requests[2],
            move_request(
                "b",
                json!({"type": "new_tab", "workspace_id": "w1", "label": "herdr-root-split"})
            )
        );
        assert_eq!(
            requests[3].1,
            json!({"target_pane_id": "a", "direction": "down", "ratio": 0.6, "cwd": "/work/b/deep", "focus": false})
        );
        assert_eq!(requests[4], move_request("b", back("right", "a", 0.3)));
        assert_eq!(requests[5].1, json!({"pane_id": "n"}));
        server.assert_finished();
    }

    #[test]
    fn a_deeper_tree_is_replayed_split_by_split() {
        // ((a / b) | (c / d)): c returns first as the right half, then b under a, then d
        // under c.
        let root = right(
            0.6,
            down(0.4, pane("a"), pane("b")),
            down(0.7, pane("c"), pane("d")),
        );
        let server = MockServer::start();
        server
            .ok("layout.export", export_result("a", false, root))
            .ok("pane.get", pane_reply("a", None, None))
            .ok("pane.move", moved("w1:t9"))
            .ok("pane.move", moved("w1:t9"))
            .ok("pane.move", moved("w1:t9"))
            .ok("pane.split", new_pane("n"))
            .ok("pane.move", moved("w1:t1"))
            .ok("pane.move", moved("w1:t1"))
            .ok("pane.move", moved("w1:t1"))
            .ok("pane.focus", json!({"type": "pane_info"}));
        root_split(&server.client(), SplitDirection::Right).unwrap();
        let requests = sent(&server);
        let parked_beside_b = |pane: &str| {
            move_request(
                pane,
                json!({"type": "tab", "tab_id": "w1:t9", "split": "down", "target_pane_id": "b", "ratio": 0.5}),
            )
        };
        assert_eq!(
            requests[2],
            move_request(
                "b",
                json!({"type": "new_tab", "workspace_id": "w1", "label": "herdr-root-split"})
            )
        );
        assert_eq!(requests[3], parked_beside_b("c"));
        assert_eq!(requests[4], parked_beside_b("d"));
        assert_eq!(requests[5].1["cwd"], Value::Null);
        assert_eq!(requests[6], move_request("c", back("right", "a", 0.6)));
        assert_eq!(requests[7], move_request("b", back("down", "a", 0.4)));
        assert_eq!(requests[8], move_request("d", back("down", "c", 0.7)));
        server.assert_finished();
    }

    #[test]
    fn a_zoomed_tab_is_unzoomed_first() {
        let server = MockServer::start();
        server
            .ok("layout.export", export_result("a", true, pane("a")))
            .ok("pane.zoom", json!({"type": "pane_zoom"}))
            .ok("pane.get", pane_reply("a", Some("/work/a"), None))
            .ok("pane.split", new_pane("n"))
            .ok("pane.focus", json!({"type": "pane_info"}));
        root_split(&server.client(), SplitDirection::Right).unwrap();
        assert_eq!(
            sent(&server)[1],
            (
                "pane.zoom".to_string(),
                json!({"pane_id": "a", "mode": "off"})
            )
        );
    }

    #[test]
    fn the_share_is_kept_within_what_herdr_allows() {
        for (share, ratio) in [
            (0.25, 0.75),
            (0.0, 0.9),
            (-3.0, 0.9),
            (5.0, 0.1),
            (f64::NAN, 0.6),
            (f64::INFINITY, 0.1),
        ] {
            let server = MockServer::start();
            server
                .ok("layout.export", export_result("a", false, pane("a")))
                .ok("pane.get", pane_reply("a", None, None))
                .ok("pane.split", new_pane("n"))
                .ok("pane.focus", json!({"type": "pane_info"}));
            root_split_with_share(&server.client(), SplitDirection::Right, share).unwrap();
            let sent_ratio = sent(&server)[2].1["ratio"].as_f64().unwrap();
            assert!(
                (sent_ratio - ratio).abs() < 1e-9,
                "share {share}: sent {sent_ratio}"
            );
        }
    }

    #[test]
    fn a_failed_split_brings_the_parked_panes_back() {
        let root = right(0.5, pane("a"), down(0.5, pane("b"), pane("c")));
        let server = MockServer::start();
        server
            .ok("layout.export", export_result("a", false, root))
            .ok("pane.get", pane_reply("a", None, None))
            .ok("pane.move", moved("w1:t9"))
            .ok("pane.move", moved("w1:t9"))
            .fail("pane.split", "pane_split_failed", "no room")
            .ok("pane.move", moved("w1:t1"))
            .ok("pane.move", moved("w1:t1"));
        let err = root_split(&server.client(), SplitDirection::Right).unwrap_err();
        assert_eq!(err.code(), Some("pane_split_failed"));
        let requests = sent(&server);
        assert_eq!(requests[5], move_request("b", back("down", "a", 0.5)));
        assert_eq!(requests[6], move_request("c", back("down", "a", 0.5)));
        server.assert_finished();
    }

    #[test]
    fn a_failure_while_putting_panes_back_rescues_only_what_is_still_parked() {
        // b returned, c's move failed: only c and d are still parked.
        let root = right(
            0.5,
            pane("a"),
            right(0.5, pane("b"), down(0.5, pane("c"), pane("d"))),
        );
        let server = MockServer::start();
        server
            .ok("layout.export", export_result("a", false, root))
            .ok("pane.get", pane_reply("a", None, None))
            .ok("pane.move", moved("w1:t9"))
            .ok("pane.move", moved("w1:t9"))
            .ok("pane.move", moved("w1:t9"))
            .ok("pane.split", new_pane("n"))
            .ok("pane.move", moved("w1:t1"))
            .fail("pane.move", "pane_move_failed", "no room")
            .ok("pane.move", moved("w1:t1"))
            .fail("pane.move", "pane_move_failed", "still no room");
        let err = root_split(&server.client(), SplitDirection::Down).unwrap_err();
        assert_eq!(err.code(), Some("pane_move_failed"));
        let requests = sent(&server);
        assert_eq!(requests[6], move_request("b", back("right", "a", 0.5)));
        assert_eq!(requests[7], move_request("c", back("right", "b", 0.5)));
        assert_eq!(requests[8], move_request("c", back("down", "a", 0.5)));
        assert_eq!(requests[9], move_request("d", back("down", "a", 0.5)));
        server.assert_finished();
    }

    #[test]
    fn a_failure_while_parking_brings_back_what_was_parked() {
        let root = right(0.5, pane("a"), down(0.5, pane("b"), pane("c")));
        let server = MockServer::start();
        server
            .ok("layout.export", export_result("a", false, root))
            .ok("pane.get", pane_reply("a", None, None))
            .ok("pane.move", moved("w1:t9"))
            .fail("pane.move", "pane_move_failed", "no room")
            .ok("pane.move", moved("w1:t1"));
        assert!(root_split(&server.client(), SplitDirection::Right).is_err());
        assert_eq!(sent(&server)[4], move_request("b", back("down", "a", 0.5)));
        server.assert_finished();
    }

    #[test]
    fn nothing_is_touched_when_the_first_steps_fail() {
        let server = MockServer::start();
        server.fail(
            "layout.export",
            "layout_not_found",
            "layout target not found",
        );
        assert_eq!(
            root_split(&server.client(), SplitDirection::Right)
                .unwrap_err()
                .code(),
            Some("layout_not_found")
        );

        let server = MockServer::start();
        server
            .ok(
                "layout.export",
                export_result("a", false, right(0.5, pane("a"), pane("b"))),
            )
            .fail("pane.get", "pane_not_found", "pane a not found");
        assert_eq!(
            root_split(&server.client(), SplitDirection::Right)
                .unwrap_err()
                .code(),
            Some("pane_not_found")
        );
        assert_eq!(server.requests().len(), 2);
    }
}
