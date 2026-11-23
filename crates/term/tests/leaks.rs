//! Starting and closing terminals must not leave file descriptors behind. This is the only
//! test in its file, so nothing else opens descriptors in the process while it counts.

mod common;

use std::time::{Duration, Instant};

use bakedit_term::TermConfig;
use common::{start, wait_for_process_to_go, TIMEOUT};

/// The number of file descriptors this process has open.
fn open_descriptors() -> usize {
    std::fs::read_dir("/dev/fd")
        .map(|entries| entries.count())
        .unwrap_or(0)
}

#[test]
fn closed_terminals_give_back_their_descriptors() {
    // One terminal first, so that anything set up once (the signal handler, say) is
    // already there when the baseline is taken.
    let warm_up = start(TermConfig {
        program: Some("cat".to_string()),
        ..TermConfig::default()
    });
    let pid = warm_up.term.pid();
    drop(warm_up);
    wait_for_process_to_go(pid);
    std::thread::sleep(Duration::from_millis(200));
    let baseline = open_descriptors();
    assert!(baseline > 0, "cannot count descriptors on this system");

    let mut pids = Vec::new();
    for round in 0..20 {
        let mut session = start(TermConfig {
            program: Some("cat".to_string()),
            ..TermConfig::default()
        });
        assert!(session.term.send_text(&format!("round {round}\r")));
        session.wait_for_text(&format!("round {round}"));
        pids.push(session.term.pid());
        // Half are closed while running, half after their child has exited by itself.
        if round % 2 == 0 {
            assert!(session.term.send_text("\u{4}"));
            assert_eq!(session.wait_for_exit(), Some(0));
        }
        assert!(
            open_descriptors() > baseline,
            "a running terminal holds descriptors"
        );
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
