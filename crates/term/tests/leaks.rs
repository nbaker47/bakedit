//! Starting and closing terminals must not leave file descriptors behind, and must not
//! hand them to the children. This is the only test in its file, so nothing else opens
//! descriptors in the process while it counts.

mod common;

use std::process::Command;
use std::time::{Duration, Instant};

use bakedit_term::TermConfig;
use common::{sh, start, wait_for_process_to_go, TIMEOUT};

/// The number of file descriptors this process has open.
fn open_descriptors() -> usize {
    std::fs::read_dir("/dev/fd")
        .map(|entries| entries.count())
        .unwrap_or(0)
}

fn cat() -> TermConfig {
    TermConfig {
        program: Some("cat".to_string()),
        ..TermConfig::default()
    }
}

#[test]
fn terminals_keep_their_descriptors_to_themselves() {
    // One terminal first, so that anything set up once (the signal handler, say) is
    // already there when the baseline is taken.
    let warm_up = start(cat());
    let pid = warm_up.term.pid();
    drop(warm_up);
    wait_for_process_to_go(pid);
    std::thread::sleep(Duration::from_millis(200));
    let baseline = open_descriptors();
    assert!(baseline > 0, "cannot count descriptors on this system");

    // What a child of this process has open when no terminal is involved: standard input,
    // output and error, plus whatever `ls` opens to do the listing.
    let list = "ls /dev/fd | tr '\\n' ' '";
    let plain = Command::new("/bin/sh")
        .args(["-c", list])
        .output()
        .expect("sh runs");
    let plain = String::from_utf8_lossy(&plain.stdout).trim().to_string();
    assert!(plain.starts_with("0 1 2"), "{plain:?}");

    // A child in a terminal has exactly the same, even with another terminal open: no
    // pty, signal socket or poller of either terminal leaks into it.
    let other = start(cat());
    let listing = start(sh(&format!("{list}; echo; echo listed; exec cat")));
    listing.wait_for_text("listed");
    assert_eq!(listing.term.row_text(0), plain);
    assert!(
        open_descriptors() > baseline,
        "running terminals hold descriptors"
    );
    let pids = [other.term.pid(), listing.term.pid()];
    drop(other);
    drop(listing);
    for pid in pids {
        wait_for_process_to_go(pid);
    }

    let mut pids = Vec::new();
    for round in 0..20 {
        let mut session = start(cat());
        assert!(session.term.send_text(&format!("round {round}\r")));
        session.wait_for_text(&format!("round {round}"));
        pids.push(session.term.pid());
        // Half are closed while running, half after their child has exited by itself.
        if round % 2 == 0 {
            assert!(session.term.send_text("\u{4}"));
            assert_eq!(session.wait_for_exit(), Some(0));
        }
    }
    for pid in pids {
        wait_for_process_to_go(pid);
    }

    // The threads of the last terminals may need a moment to finish.
    let deadline = Instant::now() + TIMEOUT;
    while open_descriptors() > baseline {
        assert!(
            Instant::now() < deadline,
            "{} descriptors open, {baseline} before",
            open_descriptors()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
