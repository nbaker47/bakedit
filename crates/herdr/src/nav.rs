//! Moving the focus between panes: a port of the `herdr-nav` script.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::client::{Client, HerdrError};
use crate::collapse::{collapse, CollapseMode};
use crate::layout::nothing_focused;
use crate::types::Direction;

/// What [`navigate`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavOutcome {
    /// The focus moved to the neighbouring pane in the same tab.
    FocusedPane {
        /// The pane that has the focus now.
        pane_id: String,
    },
    /// There was no pane on that side, so the tab before or after this one was shown.
    SwitchedTab {
        /// The tab that is showing now.
        tab_id: String,
    },
    /// There was no pane above or below, so the workspace before or after this one was
    /// shown.
    SwitchedWorkspace {
        /// The workspace that is showing now.
        workspace_id: String,
    },
    /// Nothing lies in that direction: no pane, and no tab or workspace past this one.
    AtEdge,
}

#[derive(Deserialize)]
struct Neighbor {
    pane_id: String,
    #[serde(default)]
    neighbor_pane_id: Option<String>,
    layout: Place,
}

/// The part of a layout snapshot that says where a pane is.
#[derive(Deserialize)]
struct Place {
    workspace_id: String,
    tab_id: String,
}

#[derive(Deserialize)]
struct FocusChange {
    changed: bool,
    #[serde(default)]
    focused_pane_id: Option<String>,
}

#[derive(Deserialize)]
struct Tab {
    tab_id: String,
    workspace_id: String,
}

#[derive(Deserialize)]
struct Workspace {
    workspace_id: String,
    number: u64,
}

/// Move the focus one pane over in `direction`.
///
/// When the focused pane has no neighbour on that side the move hops instead, without
/// wrapping around: left and right go to the previous and next tab of the workspace, up and
/// down to the previous and next workspace. Whatever ends up focused is then auto-expanded
/// (see [`CollapseMode::Auto`]): if it sits in a collapsed split, the split is flipped so
/// the pane gets the room.
///
/// Tabs are taken in the order herdr lists them, which is their order on screen, and
/// workspaces in the order of their numbers. A session with no pane at all is
/// [`NavOutcome::AtEdge`]. A failure of the auto-expand step is logged and otherwise
/// ignored, as in the script: the focus has moved and that is what is reported.
pub fn navigate(client: &Client, direction: Direction) -> Result<NavOutcome, HerdrError> {
    let neighbor: Neighbor = match client.call_for(
        "pane.neighbor",
        json!({ "direction": direction }),
        "neighbor",
    ) {
        Ok(neighbor) => neighbor,
        Err(err) if nothing_focused(&err) => return Ok(NavOutcome::AtEdge),
        Err(err) => return Err(err),
    };
    if neighbor
        .neighbor_pane_id
        .as_deref()
        .is_some_and(|id| !id.is_empty())
    {
        return focus_neighbor(client, &neighbor, direction);
    }
    if direction.is_horizontal() {
        hop_tab(client, &neighbor.layout, direction)
    } else {
        hop_workspace(client, &neighbor.layout, direction)
    }
}

fn focus_neighbor(
    client: &Client,
    neighbor: &Neighbor,
    direction: Direction,
) -> Result<NavOutcome, HerdrError> {
    let params = json!({ "pane_id": neighbor.pane_id, "direction": direction });
    let change: FocusChange = client.call_for("pane.focus_direction", params, "focus")?;
    if !change.changed {
        // The neighbour went away between the two calls.
        return Ok(NavOutcome::AtEdge);
    }
    auto_expand(client);
    let pane_id = change
        .focused_pane_id
        .or_else(|| neighbor.neighbor_pane_id.clone())
        .unwrap_or_default();
    Ok(NavOutcome::FocusedPane { pane_id })
}

/// Show the tab before (left) or after (right) the one `place` is in.
fn hop_tab(client: &Client, place: &Place, direction: Direction) -> Result<NavOutcome, HerdrError> {
    let tabs: Vec<Tab> = client.call_for(
        "tab.list",
        json!({ "workspace_id": place.workspace_id }),
        "tabs",
    )?;
    let ids: Vec<&str> = tabs
        .iter()
        .filter(|tab| tab.workspace_id == place.workspace_id)
        .map(|tab| tab.tab_id.as_str())
        .collect();
    let Some(target) = step(&ids, &place.tab_id, direction) else {
        return Ok(NavOutcome::AtEdge);
    };
    client.call("tab.focus", json!({ "tab_id": target }))?;
    auto_expand(client);
    Ok(NavOutcome::SwitchedTab {
        tab_id: target.to_string(),
    })
}

