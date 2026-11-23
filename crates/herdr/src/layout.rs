//! A tab's split tree, as herdr's `layout.export` describes it.
//!
//! Every tab is a binary tree: a leaf is a pane, an inner node divides its rectangle between
//! a `first` and a `second` child, `ratio` being the share of the first. A split is addressed
//! by its *path* from the root, one boolean per level: `false` goes to the first child,
//! `true` to the second. `layout.set_split_ratio` takes the same paths.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::client::{Client, HerdrError};
use crate::types::SplitDirection;

/// One node of a tab's split tree.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Node {
    /// A leaf: one pane.
    Pane {
        /// The pane's id.
        pane_id: String,
    },
    /// Two subtrees sharing a rectangle.
    Split {
        /// Which way the rectangle is divided.
        direction: SplitDirection,
        /// The share of the rectangle the first child gets, between 0.1 and 0.9.
        ratio: f64,
        /// The left or top child.
        first: Box<Node>,
        /// The right or bottom child.
        second: Box<Node>,
    },
}

/// A split node, borrowed from its tree.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Split<'a> {
    pub(crate) direction: SplitDirection,
    pub(crate) ratio: f64,
    pub(crate) first: &'a Node,
    pub(crate) second: &'a Node,
}

impl<'a> Split<'a> {
    /// The child on `side`: the second when `true`, the first when `false`.
    pub(crate) fn child(&self, side: bool) -> &'a Node {
        if side {
            self.second
        } else {
            self.first
        }
    }
}

impl Node {
    /// This node as a split, or `None` for a pane.
    pub(crate) fn as_split(&self) -> Option<Split<'_>> {
        match self {
            Node::Pane { .. } => None,
            Node::Split {
                direction,
                ratio,
                first,
                second,
            } => Some(Split {
                direction: *direction,
                ratio: *ratio,
                first,
                second,
            }),
        }
    }

    /// The ids of the panes below this node, first child before second.
    pub(crate) fn leaves(&self) -> Vec<&str> {
        let mut ids = Vec::new();
        self.collect_leaves(&mut ids);
        ids
    }

    fn collect_leaves<'a>(&'a self, ids: &mut Vec<&'a str>) {
        match self {
            Node::Pane { pane_id } => ids.push(pane_id),
            Node::Split { first, second, .. } => {
                first.collect_leaves(ids);
                second.collect_leaves(ids);
            }
        }
    }

    /// The pane reached by always taking the first child.
    pub(crate) fn first_leaf(&self) -> &str {
        let mut node = self;
        loop {
            match node {
                Node::Pane { pane_id } => return pane_id,
                Node::Split { first, .. } => node = first,
            }
        }
    }

    /// Whether `pane_id` is one of the panes below this node.
    pub(crate) fn contains(&self, pane_id: &str) -> bool {
        match self {
            Node::Pane { pane_id: id } => id == pane_id,
            Node::Split { first, second, .. } => {
                first.contains(pane_id) || second.contains(pane_id)
            }
        }
    }

    /// The path from this node down to pane `pane_id`: empty when this node is that pane,
    /// `None` when the pane is not below it.
    pub(crate) fn find_path(&self, pane_id: &str) -> Option<Vec<bool>> {
        let mut path = Vec::new();
        self.search(pane_id, &mut path).then_some(path)
    }

    fn search(&self, pane_id: &str, path: &mut Vec<bool>) -> bool {
        match self {
            Node::Pane { pane_id: id } => id == pane_id,
            Node::Split { first, second, .. } => {
                for (side, child) in [(false, first), (true, second)] {
                    path.push(side);
                    if child.search(pane_id, path) {
                        return true;
                    }
                    path.pop();
                }
                false
            }
        }
    }

    /// The node `path` leads to, or `None` when the path runs through a pane.
    pub(crate) fn node_at(&self, path: &[bool]) -> Option<&Node> {
        let mut node = self;
        for side in path {
            node = node.as_split()?.child(*side);
        }
        Some(node)
    }

    /// The split at `path`, or `None` when the path does not end at a split.
    pub(crate) fn split_at(&self, path: &[bool]) -> Option<Split<'_>> {
        self.node_at(path)?.as_split()
    }
}

/// What `layout.export` says about one tab.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct TabLayout {
    pub(crate) workspace_id: String,
    pub(crate) tab_id: String,
    /// Whether one pane currently fills the tab. The tree is the unzoomed one either way.
    #[serde(default)]
    pub(crate) zoomed: bool,
    /// The pane keys go to when this tab is showing.
    pub(crate) focused_pane_id: String,
    pub(crate) root: Node,
}

