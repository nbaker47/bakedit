//! The terminal panel: herdr (or a shell, or the user's own command) on a pty in the left
//! dock.
//!
//! # What the app calls (these signatures stay)
//!
//! * `TerminalLaunch::resolve(command: &TerminalCommand, settings: &Settings, theme: &Theme, root: &Path) -> TerminalLaunch`:
//!   what the panel runs, worked out once from the `terminal.command` setting (or
//!   `--terminal`) and the environment: herdr when it is installed and bakedit is not itself
//!   inside a herdr pane, else the login shell; a custom command with the shell to fall back
//!   on. It also says which herdr session the `herdr::*` actions drive.
//!   `TerminalLaunch::resolve_in` does the same for a given `LaunchEnv` (tests).
//! * `TerminalPanel::new(launch: TerminalLaunch) -> TerminalPanel`. Nothing runs until the
//!   panel is first drawn, so the program starts at the size it is shown at.
//! * `TerminalPanel::is_running(&self) -> bool` and `TerminalPanel::program(&self) -> &str`,
//!   for the app's messages and tests.
//!
//! Everything else goes through `View`: keys and pastes go to the program (a key the keymap
//! did not take, releases included, in the encoding the program asked for); the mouse goes
//! to the program when it asked for it, else selects and scrolls; the link modifier
//! (`link_click_modifier`, cmd by default) plus a click on a path:line or URL opens it,
//! whatever the program does with the mouse; `picker_rows(PickerKind::Links)` lists the
//! paths and URLs on screen for `terminal::OpenLinks`; `terminal::*` and `herdr::*` actions.
//!
//! herdr operations run one at a time on a worker thread: each is a few socket round trips
//! and must never hold up drawing.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bakedit_core::{Action, Key, KeyEvent, Mods, MouseButton, MouseEvent, MouseKind, Rgb, Theme};
use bakedit_herdr::{launch, Client, CollapseMode, Direction, HerdrError};
use bakedit_term::{
    default_search_bases, LinkTarget, ResolvedPath, Scroll, SelectionKind, TermColors, TermConfig, TermError, TermEvent, Terminal,
};
use crossbeam_channel::{unbounded, Receiver, Sender};
use ratatui::layout::Rect;
use ratatui::style::Modifier;

use crate::background::Waker;
use crate::picker::PickerItem;
use crate::settings::{LinkModifier, Settings, TerminalCommand};
use crate::ui::{self, Canvas, Style};
use crate::view::{Context, Cursor, CursorShape, Cx, Effect, PickerKind, RenderCx, View};

/// Lines one wheel step scrolls the scrollback.
const WHEEL_LINES: i32 = 3;
/// How long the click handler waits for herdr to say which pane a click is in.
const PANE_QUERY_TIMEOUT: Duration = Duration::from_millis(250);

/// What the terminal panel runs.
#[derive(Debug, Clone)]
pub struct TerminalLaunch {
    /// What to start, without its size (that is the panel's).
    pub config: TermConfig,
    /// What to start instead when `config` cannot be started (the program is not
    /// installed): the login shell. `None` when `config` is the shell already.
    pub fallback: Option<TermConfig>,
    /// The herdr session the panel shows, for the `herdr::*` actions. `None` when the panel
    /// does not run herdr's interface.
    pub herdr: Option<Client>,
    /// Why the panel does not run what the settings ask for, shown once when it starts.
    pub notice: Option<String>,
    /// A short name for what runs: "herdr", "zsh", "tmux".
    pub program: String,
}

/// The parts of the environment that decide what the panel runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchEnv {
    pub path: Option<std::ffi::OsString>,
    pub home: Option<PathBuf>,
    /// `HERDR_ENV`: set to 1 inside a herdr pane.
    pub herdr_env: Option<std::ffi::OsString>,
    pub xdg_config_home: Option<std::ffi::OsString>,
    /// `SHELL`, for naming the shell in messages.
    pub shell: Option<String>,
    /// Where herdr is installed, if it is (`launch::find_binary` over `PATH` and the usual
    /// places).
    pub herdr_binary: Option<PathBuf>,
}

impl LaunchEnv {
    /// This process's environment.
    pub fn current() -> LaunchEnv {
        let path = std::env::var_os("PATH");
        let home = dirs::home_dir();
        let herdr_binary = launch::find_binary(path.as_deref(), home.as_deref());
        LaunchEnv {
            path,
            home,
            herdr_env: std::env::var_os(launch::INSIDE_ENV_VAR),
            xdg_config_home: std::env::var_os("XDG_CONFIG_HOME"),
            shell: std::env::var("SHELL").ok(),
            herdr_binary,
        }
    }
}