/// Show the workspace before (up) or after (down) the one `place` is in.
fn hop_workspace(
    client: &Client,
    place: &Place,
    direction: Direction,
) -> Result<NavOutcome, HerdrError> {
    let mut workspaces: Vec<Workspace> =
        client.call_for("workspace.list", Value::Null, "workspaces")?;
    workspaces.sort_by_key(|workspace| workspace.number);
    let ids: Vec<&str> = workspaces
        .iter()
        .map(|workspace| workspace.workspace_id.as_str())
        .collect();
    let Some(target) = step(&ids, &place.workspace_id, direction) else {
        return Ok(NavOutcome::AtEdge);
    };
    client.call("workspace.focus", json!({ "workspace_id": target }))?;
    auto_expand(client);
    Ok(NavOutcome::SwitchedWorkspace {
        workspace_id: target.to_string(),
    })
}

/// The id one place before (left, up) or after (right, down) `current` in `ids`, or `None`
/// when `current` is not listed or is already the last one that way.
fn step<'a>(ids: &[&'a str], current: &str, direction: Direction) -> Option<&'a str> {
    let index = ids.iter().position(|id| *id == current)?;
    let target = if direction.is_forward() {
        index.checked_add(1)?
    } else {
        index.checked_sub(1)?
    };
    ids.get(target).copied()
}

