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
    let (run, chord): (Run, String) = match action.name.as_str() {
        "herdr::Navigate" => {
            let direction = direction()?;
            let key = match direction {
                Direction::Left => "h",
                Direction::Right => "l",
                Direction::Up => "k",
                Direction::Down => "j",
            };
            (Box::new(move |client| bakedit_herdr::navigate(client, direction).map(|_| ())), format!("\u{2}{key}"))
        }
        "herdr::MovePane" => {
            let direction = direction()?;
            // ctrl-h, ctrl-l, ctrl-k, ctrl-j after the prefix.
            let key = match direction {
                Direction::Left => "\u{8}",
                Direction::Right => "\u{c}",
                Direction::Up => "\u{b}",
                Direction::Down => "\u{a}",
            };
            (Box::new(move |client| bakedit_herdr::move_pane(client, direction).map(|_| ())), format!("\u{2}{key}"))
        }
        "herdr::PreviousWorkspace" => (Box::new(|client| bakedit_herdr::previous_workspace(client).map(|_| ())), "\u{2}u".into()),
        "herdr::NextWorkspace" => (Box::new(|client| bakedit_herdr::next_workspace(client).map(|_| ())), "\u{2}d".into()),
        "herdr::FocusWorkspace" => {
            let number = action.args.as_ref().and_then(|args| args.as_u64()).filter(|n| (1..=9).contains(n))? as usize;
            // Shift-1 to shift-9 after the prefix (plain 1-9 switch tabs).
            let key = "!@#$%^&*(".chars().nth(number - 1)?;
            (Box::new(move |client| bakedit_herdr::focus_workspace_number(client, number).map(|_| ())), format!("\u{2}{key}"))
        }
        "herdr::ToggleZoom" => (Box::new(|client| bakedit_herdr::zoom_toggle(client).map(|_| ())), "\u{2}z".into()),
        "herdr::ToggleCollapse" => (Box::new(|client| bakedit_herdr::collapse(client, CollapseMode::Toggle).map(|_| ())), "\u{2}m".into()),
        "herdr::ToggleExpand" => (Box::new(|client| bakedit_herdr::collapse(client, CollapseMode::Expand).map(|_| ())), "\u{2}e".into()),
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
            (Box::new(move |client| bakedit_herdr::scroll(client, up, lines).map(|_| ())), format!("\u{2}{key}"))
        }
        _ => return None,
    };
    Some(Operation { run, chord })
}

impl Operation {
    /// What to type into herdr instead of using its socket.
    pub fn chord(&self) -> &str {
        &self.chord
    }
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