/// Fetch the layout of the tab holding `pane_id`, or of the tab on screen when `None`.
pub(crate) fn export(client: &Client, pane_id: Option<&str>) -> Result<TabLayout, HerdrError> {
    let params = match pane_id {
        Some(pane_id) => json!({ "pane_id": pane_id }),
        None => Value::Null,
    };
    client.call_for("layout.export", params, "layout")
}

/// Whether `err` is herdr saying there is no pane or tab to act on. When the request named
/// none, that means the session is empty and there is no focused pane.
pub(crate) fn nothing_focused(err: &HerdrError) -> bool {
    matches!(
        err.code(),
        Some("pane_not_found" | "layout_not_found" | "tab_not_found" | "workspace_not_found")
    )
}

/// Set the ratio of the split at `path` in the tab that holds `pane_id`.
pub(crate) fn set_split_ratio(
    client: &Client,
    path: &[bool],
    ratio: f64,
    pane_id: &str,
) -> Result<(), HerdrError> {
    client
        .call(
            "layout.set_split_ratio",
            json!({ "path": path, "ratio": ratio, "pane_id": pane_id }),
        )
        .map(|_| ())
}

/// Give `pane_id` the keyboard focus.
pub(crate) fn focus_pane(client: &Client, pane_id: &str) -> Result<(), HerdrError> {
    client
        .call("pane.focus", json!({ "pane_id": pane_id }))
        .map(|_| ())
}

#[cfg(test)]
pub(crate) mod testing {
    //! Builders for layout JSON in the shape herdr sends it.

    use serde_json::{json, Value};

    /// A pane leaf.
    pub(crate) fn pane(id: &str) -> Value {
        json!({ "type": "pane", "pane_id": id, "cwd": format!("/work/{id}") })
    }

    /// A side-by-side split.
    pub(crate) fn right(ratio: f64, first: Value, second: Value) -> Value {
        json!({ "type": "split", "direction": "right", "ratio": ratio, "first": first, "second": second })
    }

    /// A stacked split.
    pub(crate) fn down(ratio: f64, first: Value, second: Value) -> Value {
        json!({ "type": "split", "direction": "down", "ratio": ratio, "first": first, "second": second })
    }