/// Give the pane that just got the focus its room back. Best effort, like the script, which
/// starts `herdr-collapse auto` and does not look at how it ended.
fn auto_expand(client: &Client) {
    if let Err(err) = collapse(client, CollapseMode::Auto) {
        log::warn!("herdr auto-expand after moving the focus failed: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::testing::{export_result, pane, right};
    use crate::mock::MockServer;

    fn neighbor(pane: &str, direction: &str, found: Option<&str>) -> Value {
        let mut neighbor = json!({
            "pane_id": pane,
            "direction": direction,
            "layout": {"workspace_id": "w1", "tab_id": "w1:t1", "zoomed": false, "focused_pane_id": pane, "panes": [], "splits": []},
        });
        if let Some(found) = found {
            neighbor["neighbor_pane_id"] = json!(found);
        }
        json!({"type": "pane_neighbor", "neighbor": neighbor})
    }

    fn focus_change(changed: bool, source: &str, focused: &str) -> Value {
        json!({"type": "pane_focus_direction", "focus": {"changed": changed, "source_pane_id": source, "focused_pane_id": focused}})
    }

    fn tabs(ids: &[(&str, &str, u64)]) -> Value {
        let tabs: Vec<Value> = ids
            .iter()
            .map(|(tab, workspace, number)| json!({"tab_id": tab, "workspace_id": workspace, "number": number, "label": "x"}))
            .collect();
        json!({"type": "tab_list", "tabs": tabs})
    }

    fn workspaces(ids: &[(&str, u64)]) -> Value {
        let workspaces: Vec<Value> = ids
            .iter()
            .map(|(id, number)| json!({"workspace_id": id, "number": number, "label": "x"}))
            .collect();
        json!({"type": "workspace_list", "workspaces": workspaces})
    }

    /// The layout the auto-expand step sees: `focused` in an even split, so nothing to flip.
    fn roomy(focused: &str) -> Value {
        export_result(focused, false, right(0.5, pane("w1:p1"), pane("w1:p2")))
    }

    #[test]
    fn focuses_the_neighbour_and_then_auto_expands() {
        let server = MockServer::start();
        server
            .ok("pane.neighbor", neighbor("w1:p1", "right", Some("w1:p2")))
            .ok("pane.focus_direction", focus_change(true, "w1:p1", "w1:p2"))
            .ok("layout.export", roomy("w1:p2"));
        let outcome = navigate(&server.client(), Direction::Right).unwrap();
        assert_eq!(
            outcome,
            NavOutcome::FocusedPane {
                pane_id: "w1:p2".into()
            }
        );
        let requests = server.requests();
        assert_eq!(requests[0].params, json!({"direction": "right"}));
        assert_eq!(
            requests[1].params,
            json!({"pane_id": "w1:p1", "direction": "right"})
        );
        assert_eq!(requests[2].params, json!({}));
        server.assert_finished();
    }

    #[test]
    fn the_newly_focused_pane_is_expanded_out_of_a_collapsed_split() {
        let server = MockServer::start();
        server
            .ok("pane.neighbor", neighbor("w1:p1", "right", Some("w1:p2")))
            .ok("pane.focus_direction", focus_change(true, "w1:p1", "w1:p2"))
            .ok(
                "layout.export",
                export_result("w1:p2", false, right(0.9, pane("w1:p1"), pane("w1:p2"))),
            )
            .ok(
                "layout.set_split_ratio",
                json!({"type": "layout_split_ratio_set"}),
            );
        let outcome = navigate(&server.client(), Direction::Right).unwrap();
        assert_eq!(
            outcome,
            NavOutcome::FocusedPane {
                pane_id: "w1:p2".into()
            }
        );
        assert_eq!(
            server.requests()[3].params,
            json!({"path": [], "ratio": 0.1, "pane_id": "w1:p2"})
        );
        assert_eq!(
            std::fs::read_to_string(server.state_file()).unwrap(),
            r#"[{"tab_id": "w1:t1", "pane_id": "w1:p1", "panes": ["w1:p1"], "path": [], "side": false, "ratio": 0.5}]"#
        );
        server.assert_finished();
    }

    #[test]
    fn a_failing_auto_expand_does_not_fail_the_move() {
        let server = MockServer::start();
        server
            .ok("pane.neighbor", neighbor("w1:p1", "down", Some("w1:p2")))
            .ok("pane.focus_direction", focus_change(true, "w1:p1", "w1:p2"))
            .fail("layout.export", "internal", "boom");
        let outcome = navigate(&server.client(), Direction::Down).unwrap();
        assert_eq!(
            outcome,
            NavOutcome::FocusedPane {
                pane_id: "w1:p2".into()
            }
        );
        server.assert_finished();
    }

    #[test]
    fn a_missing_state_file_does_not_fail_the_move_either() {
        let server = MockServer::start();
        server
            .ok("pane.neighbor", neighbor("w1:p1", "right", Some("w1:p2")))
            .ok("pane.focus_direction", focus_change(true, "w1:p1", "w1:p2"))
            .ok(
                "layout.export",
                export_result("w1:p2", false, right(0.9, pane("w1:p1"), pane("w1:p2"))),
            );
        let client = Client::new(server.socket_path());
        let outcome = navigate(&client, Direction::Right).unwrap();
        assert_eq!(
            outcome,
            NavOutcome::FocusedPane {
                pane_id: "w1:p2".into()
            }
        );
        server.assert_finished();
    }

    #[test]
    fn a_neighbour_that_vanished_is_an_edge() {
        let server = MockServer::start();
        server
            .ok("pane.neighbor", neighbor("w1:p1", "right", Some("w1:p2")))
            .ok("pane.focus_direction", json!({"focus": {"changed": false, "reason": "no_neighbor", "source_pane_id": "w1:p1"}}));
        assert_eq!(
            navigate(&server.client(), Direction::Right).unwrap(),
            NavOutcome::AtEdge
        );
        server.assert_finished();
    }

    #[test]
    fn the_focused_pane_falls_back_to_the_neighbour_the_first_call_named() {
        let server = MockServer::start();
        server
            .ok("pane.neighbor", neighbor("w1:p1", "left", Some("w1:p0")))
            .ok(
                "pane.focus_direction",
                json!({"focus": {"changed": true, "source_pane_id": "w1:p1"}}),
            )
            .ok("layout.export", roomy("w1:p1"));
        assert_eq!(
            navigate(&server.client(), Direction::Left).unwrap(),
            NavOutcome::FocusedPane {
                pane_id: "w1:p0".into()
            }
        );
    }

    #[test]
    fn right_at_the_edge_shows_the_next_tab() {
        let server = MockServer::start();
        server
            .ok("pane.neighbor", neighbor("w1:p1", "right", None))
            .ok(
                "tab.list",
                tabs(&[("w1:t1", "w1", 1), ("w1:t2", "w1", 2), ("w1:t3", "w1", 3)]),
            )
            .ok("tab.focus", json!({"type": "tab_info"}))
            .ok("layout.export", roomy("w1:p1"));
        let outcome = navigate(&server.client(), Direction::Right).unwrap();
        assert_eq!(
            outcome,
            NavOutcome::SwitchedTab {
                tab_id: "w1:t2".into()
            }
        );
        let requests = server.requests();
        assert_eq!(requests[1].params, json!({"workspace_id": "w1"}));
        assert_eq!(requests[2].params, json!({"tab_id": "w1:t2"}));
        server.assert_finished();
    }

    #[test]
    fn left_at_the_edge_shows_the_previous_tab() {
        let server = MockServer::start();
        let mut reply = neighbor("w1:p5", "left", None);
        reply["neighbor"]["layout"]["tab_id"] = json!("w1:t3");
        server
            .ok("pane.neighbor", reply)
            .ok(
                "tab.list",
                tabs(&[("w1:t1", "w1", 1), ("w1:t2", "w1", 2), ("w1:t3", "w1", 3)]),
            )
            .ok("tab.focus", json!({"type": "tab_info"}))
            .ok("layout.export", roomy("w1:p1"));
        let outcome = navigate(&server.client(), Direction::Left).unwrap();
        assert_eq!(
            outcome,
            NavOutcome::SwitchedTab {
                tab_id: "w1:t2".into()
            }
        );
    }

    #[test]
    fn tabs_do_not_wrap_around() {
        for (direction, current) in [(Direction::Left, "w1:t1"), (Direction::Right, "w1:t3")] {
            let server = MockServer::start();
            let mut reply = neighbor("w1:p1", direction.as_str(), None);
            reply["neighbor"]["layout"]["tab_id"] = json!(current);
            server.ok("pane.neighbor", reply).ok(
                "tab.list",
                tabs(&[("w1:t1", "w1", 1), ("w1:t2", "w1", 2), ("w1:t3", "w1", 3)]),
            );
            assert_eq!(
                navigate(&server.client(), direction).unwrap(),
                NavOutcome::AtEdge
            );
            server.assert_finished();
        }
    }

    #[test]
    fn a_lone_tab_is_an_edge_both_ways() {
        for direction in [Direction::Left, Direction::Right] {
            let server = MockServer::start();
            server
                .ok("pane.neighbor", neighbor("w1:p1", direction.as_str(), None))
                .ok("tab.list", tabs(&[("w1:t1", "w1", 1)]));
            assert_eq!(
                navigate(&server.client(), direction).unwrap(),
                NavOutcome::AtEdge
            );
        }
    }

    #[test]
    fn tabs_follow_the_listed_order_not_their_numbers() {
        // herdr numbers tabs by age; the list is in screen order. t7 was moved to the front.
        let server = MockServer::start();
        server
            .ok("pane.neighbor", neighbor("w1:p1", "left", None))
            .ok(
                "tab.list",
                tabs(&[("w1:t7", "w1", 7), ("w1:t1", "w1", 1), ("w1:t4", "w1", 4)]),
            )
            .ok("tab.focus", json!({"type": "tab_info"}))
            .ok("layout.export", roomy("w1:p1"));
        let outcome = navigate(&server.client(), Direction::Left).unwrap();
        assert_eq!(
            outcome,
            NavOutcome::SwitchedTab {
                tab_id: "w1:t7".into()
            }
        );
    }

    #[test]
    fn tabs_of_other_workspaces_are_ignored() {
        let server = MockServer::start();
        server
            .ok("pane.neighbor", neighbor("w1:p1", "right", None))
            .ok(
                "tab.list",
                tabs(&[("w0:t9", "w0", 9), ("w1:t1", "w1", 1), ("w2:t1", "w2", 1)]),
            );
        assert_eq!(
            navigate(&server.client(), Direction::Right).unwrap(),
            NavOutcome::AtEdge
        );
    }

    #[test]
    fn a_tab_missing_from_the_list_is_an_edge() {
        let server = MockServer::start();
        server
            .ok("pane.neighbor", neighbor("w1:p1", "right", None))
            .ok("tab.list", tabs(&[("w1:t8", "w1", 8)]));
        assert_eq!(
            navigate(&server.client(), Direction::Right).unwrap(),
            NavOutcome::AtEdge
        );
    }

    #[test]
    fn down_at_the_edge_shows_the_next_workspace() {
        let server = MockServer::start();
        server
            .ok("pane.neighbor", neighbor("w1:p1", "down", None))
            // Listed out of order on purpose: the numbers decide.
            .ok(
                "workspace.list",
                workspaces(&[("w3", 3), ("w1", 1), ("w2", 2)]),
            )
            .ok("workspace.focus", json!({"type": "workspace_info"}))
            .ok("layout.export", roomy("w1:p1"));
        let outcome = navigate(&server.client(), Direction::Down).unwrap();
        assert_eq!(
            outcome,
            NavOutcome::SwitchedWorkspace {
                workspace_id: "w2".into()
            }
        );
        let requests = server.requests();
        assert_eq!(requests[1].params, json!({}));
        assert_eq!(requests[2].params, json!({"workspace_id": "w2"}));
        server.assert_finished();
    }

    #[test]
    fn up_at_the_edge_shows_the_previous_workspace() {
        let server = MockServer::start();
        let mut reply = neighbor("w5:p1", "up", None);
        reply["neighbor"]["layout"]["workspace_id"] = json!("w5");
        server
            .ok("pane.neighbor", reply)
            .ok(
                "workspace.list",
                workspaces(&[("w4", 1), ("w5", 2), ("w6", 3)]),
            )
            .ok("workspace.focus", json!({"type": "workspace_info"}))
            .ok("layout.export", roomy("w1:p1"));
        let outcome = navigate(&server.client(), Direction::Up).unwrap();
        assert_eq!(
            outcome,
            NavOutcome::SwitchedWorkspace {
                workspace_id: "w4".into()
            }
        );
    }

    #[test]
    fn workspaces_do_not_wrap_around() {
        for (direction, current) in [(Direction::Up, "w1"), (Direction::Down, "w3")] {
            let server = MockServer::start();
            let mut reply = neighbor("p", direction.as_str(), None);
            reply["neighbor"]["layout"]["workspace_id"] = json!(current);
            server.ok("pane.neighbor", reply).ok(
                "workspace.list",
                workspaces(&[("w1", 1), ("w2", 2), ("w3", 3)]),
            );
            assert_eq!(
                navigate(&server.client(), direction).unwrap(),
                NavOutcome::AtEdge
            );
            server.assert_finished();
        }
    }

    #[test]
    fn a_workspace_missing_from_the_list_is_an_edge() {
        let server = MockServer::start();
        server
            .ok("pane.neighbor", neighbor("w1:p1", "up", None))
            .ok("workspace.list", workspaces(&[("w8", 1)]));
        assert_eq!(
            navigate(&server.client(), Direction::Up).unwrap(),
            NavOutcome::AtEdge
        );
    }

    #[test]
    fn an_empty_neighbour_id_counts_as_no_neighbour() {
        let server = MockServer::start();
        server
            .ok("pane.neighbor", neighbor("w1:p1", "up", Some("")))
            .ok("workspace.list", workspaces(&[("w1", 1)]));
        assert_eq!(
            navigate(&server.client(), Direction::Up).unwrap(),
            NavOutcome::AtEdge
        );
    }

    #[test]
    fn an_empty_session_is_an_edge() {
        let server = MockServer::start();
        server.fail("pane.neighbor", "pane_not_found", "pane not found");
        assert_eq!(
            navigate(&server.client(), Direction::Left).unwrap(),
            NavOutcome::AtEdge
        );
        server.assert_finished();
    }

    #[test]
    fn errors_from_herdr_are_passed_on() {
        let server = MockServer::start();
        server.fail("pane.neighbor", "invalid_request", "bad");
        assert_eq!(
            navigate(&server.client(), Direction::Left)
                .unwrap_err()
                .code(),
            Some("invalid_request")
        );

        let server = MockServer::start();
        server
            .ok("pane.neighbor", neighbor("w1:p1", "right", None))
            .fail("tab.list", "internal", "bad");
        assert_eq!(
            navigate(&server.client(), Direction::Right)
                .unwrap_err()
                .code(),
            Some("internal")
        );

        let server = MockServer::start();
        server
            .ok("pane.neighbor", neighbor("w1:p1", "down", None))
            .ok("workspace.list", workspaces(&[("w1", 1), ("w2", 2)]))
            .fail(
                "workspace.focus",
                "workspace_not_found",
                "workspace w2 not found",
            );
        assert_eq!(
            navigate(&server.client(), Direction::Down)
                .unwrap_err()
                .code(),
            Some("workspace_not_found")
        );
    }

    #[test]
    fn a_reply_without_the_expected_fields_is_a_protocol_error() {
        let server = MockServer::start();
        server.ok(
            "pane.neighbor",
            json!({"type": "pane_neighbor", "neighbor": {"pane_id": "w1:p1"}}),
        );
        assert!(matches!(
            navigate(&server.client(), Direction::Left).unwrap_err(),
            HerdrError::Protocol { .. }
        ));
    }

    #[test]
    fn step_moves_one_place_and_stops_at_the_ends() {
        let ids = ["a", "b", "c"];
        assert_eq!(step(&ids, "a", Direction::Right), Some("b"));
        assert_eq!(step(&ids, "b", Direction::Down), Some("c"));
        assert_eq!(step(&ids, "c", Direction::Right), None);
        assert_eq!(step(&ids, "c", Direction::Left), Some("b"));
        assert_eq!(step(&ids, "b", Direction::Up), Some("a"));
        assert_eq!(step(&ids, "a", Direction::Left), None);
        assert_eq!(step(&ids, "zz", Direction::Right), None);
        assert_eq!(step(&[], "a", Direction::Right), None);
    }
}
