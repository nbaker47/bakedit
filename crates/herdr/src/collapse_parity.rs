//! Collapsing checked on many random layouts, against a tab that keeps state.
//!
//! The property tests always run. The comparison with the user's `herdr-collapse` script runs
//! when `BAKEDIT_TEST_HERDR_SCRIPTS` names the directory holding it: the same layouts, state
//! files and steps go through the script and through this crate, each against a fake tab of
//! its own, and after every step the tab, the state file and the calls that were made have to
//! be the same on both sides. The script only ever sees the fake: its environment holds
//! nothing but that socket, a scratch home and a scratch cache directory.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::collapse::{collapse, collapse_pane, CollapseMode, CollapseOutcome};
use crate::layout::testing::{down, draw, leaf_ids, pane, right, FakeTab};
use crate::layout::Node;
use crate::mock::MockServer;
use crate::state::{Entry, StateFile};
use crate::Client;

const SCRIPTS_VAR: &str = "BAKEDIT_TEST_HERDR_SCRIPTS";
const TAB: &str = "w1:t1";

/// Ratios herdr could report, crowded around the thresholds the logic turns on.
const RATIOS: [f64; 13] = [
    0.1, 0.11, 0.12, 0.13, 0.2, 0.3, 0.42, 0.5, 0.7, 0.87, 0.88, 0.89, 0.9,
];
const MODES: [CollapseMode; 3] = [
    CollapseMode::Toggle,
    CollapseMode::Expand,
    CollapseMode::Auto,
];

/// A small generator with a fixed seed: the cases are the same on every run.
struct Rng(u64);

impl Rng {
    fn seeded(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, limit: usize) -> usize {
        (self.next() % limit as u64) as usize
    }

    fn pick<T: Clone>(&mut self, items: &[T]) -> T {
        items[self.below(items.len())].clone()
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

/// A random split tree over `leaves`, in the JSON shape herdr exports.
fn random_tree(rng: &mut Rng, leaves: &[String]) -> Value {
    if let [only] = leaves {
        return pane(only);
    }
    let (first, second) = leaves.split_at(1 + rng.below(leaves.len() - 1));
    let (first, second) = (random_tree(rng, first), random_tree(rng, second));
    let ratio = rng.pick(&RATIOS);
    if rng.chance(50) {
        right(ratio, first, second)
    } else {
        down(ratio, first, second)
    }
}

fn random_layout(rng: &mut Rng) -> (Value, Vec<String>) {
    let leaves: Vec<String> = (1..=1 + rng.below(6)).map(|n| format!("w1:p{n}")).collect();
    (random_tree(rng, &leaves), leaves)
}

/// The path of every split in `node`.
fn split_paths(node: &Value, path: &mut Vec<bool>, found: &mut Vec<Vec<bool>>) {
    if node["type"] != "split" {
        return;
    }
    found.push(path.clone());
    for (side, child) in [(false, "first"), (true, "second")] {
        path.push(side);
        split_paths(&node[child], path, found);
        path.pop();
    }
}

fn node_at<'a>(root: &'a Value, path: &[bool]) -> &'a Value {
    path.iter().fold(root, |node, side| {
        &node[if *side { "second" } else { "first" }]
    })
}

/// A state file a tab could have inherited: entries of this tab that still fit, entries that
/// went stale, and entries of other tabs.
fn random_state(rng: &mut Rng, root: &Value) -> Vec<Entry> {
    let mut paths = Vec::new();
    split_paths(root, &mut Vec::new(), &mut paths);
    let mut entries = Vec::new();
    for _ in 0..rng.below(4) {
        let tab = if rng.chance(30) { "w9:t9" } else { TAB };
        let side = rng.chance(50);
        let from_expand = rng.pick(&[None, Some(false), Some(true)]);
        let ratio = rng.pick(&RATIOS);
        let fits = !paths.is_empty() && rng.chance(70);
        let (path, panes) = if fits {
            let path = rng.pick(&paths);
            let child = &node_at(root, &path)[if side { "second" } else { "first" }];
            (path, leaf_ids(child))
        } else {
            let path: Vec<bool> = (0..rng.below(4)).map(|_| rng.chance(50)).collect();
            (path, vec!["w1:pGone".to_string()])
        };
        let panes: Vec<&str> = panes.iter().map(String::as_str).collect();
        entries.push(Entry::new(tab, &panes, &path, side, ratio, from_expand));
    }
    entries
}

/// One collapse request: who has the focus when it is made, what is asked, and for which
/// pane when a pane is named.
#[derive(Debug, Clone)]
struct Step {
    focus: Option<String>,
    mode: CollapseMode,
    target: Option<String>,
}

fn random_steps(rng: &mut Rng, leaves: &[String], count: usize) -> Vec<Step> {
    (0..count)
        .map(|_| Step {
            focus: rng.chance(70).then(|| rng.pick(leaves)),
            mode: rng.pick(&MODES),
            target: rng.chance(20).then(|| rng.pick(leaves)),
        })
        .collect()
}