impl TerminalLaunch {
    /// Work out what to run for `command` in this process's environment. Starts nothing.
    pub fn resolve(command: &TerminalCommand, settings: &Settings, theme: &Theme, root: &Path) -> TerminalLaunch {
        TerminalLaunch::resolve_in(command, settings, theme, root, &LaunchEnv::current())
    }

    /// [`TerminalLaunch::resolve`] in a given environment.
    pub fn resolve_in(command: &TerminalCommand, settings: &Settings, theme: &Theme, root: &Path, env: &LaunchEnv) -> TerminalLaunch {
        let base = TermConfig {
            cwd: Some(root.to_path_buf()),
            env: settings.terminal.env.clone(),
            scrollback: settings.terminal.scrollback,
            colors: TermColors::from_theme(theme),
            ..TermConfig::shell()
        };
        let home = env.home.clone();
        let shell_name = || {
            env.shell
                .as_deref()
                .and_then(|shell| Path::new(shell).file_name().map(|n| n.to_string_lossy().into_owned()))
                .unwrap_or_else(|| "shell".to_string())
        };
        let shell = |notice: Option<String>| TerminalLaunch { config: base.clone(), fallback: None, herdr: None, notice, program: shell_name() };
        let herdr_client = |session: launch::Session| -> Option<Client> {
            let config_dir = launch::config_dir(env.xdg_config_home.as_deref(), home.as_deref())?;
            Some(Client::from_env_or_default().with_socket_path(session.socket_path(&config_dir)).allow_live_mutations(true))
        };
        match command {
            TerminalCommand::Shell => shell(None),
            TerminalCommand::Herdr => {
                if launch::running_inside_herdr(env.herdr_env.as_deref()) {
                    return shell(Some("Already inside herdr: the terminal runs your shell (herdr does not nest)".to_string()));
                }
                match env.herdr_binary.clone() {
                    None => shell(Some("herdr is not installed: the terminal runs your shell".to_string())),
                    Some(binary) => TerminalLaunch {
                        config: TermConfig { program: Some(binary.to_string_lossy().into_owned()), ..base.clone() },
                        fallback: Some(base.clone()),
                        herdr: herdr_client(launch::Session::Default),
                        notice: None,
                        program: "herdr".to_string(),
                    },
                }
            }
            TerminalCommand::Custom(words) => {
                let Some((program, args)) = words.split_first() else { return shell(None) };
                let name = Path::new(program).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| program.clone());
                TerminalLaunch {
                    config: TermConfig { program: Some(program.clone()), args: args.to_vec(), ..base.clone() },
                    fallback: Some(base.clone()),
                    herdr: launch::session_of_command(words).and_then(herdr_client),
                    notice: None,
                    program: name,
                }
            }
        }
    }
}

/// A herdr operation for the worker.
type HerdrJob = Box<dyn FnOnce(&Client) -> Result<(), HerdrError> + Send>;

/// Runs herdr operations one at a time on a thread of its own, and reports failures.
struct HerdrWorker {
    jobs: Sender<HerdrJob>,
    failures: Receiver<String>,
}

impl HerdrWorker {
    fn start(client: Client, waker: Waker) -> Option<HerdrWorker> {
        let (jobs, inbox) = unbounded::<HerdrJob>();
        let (report, failures) = unbounded();
        let spawned = std::thread::Builder::new().name("bakedit-herdr".into()).spawn(move || {
            for job in inbox {
                match job(&client) {
                    Ok(()) => {}
                    // No server yet (or any more): the keys simply do nothing.
                    Err(error) if error.is_not_running() => {}
                    Err(error) => {
                        let _ = report.send(format!("herdr: {error}"));
                        waker.wake();
                    }
                }
            }
        });
        match spawned {
            Ok(_) => Some(HerdrWorker { jobs, failures }),
            Err(error) => {
                log::error!("could not start the herdr worker: {error}");
                None
            }
        }
    }
}

/// A link the link picker offered.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LinkChoice {
    File(ResolvedPath),
    Url(String),
}

