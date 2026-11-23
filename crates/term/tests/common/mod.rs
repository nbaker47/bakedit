//! Shared helpers for the integration tests: a terminal with a throwaway child in it, and
//! waits that give up with the screen in the failure message instead of hanging.

#![allow(dead_code)]

use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use bakedit_term::{TermConfig, TermEvent, Terminal};

/// How long any single wait may take. Generous, because the machine may be busy compiling.
pub const TIMEOUT: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(5);

/// A script that puts the tty in raw mode and reports what it reads.
///
/// Its first argument is written to the terminal at start, with Python string escapes
/// decoded, so a test can set modes or ask the terminal something. Then it prints `READY`.
/// From there on every chunk of input adds to a running total, printed in hex as
/// `ALL <hex>`. Input that starts with `!` is not counted: the rest of it is hex for bytes
/// the script writes to the terminal, which lets a test send escape sequences on demand.
const ECHO_SCRIPT: &str = r#"
import os, sys, termios, tty
fd = sys.stdin.fileno()
saved = termios.tcgetattr(fd)
tty.setraw(fd)
out = sys.stdout
try:
    out.write(sys.argv[1].encode().decode("unicode_escape"))
    out.write("READY\r\n")
    out.flush()
    total = b""
    while True:
        data = os.read(fd, 65536)
        if not data:
            break
        if data.startswith(b"!"):
            os.write(1, bytes.fromhex(data[1:].decode()))
            continue
        total += data
        out.write("ALL " + total.hex() + "\r\n")
        out.flush()
finally:
    termios.tcsetattr(fd, termios.TCSADRAIN, saved)
"#;

/// A script that reports the window size, at start and whenever it changes. The signal
/// handler only raises a flag: printing from it could interrupt a print in progress.
const SIZE_SCRIPT: &str = r#"
import fcntl, signal, struct, termios, time
changed = True
def on_resize(*_):
    global changed
    changed = True
signal.signal(signal.SIGWINCH, on_resize)
while True:
    if changed:
        changed = False
        rows, cols, width, height = struct.unpack("HHHH", fcntl.ioctl(0, termios.TIOCGWINSZ, b"\0" * 8))
        print(f"SIZE {cols}x{rows} {width}x{height}", flush=True)
    time.sleep(0.01)
"#;

/// The Python to run the scripts with.
pub fn python() -> String {
    let homebrew = "/opt/homebrew/bin/python3";
    if Path::new(homebrew).exists() {
        homebrew.to_string()
    } else {
        "python3".to_string()
    }
}

/// `sh -c script`.
pub fn sh(script: &str) -> TermConfig {
    TermConfig {
        program: Some("/bin/sh".to_string()),
        args: vec!["-c".to_string(), script.to_string()],
        ..TermConfig::default()
    }
}

/// The echo script, in a terminal wide enough that its hex lines rarely wrap.
pub fn echo(setup: &str) -> TermConfig {
    TermConfig {
        program: Some(python()),
        args: vec!["-c".to_string(), ECHO_SCRIPT.to_string(), setup.to_string()],
        cols: 200,
        rows: 50,
        ..TermConfig::default()
    }
}

/// The window size script.
pub fn size_reporter() -> TermConfig {
    TermConfig {
        program: Some(python()),
        args: vec!["-c".to_string(), SIZE_SCRIPT.to_string()],
        ..TermConfig::default()
    }
}

