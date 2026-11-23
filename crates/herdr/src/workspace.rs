//! What herdr's own keys for zoom and workspaces do, through the socket.
//!
//! In the user's herdr config these are `prefix+z` (zoom), `prefix+u` / `prefix+d` (previous
//! and next workspace) and `prefix+shift+1..9` (workspace by number). herdr handles those keys
//! itself; the calls below do the same things without a key press.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::client::{Client, HerdrError};
use crate::layout::nothing_focused;

/// Whether the focused pane fills its tab after [`zoom_toggle`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZoomOutcome {
    /// Whether the tab is zoomed now.
    pub zoomed: bool,
    /// Whether the call changed anything. A tab with a single pane cannot be zoomed.
    pub changed: bool,
}

/// What a workspace switch did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceOutcome {
    /// Another workspace is showing now.
    Switched {
        /// The workspace that is showing.
        workspace_id: String,
    },
    /// Nothing changed: the workspace asked for is already showing, there is no such
    /// workspace, or there is no other workspace to go to.
    Unchanged,
}

#[derive(Deserialize)]
struct Zoom {
    #[serde(default)]
    zoomed: bool,
    #[serde(default)]
    changed: bool,
}

#[derive(Deserialize)]
struct Workspace {
    workspace_id: String,
    number: u64,
    #[serde(default)]
    focused: bool,
}

/// Zoom the focused pane to fill its tab, or put the tab back if it is zoomed.
///
/// A tab with one pane, or a session with no pane at all, is left as it is and reported with
/// `changed: false`.
pub fn zoom_toggle(client: &Client) -> Result<ZoomOutcome, HerdrError> {
    match client.call_for::<Zoom>("pane.zoom", json!({ "mode": "toggle" }), "zoom") {
        Ok(zoom) => Ok(ZoomOutcome {
            zoomed: zoom.zoomed,
            changed: zoom.changed,
        }),
        Err(err) if nothing_focused(&err) => Ok(ZoomOutcome {
            zoomed: false,
            changed: false,
        }),
        Err(err) => Err(err),
    }
}

/// Show the workspace before the current one, going round to the last from the first.
///
/// Workspaces are taken in the order of their numbers, which is their order in herdr's
/// sidebar. herdr's own key skips workspaces hidden inside a folded worktree group; this
/// does not, because the socket does not say which groups are folded.
pub fn previous_workspace(client: &Client) -> Result<WorkspaceOutcome, HerdrError> {
    cycle_workspace(client, false)
}

/// Show the workspace after the current one, going round to the first from the last. See
/// [`previous_workspace`].
pub fn next_workspace(client: &Client) -> Result<WorkspaceOutcome, HerdrError> {
    cycle_workspace(client, true)
}

/// Show workspace number `number`, counting from 1 in sidebar order. Any number without a
/// workspace (0 included) and the number of the workspace already showing are
/// [`WorkspaceOutcome::Unchanged`].
pub fn focus_workspace_number(
    client: &Client,
    number: usize,
) -> Result<WorkspaceOutcome, HerdrError> {
    let workspaces = list_workspaces(client)?;
    let target = number
        .checked_sub(1)
        .and_then(|index| workspaces.get(index));
    focus_unless_showing(client, target)
}

fn cycle_workspace(client: &Client, forward: bool) -> Result<WorkspaceOutcome, HerdrError> {
    let workspaces = list_workspaces(client)?;
    let count = workspaces.len();
    if count == 0 {
        return Ok(WorkspaceOutcome::Unchanged);
    }
    // Like herdr's key, start from the first workspace when none is marked as showing.
    let current = workspaces
        .iter()
        .position(|workspace| workspace.focused)
        .unwrap_or(0);
    let target = if forward {
        (current + 1) % count
    } else {
        (current + count - 1) % count
    };
    focus_unless_showing(client, workspaces.get(target))
}

