//! Tests against a real herdr server.
//!
//! They rearrange panes, so they only ever run against a throwaway session: set
//! `BAKEDIT_TEST_HERDR_SOCKET` to its API socket (`tests/isolated-session.sh start` makes one
//! in a private directory and prints the variable). Without it every test here passes without
//! doing anything, and a socket that turns out to be the user's live session is refused.
//!
//! Set `BAKEDIT_TEST_HERDR_SCRIPTS` to the directory holding the user's `herdr-nav`,
//! `herdr-collapse`, `herdr-move`, `herdr-root-split` and `herdr-scroll` scripts as well to
//! run the same steps through the scripts and through this crate and compare what herdr looks
//! like after each one. The scripts get a sandbox of their own: a scratch home directory, the
//! test session's socket, and none of this process's `HERDR_*` variables.
//!
//! Each test builds the workspaces it needs and closes them again.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use bakedit_herdr::{
    collapse, collapse_pane, focus_workspace_number, focused_pane, move_pane, navigate,
    next_workspace, pane_at, pane_info, pane_layout, previous_workspace, root_split, scroll,
    zoom_toggle, Client, CollapseMode, CollapseOutcome, Direction, HerdrError, MoveOutcome,
    NavOutcome, ScreenSize, ScrollOutcome, SplitDirection, WorkspaceOutcome, ZoomOutcome,
};
use serde_json::{json, Value};

const SOCKET_VAR: &str = "BAKEDIT_TEST_HERDR_SOCKET";
const SCRIPTS_VAR: &str = "BAKEDIT_TEST_HERDR_SCRIPTS";

/// The tests share one herdr session and its single focus, so they take turns.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// A turn on the test session, with a scratch directory and the workspaces the test made.
struct Session {
    client: Client,
    scratch: tempfile::TempDir,
    /// Pane id to the short name the test gave it.
    names: HashMap<String, String>,
    workspaces: Vec<String>,
    _turn: MutexGuard<'static, ()>,
}

/// A pane a test created, with where it lives.
#[derive(Debug, Clone)]
struct Made {
    pane: String,
    tab: String,
    workspace: String,
}

impl Session {
    /// The test session, or `None` (after saying why) when none was provided.
    fn open() -> Option<Session> {
        let Some(socket) = std::env::var_os(SOCKET_VAR).filter(|value| !value.is_empty()) else {
            eprintln!("skipped: {SOCKET_VAR} is not set (see tests/isolated-session.sh)");
            return None;
        };
        let turn = ONE_AT_A_TIME
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let scratch = tempfile::tempdir().expect("a scratch directory");
        let client = Client::new(PathBuf::from(socket))
            .with_state_file(
                scratch
                    .path()
                    .join("cache")
                    .join("herdr-collapse")
                    .join("state.json"),
            )
            .with_timeout(Duration::from_secs(5));
        assert!(
            !client.is_live_session(),
            "{SOCKET_VAR} names the live herdr session ({}); these tests would rearrange it",
            client.socket_path().display()
        );
        client.ping().expect("the test herdr server answers a ping");
        Some(Session {
            client,
            scratch,
            names: HashMap::new(),
            workspaces: Vec::new(),
            _turn: turn,
        })
    }

    fn call(&self, method: &str, params: Value) -> Value {
        self.client
            .call(method, params)
            .unwrap_or_else(|err| panic!("{method} failed: {err}"))
    }

    fn state_file(&self) -> PathBuf {
        self.client
            .state_file()
            .expect("the test client has a state file")
            .to_path_buf()
    }

    /// The state file's contents, or an empty string when there is none.
    fn state(&self) -> String {
        std::fs::read_to_string(self.state_file()).unwrap_or_default()
    }

    fn forget_state(&self) {
        let _ = std::fs::remove_file(self.state_file());
    }

    /// A directory of the test's own for a pane to start in.
    fn dir(&self, name: &str) -> PathBuf {
        let dir = self.scratch.path().join("work").join(name);
        std::fs::create_dir_all(&dir).expect("a working directory");
        dir.canonicalize().expect("the working directory resolves")
    }

    fn remember(&mut self, name: &str, pane: &Value) -> Made {
        let made = Made {
            pane: pane["pane_id"].as_str().expect("a pane id").to_string(),
            tab: pane["tab_id"].as_str().expect("a tab id").to_string(),
            workspace: pane["workspace_id"]
                .as_str()
                .expect("a workspace id")
                .to_string(),
        };
        self.names.insert(made.pane.clone(), name.to_string());
        made
    }

    /// A new workspace at the end of the list, focused, whose one pane is called `name`.
    fn workspace(&mut self, name: &str) -> Made {
        let cwd = self.dir(name);
        let created = self.call(
            "workspace.create",
            json!({"cwd": cwd, "label": format!("test-{name}"), "focus": true}),
        );
        let made = self.remember(name, &created["root_pane"]);
        self.workspaces.push(made.workspace.clone());
        made
    }

    /// A new tab at the end of `workspace`, not focused, whose one pane is called `name`.
    fn tab(&mut self, workspace: &str, name: &str) -> Made {
        let cwd = self.dir(name);
        let created = self.call(
            "tab.create",
            json!({"workspace_id": workspace, "cwd": cwd, "label": name, "focus": false}),
        );
        self.remember(name, &created["root_pane"])
    }

    /// Split `target` and call the new pane `name`. The focus stays where it is.
    fn split(&mut self, target: &Made, direction: &str, name: &str) -> Made {
        let cwd = self.dir(name);
        let created = self.call(
            "pane.split",
            json!({"target_pane_id": target.pane, "direction": direction, "cwd": cwd, "focus": false}),
        );
        self.remember(name, &created["pane"])
    }

    fn focus(&self, pane: &Made) {
        self.call("pane.focus", json!({"pane_id": pane.pane}));
    }

    fn set_ratio(&self, pane: &Made, path: &[bool], ratio: f64) {
        self.call(
            "layout.set_split_ratio",
            json!({"path": path, "ratio": ratio, "pane_id": pane.pane}),
        );
    }

    fn name_of(&self, pane_id: &str) -> String {
        self.names
            .get(pane_id)
            .cloned()
            .unwrap_or_else(|| format!("<{pane_id}>"))
    }

    /// The name of the pane herdr sends keys to.
    fn focused(&self) -> String {
        let pane = focused_pane(&self.client).expect("a focused pane");
        self.name_of(&pane.pane_id)
    }

    /// The split tree of the tab holding `pane`, written like `(a |0.5 (b /0.3 c))`: `|` is a
    /// side-by-side split, `/` a stacked one, the number the first child's share.
    fn tree(&self, pane: &Made) -> String {
        let layout = self.call("layout.export", json!({"pane_id": pane.pane}));
        self.render(&layout["layout"]["root"])
    }

    fn render(&self, node: &Value) -> String {
        if node["type"] == "pane" {
            return self.name_of(node["pane_id"].as_str().unwrap_or_default());
        }
        let bar = if node["direction"] == "right" {
            "|"
        } else {
            "/"
        };
        format!(
            "({} {bar}{} {})",
            self.render(&node["first"]),
            node["ratio"],
            self.render(&node["second"])
        )
    }