/// The terminal panel.
pub struct TerminalPanel {
    launch: TerminalLaunch,
    terminal: Option<Terminal>,
    /// Events from the terminal's thread, collected in `tick`.
    events: Arc<Mutex<Vec<TermEvent>>>,
    /// Why the program could not be started at all.
    failed: Option<String>,
    /// The program exited, with this status (`None` when killed by a signal).
    exited: Option<Option<i32>>,
    /// The working directory the program last reported (OSC 7).
    cwd: Option<PathBuf>,
    /// The terminal's generation at the last draw.
    drawn: Option<u64>,
    /// Where the panel was last drawn.
    area: Rect,
    focused: bool,
    /// A link click went down: its release is ours too, not the program's.
    swallow_release: bool,
    /// The mouse went down for a local selection (the program does not want the mouse).
    selecting: bool,
    links: Vec<LinkChoice>,
    herdr: Option<HerdrWorker>,
    /// A line for the status bar, pushed at the next chance.
    notice: Option<(String, bool)>,
}

impl TerminalPanel {
    /// A panel that will run `launch` when it is first drawn.
    pub fn new(launch: TerminalLaunch) -> TerminalPanel {
        let notice = launch.notice.clone().map(|text| (text, false));
        TerminalPanel {
            launch,
            terminal: None,
            events: Arc::new(Mutex::new(Vec::new())),
            failed: None,
            exited: None,
            cwd: None,
            drawn: None,
            area: Rect::default(),
            focused: false,
            swallow_release: false,
            selecting: false,
            links: Vec::new(),
            herdr: None,
            notice,
        }
    }

    /// Whether the panel shows a running program: it started and its exit has not been
    /// seen (by `tick`) yet.
    pub fn is_running(&self) -> bool {
        self.terminal.is_some() && self.exited.is_none()
    }

    /// A short name for what the panel runs ("herdr", "zsh").
    pub fn program(&self) -> &str {
        &self.launch.program
    }

    /// The text on the terminal's screen, for tests and diagnostics. Empty before it starts.
    pub fn screen_text(&self) -> String {
        self.terminal.as_ref().map(Terminal::screen_text).unwrap_or_default()
    }

    fn start(&mut self, cols: u16, rows: u16, waker: &Waker) {
        let events = self.events.clone();
        let wake = waker.clone();
        let on_event = Box::new(move |event: TermEvent| {
            if let Ok(mut queue) = events.lock() {
                queue.push(event);
            }
            wake.wake();
        });
        let sized = |config: &TermConfig| TermConfig { cols: cols.max(2), rows: rows.max(1), ..config.clone() };
        let started = match &self.launch.fallback {
            Some(fallback) => Terminal::spawn_with_fallback(sized(&self.launch.config), sized(fallback), on_event),
            None => Terminal::spawn(sized(&self.launch.config), on_event).map(|terminal| (terminal, None)),
        };
        match started {
            Ok((terminal, problem)) => {
                if let Some(problem) = problem {
                    self.notice = Some((format!("{}: the terminal runs your shell instead", describe_spawn_error(&problem)), false));
                    self.launch.herdr = None;
                }
                self.terminal = Some(terminal);
                self.failed = None;
                self.exited = None;
                if self.focused {
                    if let Some(terminal) = &self.terminal {
                        terminal.send_focus(true);
                    }
                }
            }
            Err(error) => {
                let text = format!("The terminal could not start: {}", describe_spawn_error(&error));
                self.notice = Some((text.clone(), true));
                self.failed = Some(text);
            }
        }
        self.drawn = None;
    }

    /// Start the program again after it exited.
    fn restart(&mut self, waker: &Waker) {
        self.terminal = None;
        self.exited = None;
        let (cols, rows) = (self.area.width, self.area.height);
        if cols > 0 && rows > 0 {
            self.start(cols, rows, waker);
        }
    }

    fn herdr_job(&mut self, cx: &mut Cx<'_>, job: HerdrJob) -> bool {
        let Some(client) = self.launch.herdr.clone() else { return false };
        if self.herdr.is_none() {
            self.herdr = HerdrWorker::start(client, cx.project.waker().clone());
        }
        match &self.herdr {
            Some(worker) => worker.jobs.send(job).is_ok(),
            None => false,
        }
    }

