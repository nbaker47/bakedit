//! herdr, a full-screen terminal multiplexer, running inside the terminal: the proof that a
//! nested TUI draws and takes keys.
//!
//! These tests start a real herdr, so they only run when asked to: `BAKEDIT_TEST_HERDR=1
//! cargo test -p bakedit-term --test herdr -- --nocapture`. They use named sessions of
//! their own (`bakedit-term-test-...-<pid>`), never the session of whoever runs them, and
//! stop and delete those sessions when they are done.

mod common;

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use bakedit_core::{Key, KeyEvent, Mods};
use bakedit_term::{find_program, inherited_env_names, TermConfig};
use common::{start, Session, TIMEOUT};

/// A herdr session that belongs to this test and is removed when the test ends, pass or
/// fail.
struct ThrowawaySession {
    program: PathBuf,
    name: String,
    removed: bool,
}

impl ThrowawaySession {
    /// Run the herdr command line client, with nothing in its environment that could
    /// point it at another session.
    fn cli(&self, args: &[&str]) -> Output {
        let mut command = Command::new(&self.program);
        command.args(args).stdin(Stdio::null());
        for name in inherited_env_names("HERDR_") {
            command.env_remove(name);
        }
        command.output().expect("herdr runs")
    }

    /// Whether `herdr session list` knows the session.
    fn is_listed(&self) -> bool {
        String::from_utf8_lossy(&self.cli(&["session", "list"]).stdout)
            .lines()
            .any(|line| line.split_whitespace().next() == Some(self.name.as_str()))
    }

    /// Stop the session's server and delete what it kept on disk.
    fn remove(&mut self) {
        if self.removed {
            return;
        }
        self.removed = true;
        for action in ["stop", "delete"] {
            let output = self.cli(&["session", action, &self.name]);
            let said = [output.stdout, output.stderr].concat();
            eprintln!(
                "herdr session {action} {}: {} {}",
                self.name,
                output.status,
                String::from_utf8_lossy(&said).trim()
            );
        }
    }
}

impl Drop for ThrowawaySession {
    fn drop(&mut self) {
        self.remove();
    }
}

/// Wait until the screen has stopped changing for a moment.
fn wait_for_quiet(session: &Session) {
    let deadline = Instant::now() + TIMEOUT;
    let mut generation = session.term.generation();
    let mut quiet_since = Instant::now();
    while quiet_since.elapsed() < Duration::from_millis(700) {
        assert!(
            Instant::now() < deadline,
            "the screen never settled\n{}",
            session.term.screen_text()
        );
        std::thread::sleep(Duration::from_millis(50));
        let now = session.term.generation();
        if now != generation {
            generation = now;
            quiet_since = Instant::now();
        }
    }
}

/// The session to use, or `None` (with the reason printed) when the test is not to run.
fn throwaway_session(purpose: &str) -> Option<ThrowawaySession> {
    if std::env::var("BAKEDIT_TEST_HERDR").as_deref() != Ok("1") {
        eprintln!("skipped: set BAKEDIT_TEST_HERDR=1 to run herdr inside the terminal");
        return None;
    }
    let Some(program) = find_program("herdr") else {
        eprintln!("skipped: herdr is not installed");
        return None;
    };
    let herdr = ThrowawaySession {
        program,
        name: format!("bakedit-term-test-{purpose}-{}", std::process::id()),
        removed: false,
    };
    assert!(
        !herdr.is_listed(),
        "a session called {} already exists",
        herdr.name
    );
    Some(herdr)
}

#[test]
fn herdr_refuses_to_start_inside_itself() {
    // What `TermConfig::default_command` relies on: a herdr that finds HERDR_ENV=1 says so
    // and exits, without touching any session. (Hiding the variable from it is what must
    // never be done for the default session.)
    let Some(mut herdr) = throwaway_session("nested") else {
        return;
    };
    let config = TermConfig {
        env: vec![("HERDR_ENV".to_string(), "1".to_string())],
        ..TermConfig::command(&format!("herdr --session {}", herdr.name)).unwrap()
    };
    let mut session = start(config);
    assert_eq!(session.wait_for_exit(), Some(1));
    let screen = session.term.screen_text();
    eprintln!("--- nested herdr ---\n{}", screen.trim_end());
    assert!(screen.contains("nested"), "{screen}");
    assert!(!session.term.modes().alt_screen);
    assert!(
        !herdr.is_listed(),
        "the refused herdr started a session anyway"
    );
    drop(session);
    herdr.remove();
}

#[test]
fn herdr_draws_and_takes_keys_inside_the_terminal() {
    let Some(mut herdr) = throwaway_session("tui") else {
        return;
    };

    // The command line a user would put in their settings, plus one thing a user would
    // not do: every HERDR_* variable is removed. This test is usually run from inside a
    // herdr pane, where herdr refuses to start; with the variables gone it does. That is
    // safe here, and only here, because of `--session`: it gets a session of its own
    // instead of attaching to the one the test is running in.
    let work = tempfile::tempdir().unwrap();
    let config = TermConfig {
        cols: 120,
        rows: 36,
        cwd: Some(work.path().to_path_buf()),
        env_remove: inherited_env_names("HERDR_"),
        ..TermConfig::command(&format!("herdr --session {}", herdr.name)).unwrap()
    };
    assert_eq!(config.program.as_deref(), Some("herdr"));
    assert_eq!(config.args, ["--session", herdr.name.as_str()]);

    let session = start(config);
    session.wait_until("herdr to take over the screen", |term| {
        term.modes().alt_screen && term.screen_text().chars().any(|c| !c.is_whitespace())
    });
    wait_for_quiet(&session);
    let term = &session.term;
    let modes = term.modes();
    eprintln!(
        "--- herdr at 120x36 ---\n{}\n--- cursor: {:?}\n--- modes: {modes:?}",
        term.screen_text(),
        term.cursor()
    );
    assert!(herdr.is_listed(), "herdr did not start the named session");
    assert!(term.is_running());
    // A full-screen, mouse-driven program that speaks the kitty keyboard protocol.
    assert!(modes.alt_screen && modes.mouse_reporting && modes.sgr_mouse);
    assert_ne!(modes.kitty_flags, 0);
    assert!(
        term.cursor().is_some(),
        "the shell in the pane shows a cursor"
    );
    for link in term.visible_links() {
        eprintln!("--- link: {link:?}");
    }

    // Keys reach the shell in herdr's pane: type something at its prompt, without running
    // it, and see it echoed through both terminals. Then take it back.
    let marker = "bakedit-nested-ok";
    for c in marker.chars() {
        assert!(term.send_key(&KeyEvent::typed(c)));
    }
    session.wait_for_text(marker);
    eprintln!(
        "--- after typing ---\n{}\n--- cursor: {:?}",
        term.screen_text(),
        term.cursor()
    );
    assert!(term.send_key(&KeyEvent::new(Key::Char('u'), Mods::CTRL)));
    session.wait_until("the typed text to be erased", |term| {
        !term.screen_text().contains(marker)
    });

    // A resize reaches it too: it redraws for the new size.
    term.resize(90, 24);
    wait_for_quiet(&session);
    eprintln!("--- herdr at 90x24 ---\n{}", term.screen_text());
    assert_eq!(term.size(), (90, 24));
    assert!(term.screen_text().chars().any(|c| !c.is_whitespace()));
    assert!(term.is_running());

    // Closing the terminal detaches the client; the session is then stopped and deleted.
    let pid = term.pid();
    drop(session);
    common::wait_for_process_to_go(pid);
    herdr.remove();
    assert!(!herdr.is_listed(), "the test session was not cleaned up");
}