/// What can be seen from outside after a step.
#[derive(Debug, PartialEq)]
struct Seen {
    tab: String,
    state: Option<String>,
    calls: Vec<(String, Value)>,
}

/// A fake tab with a state file, for one of the two implementations.
struct Side {
    tab: Arc<Mutex<FakeTab>>,
    server: MockServer,
    dir: tempfile::TempDir,
    calls_seen: usize,
}

impl Side {
    fn new(root: &Value, focused: &str, state: &[Entry]) -> Side {
        let tab = FakeTab::new(root.clone(), focused);
        let server = FakeTab::serve(&tab);
        let side = Side {
            tab,
            server,
            dir: tempfile::tempdir().unwrap(),
            calls_seen: 0,
        };
        if !state.is_empty() {
            StateFile::at(&side.state_path()).save(state).unwrap();
        }
        side
    }

    fn state_path(&self) -> PathBuf {
        self.dir
            .path()
            .join("cache")
            .join("herdr-collapse")
            .join("state.json")
    }

    fn client(&self) -> Client {
        Client::new(self.server.socket_path()).with_state_file(self.state_path())
    }

    fn focus(&self, pane: &str) {
        self.tab.lock().unwrap().focused = pane.to_string();
    }

    fn picture(&self) -> String {
        self.tab.lock().unwrap().picture()
    }

    fn root(&self) -> Node {
        serde_json::from_value(self.tab.lock().unwrap().root.clone()).unwrap()
    }

    fn state(&self) -> Option<String> {
        std::fs::read_to_string(self.state_path()).ok()
    }

    /// The tab, the state file and the calls made since the last look.
    fn look(&mut self) -> Seen {
        let requests = self.server.requests();
        let calls = requests[self.calls_seen..]
            .iter()
            .map(|r| (r.method.clone(), r.params.clone()))
            .collect();
        self.calls_seen = requests.len();
        Seen {
            tab: self.picture(),
            state: self.state(),
            calls,
        }
    }

    fn run(&self, step: &Step) -> CollapseOutcome {
        if let Some(pane) = &step.focus {
            self.focus(pane);
        }
        let done = match &step.target {
            Some(target) => collapse_pane(&self.client(), step.mode, target),
            None => collapse(&self.client(), step.mode),
        };
        done.unwrap_or_else(|err| panic!("{step:?} failed on {}: {err}", self.picture()))
    }

