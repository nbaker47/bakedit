//! Driving herdr from the terminal panel: the `herdr::*` actions as socket operations on a
//! worker thread, the ctrl-b chords typed into the terminal when the socket is not there,
//! and a short-lived cache of which directory each herdr pane is in (for resolving the
//! paths printed in it).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use bakedit_core::Action;
use bakedit_herdr::{Client, CollapseMode, Direction, HerdrError, ScreenSize};
use crossbeam_channel::{unbounded, Receiver, Sender};
use ratatui::layout::Rect;

use crate::background::Waker;

/// How long a click waits for herdr to say which pane it is in.
pub const PANE_QUERY_TIMEOUT: Duration = Duration::from_millis(250);
/// How long what herdr said about its panes is trusted.
const PANE_CACHE_LIFETIME: Duration = Duration::from_millis(1500);

/// What a herdr operation does with the client.
type Run = Box<dyn FnOnce(&Client) -> Result<(), HerdrError> + Send>;

/// One herdr operation, and the chord that does the same thing when typed into herdr.
pub struct Operation {
    run: Run,
    /// What to type into the terminal instead when herdr's socket cannot be reached.
    chord: String,
    /// Shell helper to run over SSH when driving a `herdr --remote` tab, since `herdr --remote`
    /// does not execute local `[[keys.command]]` shell helpers against the remote session.
    remote_cmd: Option<String>,
}

/// What the worker tells the panel.
pub enum Report {
    /// The socket is not there: type this instead.
    Fallback(String),
    /// herdr answered with an error worth showing.
    Failure(String),
}

/// Runs operations one at a time on a thread of its own: each is a few socket round trips
/// and must never hold up drawing.
pub struct Worker {
    jobs: Sender<Operation>,
    pub reports: Receiver<Report>,
}

impl Worker {
    /// Start the thread. `None` when the system would not give one.
    pub fn start(client: Client, waker: Waker) -> Option<Worker> {
        let (jobs, inbox) = unbounded::<Operation>();
        let (report, reports) = unbounded();
        let spawned = std::thread::Builder::new().name("bakedit-herdr".into()).spawn(move || {
            for job in inbox {
                let told = match (job.run)(&client) {
                    Ok(()) => continue,
                    Err(error) if error.is_not_running() => report.send(Report::Fallback(job.chord)),
                    Err(error) => report.send(Report::Failure(format!("herdr: {error}"))),
                };
                if told.is_err() {
                    break;
                }
                waker.wake();
            }
        });
        match spawned {
            Ok(_) => Some(Worker { jobs, reports }),
            Err(error) => {
                log::error!("could not start the herdr worker: {error}");
                None
            }
        }
    }

    /// Queue an operation. False when the worker is gone.
    pub fn send(&self, operation: Operation) -> bool {
        self.jobs.send(operation).is_ok()
    }
}

