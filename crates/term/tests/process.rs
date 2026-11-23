//! The child process: what it is started with, how it ends and what ending the terminal
//! does to it.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use bakedit_term::{find_program, TermConfig, TermError, TermEvent, Terminal};
use common::{process_exists, sh, start, wait_for_process_to_go, TIMEOUT};

fn pairs(env: &[(&str, &str)]) -> Vec<(String, String)> {
    env.iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

#[test]
fn the_child_gets_a_terminal_environment() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir
        .path()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    // cargo sets these two for the test process, so they are there to inherit.
    let script = "echo \"1 $TERM $COLORTERM\"; echo \"2 $TERM_PROGRAM ${TERM_PROGRAM_VERSION:-none}\"; \
                  echo \"3 $BAKEDIT_TEST_EXTRA ${BAKEDIT_TEST_GONE-unset} ${CARGO_MANIFEST_DIR-unset} ${CARGO_PKG_NAME-none}\"; \
                  echo \"4 $PWD\"; echo \"5 $(pwd)\"; \
                  if [ -t 0 ] && [ -t 1 ] && [ -t 2 ]; then echo '6 tty'; fi; exec cat";
    let config = TermConfig {
        cwd: Some(dir.path().to_path_buf()),
        env: pairs(&[
            ("BAKEDIT_TEST_EXTRA", "extra"),
            ("BAKEDIT_TEST_GONE", "set anyway"),
        ]),
        env_remove: vec![
            "CARGO_MANIFEST_DIR".to_string(),
            "BAKEDIT_TEST_GONE".to_string(),
        ],
        cols: 200,
        ..sh(script)
    };
    let session = start(config);
    session.wait_for_text("6 tty");
    let term = &session.term;
    assert_eq!(term.row_text(0), "1 xterm-256color truecolor");
    assert_eq!(
        term.row_text(1),
        format!("2 bakedit {}", env!("CARGO_PKG_VERSION"))
    );
    // Removal wins over a value given in `env`, an inherited variable can be removed, and
    // one that is not mentioned is inherited.
    let inherited = std::env::var("CARGO_PKG_NAME").unwrap_or_else(|_| "none".to_string());
    assert_eq!(term.row_text(2), format!("3 extra unset unset {inherited}"));
    assert!(term.row_text(3).ends_with(&marker), "{}", term.row_text(3));
    assert!(term.row_text(4).ends_with(&marker), "{}", term.row_text(4));
    assert!(session.term.is_running());
    assert!(session.term.pid() > 1);
}

#[test]
fn the_environment_given_wins_over_the_defaults() {
    let config = TermConfig {
        env: pairs(&[("TERM", "dumb"), ("TERM_PROGRAM", "other")]),
        ..sh("echo \"$TERM/$TERM_PROGRAM/$COLORTERM\"; exec cat")
    };
    let session = start(config);
    session.wait_for_text("dumb/other/truecolor");
}

#[test]
fn a_working_directory_that_is_gone_is_not_fatal() {
    let config = TermConfig {
        cwd: Some("/nonexistent/bakedit/test/dir".into()),
        ..sh("echo started; exec cat")
    };
    let session = start(config);
    session.wait_for_text("started");
}

#[test]
fn the_exit_status_is_reported_after_the_last_output() {
    let mut session = start(sh("printf 'last words'; exit 7"));
    assert_eq!(session.wait_for_exit(), Some(7));
    assert!(!session.term.is_running());
    // The output written just before exiting made it onto the screen first.
    assert_eq!(session.term.row_text(0), "last words");
    // Nobody is listening any more.
    session.wait_until("writes to be refused", |term| !term.write(b"x"));
    assert!(!session.term.kill());
    // The screen can still be read, resized and scrolled.
    session.term.resize(40, 10);
    assert_eq!(session.term.size(), (40, 10));
    assert_eq!(session.term.row_text(0), "last words");
    // And there is exactly one exit event.
    std::thread::sleep(Duration::from_millis(100));
    assert!(!session
        .events_so_far()
        .iter()
        .any(|event| matches!(event, TermEvent::Exit(_))));
}