    fn herdr_action(&mut self, action: &Action, cx: &mut Cx<'_>) -> bool {
        let direction = || action.args.as_ref().and_then(|args| args.as_str()).and_then(|text| text.parse::<Direction>().ok());
        let job: HerdrJob = match action.name.as_str() {
            "herdr::Navigate" => {
                let Some(direction) = direction() else { return false };
                Box::new(move |client| bakedit_herdr::navigate(client, direction).map(|_| ()))
            }
            "herdr::MovePane" => {
                let Some(direction) = direction() else { return false };
                Box::new(move |client| bakedit_herdr::move_pane(client, direction).map(|_| ()))
            }
            "herdr::PreviousWorkspace" => Box::new(|client| bakedit_herdr::previous_workspace(client).map(|_| ())),
            "herdr::NextWorkspace" => Box::new(|client| bakedit_herdr::next_workspace(client).map(|_| ())),
            "herdr::FocusWorkspace" => {
                let Some(number) = action.args.as_ref().and_then(|args| args.as_u64()) else { return false };
                Box::new(move |client| bakedit_herdr::focus_workspace_number(client, number as usize).map(|_| ()))
            }
            "herdr::ToggleZoom" => Box::new(|client| bakedit_herdr::zoom_toggle(client).map(|_| ())),
            "herdr::ToggleCollapse" => Box::new(|client| bakedit_herdr::collapse(client, CollapseMode::Toggle).map(|_| ())),
            "herdr::ToggleExpand" => Box::new(|client| bakedit_herdr::collapse(client, CollapseMode::Expand).map(|_| ())),
            "herdr::Scroll" => {
                let up = action.arg_str("direction") != Some("down");
                let lines = action.arg_i64("lines").unwrap_or(3).clamp(0, 10_000) as u32;
                Box::new(move |client| bakedit_herdr::scroll(client, up, lines).map(|_| ()))
            }
            _ => return false,
        };
        self.herdr_job(cx, job)
    }

    /// Where to look for a relative path printed in the terminal: the directory of the
    /// herdr pane under the click when there is one, the program's reported directory, the
    /// project, and the usual places (`default_search_bases`).
    fn search_bases(&self, cx: &Cx<'_>, cell: Option<(u16, u16)>) -> Vec<PathBuf> {
        let mut bases = Vec::new();
        if let (Some(client), Some((col, row)), Some(terminal)) = (&self.launch.herdr, cell, &self.terminal) {
            let (cols, rows) = terminal.size();
            let client = client.clone().with_timeout(PANE_QUERY_TIMEOUT);
            if let Ok(Some(found)) = bakedit_herdr::pane_at(&client, bakedit_herdr::ScreenSize { cols, rows }, col, row) {
                if let Some(dir) = found.pane.working_dir() {
                    bases.push(dir.to_path_buf());
                }
            }
        }
        let start = self.cwd.clone().unwrap_or_else(|| cx.root.to_path_buf());
        bases.push(start.clone());
        bases.push(cx.root.to_path_buf());
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        bases.extend(default_search_bases(&start, &home));
        bases.dedup();
        bases
    }

    fn resolve(&self, target: &LinkTarget, cx: &Cx<'_>, cell: Option<(u16, u16)>) -> Option<LinkChoice> {
        match target {
            LinkTarget::Url(url) => is_openable_url(url).then(|| LinkChoice::Url(url.clone())),
            LinkTarget::Path { .. } => target.resolve(&self.search_bases(cx, cell)).map(LinkChoice::File),
        }
    }

    fn open(choice: &LinkChoice, cx: &mut Cx<'_>) {
        match choice {
            LinkChoice::File(found) => cx.push(Effect::OpenFile {
                path: found.path.clone(),
                line: found.line,
                column: found.col,
                preview: false,
                focus: true,
            }),
            LinkChoice::Url(url) => cx.push(Effect::OpenExternally(url.clone())),
        }
    }

    fn link_modifier_held(&self, mods: Mods, settings: &Settings) -> bool {
        match settings.link_click_modifier {
            LinkModifier::Cmd => mods.contains(Mods::SUPER),
            LinkModifier::Alt => mods.contains(Mods::ALT),
            LinkModifier::Ctrl => mods.contains(Mods::CTRL),
            LinkModifier::None => true,
        }
    }

    fn draw_message(canvas: &mut Canvas<'_>, area: Rect, text: &str, theme: &Theme) {
        canvas.fill(area, theme.terminal_bg);
        if area.height == 0 || area.width < 4 {
            return;
        }
        let y = area.y + area.height / 3;
        canvas.text_truncated(area.x + 2, y, text, Style::colors(theme.text_muted, theme.terminal_bg), area.width.saturating_sub(4));
    }
}