/// The operation behind a `herdr::*` action, or `None` when the name or its arguments are
/// not one.
pub fn operation(action: &Action) -> Option<Operation> {
    let direction = || action.args.as_ref().and_then(|args| args.as_str()).and_then(|text| text.parse::<Direction>().ok());
    let (run, chord, remote_cmd): (Run, String, Option<String>) = match action.name.as_str() {
        "herdr::Navigate" => {
            let direction = direction()?;
            let (key, word) = match direction {
                Direction::Left => ("h", "left"),
                Direction::Right => ("l", "right"),
                Direction::Up => ("k", "up"),
                Direction::Down => ("j", "down"),
            };
            (
                Box::new(move |client| bakedit_herdr::navigate(client, direction).map(|_| ())),
                format!("\u{2}{key}"),
                Some(format!("~/.local/bin/herdr-nav {word}")),
            )
        }
        "herdr::MovePane" => {
            let direction = direction()?;
            // ctrl-h, ctrl-l, ctrl-k, ctrl-j after the prefix.
            let (key, word) = match direction {
                Direction::Left => ("\u{8}", "left"),
                Direction::Right => ("\u{c}", "right"),
                Direction::Up => ("\u{b}", "up"),
                Direction::Down => ("\u{a}", "down"),
            };
            (
                Box::new(move |client| bakedit_herdr::move_pane(client, direction).map(|_| ())),
                format!("\u{2}{key}"),
                Some(format!("~/.local/bin/herdr-move {word}")),
            )
        }
        "herdr::PreviousWorkspace" => (Box::new(|client| bakedit_herdr::previous_workspace(client).map(|_| ())), "\u{2}u".into(), None),
        "herdr::NextWorkspace" => (Box::new(|client| bakedit_herdr::next_workspace(client).map(|_| ())), "\u{2}d".into(), None),
        "herdr::FocusWorkspace" => {
            let number = action.args.as_ref().and_then(|args| args.as_u64()).filter(|n| (1..=9).contains(n))? as usize;
            // Shift-1 to shift-9 after the prefix (plain 1-9 switch tabs).
            let key = "!@#$%^&*(".chars().nth(number - 1)?;
            (Box::new(move |client| bakedit_herdr::focus_workspace_number(client, number).map(|_| ())), format!("\u{2}{key}"), None)
        }
        "herdr::ToggleZoom" => (Box::new(|client| bakedit_herdr::zoom_toggle(client).map(|_| ())), "\u{2}z".into(), None),
        "herdr::ToggleCollapse" => (
            Box::new(|client| bakedit_herdr::collapse(client, CollapseMode::Toggle).map(|_| ())),
            "\u{2}m".into(),
            Some("~/.local/bin/herdr-collapse".into()),
        ),
        "herdr::ToggleExpand" => (
            Box::new(|client| bakedit_herdr::collapse(client, CollapseMode::Expand).map(|_| ())),
            "\u{2}e".into(),
            Some("~/.local/bin/herdr-collapse expand".into()),
        ),
        "herdr::Scroll" => {
            let up = action.arg_str("direction") != Some("down");
            let lines = action.arg_i64("lines").unwrap_or(3).clamp(0, 10_000) as u32;
            // ctrl-u / ctrl-d scroll by 3 lines, ctrl-y / ctrl-e by 9.
            let key = match (up, lines >= 6) {
                (true, false) => "\u{15}",
                (false, false) => "\u{4}",
                (true, true) => "\u{19}",
                (false, true) => "\u{5}",
            };
            let dir_word = if up { "up" } else { "down" };
            (
                Box::new(move |client| bakedit_herdr::scroll(client, up, lines).map(|_| ())),
                format!("\u{2}{key}"),
                Some(format!("~/.local/bin/herdr-scroll {dir_word} {lines}")),
            )
        }
        _ => return None,
    };
    Some(Operation { run, chord, remote_cmd })
}

impl Operation {
    /// What to type into herdr instead of using its socket.
    pub fn chord(&self) -> &str {
        &self.chord
    }

    /// Remote shell helper to invoke over SSH when driving a `herdr --remote` tab.
    pub fn remote_command(&self) -> Option<&str> {
        self.remote_cmd.as_deref()
    }
}

/// A `herdr --remote <ssh_target> [--session <session>]` process running inside a terminal tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteTarget {
    pub ssh_target: String,
    pub session: Option<String>,
}

/// Parse a `ps` command line for `herdr --remote <target> [--session <name>]`.
pub fn parse_remote_herdr_args(cmdline: &str) -> Option<RemoteTarget> {
    let mut parts = cmdline.split_whitespace();
    let exe = parts.next()?;
    let base = exe.rsplit('/').next().unwrap_or(exe);
    if base != "herdr" {
        return None;
    }
    let mut ssh_target: Option<String> = None;
    let mut session: Option<String> = None;
    while let Some(arg) = parts.next() {
        if arg == "--remote" {
            ssh_target = parts.next().map(str::to_string);
        } else if let Some(val) = arg.strip_prefix("--remote=") {
            ssh_target = Some(val.to_string());
        } else if arg == "--session" {
            session = parts.next().map(str::to_string);
        } else if let Some(val) = arg.strip_prefix("--session=") {
            session = Some(val.to_string());
        }
    }
    ssh_target.filter(|s| !s.is_empty()).map(|ssh_target| RemoteTarget { ssh_target, session })
}

fn parse_ps_line(line: &str) -> Option<(u32, u32, &str)> {
    let s = line.trim_start();
    let i = s.find(char::is_whitespace)?;
    let pid = s[..i].parse::<u32>().ok()?;
    let s = s[i..].trim_start();
    let j = s.find(char::is_whitespace)?;
    let ppid = s[..j].parse::<u32>().ok()?;
    let args = s[j..].trim_start();
    Some((pid, ppid, args))
}