#[test]
fn success_and_death_by_signal_are_told_apart() {
    let mut session = start(sh("exit 0"));
    assert_eq!(session.wait_for_exit(), Some(0));
    let mut session = start(sh("kill -TERM $$"));
    assert_eq!(session.wait_for_exit(), None);
}

#[test]
fn a_burst_of_output_before_exit_is_not_lost() {
    for _ in 0..5 {
        let mut session = start(sh("seq 1 3000; printf 'THE END'"));
        assert_eq!(session.wait_for_exit(), Some(0));
        assert_eq!(session.term.row_text(22), "3000");
        assert_eq!(session.term.row_text(23), "THE END");
    }
}

#[test]
fn kill_hangs_up_on_the_whole_process_group() {
    // The shell starts a second process and waits for it; both are in the child's group.
    let mut session = start(sh("sleep 300 & echo \"sleeper $!\"; wait"));
    // The line is complete once the cursor has moved below it.
    session.wait_until("the sleeper's pid", |term| {
        term.row_text(0).starts_with("sleeper ")
            && term.cursor().is_some_and(|cursor| cursor.row == 1)
    });
    let line = session.term.row_text(0);
    let sleeper: u32 = line["sleeper ".len()..]
        .parse()
        .unwrap_or_else(|_| panic!("no pid in {line:?}"));
    let shell = session.term.pid();
    assert!(process_exists(sleeper));
    assert!(process_exists(shell));

    assert!(session.term.kill());
    assert_eq!(session.wait_for_exit(), None);
    assert!(!session.term.is_running());
    wait_for_process_to_go(shell);
    wait_for_process_to_go(sleeper);
    // There is nothing left to signal.
    assert!(!session.term.kill());
}

#[test]
fn dropping_the_terminal_hangs_up_on_the_child() {
    let session = start(sh("echo waiting; exec sleep 300"));
    session.wait_for_text("waiting");
    let pid = session.term.pid();
    assert!(process_exists(pid));
    drop(session);
    // Gone, and reaped: not even a zombie is left behind.
    wait_for_process_to_go(pid);
}

#[test]
fn a_child_that_ignores_the_hangup_does_not_hold_up_the_drop() {
    let session = start(sh("trap '' HUP; echo ignoring; while :; do sleep 1; done"));
    session.wait_for_text("ignoring");
    let pid = session.term.pid();
    let started = std::time::Instant::now();
    drop(session);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "dropping must not wait for the child"
    );
    // It lost its terminal but is still there; this test owns it, so it cleans it up.
    std::thread::sleep(Duration::from_millis(200));
    if process_exists(pid) {
        let _ = std::process::Command::new("/bin/kill")
            .args(["-KILL", &pid.to_string()])
            .status();
    }
    wait_for_process_to_go(pid);
}

#[test]
fn a_program_that_does_not_exist_is_an_error_not_a_dead_terminal() {
    let on_event = || Box::new(|_: TermEvent| {});
    for program in [
        "/nonexistent/bakedit-test-program",
        "bakedit-test-no-such-program",
    ] {
        let config = TermConfig {
            program: Some(program.to_string()),
            ..TermConfig::default()
        };
        let error = Terminal::spawn(config, on_event()).expect_err("there is no such program");
        assert!(error.is_program_missing(), "{error}");
        assert!(
            matches!(&error, TermError::Spawn { program: name, .. } if name == program),
            "{error:?}"
        );
        assert!(error.to_string().contains(program), "{error}");
        assert_eq!(find_program(program), None);
    }

    // A file that is there but cannot be run is a different failure.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("not-a-program");
    std::fs::write(&file, "data").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    let config = TermConfig {
        program: Some(file.to_str().unwrap().to_string()),
        ..TermConfig::default()
    };
    let error = Terminal::spawn(config, on_event()).expect_err("the file is not executable");
    assert!(matches!(error, TermError::Spawn { .. }), "{error:?}");
    assert!(!error.is_program_missing(), "{error}");
}

