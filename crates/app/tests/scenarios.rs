//! The real binary, driven through `scripts/ptyshot.py` (a pty and a VT emulator answering
//! as Ghostty does): the scenarios in `tests/scenarios/*.keys` that prove what bakedit is
//! for, each checked by the `expect` lines in it. `run.sh` says what each one covers and
//! how nothing of the user's is touched.
//!
//! The scenarios need `uv` (which brings ptyshot's one dependency, pyte); without it this
//! test says so and passes. `herdr-session` also needs herdr, and is skipped without it.

use std::path::Path;
use std::process::Command;

/// What the editor is for: the panel keys, editing in nav and edit mode and the one key
/// between them, finding, git, the tree, links from the terminal, the nav layer, herdr's
/// keys, a big file, a terminal without the kitty protocol, files and actions from other
/// processes, the session, and quitting.
const ESSENTIAL: &[&str] = &[
    "kitty", "restore", "panels", "editing", "modes", "search", "git", "tree", "links", "navlayer", "herdr", "bigfile", "legacy", "ipc",
    "quit", "dump", "zoom", "herdr-session",
];

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

#[test]
fn the_essential_scenarios_pass() {
    if !on_path("uv") {
        eprintln!("skipped: uv is not installed, and ptyshot needs it for pyte");
        return;
    }
    let here = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/scenarios");
    let out = tempfile::tempdir().expect("a temp dir");
    let output = Command::new(here.join("run.sh"))
        .arg(out.path())
        .arg(env!("CARGO_BIN_EXE_bakedit"))
        .args(ESSENTIAL)
        .env("BAKEDIT_SCENARIO_PICTURES", "0")
        .output()
        .expect("run.sh runs");
    let report = String::from_utf8_lossy(&output.stdout);
    println!("{report}");
    assert!(output.status.success(), "a scenario failed (its screen is in <out>/<scenario>-failed.txt):\n{report}{}", String::from_utf8_lossy(&output.stderr));
    for scenario in ESSENTIAL {
        assert!(
            report.contains(&format!("PASS {scenario} ")) || report.contains(&format!("SKIP {scenario}:")),
            "{scenario} did not run:\n{report}"
        );
    }
}