/// Find a `herdr --remote ...` child process descending from `shell_pid`.
pub fn detect_remote(shell_pid: u32) -> Option<RemoteTarget> {
    use std::collections::{HashMap, VecDeque};
    use std::process::{Command, Stdio};

    let out = Command::new("ps")
        .args(["-axo", "pid=,ppid=,args="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut children: HashMap<u32, Vec<(u32, &str)>> = HashMap::new();
    for line in text.lines() {
        if let Some((pid, ppid, args)) = parse_ps_line(line) {
            children.entry(ppid).or_default().push((pid, args));
        }
    }

    let mut queue = VecDeque::from([shell_pid]);
    while let Some(parent) = queue.pop_front() {
        if let Some(kids) = children.get(&parent) {
            for &(pid, args) in kids {
                if let Some(target) = parse_remote_herdr_args(args) {
                    return Some(target);
                }
                queue.push_back(pid);
            }
        }
    }
    None
}

/// Run a `herdr-*` helper on a remote host over SSH without blocking the UI thread.
pub fn run_remote_async(target: RemoteTarget, cmd: String) {
    use std::process::{Command, Stdio};

    let _ = std::thread::Builder::new().name("bakedit-herdr-ssh".into()).spawn(move || {
        let remote_cmd = match &target.session {
            Some(sess) if !sess.is_empty() => {
                let escaped = sess.replace('\'', "'\\''");
                format!("HERDR_SESSION='{escaped}' {cmd}")
            }
            _ => cmd,
        };
        let _ = Command::new("ssh")
            .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=3", &target.ssh_target, &remote_cmd])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    });
}

/// Which directory each herdr pane is in, as last asked, for a moment. Asking costs a few
/// socket round trips, and a click or a list of links asks about many cells.
#[derive(Default)]
pub struct PaneDirs {
    asked: Option<Instant>,
    /// Where the socket did not answer: not asked again until the cache expires.
    unreachable: bool,
    panes: Vec<(Rect, Option<PathBuf>)>,
}

impl PaneDirs {
    /// The directory of the herdr pane under terminal cell `(col, row)` on a terminal of
    /// `size`, or `None` when herdr cannot say (not running, a sidebar cell, no directory).
    pub fn dir_at(&mut self, client: &Client, size: (u16, u16), col: u16, row: u16) -> Option<PathBuf> {
        if self.asked.is_some_and(|at| at.elapsed() > PANE_CACHE_LIFETIME) {
            *self = PaneDirs::default();
        }
        if let Some((_, dir)) = self.panes.iter().find(|(rect, _)| contains(*rect, col, row)) {
            return dir.clone();
        }
        if self.unreachable {
            return None;
        }
        self.asked.get_or_insert_with(Instant::now);
        let client = client.clone().with_timeout(PANE_QUERY_TIMEOUT);
        match bakedit_herdr::pane_at(&client, ScreenSize { cols: size.0, rows: size.1 }, col, row) {
            Ok(Some(found)) => {
                let rect = Rect::new(found.rect.x, found.rect.y, found.rect.width, found.rect.height);
                let dir = found.pane.working_dir().map(PathBuf::from);
                self.panes.push((rect, dir.clone()));
                dir
            }
            Ok(None) => None,
            Err(error) => {
                log::debug!("herdr could not say which pane is at {col},{row}: {error}");
                self.unreachable = true;
                None
            }
        }
    }
}

fn contains(rect: Rect, col: u16, row: u16) -> bool {
    col >= rect.x && col < rect.right() && row >= rect.y && row < rect.bottom()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_remote_herdr_cmdline_and_ps_lines() {
        assert_eq!(
            parse_remote_herdr_args("herdr --remote nathanb@podvid --session aiserver"),
            Some(RemoteTarget { ssh_target: "nathanb@podvid".into(), session: Some("aiserver".into()) })
        );
        assert_eq!(
            parse_remote_herdr_args("/Users/user/.local/bin/herdr --remote=podvid"),
            Some(RemoteTarget { ssh_target: "podvid".into(), session: None })
        );
        assert_eq!(parse_remote_herdr_args("herdr client"), None);
        assert_eq!(
            parse_ps_line("  63358   63324 herdr --remote nathanb@podvid --session aiserver"),
            Some((63358, 63324, "herdr --remote nathanb@podvid --session aiserver"))
        );
    }
}