/// One line for the status bar about a program that would not start.
fn describe_spawn_error(error: &TermError) -> String {
    match error {
        TermError::Spawn { program, .. } if error.is_program_missing() => format!("{program} is not installed"),
        other => other.to_string(),
    }
}

/// Only web addresses and file URLs are handed to the system; anything else printed in a
/// terminal could start some other application.
fn is_openable_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("file://")
}

fn cell_color(colors: &TermColors, color: bakedit_core::Color, default: Rgb) -> Rgb {
    colors.resolve(color, default)
}

impl View for TerminalPanel {
    fn render(&mut self, canvas: &mut Canvas<'_>, area: Rect, cx: &RenderCx<'_>) -> Option<Cursor> {
        let theme = cx.theme;
        self.area = area;
        if area.width == 0 || area.height == 0 {
            return None;
        }
        if self.terminal.is_none() && self.failed.is_none() && self.exited.is_none() {
            self.start(area.width, area.height, cx.project.waker());
        }
        let Some(terminal) = &self.terminal else {
            let text = self.failed.clone().unwrap_or_else(|| "Starting the terminal".to_string());
            TerminalPanel::draw_message(canvas, area, &text, theme);
            return None;
        };
        terminal.resize(area.width, area.height);
        self.drawn = Some(terminal.generation());

        canvas.fill(area, theme.terminal_bg);
        let colors = TermColors::from_theme(theme);
        let selection_bg = theme.selection_bg;
        terminal.render(&mut |col, row, cell| {
            if cell.wide_spacer {
                return;
            }
            let (x, y) = (area.x + col, area.y + row);
            if x >= area.right() || y >= area.bottom() {
                return;
            }
            let mut fg = cell_color(&colors, cell.fg, colors.fg);
            let mut bg = cell_color(&colors, cell.bg, colors.bg);
            if cell.inverse {
                std::mem::swap(&mut fg, &mut bg);
            }
            if cell.selected {
                bg = selection_bg;
            }
            if cell.hidden {
                fg = bg;
            }
            let mut symbol = String::new();
            symbol.push(if cell.ch == '\0' { ' ' } else { cell.ch });
            symbol.extend(cell.zerowidth.iter());
            // A wide character needs its second cell inside the panel.
            if cell.wide && x + 1 >= area.right() {
                symbol = " ".to_string();
            }
            if let Some(target) = canvas.cell_mut(x, y) {
                target.set_symbol(&symbol);
                target.set_fg(ui::color(fg));
                target.set_bg(ui::color(bg));
                let mut modifier = Modifier::empty();
                if cell.bold {
                    modifier |= Modifier::BOLD;
                }
                if cell.italic {
                    modifier |= Modifier::ITALIC;
                }
                if cell.dim {
                    modifier |= Modifier::DIM;
                }
                if cell.underline != bakedit_term::Underline::None {
                    modifier |= Modifier::UNDERLINED;
                }
                if cell.strikeout {
                    modifier |= Modifier::CROSSED_OUT;
                }
                target.modifier = modifier;
            }
        });

        if let Some(code) = self.exited {
            // The last screen stays; one quiet row at the bottom says what happened.
            let bar = Rect::new(area.x, area.bottom() - 1, area.width, 1);
            let bg = theme.elevated_surface;
            canvas.fill(bar, bg);
            let how = match code {
                Some(0) => format!("{} exited", self.launch.program),
                Some(code) => format!("{} exited with code {code}", self.launch.program),
                None => format!("{} was stopped", self.launch.program),
            };
            let text = format!("{how}  ·  enter starts it again");
            canvas.text_truncated(bar.x + 1, bar.y, &text, Style::colors(theme.text_muted, bg), bar.width.saturating_sub(2));
            return None;
        }

        if !cx.focused {
            return None;
        }
        let cursor = terminal.cursor()?;
        let (col, row) = (area.x + cursor.col, area.y + cursor.row);
        if col >= area.right() || row >= area.bottom() {
            return None;
        }
        let shape = match cursor.shape {
            bakedit_term::CursorShape::Bar => CursorShape::Bar,
            bakedit_term::CursorShape::Underline => CursorShape::Underline,
            bakedit_term::CursorShape::Block | bakedit_term::CursorShape::Hollow => CursorShape::Block,
        };
        Some(Cursor { col, row, shape })
    }