    fn zoomed(&self, pane: &Made) -> bool {
        self.call("layout.export", json!({"pane_id": pane.pane}))["layout"]["zoomed"] == true
    }

    fn tab_ids(&self, workspace: &str) -> Vec<String> {
        let tabs = self.call("tab.list", json!({"workspace_id": workspace}));
        tabs["tabs"]
            .as_array()
            .expect("tabs")
            .iter()
            .map(|tab| tab["tab_id"].as_str().unwrap().to_string())
            .collect()
    }

    fn workspace_ids(&self) -> Vec<String> {
        let list = self.call("workspace.list", json!({}));
        let mut rows: Vec<(u64, String)> = list["workspaces"]
            .as_array()
            .expect("workspaces")
            .iter()
            .map(|row| {
                (
                    row["number"].as_u64().unwrap(),
                    row["workspace_id"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        rows.sort();
        rows.into_iter().map(|(_, id)| id).collect()
    }

    /// Type `text` into `pane` and give the shell a moment to act on it.
    fn type_into(&self, pane: &Made, text: &str) {
        self.call(
            "pane.send_text",
            json!({"pane_id": pane.pane, "text": text}),
        );
        std::thread::sleep(Duration::from_millis(400));
    }

    fn screen_text(&self, pane: &Made) -> String {
        let read = self.call(
            "pane.read",
            json!({"pane_id": pane.pane, "source": "visible"}),
        );
        read["read"]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    fn scroll_offset(&self, pane: &Made) -> u64 {
        pane_info(&self.client, &pane.pane)
            .expect("the pane")
            .scroll
            .expect("scroll state")
            .offset_from_bottom
    }

    /// Fill `pane` with enough output to scroll through and wait until herdr has it.
    fn fill_scrollback(&self, pane: &Made) {
        self.type_into(pane, "seq 1 300\n");
        for _ in 0..50 {
            let scroll = pane_info(&self.client, &pane.pane)
                .expect("the pane")
                .scroll
                .expect("scroll state");
            if scroll.max_offset_from_bottom >= 200 {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("the pane never got its scrollback");
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        for workspace in &self.workspaces {
            let _ = self
                .client
                .call("workspace.close", json!({"workspace_id": workspace}));
        }
    }
}

/// The three-pane tab most tests use: `a` on the left, `b` over `c` on the right.
struct ThreePanes {
    a: Made,
    b: Made,
    c: Made,
}

fn three_panes(session: &mut Session, prefix: &str) -> ThreePanes {
    let a = session.workspace(&format!("{prefix}a"));
    let b = session.split(&a, "right", &format!("{prefix}b"));
    let c = session.split(&b, "down", &format!("{prefix}c"));
    ThreePanes { a, b, c }
}

// ---- the session itself ----------------------------------------------------------------

#[test]
fn the_test_session_is_not_the_live_one() {
    let Some(session) = Session::open() else {
        return;
    };
    let socket = session
        .client
        .socket_path()
        .canonicalize()
        .expect("the test socket exists");
    if let Some(home) = std::env::var_os("HOME") {
        assert_ne!(socket, PathBuf::from(home).join(".config/herdr/herdr.sock"));
    }
    if let Some(live) = std::env::var_os("HERDR_SOCKET_PATH").filter(|value| !value.is_empty()) {
        let live = PathBuf::from(live);
        assert_ne!(socket, live.canonicalize().unwrap_or(live));
    }
    // A client for the very same socket refuses to change it once the socket is protected.
    let guarded = Client::new(session.client.socket_path())
        .with_protected_socket(session.client.socket_path());
    let err = guarded
        .call("pane.focus", json!({"pane_id": "nope"}))
        .unwrap_err();
    assert!(matches!(err, HerdrError::MutationRefused { .. }), "{err:?}");
    assert!(guarded.is_available(), "read-only calls still go through");
}

// ---- navigate ----------------------------------------------------------------------------

#[test]
fn navigate_walks_panes_then_tabs_then_workspaces() {
    let Some(mut session) = Session::open() else {
        return;
    };
    // Workspace one: tab one is `a | b`, tab two is `c`. Workspace two is `d`.
    let a = session.workspace("a");
    let b = session.split(&a, "right", "b");
    let c = session.tab(&a.workspace, "c");
    let d = session.workspace("d");
    session.focus(&a);
    let client = &session.client;

    assert_eq!(
        navigate(client, Direction::Right).unwrap(),
        NavOutcome::FocusedPane {
            pane_id: b.pane.clone()
        }
    );
    assert_eq!(session.focused(), "b");
    assert_eq!(
        navigate(client, Direction::Right).unwrap(),
        NavOutcome::SwitchedTab {
            tab_id: c.tab.clone()
        }
    );
    assert_eq!(session.focused(), "c");
    assert_eq!(
        navigate(client, Direction::Right).unwrap(),
        NavOutcome::AtEdge,
        "tabs do not wrap"
    );
    assert_eq!(session.focused(), "c");
    assert_eq!(
        navigate(client, Direction::Left).unwrap(),
        NavOutcome::SwitchedTab {
            tab_id: a.tab.clone()
        }
    );
    assert_eq!(
        session.focused(),
        "b",
        "a tab comes back with the pane it had focused"
    );
    assert_eq!(
        navigate(client, Direction::Left).unwrap(),
        NavOutcome::FocusedPane {
            pane_id: a.pane.clone()
        }
    );
    assert_eq!(
        navigate(client, Direction::Left).unwrap(),
        NavOutcome::AtEdge
    );
    assert_eq!(session.focused(), "a");

    // Nothing is below `a`, so down leaves the workspace.
    assert_eq!(
        navigate(client, Direction::Down).unwrap(),
        NavOutcome::SwitchedWorkspace {
            workspace_id: d.workspace.clone()
        }
    );
    assert_eq!(session.focused(), "d");
    assert_eq!(
        navigate(client, Direction::Down).unwrap(),
        NavOutcome::AtEdge,
        "workspaces do not wrap"
    );
    assert_eq!(
        navigate(client, Direction::Up).unwrap(),
        NavOutcome::SwitchedWorkspace {
            workspace_id: a.workspace.clone()
        }
    );
    assert_eq!(session.focused(), "a");
    assert_eq!(
        session.state(),
        "",
        "nothing was collapsed, so nothing was recorded"
    );
}

#[test]
fn navigate_follows_the_order_of_the_tab_bar() {
    let Some(mut session) = Session::open() else {
        return;
    };
    let a = session.workspace("a");
    let b = session.tab(&a.workspace, "b");
    let c = session.tab(&a.workspace, "c");
    // Drag the newest tab to the front: herdr keeps its number but lists it first.
    session.call("tab.move", json!({"tab_id": c.tab, "insert_index": 0}));
    assert_eq!(
        session.tab_ids(&a.workspace),
        vec![c.tab.clone(), a.tab.clone(), b.tab.clone()]
    );
    session.focus(&a);

    assert_eq!(
        navigate(&session.client, Direction::Left).unwrap(),
        NavOutcome::SwitchedTab {
            tab_id: c.tab.clone()
        }
    );
    assert_eq!(
        navigate(&session.client, Direction::Left).unwrap(),
        NavOutcome::AtEdge
    );
    assert_eq!(
        navigate(&session.client, Direction::Right).unwrap(),
        NavOutcome::SwitchedTab {
            tab_id: a.tab.clone()
        }
    );
    assert_eq!(
        navigate(&session.client, Direction::Right).unwrap(),
        NavOutcome::SwitchedTab {
            tab_id: b.tab.clone()
        }
    );
    assert_eq!(
        navigate(&session.client, Direction::Right).unwrap(),
        NavOutcome::AtEdge
    );
}

#[test]
fn navigate_gives_a_collapsed_pane_its_room_back() {
    let Some(mut session) = Session::open() else {
        return;
    };
    let ThreePanes { a, b, c } = three_panes(&mut session, "");
    // Squeeze `c` to the 10% herdr allows, as a collapse would, and start from `b`.
    session.set_ratio(&c, &[true], 0.9);
    session.focus(&b);
    assert_eq!(session.tree(&a), "(a |0.5 (b /0.9 c))");

    assert_eq!(
        navigate(&session.client, Direction::Down).unwrap(),
        NavOutcome::FocusedPane {
            pane_id: c.pane.clone()
        }
    );
    assert_eq!(
        session.tree(&a),
        "(a |0.5 (b /0.1 c))",
        "the split flipped in favour of the pane that got the focus"
    );
    let expected = format!(
        r#"[{{"tab_id": "{}", "pane_id": "{}", "panes": ["{}"], "path": [true], "side": false, "ratio": 0.5}}]"#,
        a.tab, b.pane, b.pane
    );
    assert_eq!(session.state(), expected);

    // Going back flips it again and keeps the one record.
    assert_eq!(
        navigate(&session.client, Direction::Up).unwrap(),
        NavOutcome::FocusedPane {
            pane_id: b.pane.clone()
        }
    );
    assert_eq!(session.tree(&a), "(a |0.5 (b /0.9 c))");
    assert_eq!(session.state().matches("tab_id").count(), 1);
    // And a toggle puts the split back to the half the record remembers.
    assert!(matches!(
        collapse(&session.client, CollapseMode::Toggle).unwrap(),
        CollapseOutcome::Restored { splits: 1, .. }
    ));
    assert_eq!(session.tree(&a), "(a |0.5 (b /0.5 c))");
    assert_eq!(session.state(), "[]");
}

#[test]
fn navigate_in_a_zoomed_tab_moves_the_zoom_along() {
    let Some(mut session) = Session::open() else {
        return;
    };
    let a = session.workspace("a");
    let b = session.split(&a, "right", "b");
    session.focus(&a);
    assert_eq!(
        zoom_toggle(&session.client).unwrap(),
        ZoomOutcome {
            zoomed: true,
            changed: true
        }
    );
    assert_eq!(
        navigate(&session.client, Direction::Right).unwrap(),
        NavOutcome::FocusedPane {
            pane_id: b.pane.clone()
        }
    );
    assert_eq!(session.focused(), "b");
    assert!(
        session.zoomed(&a),
        "herdr keeps the tab zoomed, now on the other pane"
    );
}

// ---- move_pane ---------------------------------------------------------------------------

#[test]
fn move_pane_swaps_along_a_row_and_turns_splits() {
    let Some(mut session) = Session::open() else {
        return;
    };
    let a = session.workspace("a");
    let b = session.split(&a, "right", "b");
    session.focus(&a);
    let tabs_before = session.tab_ids(&a.workspace);
    let client = &session.client;

    assert_eq!(
        move_pane(client, Direction::Right).unwrap(),
        MoveOutcome::Swapped {
            with_pane_id: b.pane.clone()
        }
    );
    assert_eq!(session.tree(&a), "(b |0.5 a)");
    assert_eq!(session.focused(), "a", "the moved pane keeps the focus");
    assert_eq!(
        move_pane(client, Direction::Right).unwrap(),
        MoveOutcome::Unchanged,
        "already at the right edge"
    );

    // Down out of a row: `a` goes under `b`.
    assert_eq!(
        move_pane(client, Direction::Down).unwrap(),
        MoveOutcome::Reoriented {
            beside_pane_id: b.pane.clone()
        }
    );
    assert_eq!(session.tree(&a), "(b /0.5 a)");
    assert_eq!(session.focused(), "a");
    assert_eq!(
        move_pane(client, Direction::Up).unwrap(),
        MoveOutcome::Swapped {
            with_pane_id: b.pane.clone()
        }
    );
    assert_eq!(session.tree(&a), "(a /0.5 b)");

    // Left out of a stack: `b` is sent to the far side so `a` ends up on the left.
    assert_eq!(
        move_pane(client, Direction::Left).unwrap(),
        MoveOutcome::Reoriented {
            beside_pane_id: b.pane.clone()
        }
    );
    assert_eq!(session.tree(&a), "(a |0.5 b)");
    assert_eq!(session.focused(), "a");
    assert_eq!(
        move_pane(client, Direction::Left).unwrap(),
        MoveOutcome::Unchanged
    );
    assert_eq!(
        session.tab_ids(&a.workspace),
        tabs_before,
        "the temporary tab is gone again"
    );
}

#[test]
fn move_pane_unzooms_first_and_handles_split_siblings() {
    let Some(mut session) = Session::open() else {
        return;
    };
    // (a | b) over c.
    let a = session.workspace("a");
    let c = session.split(&a, "down", "c");
    let b = session.split(&a, "right", "b");
    assert_eq!(session.tree(&a), "((a |0.5 b) /0.5 c)");

    session.focus(&c);
    assert_eq!(
        move_pane(&session.client, Direction::Left).unwrap(),
        MoveOutcome::Unchanged,
        "not past a split sibling"
    );
    // Right: `c` lands beside the first pane of the row above.
    assert_eq!(
        move_pane(&session.client, Direction::Right).unwrap(),
        MoveOutcome::Reoriented {
            beside_pane_id: a.pane.clone()
        }
    );
    assert_eq!(session.tree(&a), "((a |0.5 c) |0.5 b)");
    assert_eq!(session.focused(), "c");

    assert_eq!(
        zoom_toggle(&session.client).unwrap(),
        ZoomOutcome {
            zoomed: true,
            changed: true
        }
    );
    assert_eq!(
        move_pane(&session.client, Direction::Right).unwrap(),
        MoveOutcome::Swapped {
            with_pane_id: b.pane.clone()
        }
    );
    assert!(!session.zoomed(&a));
    assert_eq!(session.tree(&a), "((a |0.5 b) |0.5 c)");
}

#[test]
fn move_pane_in_a_single_pane_tab_does_nothing() {
    let Some(mut session) = Session::open() else {
        return;
    };
    let a = session.workspace("a");
    for direction in Direction::ALL {
        assert_eq!(
            move_pane(&session.client, direction).unwrap(),
            MoveOutcome::Unchanged
        );
    }
    assert_eq!(session.tree(&a), "a");
}

// ---- collapse ----------------------------------------------------------------------------

#[test]
fn collapse_toggle_squeezes_and_restores() {
    let Some(mut session) = Session::open() else {
        return;
    };
    let ThreePanes { a, b, c } = three_panes(&mut session, "");
    session.set_ratio(&c, &[true], 0.3);
    session.focus(&c);

    let outcome = collapse(&session.client, CollapseMode::Toggle).unwrap();
    assert_eq!(
        outcome,
        CollapseOutcome::Collapsed {
            pane_id: c.pane.clone(),
            focused_pane_id: b.pane.clone()
        }
    );
    assert_eq!(session.tree(&a), "(a |0.5 (b /0.9 c))");
    assert_eq!(session.focused(), "b");
    let recorded = format!(
        r#"[{{"tab_id": "{}", "pane_id": "{}", "panes": ["{}"], "path": [true], "side": true, "ratio": 0.3, "from_expand": false}}]"#,
        a.tab, c.pane, c.pane
    );
    assert_eq!(session.state(), recorded);

    let outcome = collapse(&session.client, CollapseMode::Toggle).unwrap();
    assert_eq!(
        outcome,
        CollapseOutcome::Restored {
            splits: 1,
            focused_pane_id: c.pane.clone()
        }
    );
    assert_eq!(session.tree(&a), "(a |0.5 (b /0.3 c))");
    assert_eq!(session.focused(), "c");
    assert_eq!(session.state(), "[]");
}

#[test]
fn collapse_expand_grows_along_every_split_and_undoes_itself() {
    let Some(mut session) = Session::open() else {
        return;
    };
    let ThreePanes { a, b: _, c } = three_panes(&mut session, "");
    session.focus(&c);

    let outcome = collapse(&session.client, CollapseMode::Expand).unwrap();
    assert_eq!(
        outcome,
        CollapseOutcome::Expanded {
            pane_id: c.pane.clone(),
            splits: 2
        }
    );
    assert_eq!(session.tree(&a), "(a |0.1 (b /0.1 c))");
    assert_eq!(session.focused(), "c");
    assert_eq!(session.state().matches("\"from_expand\": true").count(), 2);

    let outcome = collapse(&session.client, CollapseMode::Expand).unwrap();
    assert_eq!(
        outcome,
        CollapseOutcome::Restored {
            splits: 2,
            focused_pane_id: c.pane.clone()
        }
    );
    assert_eq!(session.tree(&a), "(a |0.5 (b /0.5 c))");
    assert_eq!(session.state(), "[]");

    // Expanded by hand with nothing on record: pressing expand evens the splits out.
    session.set_ratio(&c, &[], 0.1);
    session.set_ratio(&c, &[true], 0.1);
    let outcome = collapse(&session.client, CollapseMode::Expand).unwrap();
    assert_eq!(
        outcome,
        CollapseOutcome::Evened {
            pane_id: c.pane.clone(),
            splits: 2
        }
    );
    assert_eq!(session.tree(&a), "(a |0.5 (b /0.5 c))");
}

#[test]
fn collapse_auto_flips_only_for_the_focused_pane() {
    let Some(mut session) = Session::open() else {
        return;
    };
    let ThreePanes { a, b, c } = three_panes(&mut session, "");
    session.focus(&a);
    assert_eq!(
        collapse(&session.client, CollapseMode::Auto).unwrap(),
        CollapseOutcome::Unchanged
    );

    // Collapse `a`, then pretend a click put the focus back on it.
    assert!(matches!(
        collapse(&session.client, CollapseMode::Toggle).unwrap(),
        CollapseOutcome::Collapsed { .. }
    ));
    assert_eq!(session.tree(&a), "(a |0.1 (b /0.5 c))");
    assert_eq!(session.focused(), "b");
    // An event naming a pane that is not focused changes nothing.
    assert_eq!(
        collapse_pane(&session.client, CollapseMode::Auto, &a.pane).unwrap(),
        CollapseOutcome::Unchanged
    );
    assert_eq!(session.tree(&a), "(a |0.1 (b /0.5 c))");

    session.focus(&a);
    let outcome = collapse_pane(&session.client, CollapseMode::Auto, &a.pane).unwrap();
    assert_eq!(
        outcome,
        CollapseOutcome::AutoExpanded {
            pane_id: a.pane.clone(),
            splits: 1
        }
    );
    assert_eq!(session.tree(&a), "(a |0.9 (b /0.5 c))");
    // The record now says the right side is the squeezed one, with the ratio from before.
    let recorded = format!(
        r#"[{{"tab_id": "{}", "pane_id": "{}", "panes": ["{}", "{}"], "path": [], "side": true, "ratio": 0.5, "from_expand": false}}]"#,
        a.tab, b.pane, b.pane, c.pane
    );
    assert_eq!(session.state(), recorded);
    // One collapse on record: the toggle restores it and hands the focus to the pane it names.
    let outcome = collapse(&session.client, CollapseMode::Toggle).unwrap();
    assert_eq!(
        outcome,
        CollapseOutcome::Restored {
            splits: 1,
            focused_pane_id: b.pane.clone()
        }
    );
    assert_eq!(session.tree(&a), "(a |0.5 (b /0.5 c))");
}

#[test]
fn collapse_in_a_single_pane_tab_does_nothing() {
    let Some(mut session) = Session::open() else {
        return;
    };
    let a = session.workspace("a");
    assert_eq!(
        collapse(&session.client, CollapseMode::Toggle).unwrap(),
        CollapseOutcome::SinglePane
    );
    assert_eq!(
        collapse(&session.client, CollapseMode::Expand).unwrap(),
        CollapseOutcome::SinglePane
    );
    assert_eq!(
        collapse(&session.client, CollapseMode::Auto).unwrap(),
        CollapseOutcome::Unchanged
    );
    assert_eq!(session.tree(&a), "a");
    let err = collapse_pane(&session.client, CollapseMode::Toggle, "w999:p999").unwrap_err();
    assert!(
        matches!(err.code(), Some("layout_not_found" | "pane_not_found")),
        "{err:?}"
    );
}

// ---- scroll ------------------------------------------------------------------------------

#[test]
fn scroll_moves_through_the_scrollback() {
    let Some(mut session) = Session::open() else {
        return;
    };
    let a = session.workspace("a");
    session.fill_scrollback(&a);
    let client = &session.client;

    assert_eq!(
        scroll(client, true, 3).unwrap(),
        ScrollOutcome::Scrolled {
            offset_from_bottom: 3
        }
    );
    assert_eq!(
        scroll(client, true, 9).unwrap(),
        ScrollOutcome::Scrolled {
            offset_from_bottom: 12
        }
    );
    assert_eq!(session.scroll_offset(&a), 12);
    assert_eq!(
        scroll(client, false, 3).unwrap(),
        ScrollOutcome::Scrolled {
            offset_from_bottom: 9
        }
    );
    assert_eq!(
        scroll(client, false, 100).unwrap(),
        ScrollOutcome::Scrolled {
            offset_from_bottom: 0
        }
    );
    assert_eq!(scroll(client, false, 3).unwrap(), ScrollOutcome::Unchanged);
    assert_eq!(scroll(client, true, 0).unwrap(), ScrollOutcome::Unchanged);

    let top = pane_info(client, &a.pane)
        .unwrap()
        .scroll
        .unwrap()
        .max_offset_from_bottom;
    assert_eq!(
        scroll(client, true, u32::MAX).unwrap(),
        ScrollOutcome::Scrolled {
            offset_from_bottom: top
        }
    );
    assert_eq!(scroll(client, true, 3).unwrap(), ScrollOutcome::Unchanged);
    assert_eq!(session.scroll_offset(&a), top);
}

#[test]
fn scroll_sends_wheel_events_to_a_full_screen_program() {
    let Some(mut session) = Session::open() else {
        return;
    };
    let a = session.workspace("a");
    // The alternate screen has no scrollback; `cat -v` shows what the terminal is sent.
    session.type_into(&a, "printf '\\033[?1049h'; cat -v\n");
    let rows = pane_info(&session.client, &a.pane).unwrap().scroll.unwrap();
    assert_eq!(
        rows.max_offset_from_bottom, 0,
        "the alternate screen reports no scrollback"
    );
    let middle = (rows.viewport_rows / 2).max(2);

    assert_eq!(
        scroll(&session.client, true, 3).unwrap(),
        ScrollOutcome::WheelSent { events: 1 }
    );
    assert_eq!(
        scroll(&session.client, false, 9).unwrap(),
        ScrollOutcome::WheelSent { events: 3 }
    );
    std::thread::sleep(Duration::from_millis(400));
    let screen = session.screen_text(&a);
    let up = format!("^[[<64;2;{middle}M");
    let down = format!("^[[<65;2;{middle}M");
    assert_eq!(screen.matches(&up).count(), 1, "{screen:?}");
    assert_eq!(screen.matches(&down).count(), 3, "{screen:?}");
}

// ---- zoom and workspaces -----------------------------------------------------------------

#[test]
fn zoom_toggles_and_needs_two_panes() {
    let Some(mut session) = Session::open() else {
        return;
    };
    let a = session.workspace("a");
    assert_eq!(
        zoom_toggle(&session.client).unwrap(),
        ZoomOutcome {
            zoomed: false,
            changed: false
        }
    );
    session.split(&a, "right", "b");
    assert_eq!(
        zoom_toggle(&session.client).unwrap(),
        ZoomOutcome {
            zoomed: true,
            changed: true
        }
    );
    assert!(session.zoomed(&a));
    assert_eq!(
        zoom_toggle(&session.client).unwrap(),
        ZoomOutcome {
            zoomed: false,
            changed: true
        }
    );
    assert!(!session.zoomed(&a));
}

#[test]
fn workspaces_cycle_and_are_reached_by_number() {
    let Some(mut session) = Session::open() else {
        return;
    };
    let first = session.workspace("a");
    let second = session.workspace("b");
    let third = session.workspace("c");
    let order = session.workspace_ids();
    let position = |id: &str| {
        order
            .iter()
            .position(|workspace| workspace == id)
            .expect("the workspace is listed")
    };
    let switched = |id: &str| WorkspaceOutcome::Switched {
        workspace_id: id.to_string(),
    };
    let client = &session.client;

    // `third` is the last one and has the focus.
    assert_eq!(position(&third.workspace), order.len() - 1);
    assert_eq!(
        previous_workspace(client).unwrap(),
        switched(&second.workspace)
    );
    assert_eq!(
        previous_workspace(client).unwrap(),
        switched(&first.workspace)
    );
    assert_eq!(session.focused(), "a");
    assert_eq!(next_workspace(client).unwrap(), switched(&second.workspace));
    assert_eq!(next_workspace(client).unwrap(), switched(&third.workspace));
    // Past the last one it goes round to the first workspace of the session.
    assert_eq!(next_workspace(client).unwrap(), switched(&order[0]));
    assert_eq!(
        previous_workspace(client).unwrap(),
        switched(&third.workspace)
    );

    let number = position(&first.workspace) + 1;
    assert_eq!(
        focus_workspace_number(client, number).unwrap(),
        switched(&first.workspace)
    );
    assert_eq!(session.focused(), "a");
    assert_eq!(
        focus_workspace_number(client, number).unwrap(),
        WorkspaceOutcome::Unchanged
    );
    assert_eq!(
        focus_workspace_number(client, order.len() + 1).unwrap(),
        WorkspaceOutcome::Unchanged
    );
    assert_eq!(
        focus_workspace_number(client, 0).unwrap(),
        WorkspaceOutcome::Unchanged
    );
    assert_eq!(session.focused(), "a");
}

// ---- queries -----------------------------------------------------------------------------

#[test]
fn focused_pane_and_pane_at_know_the_working_directories() {
    let Some(mut session) = Session::open() else {
        return;
    };
    let a = session.workspace("a");
    let b = session.split(&a, "right", "b");
    session.focus(&a);
    let client = &session.client;

    let pane = focused_pane(client).unwrap();
    assert_eq!(pane.pane_id, a.pane);
    assert_eq!(
        (pane.tab_id.as_str(), pane.workspace_id.as_str()),
        (a.tab.as_str(), a.workspace.as_str())
    );
    assert!(pane.focused);
    assert_eq!(pane.cwd.as_deref(), Some(session.dir("a").as_path()));
    assert_eq!(pane.working_dir(), Some(session.dir("a").as_path()));

    // With no client attached herdr lays out for its headless size and draws no chrome, so a
    // screen of exactly that size has the pane area at its corner.
    let layout = pane_layout(client).unwrap();
    assert_eq!((layout.area.x, layout.area.y), (0, 0));
    let screen = ScreenSize {
        cols: layout.area.width,
        rows: layout.area.height,
    };
    assert_eq!(layout.origin_on(screen), Some((0, 0)));

    let left = pane_at(client, screen, 0, 0)
        .unwrap()
        .expect("a pane at the top-left cell");
    assert_eq!(left.pane.pane_id, a.pane);
    assert_eq!(left.pane.working_dir(), Some(session.dir("a").as_path()));
    let right = pane_at(client, screen, screen.cols - 1, screen.rows - 1)
        .unwrap()
        .expect("a pane at the last cell");
    assert_eq!(right.pane.pane_id, b.pane);
    assert_eq!(right.pane.working_dir(), Some(session.dir("b").as_path()));
    assert_eq!(left.rect.width + right.rect.width, screen.cols);
    assert_eq!(pane_at(client, screen, screen.cols, 0).unwrap(), None);

    // A wider screen means chrome on the left and on top: the same panes, shifted.
    let wider = ScreenSize {
        cols: screen.cols + 26,
        rows: screen.rows + 1,
    };
    assert_eq!(pane_at(client, wider, 25, 5).unwrap(), None, "the sidebar");
    assert_eq!(pane_at(client, wider, 40, 0).unwrap(), None, "the tab bar");
    assert_eq!(
        pane_at(client, wider, 26, 1)
            .unwrap()
            .expect("the first pane cell")
            .pane
            .pane_id,
        a.pane
    );
    // A screen smaller than the layout cannot be the one herdr laid out for.
    let smaller = ScreenSize {
        cols: screen.cols - 1,
        rows: screen.rows,
    };
    assert_eq!(pane_at(client, smaller, 0, 0).unwrap(), None);

    // Zoomed, the focused pane is everywhere.
    assert_eq!(
        zoom_toggle(client).unwrap(),
        ZoomOutcome {
            zoomed: true,
            changed: true
        }
    );
    let anywhere = pane_at(client, screen, screen.cols - 1, 0)
        .unwrap()
        .expect("the zoomed pane");
    assert_eq!(anywhere.pane.pane_id, a.pane);
    assert_eq!(
        (anywhere.rect.width, anywhere.rect.height),
        (screen.cols, screen.rows)
    );
}

#[test]
fn the_foreground_directory_follows_a_cd() {
    let Some(mut session) = Session::open() else {
        return;
    };
    let a = session.workspace("a");
    let elsewhere = session.dir("elsewhere");
    session.type_into(&a, &format!("cd '{}'\n", elsewhere.display()));
    for _ in 0..30 {
        if focused_pane(&session.client).unwrap().working_dir() == Some(elsewhere.as_path()) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!(
        "herdr never reported the new directory: {:?}",
        focused_pane(&session.client).unwrap()
    );
}

// ---- root_split --------------------------------------------------------------------------

#[test]
fn root_split_adds_a_pane_beside_the_whole_layout() {
    let Some(mut session) = Session::open() else {
        return;
    };
    let ThreePanes { a, b, c } = three_panes(&mut session, "");
    session.set_ratio(&a, &[], 0.3);
    session.set_ratio(&a, &[true], 0.6);
    session.focus(&b);
    let tabs_before = session.tab_ids(&a.workspace);

    let right = root_split(&session.client, SplitDirection::Right).unwrap();
    session.names.insert(right.clone(), "right".to_string());
    assert_eq!(session.tree(&a), "((a |0.3 (b /0.6 c)) |0.6 right)");
    assert_eq!(session.focused(), "right");
    let new_pane = pane_info(&session.client, &right).unwrap();
    assert_eq!(new_pane.tab_id, a.tab);
    assert_eq!(
        new_pane.cwd.as_deref(),
        Some(session.dir("b").as_path()),
        "it starts where the focused pane was"
    );

    // Again along the bottom, from a zoomed tab, with the new pane taking a quarter.
    session.focus(&c);
    assert_eq!(
        zoom_toggle(&session.client).unwrap(),
        ZoomOutcome {
            zoomed: true,
            changed: true
        }
    );
    let bottom =
        bakedit_herdr::root_split_with_share(&session.client, SplitDirection::Down, 0.25).unwrap();
    session.names.insert(bottom, "bottom".to_string());
    assert_eq!(
        session.tree(&a),
        "(((a |0.3 (b /0.6 c)) |0.6 right) /0.75 bottom)"
    );
    assert_eq!(session.focused(), "bottom");
    assert!(!session.zoomed(&a));
    assert_eq!(
        session.tab_ids(&a.workspace),
        tabs_before,
        "the temporary tab is gone again"
    );
}

#[test]
fn root_split_of_a_single_pane_is_a_plain_split() {
    let Some(mut session) = Session::open() else {
        return;
    };
    let a = session.workspace("a");
    let new = root_split(&session.client, SplitDirection::Down).unwrap();
    session.names.insert(new, "new".to_string());
    assert_eq!(session.tree(&a), "(a /0.6 new)");
    assert_eq!(session.focused(), "new");
}

// ---- the same steps through the user's scripts --------------------------------------------

/// The user's scripts, run in a sandbox that can only reach the test session.
struct Scripts {
    dir: PathBuf,
    home: PathBuf,
    cache: PathBuf,
    socket: PathBuf,
    path_var: OsString,
}

impl Scripts {
    /// The scripts, or `None` (after saying why) when they were not provided or cannot run.
    fn find(session: &Session) -> Option<Scripts> {
        let Some(dir) = std::env::var_os(SCRIPTS_VAR)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
        else {
            eprintln!("skipped: {SCRIPTS_VAR} is not set");
            return None;
        };
        let path_var = std::env::var_os("PATH").unwrap_or_default();
        let on_path =
            |program: &str| std::env::split_paths(&path_var).any(|dir| dir.join(program).is_file());
        if !on_path("python3") || !on_path("herdr") {
            eprintln!("skipped: python3 and herdr must be on PATH to run the scripts");
            return None;
        }
        // A home of its own: the scripts find `~/.local/bin/herdr-collapse` there, and the
        // socket they fall back to (`~/.config/herdr/herdr.sock`) is the test session's too.
        let home = session.scratch.path().join("home");
        std::fs::create_dir_all(home.join(".local/bin")).unwrap();
        std::fs::create_dir_all(home.join(".config/herdr")).unwrap();
        std::fs::copy(
            dir.join("herdr-collapse"),
            home.join(".local/bin/herdr-collapse"),
        )
        .expect("the collapse script");
        let socket = session.client.socket_path().to_path_buf();
        std::os::unix::fs::symlink(&socket, home.join(".config/herdr/herdr.sock")).unwrap();
        let cache = session.scratch.path().join("cache");
        Some(Scripts {
            dir,
            home,
            cache,
            socket,
            path_var,
        })
    }

    /// `command` with nothing in its environment but the sandbox.
    fn sandboxed(&self, mut command: Command) -> Command {
        command
            .env_clear()
            .env("PATH", &self.path_var)
            .env("HOME", &self.home)
            .env("XDG_CACHE_HOME", &self.cache)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("HERDR_SOCKET_PATH", &self.socket);
        command
    }

    /// The script file started the way herdr starts it, through its `#!` line.
    fn direct(&self, script: &str, args: &[&str]) -> Command {
        let mut command = Command::new(self.dir.join(script));
        command.args(args);
        self.sandboxed(command)
    }

    /// Run a script to the end and return what it printed.
    fn run(&self, script: &str, args: &[&str]) -> String {
        let mut command = Command::new("python3");
        command.arg(self.dir.join(script)).args(args);
        let output = self
            .sandboxed(command)
            .output()
            .unwrap_or_else(|err| panic!("{script} did not start: {err}"));
        assert!(
            output.status.success(),
            "{script} {args:?} failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }
}

/// What herdr and the state file look like at one moment.
#[derive(Debug, Clone, PartialEq)]
struct Picture {
    focused: String,
    tree: String,
    state: String,
}

fn picture(session: &Session, tab_of: &Made) -> Picture {
    Picture {
        focused: session.focused(),
        tree: session.tree(tab_of),
        state: session.state(),
    }
}

#[test]
fn navigate_matches_the_nav_script_step_for_step() {
    let Some(mut session) = Session::open() else {
        return;
    };
    let Some(scripts) = Scripts::find(&session) else {
        return;
    };
    // Workspace one: tab one is `a | (b / c)` with `c` collapsed, tab two is `d`. Workspace
    // two is `e`.
    let ThreePanes { a, b: _, c } = three_panes(&mut session, "");
    let _d = session.tab(&a.workspace, "d");
    let _e = session.workspace("e");
    let reset = |session: &Session| {
        session.set_ratio(&a, &[], 0.5);
        session.set_ratio(&a, &[true], 0.9);
        session.forget_state();
        session.focus(&c);
        session.focus(&a);
    };
    use Direction::{Down, Left, Right, Up};
    let walk = [
        Right, Down, Up, Down, Right, Right, Left, Up, Left, Left, Down, Down, Up, Up, Right,
    ];

    reset(&session);
    let mut by_script = Vec::new();
    for direction in walk {
        scripts.run("herdr-nav", &[direction.as_str()]);
        by_script.push((direction, picture(&session, &a)));
    }
    reset(&session);
    let mut by_crate = Vec::new();
    for direction in walk {
        navigate(&session.client, direction).unwrap();
        by_crate.push((direction, picture(&session, &a)));
    }

    for (step, (script, ours)) in by_script.iter().zip(&by_crate).enumerate() {
        assert_eq!(
            script, ours,
            "step {step} ({}) differs: script on the left",
            script.0
        );
        let (direction, picture) = ours;
        eprintln!(
            "nav {step:>2} {direction:<5} -> {:<4} {} {}",
            picture.focused, picture.tree, picture.state
        );
    }
    // The walk really did go everywhere: every pane had the focus at some point, and the
    // collapsed split was flipped along the way.
    for name in ["a", "b", "c", "d", "e"] {
        assert!(
            by_crate.iter().any(|(_, picture)| picture.focused == name),
            "{name} was never focused"
        );
    }
    assert!(by_crate
        .iter()
        .any(|(_, picture)| picture.tree == "(a |0.5 (b /0.1 c))"));
    assert!(by_crate
        .iter()
        .any(|(_, picture)| !picture.state.is_empty()));
}

#[test]
fn collapse_matches_the_collapse_script_step_for_step() {
    let Some(mut session) = Session::open() else {
        return;
    };
    let Some(scripts) = Scripts::find(&session) else {
        return;
    };
    let ThreePanes { a, b, c } = three_panes(&mut session, "");
    let reset = |session: &Session| {
        session.set_ratio(&a, &[], 0.5);
        session.set_ratio(&a, &[true], 0.3);
        session.forget_state();
        session.focus(&a);
    };
    // Each step: the pane to focus first (if any), then the mode.
    use CollapseMode::{Auto, Expand, Toggle};
    let steps: Vec<(Option<&Made>, CollapseMode)> = vec![
        (Some(&c), Toggle),
        (None, Toggle),
        (Some(&a), Expand),
        (None, Expand),
        (Some(&c), Expand),
        (None, Toggle),
        (Some(&b), Toggle),
        (Some(&b), Auto),
        (None, Toggle),
        (Some(&a), Toggle),
        (Some(&a), Auto),
        (None, Expand),
        (Some(&c), Toggle),
        (Some(&c), Auto),
        (Some(&a), Expand),
        (Some(&b), Auto),
        (None, Expand),
        (None, Expand),
        (Some(&a), Auto),
        (None, Toggle),
        (None, Toggle),
    ];
    let script_args = |mode: CollapseMode| -> &'static [&'static str] {
        match mode {
            Toggle => &[],
            Expand => &["expand"],
            Auto => &["auto"],
        }
    };

    reset(&session);
    let mut by_script = Vec::new();
    for (focus, mode) in &steps {
        if let Some(pane) = focus {
            session.focus(pane);
        }
        scripts.run("herdr-collapse", script_args(*mode));
        by_script.push(picture(&session, &a));
    }
    reset(&session);
    let mut by_crate = Vec::new();
    let mut outcomes = Vec::new();
    for (focus, mode) in &steps {
        if let Some(pane) = focus {
            session.focus(pane);
        }
        outcomes.push(collapse(&session.client, *mode).unwrap());
        by_crate.push(picture(&session, &a));
    }

    for (step, (script, ours)) in by_script.iter().zip(&by_crate).enumerate() {
        assert_eq!(
            script, ours,
            "step {step} {:?} differs: script on the left; ours did {:?}",
            steps[step].1, outcomes[step]
        );
        eprintln!(
            "collapse {step:>2} {:<6} -> {:<2} {} {}",
            format!("{:?}", steps[step].1),
            ours.focused,
            ours.tree,
            ours.state
        );
    }
    // The steps covered every kind of outcome that changes something.
    let did = |wanted: fn(&CollapseOutcome) -> bool| outcomes.iter().any(wanted);
    assert!(did(|outcome| matches!(
        outcome,
        CollapseOutcome::Collapsed { .. }
    )));
    assert!(did(|outcome| matches!(
        outcome,
        CollapseOutcome::Restored { .. }
    )));
    assert!(did(|outcome| matches!(
        outcome,
        CollapseOutcome::Expanded { .. }
    )));
    assert!(did(|outcome| matches!(
        outcome,
        CollapseOutcome::AutoExpanded { .. }
    )));
}

#[test]
fn collapse_reads_what_the_script_wrote_and_the_other_way_round() {
    let Some(mut session) = Session::open() else {
        return;
    };
    let Some(scripts) = Scripts::find(&session) else {
        return;
    };
    let ThreePanes { a, b: _, c } = three_panes(&mut session, "");
    session.set_ratio(&a, &[true], 0.3);

    // The script collapses, this crate restores.
    session.focus(&c);
    scripts.run("herdr-collapse", &[]);
    assert_eq!(session.tree(&a), "(a |0.5 (b /0.9 c))");
    assert_eq!(
        collapse(&session.client, CollapseMode::Toggle).unwrap(),
        CollapseOutcome::Restored {
            splits: 1,
            focused_pane_id: c.pane.clone()
        }
    );
    assert_eq!(session.tree(&a), "(a |0.5 (b /0.3 c))");

    // This crate expands, the script restores.
    session.focus(&a);
    assert!(matches!(
        collapse(&session.client, CollapseMode::Expand).unwrap(),
        CollapseOutcome::Expanded { .. }
    ));
    assert_eq!(session.tree(&a), "(a |0.9 (b /0.3 c))");
    let said = scripts.run("herdr-collapse", &[]);
    assert!(said.contains("restored 1 split(s)"), "{said}");
    assert_eq!(session.tree(&a), "(a |0.5 (b /0.3 c))");
    assert_eq!(session.state(), "[]");
}

#[test]
fn move_pane_matches_the_move_script() {
    let Some(mut session) = Session::open() else {
        return;
    };
    let Some(scripts) = Scripts::find(&session) else {
        return;
    };
    // Two tabs with the same three panes; the script works on the first, this crate on the
    // second, and the pictures are compared with the panes called by the same names.
    let theirs = three_panes(&mut session, "");
    let ours = three_panes(&mut session, "");
    use Direction::{Down, Left, Right, Up};
    // Each step: which pane to focus (by name), then the direction to move it.
    let steps = [
        ("c", Left),
        ("c", Left),
        ("c", Up),
        ("c", Down),
        ("a", Down),
        ("a", Right),
        ("b", Up),
        ("b", Left),
        ("a", Right),
        ("c", Right),
        ("c", Up),
        ("a", Left),
        ("b", Down),
        ("b", Right),
    ];
    let pick = |set: &ThreePanes, name: &str| match name {
        "a" => set.a.clone(),
        "b" => set.b.clone(),
        _ => set.c.clone(),
    };
    for (step, (name, direction)) in steps.into_iter().enumerate() {
        session.focus(&pick(&theirs, name));
        scripts.run("herdr-move", &[direction.as_str()]);
        let script = (session.tree(&theirs.a), session.focused());
        session.focus(&pick(&ours, name));
        let outcome = move_pane(&session.client, direction).unwrap();
        let crate_ = (session.tree(&ours.a), session.focused());
        assert_eq!(
            script, crate_,
            "step {step}: {name} {direction} (ours did {outcome:?}); script on the left"
        );
    }
}

/// `text` with every `<pane id>` (a pane the test did not name) written as `new`.
fn unnamed_as_new(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        let end = rest[start..]
            .find('>')
            .map_or(rest.len(), |end| start + end + 1);
        out.push_str(&rest[..start]);
        out.push_str("new");
        rest = &rest[end..];
    }
    out + rest
}

#[test]
fn root_split_matches_the_root_split_script() {
    let Some(mut session) = Session::open() else {
        return;
    };
    let Some(scripts) = Scripts::find(&session) else {
        return;
    };
    let theirs = three_panes(&mut session, "");
    let ours = three_panes(&mut session, "");
    for set in [&theirs, &ours] {
        session.set_ratio(&set.a, &[], 0.3);
        session.set_ratio(&set.a, &[true], 0.6);
    }
    // Panes the split made are not in the name table; call each one `new`.
    let nameless_picture = |session: &Session, set: &ThreePanes| {
        (
            unnamed_as_new(&session.tree(&set.a)),
            unnamed_as_new(&session.focused()),
        )
    };
    for (step, (focus, direction)) in [("b", SplitDirection::Right), ("a", SplitDirection::Down)]
        .into_iter()
        .enumerate()
    {
        let pick = |set: &ThreePanes| {
            if focus == "a" {
                set.a.clone()
            } else {
                set.b.clone()
            }
        };
        session.focus(&pick(&theirs));
        scripts.run("herdr-root-split", &[direction.as_str()]);
        let script = nameless_picture(&session, &theirs);
        session.focus(&pick(&ours));
        root_split(&session.client, direction).unwrap();
        let crate_ = nameless_picture(&session, &ours);
        assert_eq!(
            script, crate_,
            "step {step}: root split {direction} from {focus}; script on the left"
        );
        assert_eq!(crate_.1, "new");
    }
    assert_eq!(session.tab_ids(&ours.a.workspace).len(), 1);
    assert_eq!(session.tab_ids(&theirs.a.workspace).len(), 1);
}

#[test]
fn scroll_matches_the_scroll_script() {
    let Some(mut session) = Session::open() else {
        return;
    };
    let Some(scripts) = Scripts::find(&session) else {
        return;
    };
    let a = session.workspace("a");
    session.fill_scrollback(&a);
    let steps: [(bool, u32); 8] = [
        (true, 3),
        (true, 9),
        (false, 3),
        (true, 3),
        (false, 9),
        (false, 9),
        (false, 3),
        (true, 9),
    ];

    let mut by_script = Vec::new();
    for (up, lines) in steps {
        scripts.run(
            "herdr-scroll",
            &[if up { "up" } else { "down" }, &lines.to_string()],
        );
        by_script.push(session.scroll_offset(&a));
    }
    session.call(
        "pane.scroll",
        json!({"pane_id": a.pane, "offset_from_bottom": 0}),
    );
    let mut by_crate = Vec::new();
    for (up, lines) in steps {
        scroll(&session.client, up, lines).unwrap();
        by_crate.push(session.scroll_offset(&a));
    }
    assert_eq!(by_script, by_crate);
    assert_eq!(by_crate, vec![3, 12, 9, 12, 3, 0, 0, 9]);

    // On the alternate screen both send the same wheel reports.
    session.call(
        "pane.scroll",
        json!({"pane_id": a.pane, "offset_from_bottom": 0}),
    );
    session.type_into(&a, "printf '\\033[?1049h'; cat -v\n");
    scripts.run("herdr-scroll", &["up"]);
    scripts.run("herdr-scroll", &["down", "9"]);
    session.type_into(&a, "\n");
    let by_script = session.screen_text(&a);
    session.type_into(&a, "\x03");
    session.type_into(&a, "clear; cat -v\n");
    scroll(&session.client, true, 3).unwrap();
    scroll(&session.client, false, 9).unwrap();
    session.type_into(&a, "\n");
    let by_crate = session.screen_text(&a);
    let reports = |screen: &str| -> Vec<String> {
        screen
            .split("^[[<")
            .skip(1)
            .filter_map(|rest| rest.split('M').next())
            .map(str::to_string)
            .collect()
    };
    assert_eq!(reports(&by_script).len(), 8, "{by_script:?}");
    assert_eq!(
        reports(&by_script),
        reports(&by_crate),
        "script: {by_script:?} crate: {by_crate:?}"
    );
}

#[test]
fn navigate_is_much_faster_than_the_nav_script() {
    let Some(mut session) = Session::open() else {
        return;
    };
    let Some(scripts) = Scripts::find(&session) else {
        return;
    };
    let a = session.workspace("a");
    session.split(&a, "right", "b");
    session.focus(&a);
    const ROUNDS: usize = 15;

    let time = |step: &dyn Fn(Direction)| -> Vec<Duration> {
        let mut times = Vec::new();
        for _ in 0..ROUNDS {
            for direction in [Direction::Right, Direction::Left] {
                let started = Instant::now();
                step(direction);
                times.push(started.elapsed());
            }
        }
        times.sort();
        times
    };
    let by_script = time(&|direction| {
        let status = scripts
            .direct("herdr-nav", &[direction.as_str()])
            .status()
            .expect("herdr-nav starts");
        assert!(status.success());
    });
    assert_eq!(session.focused(), "a");
    let by_crate = time(&|direction| {
        navigate(&session.client, direction).unwrap();
    });
    assert_eq!(session.focused(), "a");

    let median = |times: &[Duration]| times[times.len() / 2];
    let mean = |times: &[Duration]| times.iter().sum::<Duration>() / times.len() as u32;
    eprintln!(
        "herdr-nav (python script):  median {:>9.3?}  mean {:>9.3?}  min {:>9.3?}  max {:>9.3?}  ({} runs)",
        median(&by_script),
        mean(&by_script),
        by_script[0],
        by_script[by_script.len() - 1],
        by_script.len()
    );
    eprintln!(
        "navigate() (this crate):    median {:>9.3?}  mean {:>9.3?}  min {:>9.3?}  max {:>9.3?}  ({} runs)",
        median(&by_crate),
        mean(&by_crate),
        by_crate[0],
        by_crate[by_crate.len() - 1],
        by_crate.len()
    );
    eprintln!(
        "navigate() is {:.0}x faster at the median",
        median(&by_script).as_secs_f64() / median(&by_crate).as_secs_f64()
    );
    assert!(median(&by_crate) < median(&by_script));
}

/// The scripts directory is only worth setting when it holds the scripts.
#[test]
fn the_scripts_directory_holds_the_scripts() {
    let Some(dir) = std::env::var_os(SCRIPTS_VAR)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
    else {
        return;
    };
    for script in [
        "herdr-nav",
        "herdr-collapse",
        "herdr-move",
        "herdr-root-split",
        "herdr-scroll",
    ] {
        assert!(
            Path::new(&dir).join(script).is_file(),
            "{script} is missing from {}",
            dir.display()
        );
    }
}