    /// The same step through the script, which may only reach this side's fake tab.
    fn run_script(&self, script: &Path, step: &Step) {
        if let Some(pane) = &step.focus {
            self.focus(pane);
        }
        let mode: &[&str] = match step.mode {
            CollapseMode::Toggle => &[],
            CollapseMode::Expand => &["expand"],
            CollapseMode::Auto => &["auto"],
        };
        let mut command = Command::new("python3");
        command
            .arg(script)
            .args(mode)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.dir.path())
            .env("XDG_CACHE_HOME", self.dir.path().join("cache"))
            .env("HERDR_SOCKET_PATH", self.server.socket_path());
        if let Some(target) = &step.target {
            command.env("HERDR_COLLAPSE_PANE", target);
        }
        let output = command.output().expect("python3 runs the collapse script");
        assert!(
            output.status.success(),
            "the script failed on {step:?}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn the_collapse_script_and_this_crate_agree_on_random_layouts() {
    let Some(dir) = std::env::var_os(SCRIPTS_VAR).filter(|value| !value.is_empty()) else {
        eprintln!("skipped: {SCRIPTS_VAR} is not set");
        return;
    };
    let script = Path::new(&dir).join("herdr-collapse");
    assert!(script.is_file(), "{} is missing", script.display());

    let mut rng = Rng::seeded(1);
    let (mut steps_run, mut changes) = (0, 0);
    for case in 0..60 {
        let (root, leaves) = random_layout(&mut rng);
        let focused = rng.pick(&leaves);
        let state = if rng.chance(40) {
            random_state(&mut rng, &root)
        } else {
            Vec::new()
        };
        let steps = random_steps(&mut rng, &leaves, 5);
        let mut theirs = Side::new(&root, &focused, &state);
        let mut ours = Side::new(&root, &focused, &state);
        let start = ours.picture();

        for (number, step) in steps.iter().enumerate() {
            theirs.run_script(&script, step);
            let outcome = ours.run(step);
            let (script_saw, we_saw) = (theirs.look(), ours.look());
            assert_eq!(
                script_saw, we_saw,
                "case {case}, step {number} {step:?} (ours: {outcome:?}) from {start}: script on the left"
            );
            steps_run += 1;
            changes += usize::from(outcome.changed());
        }
    }
    eprintln!(
        "compared {steps_run} collapse steps with the script; {changes} of them changed the tab"
    );
    assert!(
        changes > steps_run / 4,
        "the cases were too tame to mean much: {changes} of {steps_run}"
    );
}

#[test]
fn toggling_twice_puts_everything_back() {
    let mut rng = Rng::seeded(2);
    let mut collapsed = 0;
    for _ in 0..150 {
        let (root, leaves) = random_layout(&mut rng);
        let focused = rng.pick(&leaves);
        let side = Side::new(&root, &focused, &[]);
        let start = side.picture();
        let step = Step {
            focus: None,
            mode: CollapseMode::Toggle,
            target: None,
        };
        match side.run(&step) {
            CollapseOutcome::Collapsed {
                pane_id,
                focused_pane_id,
            } => {
                collapsed += 1;
                assert_eq!(pane_id, focused);
                assert_ne!(focused_pane_id, focused);
                assert_ne!(side.picture(), start);
                let back = side.run(&step);
                assert_eq!(
                    back,
                    CollapseOutcome::Restored {
                        splits: 1,
                        focused_pane_id: focused.clone()
                    }
                );
                assert_eq!(side.picture(), start);
                assert_eq!(side.state().as_deref(), Some("[]"));
            }
            other => {
                assert!(!other.changed(), "{other:?} from {start}");
                assert_eq!(side.picture(), start);
            }
        }
    }
    assert!(
        collapsed > 50,
        "only {collapsed} layouts had something to collapse"
    );
}

#[test]
fn expanding_twice_puts_everything_back() {
    let mut rng = Rng::seeded(3);
    let mut expanded = 0;
    for _ in 0..150 {
        let (root, leaves) = random_layout(&mut rng);
        let focused = rng.pick(&leaves);
        let side = Side::new(&root, &focused, &[]);
        let start = side.picture();
        let step = Step {
            focus: None,
            mode: CollapseMode::Expand,
            target: None,
        };
        match side.run(&step) {
            CollapseOutcome::Expanded { pane_id, splits } => {
                expanded += 1;
                assert_eq!(pane_id, focused);
                assert_eq!(splits, side.root().find_path(&focused).unwrap().len());
                assert!(side.picture().ends_with(&format!("focus={focused}")));
                let back = side.run(&step);
                assert_eq!(
                    back,
                    CollapseOutcome::Restored {
                        splits,
                        focused_pane_id: focused.clone()
                    }
                );
                assert_eq!(side.picture(), start);
                assert_eq!(side.state().as_deref(), Some("[]"));
            }
            CollapseOutcome::Evened { .. } => {}
            other => {
                assert_eq!(other, CollapseOutcome::SinglePane, "from {start}");
                assert_eq!(side.picture(), start);
            }
        }
    }
    assert!(
        expanded > 50,
        "only {expanded} layouts had something to expand"
    );
}

#[test]
fn an_expanded_pane_has_nine_tenths_of_every_split_above_it() {
    let mut rng = Rng::seeded(4);
    for _ in 0..100 {
        let (root, leaves) = random_layout(&mut rng);
        let focused = rng.pick(&leaves);
        let side = Side::new(&root, &focused, &[]);
        let step = Step {
            focus: None,
            mode: CollapseMode::Expand,
            target: None,
        };
        if !matches!(side.run(&step), CollapseOutcome::Expanded { .. }) {
            continue;
        }
        let root = side.root();
        let path = root.find_path(&focused).unwrap();
        for depth in 0..path.len() {
            let split = root.split_at(&path[..depth]).unwrap();
            let share = if path[depth] {
                1.0 - split.ratio
            } else {
                split.ratio
            };
            assert!(
                share > 0.87,
                "{} gives {focused} only {share}",
                side.picture()
            );
        }
    }
}

#[test]
fn after_any_sequence_the_record_describes_the_tab() {
    let mut rng = Rng::seeded(5);
    for _ in 0..80 {
        let (root, leaves) = random_layout(&mut rng);
        let side = Side::new(&root, &rng.pick(&leaves), &[]);
        for step in random_steps(&mut rng, &leaves, 8) {
            let before = draw(&side.tab.lock().unwrap().root);
            let outcome = side.run(&step);
            if !outcome.changed() {
                assert_eq!(
                    draw(&side.tab.lock().unwrap().root),
                    before,
                    "{outcome:?} moved a split"
                );
            }
            let tree = side.root();
            let entries = StateFile::at(&side.state_path()).load();
            let mut paths: Vec<&[bool]> = Vec::new();
            for entry in &entries {
                assert!(entry.in_tab(TAB));
                assert!(
                    entry.is_valid_in(&tree),
                    "stale {entry:?} after {step:?} on {}",
                    side.picture()
                );
                assert!((0.1..=0.9).contains(&entry.ratio));
                assert!(
                    !paths.contains(&entry.path.as_slice()),
                    "two records for one split: {entries:?}"
                );
                paths.push(&entry.path);
            }
            assert_eq!(
                leaf_ids(&side.tab.lock().unwrap().root),
                leaves,
                "panes went missing"
            );
        }
    }
}