    fn contexts(&self, out: &mut Vec<Context>) {
        out.push(Context::new("Pane"));
        out.push(Context::new("Terminal"));
    }

    fn handle_action(&mut self, action: &Action, cx: &mut Cx<'_>) -> bool {
        let name = action.name.as_str();
        if name.starts_with("herdr::") {
            return self.herdr_action(action, cx);
        }
        if self.exited.is_some() && name == "menu::Confirm" {
            let waker = cx.project.waker().clone();
            self.restart(&waker);
            return true;
        }
        let Some(terminal) = &self.terminal else { return false };
        match name {
            "terminal::Copy" => match terminal.selection_text() {
                Some(text) => {
                    cx.clipboard.set(&text, false);
                    terminal.selection_clear();
                }
                // Nothing selected: the key goes to the program, as Ghostty does.
                None => return false,
            },
            "terminal::Paste" => {
                if let Some(clip) = cx.clipboard.get() {
                    terminal.send_paste(&clip.text);
                }
            }
            "terminal::Clear" => {
                terminal.send_text("\u{c}");
            }
            "terminal::SendText" => {
                let Some(text) = action.args.as_ref().and_then(|args| args.as_str()) else { return false };
                terminal.send_text(text);
            }
            "terminal::SendKeystroke" => {
                let Some(spelled) = action.args.as_ref().and_then(|args| args.as_str()) else { return false };
                match bakedit_keymap::Keystroke::parse(spelled) {
                    Ok(keystroke) => {
                        terminal.send_key(&keystroke.to_event());
                    }
                    Err(error) => cx.error(format!("terminal::SendKeystroke: {error}")),
                }
            }
            "terminal::ScrollLineUp" => {
                terminal.scroll(Scroll::Lines(1));
            }
            "terminal::ScrollLineDown" => {
                terminal.scroll(Scroll::Lines(-1));
            }
            "terminal::ScrollPageUp" => {
                terminal.scroll(Scroll::PageUp);
            }
            "terminal::ScrollPageDown" => {
                terminal.scroll(Scroll::PageDown);
            }
            "terminal::ScrollToTop" => {
                terminal.scroll(Scroll::Top);
            }
            "terminal::ScrollToBottom" => {
                terminal.scroll(Scroll::Bottom);
            }
            _ => return false,
        }
        true
    }

    fn handle_key(&mut self, key: &KeyEvent, cx: &mut Cx<'_>) -> bool {
        if self.exited.is_some() {
            if key.is_down() && key.key == Key::Enter {
                let waker = cx.project.waker().clone();
                self.restart(&waker);
            }
            return key.is_down();
        }
        let Some(terminal) = &self.terminal else { return false };
        if key.is_text_only() {
            return key.text.as_deref().is_some_and(|text| terminal.send_text(text));
        }
        terminal.send_key(key) || key.is_down()
    }

    fn handle_paste(&mut self, text: &str, _cx: &mut Cx<'_>) -> bool {
        match &self.terminal {
            Some(terminal) if self.exited.is_none() => {
                terminal.send_paste(text);
                true
            }
            _ => false,
        }
    }

    fn handle_mouse(&mut self, event: &MouseEvent, area: Rect, cx: &mut Cx<'_>) -> bool {
        let local = MouseEvent {
            col: event.col.saturating_sub(area.x).min(area.width.saturating_sub(1)),
            row: event.row.saturating_sub(area.y).min(area.height.saturating_sub(1)),
            ..*event
        };
        if self.swallow_release && matches!(event.kind, MouseKind::Release(_) | MouseKind::Drag(_)) {
            if matches!(event.kind, MouseKind::Release(_)) {
                self.swallow_release = false;
            }
            return true;
        }
        let Some(terminal) = &self.terminal else { return false };
        // A click with the link modifier opens what is under it, whatever the program
        // does with the mouse: that is how a path an agent printed gets into the editor.
        if event.kind == MouseKind::Press(MouseButton::Left) && self.link_modifier_held(event.mods, cx.settings) {
            if let Some(link) = terminal.link_at(local.col, local.row) {
                if let Some(choice) = self.resolve(&link.target, cx, Some((local.col, local.row))) {
                    TerminalPanel::open(&choice, cx);
                    self.swallow_release = true;
                    return true;
                }
            }
        }
        let terminal = self.terminal.as_ref().expect("checked above");
        if terminal.send_mouse(&local) {
            return true;
        }
        match event.kind {
            MouseKind::ScrollUp => {
                terminal.scroll(Scroll::Lines(WHEEL_LINES));
            }
            MouseKind::ScrollDown => {
                terminal.scroll(Scroll::Lines(-WHEEL_LINES));
            }
            MouseKind::Press(MouseButton::Left) => {
                terminal.selection_start(local.col, local.row, SelectionKind::Simple);
                self.selecting = true;
            }
            MouseKind::Drag(MouseButton::Left) if self.selecting => terminal.selection_update(local.col, local.row),
            MouseKind::Release(MouseButton::Left) if self.selecting => {
                self.selecting = false;
                if cx.settings.terminal.copy_on_select {
                    if let Some(text) = terminal.selection_text() {
                        cx.clipboard.set(&text, false);
                    }
                }
            }
            _ => return false,
        }
        true
    }

