//! Rearranging the focused pane within its tab: a port of the `herdr-move` script.

use serde::Deserialize;
use serde_json::json;

use crate::client::{Client, HerdrError};
use crate::layout::{self, Node, TabLayout};
use crate::types::{Direction, SplitDirection};

/// The label of the short-lived tab a pane is parked in while its split is turned.
const PARKING_TAB_LABEL: &str = "herdr-move";

/// What [`move_pane`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoveOutcome {
    /// The pane changed places with its neighbour on that side.
    Swapped {
        /// The pane it changed places with.
        with_pane_id: String,
    },
    /// The pane had no neighbour on that side and its split ran the other way, so the split
    /// was turned: the pane now sits on that side of what was its sibling.
    Reoriented {
        /// The pane it now sits beside.
        beside_pane_id: String,
    },
    /// Nothing moved: the tab has one pane, the pane is already at that edge of its split,
    /// or the move is one the script does not do (left or up past a sibling that is itself
    /// split).
    Unchanged,
}

#[derive(Deserialize)]
struct Neighbor {
    #[serde(default)]
    neighbor_pane_id: Option<String>,
}

#[derive(Deserialize)]
struct Swap {
    #[serde(default)]
    changed: bool,
}

#[derive(Deserialize)]
struct Move {
    #[serde(default)]
    changed: bool,
    pane: MovedPane,
}

#[derive(Deserialize)]
struct MovedPane {
    pane_id: String,
}

/// Move the focused pane one place over in `direction`, keeping the focus on it.
///
/// With a neighbour on that side the two panes swap places, so a pane can be walked along a
/// row or column. Without one, a pane whose split runs the other way is put on that side of
/// its sibling: of two stacked panes, moving the lower one right makes them sit side by
/// side with it on the right. herdr cannot re-split within a tab, so that is done by parking
/// one pane in a temporary tab named `herdr-move` and moving it straight back; its terminal
/// keeps running throughout. A zoomed tab is unzoomed first.
///
/// An empty session is [`MoveOutcome::Unchanged`]. If the move back fails the pane is put
/// back below its sibling so it is not left in the temporary tab, and the original error is
/// returned.
pub fn move_pane(client: &Client, direction: Direction) -> Result<MoveOutcome, HerdrError> {
    let layout = match layout::export(client, None) {
        Ok(layout) => layout,
        Err(err) if layout::nothing_focused(&err) => return Ok(MoveOutcome::Unchanged),
        Err(err) => return Err(err),
    };
    let pane = layout.focused_pane_id.as_str();
    if layout.zoomed {
        client.call("pane.zoom", json!({ "pane_id": pane, "mode": "off" }))?;
    }

    let neighbor: Neighbor = client.call_for(
        "pane.neighbor",
        json!({ "pane_id": pane, "direction": direction }),
        "neighbor",
    )?;
    if let Some(with) = neighbor.neighbor_pane_id.filter(|id| !id.is_empty()) {
        let swap: Swap = client.call_for(
            "pane.swap",
            json!({ "pane_id": pane, "direction": direction }),
            "swap",
        )?;
        return Ok(if swap.changed {
            MoveOutcome::Swapped { with_pane_id: with }
        } else {
            MoveOutcome::Unchanged
        });
    }

    match plan_turn(&layout.root, pane, direction) {
        Some(turn) => turn_split(client, &layout, &turn),
        None => Ok(MoveOutcome::Unchanged),
    }
}

/// Turning a split: `mover` leaves the tab and comes back on the `split` side of `target`.
#[derive(Debug, PartialEq, Eq)]
struct Turn<'a> {
    mover: &'a str,
    target: &'a str,
    split: SplitDirection,
    /// The pane the user's pane ends up beside.
    beside: &'a str,
}