    /// The result of `layout.export` for tab `w1:t1` of workspace `w1`.
    pub(crate) fn export_result(focused: &str, zoomed: bool, root: Value) -> Value {
        json!({
            "type": "layout_export",
            "layout": {
                "workspace_id": "w1",
                "tab_id": "w1:t1",
                "zoomed": zoomed,
                "focused_pane_id": focused,
                "root": root,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{down, export_result, pane, right};
    use super::*;
    use crate::mock::MockServer;

    fn tree(value: Value) -> Node {
        serde_json::from_value(value).unwrap()
    }

    /// `(a | (b / c))`
    fn sample() -> Node {
        tree(right(0.5, pane("a"), down(0.3, pane("b"), pane("c"))))
    }

    #[test]
    fn parses_the_shape_herdr_exports() {
        let node = sample();
        let split = node.as_split().unwrap();
        assert_eq!(split.direction, SplitDirection::Right);
        assert_eq!(split.ratio, 0.5);
        assert_eq!(
            split.first,
            &Node::Pane {
                pane_id: "a".into()
            }
        );
        let inner = split.second.as_split().unwrap();
        assert_eq!(inner.direction, SplitDirection::Down);
        assert_eq!(inner.ratio, 0.3);
        assert_eq!(split.child(false), split.first);
        assert_eq!(split.child(true), split.second);
        assert!(split.first.as_split().is_none());
    }

    #[test]
    fn a_pane_without_an_id_is_rejected() {
        assert!(serde_json::from_value::<Node>(json!({"type": "pane"})).is_err());
        assert!(serde_json::from_value::<Node>(json!({"type": "pane", "pane_id": null})).is_err());
        assert!(serde_json::from_value::<Node>(json!({"type": "window"})).is_err());
        assert!(serde_json::from_value::<Node>(
            json!({"type": "split", "direction": "right", "ratio": 0.5})
        )
        .is_err());
    }

    #[test]
    fn leaves_come_in_reading_order() {
        assert_eq!(sample().leaves(), vec!["a", "b", "c"]);
        assert_eq!(sample().first_leaf(), "a");
        assert_eq!(tree(pane("only")).leaves(), vec!["only"]);
        assert_eq!(tree(pane("only")).first_leaf(), "only");
        let nested = tree(down(0.5, right(0.5, pane("x"), pane("y")), pane("z")));
        assert_eq!(nested.leaves(), vec!["x", "y", "z"]);
        assert_eq!(nested.first_leaf(), "x");
    }

    #[test]
    fn paths_lead_to_panes_and_back() {
        let node = sample();
        assert_eq!(node.find_path("a"), Some(vec![false]));
        assert_eq!(node.find_path("b"), Some(vec![true, false]));
        assert_eq!(node.find_path("c"), Some(vec![true, true]));
        assert_eq!(node.find_path("zz"), None);
        assert_eq!(tree(pane("only")).find_path("only"), Some(vec![]));
        assert_eq!(tree(pane("only")).find_path("other"), None);
        for id in ["a", "b", "c"] {
            let path = node.find_path(id).unwrap();
            assert_eq!(
                node.node_at(&path),
                Some(&Node::Pane { pane_id: id.into() })
            );
        }
    }

    #[test]
    fn node_at_stops_at_panes() {
        let node = sample();
        assert_eq!(node.node_at(&[]), Some(&node));
        assert!(node.node_at(&[true]).unwrap().as_split().is_some());
        assert_eq!(node.node_at(&[false, false]), None);
        assert_eq!(node.node_at(&[true, true, true]), None);
        assert!(node.split_at(&[]).is_some());
        assert!(node.split_at(&[true]).is_some());
        assert!(node.split_at(&[false]).is_none());
        assert!(node.split_at(&[false, true]).is_none());
    }

    #[test]
    fn contains_looks_through_the_whole_subtree() {
        let node = sample();
        assert!(node.contains("a") && node.contains("b") && node.contains("c"));
        assert!(!node.contains("d"));
        assert!(!node.split_at(&[true]).unwrap().first.contains("c"));
    }

    #[test]
    fn export_asks_for_the_tab_on_screen_or_the_tab_of_a_pane() {
        let server = MockServer::start();
        server.ok(
            "layout.export",
            export_result("a", false, right(0.5, pane("a"), pane("b"))),
        );
        server.ok("layout.export", export_result("b", true, pane("b")));
        let client = server.client();
        let layout = export(&client, None).unwrap();
        assert_eq!(layout.workspace_id, "w1");
        assert_eq!(layout.tab_id, "w1:t1");
        assert_eq!(layout.focused_pane_id, "a");
        assert!(!layout.zoomed);
        assert_eq!(layout.root.leaves(), vec!["a", "b"]);
        let layout = export(&client, Some("b")).unwrap();
        assert!(layout.zoomed);
        let requests = server.requests();
        assert_eq!(requests[0].params, json!({}));
        assert_eq!(requests[1].params, json!({"pane_id": "b"}));
    }

    #[test]
    fn a_layout_reply_missing_its_tree_is_a_protocol_error() {
        let server = MockServer::start();
        server.ok(
            "layout.export",
            json!({"layout": {"workspace_id": "w1", "tab_id": "w1:t1", "focused_pane_id": "a"}}),
        );
        server.ok("layout.export", json!({"type": "layout_export"}));
        let client = server.client();
        assert!(matches!(
            export(&client, None).unwrap_err(),
            HerdrError::Protocol { .. }
        ));
        assert!(matches!(
            export(&client, None).unwrap_err(),
            HerdrError::Protocol { .. }
        ));
    }

    #[test]
    fn helpers_send_the_parameters_the_scripts_send() {
        let server = MockServer::start();
        server.ok(
            "layout.set_split_ratio",
            json!({"type": "layout_split_ratio_set"}),
        );
        server.ok("pane.focus", json!({"type": "pane_info"}));
        let client = server.client();
        set_split_ratio(&client, &[true, false], 0.9, "w1:p2").unwrap();
        focus_pane(&client, "w1:p3").unwrap();
        let requests = server.requests();
        assert_eq!(
            requests[0].params,
            json!({"path": [true, false], "ratio": 0.9, "pane_id": "w1:p2"})
        );
        assert_eq!(requests[1].params, json!({"pane_id": "w1:p3"}));
    }

    #[test]
    fn not_found_codes_mean_nothing_is_focused() {
        for code in [
            "pane_not_found",
            "layout_not_found",
            "tab_not_found",
            "workspace_not_found",
        ] {
            let err = HerdrError::Server {
                method: "x".into(),
                code: code.into(),
                message: String::new(),
            };
            assert!(nothing_focused(&err), "{code}");
        }
        let err = HerdrError::Server {
            method: "x".into(),
            code: "invalid_request".into(),
            message: String::new(),
        };
        assert!(!nothing_focused(&err));
        assert!(!nothing_focused(&HerdrError::NoStateFile));
    }
}