/// Lower-case hex of `bytes`, as the echo script prints it.
pub fn hex(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A terminal and the events it has sent.
pub struct Session {
    pub term: Terminal,
    events: Receiver<TermEvent>,
    /// Events received while waiting for another one, oldest first.
    backlog: Vec<TermEvent>,
}

/// Start a terminal. Panics with the reason if it cannot be started.
pub fn start(config: TermConfig) -> Session {
    let (sender, events) = mpsc::channel();
    let on_event = Box::new(move |event: TermEvent| {
        let _ = sender.send(event);
    });
    let term = Terminal::spawn(config, on_event).expect("the terminal starts");
    Session {
        term,
        events,
        backlog: Vec::new(),
    }
}

impl Session {
    /// Wait until `done` says so. Panics with `what` and the screen after [`TIMEOUT`].
    pub fn wait_until(&self, what: &str, mut done: impl FnMut(&Terminal) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        while !done(&self.term) {
            if Instant::now() > deadline {
                panic!(
                    "timed out waiting for {what}\n--- screen ---\n{}\n--------------",
                    self.term.screen_text()
                );
            }
            std::thread::sleep(POLL);
        }
    }

    /// Wait until the screen shows `needle`. Row breaks are ignored, so text that wrapped
    /// at the right edge is still found. Returns the screen text.
    pub fn wait_for_text(&self, needle: &str) -> String {
        self.wait_until(&format!("{needle:?} on screen"), |term| {
            term.screen_text().replace('\n', "").contains(needle)
        });
        self.term.screen_text()
    }

    /// Wait until the echo script is reading input.
    pub fn wait_ready(&self) {
        self.wait_for_text("READY");
    }

    /// Everything the echo script has read so far, in hex. Empty before the first input.
    pub fn received(&self) -> String {
        let text = self.term.screen_text();
        // The newest total is the last one; it may wrap over several rows.
        match text.rfind("ALL ") {
            Some(start) => text[start + 4..]
                .chars()
                .take_while(|c| c.is_ascii_hexdigit() || *c == '\n')
                .filter(|c| *c != '\n')
                .collect(),
            None => String::new(),
        }
    }

    /// Wait until the echo script has read exactly `bytes` in total.
    pub fn wait_received(&self, bytes: impl AsRef<[u8]>) {
        let wanted = hex(bytes);
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let got = self.received();
            if got == wanted {
                return;
            }
            if Instant::now() > deadline || !wanted.starts_with(&got) {
                panic!(
                    "the child read\n  {got}\nbut should have read\n  {wanted}\n--- screen ---\n{}",
                    self.term.screen_text()
                );
            }
            std::thread::sleep(POLL);
        }
    }

    /// Have the echo script write `bytes` to the terminal.
    pub fn child_writes(&self, bytes: impl AsRef<[u8]>) {
        assert!(self.term.send_text(&format!("!{}", hex(bytes))));
    }

    /// Wait for an event that `wanted` accepts and return it. Other events are kept for
    /// later calls. Panics after [`TIMEOUT`].
    pub fn wait_for_event(&mut self, what: &str, wanted: impl Fn(&TermEvent) -> bool) -> TermEvent {
        if let Some(index) = self.backlog.iter().position(&wanted) {
            return self.backlog.remove(index);
        }
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(left) {
                Ok(event) if wanted(&event) => return event,
                Ok(event) => self.backlog.push(event),
                Err(RecvTimeoutError::Timeout) => {
                    panic!(
                        "timed out waiting for {what}; got {:?}\n--- screen ---\n{}",
                        self.backlog,
                        self.term.screen_text()
                    )
                }
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("the terminal stopped sending events while waiting for {what}")
                }
            }
        }
    }

    /// Wait for the child to exit and return its exit code.
    pub fn wait_for_exit(&mut self) -> Option<i32> {
        match self.wait_for_event("the child to exit", |event| {
            matches!(event, TermEvent::Exit(_))
        }) {
            TermEvent::Exit(code) => code,
            other => unreachable!("{other:?}"),
        }
    }

    /// Every event received so far, without waiting, oldest first.
    pub fn events_so_far(&mut self) -> Vec<TermEvent> {
        self.backlog.extend(self.events.try_iter());
        std::mem::take(&mut self.backlog)
    }

    /// The next event of any kind, or `None` if none arrives within `patience`.
    pub fn next_event(&mut self, patience: Duration) -> Option<TermEvent> {
        if !self.backlog.is_empty() {
            return Some(self.backlog.remove(0));
        }
        self.events.recv_timeout(patience).ok()
    }
}

/// Whether a process with this id exists (a zombie counts).
pub fn process_exists(pid: u32) -> bool {
    std::process::Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Wait until no process has this id any more.
pub fn wait_for_process_to_go(pid: u32) {
    let deadline = Instant::now() + TIMEOUT;
    while process_exists(pid) {
        assert!(Instant::now() < deadline, "process {pid} is still there");
        std::thread::sleep(Duration::from_millis(20));
    }
}