/// Every workspace, in sidebar order.
fn list_workspaces(client: &Client) -> Result<Vec<Workspace>, HerdrError> {
    let mut workspaces: Vec<Workspace> =
        client.call_for("workspace.list", Value::Null, "workspaces")?;
    workspaces.sort_by_key(|workspace| workspace.number);
    Ok(workspaces)
}

fn focus_unless_showing(
    client: &Client,
    target: Option<&Workspace>,
) -> Result<WorkspaceOutcome, HerdrError> {
    match target {
        Some(workspace) if !workspace.focused => {
            client.call(
                "workspace.focus",
                json!({ "workspace_id": workspace.workspace_id }),
            )?;
            Ok(WorkspaceOutcome::Switched {
                workspace_id: workspace.workspace_id.clone(),
            })
        }
        _ => Ok(WorkspaceOutcome::Unchanged),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockServer;

    fn workspaces(rows: &[(&str, u64, bool)]) -> Value {
        let rows: Vec<Value> = rows
            .iter()
            .map(|(id, number, focused)| json!({"workspace_id": id, "number": number, "focused": focused, "label": "x"}))
            .collect();
        json!({"type": "workspace_list", "workspaces": rows})
    }

    fn three(focused: &str) -> Value {
        workspaces(&[
            ("w1", 1, focused == "w1"),
            ("w4", 2, focused == "w4"),
            ("w5", 3, focused == "w5"),
        ])
    }

    fn server_with(list: Value) -> MockServer {
        let server = MockServer::start();
        server.ok("workspace.list", list);
        server
    }

    fn switched(id: &str) -> WorkspaceOutcome {
        WorkspaceOutcome::Switched {
            workspace_id: id.into(),
        }
    }

    #[test]
    fn zoom_toggle_reports_the_new_state() {
        let server = MockServer::start();
        server.ok("pane.zoom", json!({"type": "pane_zoom", "zoom": {"changed": true, "zoomed": true, "pane_id": "w1:p1"}}));
        server.ok("pane.zoom", json!({"type": "pane_zoom", "zoom": {"changed": true, "zoomed": false, "pane_id": "w1:p1"}}));
        let client = server.client();
        assert_eq!(
            zoom_toggle(&client).unwrap(),
            ZoomOutcome {
                zoomed: true,
                changed: true
            }
        );
        assert_eq!(
            zoom_toggle(&client).unwrap(),
            ZoomOutcome {
                zoomed: false,
                changed: true
            }
        );
        assert_eq!(server.requests()[0].params, json!({"mode": "toggle"}));
        server.assert_finished();
    }

    #[test]
    fn zooming_a_single_pane_changes_nothing() {
        let server = MockServer::start();
        server.ok(
            "pane.zoom",
            json!({"zoom": {"changed": false, "zoomed": false, "reason": "single_pane"}}),
        );
        assert_eq!(
            zoom_toggle(&server.client()).unwrap(),
            ZoomOutcome {
                zoomed: false,
                changed: false
            }
        );
    }

    #[test]
    fn zooming_an_empty_session_changes_nothing() {
        let server = MockServer::start();
        server.fail("pane.zoom", "pane_not_found", "pane not found");
        assert_eq!(
            zoom_toggle(&server.client()).unwrap(),
            ZoomOutcome {
                zoomed: false,
                changed: false
            }
        );
    }

    #[test]
    fn zoom_failures_are_passed_on() {
        let server = MockServer::start();
        server.fail("pane.zoom", "internal", "boom");
        assert_eq!(
            zoom_toggle(&server.client()).unwrap_err().code(),
            Some("internal")
        );
    }

    #[test]
    fn next_and_previous_step_through_the_workspaces() {
        let server = server_with(three("w4"));
        server.ok("workspace.focus", json!({"type": "workspace_info"}));
        assert_eq!(next_workspace(&server.client()).unwrap(), switched("w5"));
        assert_eq!(server.requests()[1].params, json!({"workspace_id": "w5"}));

        let server = server_with(three("w4"));
        server.ok("workspace.focus", json!({"type": "workspace_info"}));
        assert_eq!(
            previous_workspace(&server.client()).unwrap(),
            switched("w1")
        );
        server.assert_finished();
    }

    #[test]
    fn next_and_previous_wrap_around() {
        let server = server_with(three("w5"));
        server.ok("workspace.focus", json!({"type": "workspace_info"}));
        assert_eq!(next_workspace(&server.client()).unwrap(), switched("w1"));

        let server = server_with(three("w1"));
        server.ok("workspace.focus", json!({"type": "workspace_info"}));
        assert_eq!(
            previous_workspace(&server.client()).unwrap(),
            switched("w5")
        );
    }

    #[test]
    fn the_numbers_decide_the_order() {
        let server = server_with(workspaces(&[
            ("w9", 3, false),
            ("w2", 1, true),
            ("w7", 2, false),
        ]));
        server.ok("workspace.focus", json!({"type": "workspace_info"}));
        assert_eq!(next_workspace(&server.client()).unwrap(), switched("w7"));
    }

    #[test]
    fn a_lone_workspace_or_none_is_unchanged() {
        for list in [workspaces(&[("w1", 1, true)]), workspaces(&[])] {
            for forward in [true, false] {
                let server = server_with(list.clone());
                let outcome = if forward {
                    next_workspace(&server.client())
                } else {
                    previous_workspace(&server.client())
                };
                assert_eq!(outcome.unwrap(), WorkspaceOutcome::Unchanged);
                assert_eq!(server.requests().len(), 1);
            }
        }
    }

    #[test]
    fn with_no_workspace_marked_as_showing_cycling_starts_from_the_first() {
        let server = server_with(three("none"));
        server.ok("workspace.focus", json!({"type": "workspace_info"}));
        assert_eq!(next_workspace(&server.client()).unwrap(), switched("w4"));
    }

    #[test]
    fn workspaces_are_reached_by_number() {
        let server = server_with(three("w1"));
        server.ok("workspace.focus", json!({"type": "workspace_info"}));
        assert_eq!(
            focus_workspace_number(&server.client(), 3).unwrap(),
            switched("w5")
        );
        assert_eq!(server.requests()[1].params, json!({"workspace_id": "w5"}));

        let server = server_with(three("w5"));
        server.ok("workspace.focus", json!({"type": "workspace_info"}));
        assert_eq!(
            focus_workspace_number(&server.client(), 1).unwrap(),
            switched("w1")
        );
    }

    #[test]
    fn a_number_without_a_workspace_is_unchanged() {
        for number in [0, 4, 9, usize::MAX] {
            let server = server_with(three("w1"));
            assert_eq!(
                focus_workspace_number(&server.client(), number).unwrap(),
                WorkspaceOutcome::Unchanged
            );
            assert_eq!(server.requests().len(), 1, "{number}");
        }
    }

    #[test]
    fn the_number_of_the_workspace_already_showing_is_unchanged() {
        let server = server_with(three("w4"));
        assert_eq!(
            focus_workspace_number(&server.client(), 2).unwrap(),
            WorkspaceOutcome::Unchanged
        );
        assert_eq!(server.requests().len(), 1);
    }

    #[test]
    fn workspace_failures_are_passed_on() {
        let server = MockServer::start();
        server.fail("workspace.list", "internal", "boom");
        assert_eq!(
            next_workspace(&server.client()).unwrap_err().code(),
            Some("internal")
        );

        let server = server_with(three("w1"));
        server.fail(
            "workspace.focus",
            "workspace_not_found",
            "workspace w4 not found",
        );
        assert_eq!(
            focus_workspace_number(&server.client(), 2)
                .unwrap_err()
                .code(),
            Some("workspace_not_found")
        );
    }
}