    fn focus_changed(&mut self, focused: bool, _cx: &mut Cx<'_>) {
        self.focused = focused;
        if let Some(terminal) = &self.terminal {
            terminal.send_focus(focused);
        }
    }

    fn tick(&mut self, cx: &mut Cx<'_>) -> bool {
        let mut dirty = false;
        let events = match self.events.lock() {
            Ok(mut queue) => std::mem::take(&mut *queue),
            Err(_) => Vec::new(),
        };
        for event in events {
            match event {
                TermEvent::Wakeup | TermEvent::Bell | TermEvent::Title(_) | TermEvent::ResetTitle => {}
                TermEvent::Exit(code) => {
                    self.exited = Some(code);
                    dirty = true;
                }
                TermEvent::ClipboardStore(text) => {
                    cx.clipboard.set(&text, false);
                }
                // Reading the clipboard is not offered to programs in the terminal.
                TermEvent::ClipboardLoad => {}
                TermEvent::Cwd(path) => self.cwd = Some(path),
                TermEvent::Notification { title, body } => {
                    let text = if body.is_empty() { title } else if title.is_empty() { body } else { format!("{title}: {body}") };
                    cx.status(text);
                }
            }
        }
        if let Some((text, error)) = self.notice.take() {
            if error {
                cx.error(text);
            } else {
                cx.status(text);
            }
        }
        if let Some(worker) = &self.herdr {
            while let Ok(failure) = worker.failures.try_recv() {
                cx.error(failure);
            }
        }
        if let Some(terminal) = &self.terminal {
            dirty |= Some(terminal.generation()) != self.drawn;
        }
        dirty
    }

    fn picker_rows(&mut self, kind: PickerKind, cx: &mut Cx<'_>) -> Option<Vec<PickerItem>> {
        if kind != PickerKind::Links {
            return None;
        }
        let terminal = self.terminal.as_ref()?;
        let mut seen = std::collections::HashSet::new();
        let mut rows = Vec::new();
        self.links.clear();
        // Newest output is at the bottom: list from there up, each link once.
        for link in terminal.visible_links().into_iter().rev() {
            let label = match &link.target {
                LinkTarget::Url(url) => url.clone(),
                LinkTarget::Path { path, line, col } => match (line, col) {
                    (Some(line), Some(col)) => format!("{path}:{line}:{col}"),
                    (Some(line), None) => format!("{path}:{line}"),
                    _ => path.clone(),
                },
            };
            if !seen.insert(label.clone()) {
                continue;
            }
            let Some(choice) = self.resolve(&link.target, cx, None) else { continue };
            let (detail, icon) = match &choice {
                LinkChoice::File(found) => {
                    let shown = found.path.strip_prefix(cx.root).unwrap_or(&found.path).display().to_string();
                    let name = found.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    (Some(shown), Some(crate::icons::file_icon(cx.settings, &name, false, false)))
                }
                LinkChoice::Url(_) => (None, None),
            };
            let mut item = PickerItem::new(label);
            item.detail = detail.filter(|detail| *detail != item.label);
            item.icon = icon;
            rows.push(item);
            self.links.push(choice);
        }
        Some(rows)
    }

    fn picker_confirm(&mut self, kind: PickerKind, row: usize, _secondary: bool, cx: &mut Cx<'_>) {
        if kind != PickerKind::Links {
            return;
        }
        if let Some(choice) = self.links.get(row).cloned() {
            TerminalPanel::open(&choice, cx);
        }
    }
}

#[cfg(test)]
mod tests;