/// How to put `pane` on the `direction` side of its sibling, or `None` when that is not a
/// move the script makes.
fn plan_turn<'a>(root: &'a Node, pane: &'a str, direction: Direction) -> Option<Turn<'a>> {
    let path = root.find_path(pane)?;
    let (side, parent_path) = path.split_last()?;
    let parent = root.split_at(parent_path)?;
    let split = SplitDirection::along(direction);
    if parent.direction == split {
        // The pane is already at the edge along this axis.
        return None;
    }
    let sibling = parent.child(!side);
    // Past a sibling that is itself split the pane can only go to the far side of the
    // sibling's first pane; the script does not move it left or up.
    if sibling.as_split().is_some() && !direction.is_forward() {
        return None;
    }
    let sibling_pane = sibling.first_leaf();
    Some(if direction.is_forward() {
        Turn {
            mover: pane,
            target: sibling_pane,
            split,
            beside: sibling_pane,
        }
    } else {
        // To end up before the sibling, send the sibling to the far side of the pane.
        Turn {
            mover: sibling_pane,
            target: pane,
            split,
            beside: sibling_pane,
        }
    })
}

fn turn_split(
    client: &Client,
    layout: &TabLayout,
    turn: &Turn<'_>,
) -> Result<MoveOutcome, HerdrError> {
    // herdr will not move a pane within its own tab, so it goes out and comes back.
    let parking = json!({ "type": "new_tab", "workspace_id": layout.workspace_id, "label": PARKING_TAB_LABEL });
    let parked: Move = client.call_for(
        "pane.move",
        json!({ "pane_id": turn.mover, "destination": parking }),
        "move_result",
    )?;
    let back = |split: SplitDirection| {
        let destination = json!({
            "type": "tab",
            "tab_id": layout.tab_id,
            "split": split,
            "target_pane_id": turn.target,
            "ratio": 0.5,
        });
        client.call_for::<Move>(
            "pane.move",
            json!({ "pane_id": parked.pane.pane_id, "destination": destination }),
            "move_result",
        )
    };
    let returned = match back(turn.split) {
        Ok(returned) => returned,
        Err(err) => {
            // Do not leave the pane stranded in the temporary tab.
            let _ = back(SplitDirection::Down);
            return Err(err);
        }
    };
    layout::focus_pane(client, &layout.focused_pane_id)?;
    Ok(if parked.changed && returned.changed {
        MoveOutcome::Reoriented {
            beside_pane_id: turn.beside.to_string(),
        }
    } else {
        MoveOutcome::Unchanged
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::testing::{down, export_result, pane, right};
    use crate::mock::MockServer;
    use serde_json::Value;

    fn tree(value: Value) -> Node {
        serde_json::from_value(value).unwrap()
    }

    fn neighbor(found: Option<&str>) -> Value {
        match found {
            Some(found) => {
                json!({"type": "pane_neighbor", "neighbor": {"pane_id": "x", "neighbor_pane_id": found}})
            }
            None => json!({"type": "pane_neighbor", "neighbor": {"pane_id": "x"}}),
        }
    }

    fn moved(pane: &str, changed: bool) -> Value {
        json!({"type": "pane_move", "move_result": {"changed": changed, "pane": {"pane_id": pane, "tab_id": "w1:t9"}}})
    }

    #[test]
    fn swaps_with_the_neighbour_when_there_is_one() {
        let server = MockServer::start();
        server
            .ok("layout.export", export_result("a", false, right(0.5, pane("a"), pane("b"))))
            .ok("pane.neighbor", neighbor(Some("b")))
            .ok("pane.swap", json!({"type": "pane_swap", "swap": {"changed": true, "source_pane_id": "a", "target_pane_id": "b"}}));
        let outcome = move_pane(&server.client(), Direction::Right).unwrap();
        assert_eq!(
            outcome,
            MoveOutcome::Swapped {
                with_pane_id: "b".into()
            }
        );
        let requests = server.requests();
        assert_eq!(requests[0].params, json!({}));
        assert_eq!(
            requests[1].params,
            json!({"pane_id": "a", "direction": "right"})
        );
        assert_eq!(
            requests[2].params,
            json!({"pane_id": "a", "direction": "right"})
        );
        server.assert_finished();
    }

    #[test]
    fn a_swap_herdr_did_not_make_is_unchanged() {
        let server = MockServer::start();
        server
            .ok(
                "layout.export",
                export_result("a", false, right(0.5, pane("a"), pane("b"))),
            )
            .ok("pane.neighbor", neighbor(Some("b")))
            .ok(
                "pane.swap",
                json!({"swap": {"changed": false, "reason": "no_neighbor", "source_pane_id": "a"}}),
            );
        assert_eq!(
            move_pane(&server.client(), Direction::Right).unwrap(),
            MoveOutcome::Unchanged
        );
    }

    #[test]
    fn a_zoomed_tab_is_unzoomed_first() {
        let server = MockServer::start();
        server
            .ok(
                "layout.export",
                export_result("a", true, right(0.5, pane("a"), pane("b"))),
            )
            .ok(
                "pane.zoom",
                json!({"type": "pane_zoom", "zoom": {"changed": true, "zoomed": false}}),
            )
            .ok("pane.neighbor", neighbor(Some("b")))
            .ok("pane.swap", json!({"swap": {"changed": true}}));
        move_pane(&server.client(), Direction::Right).unwrap();
        assert_eq!(
            server.requests()[1].params,
            json!({"pane_id": "a", "mode": "off"})
        );
        server.assert_finished();
    }

    #[test]
    fn a_single_pane_goes_nowhere() {
        let server = MockServer::start();
        server
            .ok("layout.export", export_result("a", false, pane("a")))
            .ok("pane.neighbor", neighbor(None));
        assert_eq!(
            move_pane(&server.client(), Direction::Left).unwrap(),
            MoveOutcome::Unchanged
        );
        server.assert_finished();
    }

    #[test]
    fn a_pane_at_the_edge_of_its_own_axis_stays() {
        // a | b, moving a left: no neighbour, and the split already runs left to right.
        let server = MockServer::start();
        server
            .ok(
                "layout.export",
                export_result("a", false, right(0.5, pane("a"), pane("b"))),
            )
            .ok("pane.neighbor", neighbor(None));
        assert_eq!(
            move_pane(&server.client(), Direction::Left).unwrap(),
            MoveOutcome::Unchanged
        );
        server.assert_finished();
    }

    #[test]
    fn moving_right_out_of_a_stack_puts_the_pane_beside_its_sibling() {
        // a over b, b focused, move right: b is parked and comes back to the right of a.
        let server = MockServer::start();
        server
            .ok(
                "layout.export",
                export_result("b", false, down(0.5, pane("a"), pane("b"))),
            )
            .ok("pane.neighbor", neighbor(None))
            .ok("pane.move", moved("b", true))
            .ok("pane.move", moved("b", true))
            .ok("pane.focus", json!({"type": "pane_info"}));
        let outcome = move_pane(&server.client(), Direction::Right).unwrap();
        assert_eq!(
            outcome,
            MoveOutcome::Reoriented {
                beside_pane_id: "a".into()
            }
        );
        let requests = server.requests();
        assert_eq!(
            requests[2].params,
            json!({"pane_id": "b", "destination": {"type": "new_tab", "workspace_id": "w1", "label": "herdr-move"}})
        );
        assert_eq!(
            requests[3].params,
            json!({"pane_id": "b", "destination": {"type": "tab", "tab_id": "w1:t1", "split": "right", "target_pane_id": "a", "ratio": 0.5}})
        );
        assert_eq!(requests[4].params, json!({"pane_id": "b"}));
        server.assert_finished();
    }

    #[test]
    fn moving_left_out_of_a_stack_sends_the_sibling_to_the_far_side() {
        // a over b, b focused, move left: a is parked and comes back to the right of b.
        let server = MockServer::start();
        server
            .ok(
                "layout.export",
                export_result("b", false, down(0.5, pane("a"), pane("b"))),
            )
            .ok("pane.neighbor", neighbor(None))
            .ok("pane.move", moved("a", true))
            .ok("pane.move", moved("a", true))
            .ok("pane.focus", json!({"type": "pane_info"}));
        let outcome = move_pane(&server.client(), Direction::Left).unwrap();
        assert_eq!(
            outcome,
            MoveOutcome::Reoriented {
                beside_pane_id: "a".into()
            }
        );
        let requests = server.requests();
        assert_eq!(requests[2].params["pane_id"], "a");
        assert_eq!(
            requests[3].params["destination"],
            json!({"type": "tab", "tab_id": "w1:t1", "split": "right", "target_pane_id": "b", "ratio": 0.5})
        );
        // The focus goes back to the pane the user was moving.
        assert_eq!(requests[4].params, json!({"pane_id": "b"}));
    }

    #[test]
    fn moving_down_out_of_a_row_stacks_the_pane_under_its_sibling() {
        let server = MockServer::start();
        server
            .ok(
                "layout.export",
                export_result("a", false, right(0.5, pane("a"), pane("b"))),
            )
            .ok("pane.neighbor", neighbor(None))
            .ok("pane.move", moved("a", true))
            .ok("pane.move", moved("a", true))
            .ok("pane.focus", json!({"type": "pane_info"}));
        let outcome = move_pane(&server.client(), Direction::Down).unwrap();
        assert_eq!(
            outcome,
            MoveOutcome::Reoriented {
                beside_pane_id: "b".into()
            }
        );
        assert_eq!(
            server.requests()[3].params["destination"],
            json!({"type": "tab", "tab_id": "w1:t1", "split": "down", "target_pane_id": "b", "ratio": 0.5})
        );
    }

    #[test]
    fn the_parked_pane_is_addressed_by_the_id_herdr_gave_it() {
        let server = MockServer::start();
        server
            .ok(
                "layout.export",
                export_result("b", false, down(0.5, pane("a"), pane("b"))),
            )
            .ok("pane.neighbor", neighbor(None))
            .ok("pane.move", moved("renamed", true))
            .ok("pane.move", moved("renamed", true))
            .ok("pane.focus", json!({"type": "pane_info"}));
        move_pane(&server.client(), Direction::Right).unwrap();
        assert_eq!(server.requests()[3].params["pane_id"], "renamed");
    }

    #[test]
    fn a_failed_return_puts_the_pane_back_below_and_reports_the_failure() {
        let server = MockServer::start();
        server
            .ok(
                "layout.export",
                export_result("b", false, down(0.5, pane("a"), pane("b"))),
            )
            .ok("pane.neighbor", neighbor(None))
            .ok("pane.move", moved("b", true))
            .fail("pane.move", "pane_move_failed", "no room")
            .ok("pane.move", moved("b", true));
        let err = move_pane(&server.client(), Direction::Right).unwrap_err();
        assert_eq!(err.code(), Some("pane_move_failed"));
        assert_eq!(
            server.requests()[4].params["destination"],
            json!({"type": "tab", "tab_id": "w1:t1", "split": "down", "target_pane_id": "a", "ratio": 0.5})
        );
        server.assert_finished();
    }

    #[test]
    fn a_move_herdr_did_not_make_is_unchanged() {
        let server = MockServer::start();
        server
            .ok(
                "layout.export",
                export_result("b", false, down(0.5, pane("a"), pane("b"))),
            )
            .ok("pane.neighbor", neighbor(None))
            .ok("pane.move", moved("b", false))
            .ok("pane.move", moved("b", false))
            .ok("pane.focus", json!({"type": "pane_info"}));
        assert_eq!(
            move_pane(&server.client(), Direction::Right).unwrap(),
            MoveOutcome::Unchanged
        );
    }

    #[test]
    fn an_empty_session_is_unchanged() {
        let server = MockServer::start();
        server.fail(
            "layout.export",
            "layout_not_found",
            "layout target not found",
        );
        assert_eq!(
            move_pane(&server.client(), Direction::Up).unwrap(),
            MoveOutcome::Unchanged
        );
    }

    #[test]
    fn errors_are_passed_on() {
        let server = MockServer::start();
        server
            .ok(
                "layout.export",
                export_result("a", false, right(0.5, pane("a"), pane("b"))),
            )
            .fail("pane.neighbor", "pane_not_found", "pane not found");
        assert_eq!(
            move_pane(&server.client(), Direction::Up)
                .unwrap_err()
                .code(),
            Some("pane_not_found")
        );

        let server = MockServer::start();
        server
            .ok(
                "layout.export",
                export_result("b", false, down(0.5, pane("a"), pane("b"))),
            )
            .ok("pane.neighbor", neighbor(None))
            .fail("pane.move", "internal", "boom");
        assert_eq!(
            move_pane(&server.client(), Direction::Right)
                .unwrap_err()
                .code(),
            Some("internal")
        );
        assert_eq!(
            server.requests().len(),
            3,
            "nothing to put back when the pane never left"
        );
    }

    #[test]
    fn plan_turn_covers_the_four_directions_of_a_two_pane_tab() {
        let stacked = tree(down(0.5, pane("a"), pane("b")));
        let right_split = SplitDirection::Right;
        assert_eq!(
            plan_turn(&stacked, "a", Direction::Right),
            Some(Turn {
                mover: "a",
                target: "b",
                split: right_split,
                beside: "b"
            })
        );
        assert_eq!(
            plan_turn(&stacked, "a", Direction::Left),
            Some(Turn {
                mover: "b",
                target: "a",
                split: right_split,
                beside: "b"
            })
        );
        assert_eq!(plan_turn(&stacked, "a", Direction::Up), None);
        assert_eq!(plan_turn(&stacked, "a", Direction::Down), None);

        let row = tree(right(0.5, pane("a"), pane("b")));
        let down_split = SplitDirection::Down;
        assert_eq!(
            plan_turn(&row, "b", Direction::Down),
            Some(Turn {
                mover: "b",
                target: "a",
                split: down_split,
                beside: "a"
            })
        );
        assert_eq!(
            plan_turn(&row, "b", Direction::Up),
            Some(Turn {
                mover: "a",
                target: "b",
                split: down_split,
                beside: "a"
            })
        );
        assert_eq!(plan_turn(&row, "b", Direction::Left), None);
        assert_eq!(plan_turn(&row, "b", Direction::Right), None);
    }

    #[test]
    fn plan_turn_goes_beside_the_first_pane_of_a_split_sibling() {
        // (a | b) over c: c can go right, landing beside a; it cannot go left.
        let root = tree(down(0.5, right(0.5, pane("a"), pane("b")), pane("c")));
        assert_eq!(
            plan_turn(&root, "c", Direction::Right),
            Some(Turn {
                mover: "c",
                target: "a",
                split: SplitDirection::Right,
                beside: "a"
            })
        );
        assert_eq!(plan_turn(&root, "c", Direction::Left), None);
        // a's parent is the row, which already runs left to right.
        assert_eq!(plan_turn(&root, "a", Direction::Left), None);
        // a can leave the row downwards, landing under b.
        assert_eq!(
            plan_turn(&root, "a", Direction::Down),
            Some(Turn {
                mover: "a",
                target: "b",
                split: SplitDirection::Down,
                beside: "b"
            })
        );
        assert_eq!(
            plan_turn(&root, "a", Direction::Up),
            Some(Turn {
                mover: "b",
                target: "a",
                split: SplitDirection::Down,
                beside: "b"
            })
        );
    }

    #[test]
    fn plan_turn_needs_a_pane_with_a_parent() {
        assert_eq!(plan_turn(&tree(pane("a")), "a", Direction::Right), None);
        assert_eq!(
            plan_turn(
                &tree(right(0.5, pane("a"), pane("b"))),
                "zz",
                Direction::Down
            ),
            None
        );
    }
}