#[test]
fn a_program_that_is_missing_can_fall_back_to_another() {
    let callback = || {
        let (sender, events) = mpsc::channel();
        let on_event: Box<dyn Fn(TermEvent) + Send + Sync> = Box::new(move |event| {
            let _ = sender.send(event);
        });
        (on_event, events)
    };
    let missing = || TermConfig {
        program: Some("bakedit-test-no-such-program".to_string()),
        ..TermConfig::default()
    };
    let wait_for_text = |term: &Terminal, needle: &str| {
        let deadline = Instant::now() + TIMEOUT;
        while !term.screen_text().contains(needle) {
            assert!(
                Instant::now() < deadline,
                "no {needle:?} in\n{}",
                term.screen_text()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    };

    // The first choice is not installed: the second runs, with the same callback, and the
    // caller is told why.
    let (on_event, events) = callback();
    let (term, problem) =
        Terminal::spawn_with_fallback(missing(), sh("echo second choice; exec cat"), on_event)
            .expect("the fallback starts");
    let problem = problem.expect("the first choice failed");
    assert!(problem.is_program_missing(), "{problem}");
    wait_for_text(&term, "second choice");
    assert_eq!(events.recv_timeout(TIMEOUT), Ok(TermEvent::Wakeup));
    drop(term);

    // The first choice is there: no fallback, nothing to report.
    let (on_event, _events) = callback();
    let (term, problem) = Terminal::spawn_with_fallback(
        sh("echo first choice; exec cat"),
        sh("echo second choice; exec cat"),
        on_event,
    )
    .expect("the first choice starts");
    assert!(problem.is_none(), "{problem:?}");
    wait_for_text(&term, "first choice");
    assert!(!term.screen_text().contains("second choice"));
    drop(term);

    // Neither is there: the error is the fallback's.
    let (on_event, _events) = callback();
    let other = TermConfig {
        program: Some("/nonexistent/bakedit-other".to_string()),
        ..TermConfig::default()
    };
    let error =
        Terminal::spawn_with_fallback(missing(), other, on_event).expect_err("nothing can start");
    assert!(
        matches!(&error, TermError::Spawn { program, .. } if program == "/nonexistent/bakedit-other"),
        "{error:?}"
    );
}

#[test]
fn a_command_line_from_the_settings_runs_as_written() {
    let config =
        TermConfig::command("/bin/sh -c 'echo \"args: $0 $1\"; exec cat' first 'second word'")
            .unwrap();
    let session = start(config);
    session.wait_for_text("args: first second word");
}

#[test]
fn the_default_shell_is_a_login_shell() {
    // A stand-in for the user's shell, with a home of its own so no real profile or
    // history file is read or written.
    let home = tempfile::tempdir().unwrap();
    let config = TermConfig {
        program: None,
        env: pairs(&[
            ("SHELL", "/bin/sh"),
            ("HOME", home.path().to_str().unwrap()),
            ("HISTFILE", "/dev/null"),
            ("ENV", ""),
        ]),
        cwd: Some(home.path().to_path_buf()),
        ..TermConfig::default()
    };
    let mut session = start(config);
    let pid = session.term.pid();
    // `ps` shows the name the shell was started under: a leading dash marks a login shell.
    session.wait_until("the shell to be running as -sh", |_| {
        let output = std::process::Command::new("/bin/ps")
            .args(["-o", "command=", "-p", &pid.to_string()])
            .output();
        output.is_ok_and(|output| {
            String::from_utf8_lossy(&output.stdout)
                .trim_start()
                .starts_with("-sh")
        })
    });
    assert!(session.term.is_running());
    assert!(session.term.kill());
    // Whether the shell dies of the signal or exits on it is its own business.
    let _ = session.wait_for_exit();
    wait_for_process_to_go(pid);
}

#[test]
fn many_terminals_can_run_at_once() {
    let mut sessions: Vec<_> = (0..8)
        .map(|n| start(sh(&format!("echo terminal-{n}; read line; exit {n}"))))
        .collect();
    for (n, session) in sessions.iter().enumerate() {
        session.wait_for_text(&format!("terminal-{n}"));
    }
    // Each child only hears from its own terminal.
    for (n, session) in sessions.iter_mut().enumerate().rev() {
        assert!(session.term.send_text("go\r"));
        assert_eq!(session.wait_for_exit(), Some(n as i32));
    }
}
