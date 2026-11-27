//! The running editor: the main loop.
//!
//! One thread owns everything on screen. Other threads only send an [`AppEvent`] down one
//! channel: the stdin reader (bytes from the host terminal), the signal watcher (resizes,
//! SIGTERM and SIGHUP), the [`Waker`] (background work has something to collect), and the
//! IPC server (`bakedit file.rs:12` from another shell). The loop sleeps until an event or
//! the next timer and draws only when something changed, inside a synchronized update.
//!
//! Input follows `bakedit_input`'s wiring: the parser's key events go first to
//! [`HeldMods`] as they came, then through the [`NavLayer`] (hold `;`), then to the keymap.
//! The kitty keyboard flags the terminal reports decide whether the nav layer can work;
//! without the protocol (no key releases, no cmd key) it stays off, the status bar says so,
//! and `ctrl-]` is the prefix that reaches bakedit. Mouse events carry the modifiers the
//! terminal cannot report (cmd) from the keys seen.
//!
//! Timers are deadlines: the escape flush, the chord timeout, the which-key delay, the
//! status message, the settings and keymap check, the session save, and whatever a view
//! asked for with [`Waker::wake_at`].

use std::io::{self, BufWriter, Stdout, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use anyhow::Context as _;
use bakedit_core::{Action, InputEvent, Key, KeyEvent, MouseEvent, TerminalReply};
use bakedit_input::host as seq;
use bakedit_input::{HeldMods, NavLayer, NavLayerConfig, NumLayer, NumLayerConfig, Parser};
use crossbeam_channel::{unbounded, Receiver, RecvTimeoutError};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Position;

use crate::background::Waker;
use crate::chrome::NavLayerIndicator;
use crate::cli::PathSpec;
use crate::clipboard::{Clipboard, MemoryBackend};
use crate::dots::overlay::DotsOverlay;
use crate::dots::DotsWatcher;
use crate::factory::{AppFactory, AppSource};
use crate::host::{self, Host, HostEvent};
use crate::ipc;
use crate::keys::KeyRouter;
use crate::onboarding;
use crate::panels::git::GitPanel;
use crate::panels::project::ProjectPanel;
use crate::panels::terminal::{TerminalLaunch, TerminalPanel};
use crate::project::Project;
use crate::session;
use crate::settings::{self, Autosave, Settings, TerminalCommand};
use crate::shell::{Modal, Request, Shell};
use crate::themes;
use crate::usage::UsageWatcher;
use crate::view::CursorShape;
use crate::window_state::{self, WindowState};
use crate::workspace::{Focus, PanelId};

/// How long the start of an escape sequence waits for the rest before it is read as Escape.
const ESCAPE_TIMEOUT: Duration = Duration::from_millis(25);
/// How long after a change the session is written.
const SESSION_DEBOUNCE: Duration = Duration::from_millis(1500);
/// How often the settings and keymap files are looked at for changes.
const CONFIG_CHECK: Duration = Duration::from_millis(1500);
/// How fast the Agent Dots working spinner and unread badge pulse inside bakedit.
const DOTS_PULSE: Duration = Duration::from_millis(550);

/// Something for the main loop, from another thread.
#[derive(Debug)]
pub enum AppEvent {
    /// Bytes from the host terminal.
    Input(Vec<u8>),
    /// Standard input closed: the terminal is gone.
    InputClosed,
    /// The window changed size.
    Resized,
    /// SIGTERM or SIGHUP.
    Terminate,
    /// Background work has something to collect.
    Wake,
    /// A request from another bakedit process.
    Ipc(ipc::Request),
}

/// What the editor starts with, from the command line.
#[derive(Debug, Clone)]
pub struct StartOptions {
    /// The project directory.
    pub root: PathBuf,
    /// Files to open, the last one in front.
    pub files: Vec<PathSpec>,
    /// `--terminal`: run this in the terminal panel instead of the setting.
    pub terminal_command: Option<TerminalCommand>,
    /// Reopen the last session's tabs (`--no-restore` turns it off).
    pub restore: bool,
    /// Open the onboarding wizard immediately (`--onboarding`).
    pub onboarding: bool,
    /// Why bakedit runs here rather than in the Ghostty window it would have opened, shown
    /// once in the status bar.
    pub notice: Option<String>,
}

/// The files whose changes reload the configuration, and when each last changed.
struct ConfigFiles {
    paths: Vec<PathBuf>,
    stamps: Vec<Option<SystemTime>>,
}

impl ConfigFiles {
    fn new() -> ConfigFiles {
        let config_home = settings::config_dir().parent().map(Path::to_path_buf).unwrap_or_default();
        ConfigFiles::watching(vec![settings::settings_path(), settings::keymap_path(), config_home.join("zed").join("settings.json")])
    }

    /// Watch these files, as they are now.
    fn watching(paths: Vec<PathBuf>) -> ConfigFiles {
        let mut files = ConfigFiles { stamps: Vec::new(), paths };
        files.stamps = files.read();
        files
    }

    fn read(&self) -> Vec<Option<SystemTime>> {
        self.paths.iter().map(|path| std::fs::metadata(path).and_then(|meta| meta.modified()).ok()).collect()
    }

    /// Whether any of the files changed since the last call.
    fn changed(&mut self) -> bool {
        let now = self.read();
        let changed = now != self.stamps;
        self.stamps = now;
        changed
    }
}

/// Which modal is open, for noticing that the focus moved.
fn modal_name(modal: &Option<Modal>) -> Option<String> {
    match modal {
        Some(Modal::Picker { kind, .. }) => Some(format!("{kind:?}")),
        Some(Modal::Prompt { .. }) => Some("prompt".into()),
        Some(Modal::Input { purpose, .. }) => Some(format!("{purpose:?}")),
        Some(Modal::Onboarding(_)) => Some("onboarding".into()),
        Some(Modal::KeymapsGuide(_)) => Some("keymaps_guide".into()),
        None => None,
    }
}

type Screen = ratatui::Terminal<CrosstermBackend<BufWriter<Stdout>>>;

/// The running editor.
pub struct App {
    shell: Shell,
    router: KeyRouter,
    parser: Parser,
    nav: NavLayer,
    num: NumLayer,
    held: HeldMods,
    events: Receiver<AppEvent>,
    waker: Waker,
    screen: Screen,
    ipc: Option<ipc::Server>,
    usage: Option<UsageWatcher>,
    dots_watcher: Option<DotsWatcher>,
    dots_overlay: DotsOverlay,
    terminal_command: TerminalCommand,
    state_dir: PathBuf,
    sessions_dir: PathBuf,
    /// The keyboard flags the terminal reported; `None` until it has answered.
    kitty_flags: Option<u8>,
    /// The nav layer is switched on (by the setting, then by the toggle).
    nav_wanted: bool,
    escape_deadline: Option<Instant>,
    session_deadline: Option<Instant>,
    config_deadline: Instant,
    dots_pulse_deadline: Instant,
    config: ConfigFiles,
    /// The session as last written, to skip writing the same again.
    saved_session: Option<String>,
    /// Persisted host window zoom level and geometry.
    window_state: WindowState,
    /// Whether the host window was resized or unfocused since geometry was last captured.
    window_geometry_dirty: bool,
    /// Colours reported by the host terminal via OSC 10/11/12/4 and CSI ? 997 n.
    terminal_palette: bakedit_core::TerminalPalette,
    /// Where the keyboard was after the last event, to notice it moving.
    focus_key: (Focus, Option<String>),
    cursor_shape: Option<seq::CursorShape>,
    /// Last Kitty Graphics Protocol image transmitted to the host terminal.
    kitty_image_key: Option<crate::items::image::KittyImageKey>,
    /// Background TTS playback owned by this window (for `--speak` / code-walkthrough).
    speaker: crate::tts::Speaker,
    dirty: bool,
    quit: bool,
    // Dropped last: the screen restored after everything else is done with it.
    _host: Host,
}

/// The theme, keymap and settings as read from disk, with what could not be understood.
struct Loaded {
    settings: Settings,
    theme: bakedit_core::Theme,
    router: KeyRouter,
    warnings: Vec<String>,
}

fn load_config(palette: Option<&bakedit_core::TerminalPalette>) -> Loaded {
    let loaded = settings::load();
    let mut warnings = loaded.warnings;
    let (theme, theme_warning) = match palette {
        Some(pal) => themes::load_with_palette(&loaded.settings, pal),
        None => themes::load(&loaded.settings),
    };
    warnings.extend(theme_warning);
    let keymap_text = match std::fs::read_to_string(settings::keymap_path()) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            warnings.push(format!("{}: {error}", settings::keymap_path().display()));
            None
        }
    };
    let (router, keymap_warnings) = KeyRouter::new(keymap_text.as_deref());
    warnings.extend(keymap_warnings.into_iter().map(|w| format!("keymap.json: {w}")));
    Loaded { settings: loaded.settings, theme, router, warnings }
}

/// Make the shell for a project: the project with its background work, the three panels,
/// and the factory and picker source behind them.
fn build_shell(root: &Path, loaded: (Settings, bakedit_core::Theme), clipboard: Clipboard, waker: &Waker, command: &TerminalCommand, state_dir: &Path, router: &KeyRouter) -> Shell {
    let (settings, theme) = loaded;
    let project = Project::open(root, waker.clone());
    let mut shell = Shell::new(settings, theme, project, clipboard, Box::new(AppFactory), Box::new(AppSource::new(state_dir.to_path_buf())));
    shell.binding_hint = router.binding_hint();
    let launch = TerminalLaunch::resolve(command, &shell.settings, &shell.theme, root);
    shell.set_panel(PanelId::Terminal, Box::new(TerminalPanel::new(launch)));
    shell.build_panel(PanelId::Git, |cx| Box::new(GitPanel::new(cx)));
    shell.build_panel(PanelId::Project, |cx| Box::new(ProjectPanel::new(cx)));
    shell
}

/// Put the saved session back, or with none (or `restore` off) open the terminal and the
/// file tree with the keyboard in the centre.
fn open_session(shell: &mut Shell, sessions_dir: &Path, restore: bool) {
    let saved = if restore && shell.settings.restore_on_startup { session::load(sessions_dir, shell.root()) } else { None };
    match saved {
        Some(saved) => session::restore(shell, &saved),
        None => {
            shell.workspace.open_panel(PanelId::Terminal);
            shell.workspace.open_panel(PanelId::Project);
            shell.set_focus(Focus::Center);
        }
    }
}

fn window_title(root: &Path) -> String {
    let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| root.display().to_string());
    seq::set_title(&format!("{name} · bakedit"))
}

impl App {
    /// Run the editor on this terminal until it quits. The terminal is put back however it
    /// ends: quitting, SIGTERM or SIGHUP, the terminal going away, or a panic.
    pub fn run(options: StartOptions) -> anyhow::Result<()> {
        host::install_panic_hook();
        let (sender, events) = unbounded();
        let waker = {
            let sender = sender.clone();
            Waker::new(move || {
                let _ = sender.send(AppEvent::Wake);
            })
        };
        let loaded = load_config(None);
        let terminal_command = options.terminal_command.clone().unwrap_or_else(|| loaded.settings.terminal.command.clone());
        let state_dir = ipc::state_dir();
        let sessions_dir = state_dir.join("sessions");

        let host = Host::enter(host::editor_options()).context("could not put the terminal in raw mode")?;
        let backend = CrosstermBackend::new(BufWriter::with_capacity(1 << 16, io::stdout()));
        let screen = ratatui::Terminal::new(backend).context("could not take over the terminal")?;
        {
            let sender = sender.clone();
            host::spawn_reader(move |event| {
                let _ = sender.send(match event {
                    HostEvent::Bytes(bytes) => AppEvent::Input(bytes),
                    HostEvent::Closed => AppEvent::InputClosed,
                    HostEvent::Resized => AppEvent::Resized,
                    HostEvent::Terminate => AppEvent::Terminate,
                });
            })?;
        }
        {
            let sender = sender.clone();
            host::spawn_signal_watcher(move |event| {
                let _ = sender.send(match event {
                    HostEvent::Resized => AppEvent::Resized,
                    _ => AppEvent::Terminate,
                });
            })?;
        }

        let nav_wanted = loaded.settings.nav_layer;
        let usage_meter = loaded.settings.usage_meter;
        let agent_dots = loaded.settings.agent_dots;
        let window_state = window_state::load(&state_dir);
        let mut shell = build_shell(
            &options.root,
            (loaded.settings, loaded.theme),
            Clipboard::new(),
            &waker,
            &terminal_command,
            &state_dir,
            &loaded.router,
        );
        shell.zoom_offset = window_state.zoom_offset;
        window_state::spawn_restore(window_state.clone());
        open_session(&mut shell, &sessions_dir, options.restore);
        if let Some(want) = window_state.show_all_left_panels {
            if shell.workspace.is_showing_all_left_panels() != want {
                shell.workspace.toggle_show_all_left_panels();
            }
        }
        for file in &options.files {
            shell.open_file_range(&file.path, file.line, file.end_line, file.column, false, true);
        }
        // Why there is no window comes first: it explains what the user is looking at.
        let messages: Vec<&String> = options.notice.iter().chain(&loaded.warnings).collect();
        let error = !loaded.warnings.is_empty();
        match messages.as_slice() {
            [] => {}
            [one] => shell.set_status(one.as_str(), error),
            [first, rest @ ..] => shell.set_status(format!("{first} (and {} more: see --log)", rest.len()), error),
        }
        for message in &messages {
            log::warn!("{message}");
        }
        if let Err(error) = session::remember_project(&state_dir, &options.root) {
            log::warn!("could not update the recent projects: {error}");
        }
        let ipc = {
            let sender = sender.clone();
            match ipc::Server::start(&ipc::instances_dir(), &options.root, Box::new(move |request| {
                let _ = sender.send(AppEvent::Ipc(request));
            })) {
                Ok(server) => Some(server),
                Err(error) => {
                    log::warn!("other shells cannot open files here: {error}");
                    None
                }
            }
        };
        let usage = usage_meter.then(|| {
            let waker = waker.clone();
            UsageWatcher::start(move || waker.wake())
        });
        let dots_watcher = agent_dots.then(|| {
            let waker = waker.clone();
            DotsWatcher::start(move || waker.wake())
        });
        let mut dots_overlay = DotsOverlay::new(state_dir.clone(), agent_dots);
        if agent_dots {
            dots_overlay.sync_theme(&shell.theme);
        }
        if let Err(error) = crate::skills::install_default(false) {
            log::debug!("could not install bundled skills: {error}");
        }
        if options.onboarding || onboarding::should_auto_open(&state_dir) {
            shell.open_onboarding();
        }

        let mut app = App {
            focus_key: (shell.workspace.focus(), None),
            shell,
            router: loaded.router,
            parser: Parser::new(),
            // Off until the terminal says it reports key releases (see on_reply).
            nav: NavLayer::new(NavLayerConfig { enabled: false, ..NavLayerConfig::default() }),
            num: NumLayer::new(NumLayerConfig { enabled: false, ..NumLayerConfig::default() }),
            held: HeldMods::new(),
            events,
            waker,
            screen,
            ipc,
            usage,
            dots_watcher,
            dots_overlay,
            terminal_command,
            state_dir,
            sessions_dir,
            kitty_flags: None,
            nav_wanted,
            escape_deadline: None,
            session_deadline: None,
            config_deadline: Instant::now() + CONFIG_CHECK,
            dots_pulse_deadline: Instant::now() + DOTS_PULSE,
            config: ConfigFiles::new(),
            saved_session: None,
            window_state,
            window_geometry_dirty: false,
            terminal_palette: bakedit_core::TerminalPalette::default(),
            cursor_shape: None,
            kitty_image_key: None,
            speaker: crate::tts::Speaker::new(),
            dirty: true,
            quit: false,
            _host: host,
        };
        app.update_nav_indicator();
        let palette_query = seq::palette_query_sequence();
        let _ = app.screen.backend_mut().write_all(palette_query.as_bytes());
        let title = window_title(&options.root);
        let _ = app.screen.backend_mut().write_all(title.as_bytes());
        let result = app.event_loop();
        app.save_session();
        window_state::capture_and_save_sync(&app.state_dir, &mut app.window_state);
        drop(app);
        host::restore();
        result
    }

    fn event_loop(&mut self) -> anyhow::Result<()> {
        loop {
            // Leaving draws nothing more: the terminal may already be going away.
            if self.quit {
                return Ok(());
            }
            if self.dirty {
                self.draw().context("could not draw")?;
                self.dirty = false;
            }
            let first = match self.next_deadline() {
                Some(deadline) => match self.events.recv_deadline(deadline) {
                    Ok(event) => Some(event),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return Ok(()),
                },
                None => match self.events.recv() {
                    Ok(event) => Some(event),
                    Err(_) => return Ok(()),
                },
            };
            if let Some(event) = first {
                self.handle(event);
                // Whatever else is already waiting goes into the same frame.
                while !self.quit {
                    match self.events.try_recv() {
                        Ok(event) => self.handle(event),
                        Err(_) => break,
                    }
                }
            }
            self.run_timers(Instant::now());
            self.after_events();
        }
    }

    /// Whether any Agent Dots indicator currently needs the 550ms pulse animation.
    fn needs_dots_pulse(&self) -> bool {
        if matches!(self.shell.modal, Some(Modal::Onboarding(_))) {
            return true;
        }
        self.shell.settings.agent_dots
            && self.shell.dots.as_ref().is_some_and(|s| {
                !s.unread.is_empty()
                    || s.agents.iter().any(|a| a.status == bakedit_herdr::AgentStatus::Working)
            })
    }

    /// The earliest moment the loop has to wake up without an event.
    fn next_deadline(&self) -> Option<Instant> {
        // While the which-key popup lists a chord's completions it waits for the user, as
        // in Zed: the chord does not time out under it.
        let chord = self.router.chord_deadline.filter(|_| self.shell.which_key.is_empty());
        [
            self.escape_deadline,
            chord,
            self.router.hint_deadline,
            self.shell.message_deadline(),
            self.waker.deadline(),
            self.session_deadline,
            self.needs_dots_pulse().then_some(self.dots_pulse_deadline),
            Some(self.config_deadline),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn handle(&mut self, event: AppEvent) {
        match event {
            AppEvent::Input(bytes) => {
                let events = self.parser.feed(&bytes);
                self.handle_input(events);
                self.escape_deadline = self.parser.has_pending().then(|| Instant::now() + ESCAPE_TIMEOUT);
            }
            AppEvent::InputClosed | AppEvent::Terminate => self.quit = true,
            AppEvent::Resized => {
                self.dirty = true;
                self.window_geometry_dirty = true;
            }
            AppEvent::Wake => self.on_wake(),
            AppEvent::Ipc(request) => {
                self.handle_ipc(request);
                self.dirty = true;
            }
        }
    }

    fn on_wake(&mut self) {
        self.waker.take_woken();
        self.dirty |= self.shell.poll_project();
        self.dirty |= self.shell.tick();
        if let Some(usage) = &mut self.usage {
            if usage.poll() {
                self.shell.usage = usage.vendors().to_vec();
                self.dirty = true;
            }
        }
        if let Some(watcher) = &self.dots_watcher {
            if let Some(snap) = watcher.poll() {
                self.shell.dots = Some(snap);
                self.dirty = true;
            }
        }
    }

    fn run_timers(&mut self, now: Instant) {
        if self.escape_deadline.is_some_and(|at| at <= now) {
            self.escape_deadline = None;
            let events = self.parser.flush();
            self.handle_input(events);
        }
        if self.router.chord_deadline.is_some_and(|at| at <= now) && self.shell.which_key.is_empty() {
            self.router.chord_timeout(&mut self.shell);
            self.dirty = true;
        }
        if self.router.hint_deadline.is_some_and(|at| at <= now) {
            self.router.show_hints(&mut self.shell);
            self.dirty = true;
        }
        if self.shell.message_deadline().is_some_and(|at| at <= now) {
            self.dirty |= self.shell.tick();
        }
        if self.waker.take_due(now) {
            self.dirty |= self.shell.poll_project();
            self.dirty |= self.shell.tick();
        }
        if self.session_deadline.is_some_and(|at| at <= now) {
            self.save_session();
        }
        if self.config_deadline <= now {
            self.config_deadline = now + CONFIG_CHECK;
            if self.config.changed() {
                self.reload_config(false);
            }
        }
        if self.needs_dots_pulse() && self.dots_pulse_deadline <= now {
            self.dots_pulse_deadline = now + DOTS_PULSE;
            self.shell.dots_pulse = !self.shell.dots_pulse;
            self.dirty = true;
        }
    }

    /// What has to follow any batch of events: the shell's requests, the matcher forgetting
    /// a chord when the focus moved, the nav layer's indicator, the next session save.
    fn after_events(&mut self) {
        let requests = std::mem::take(&mut self.shell.requests);
        for request in requests {
            self.apply_request(request);
        }
        let focus_key = (self.shell.workspace.focus(), modal_name(&self.shell.modal));
        if focus_key != self.focus_key {
            self.focus_key = focus_key;
            self.router.clear(&mut self.shell);
            self.dirty = true;
        }
        self.update_nav_indicator();
        if self.dirty && self.session_deadline.is_none() {
            self.session_deadline = Some(Instant::now() + SESSION_DEBOUNCE);
        }
        let show_all_left = self.shell.workspace.is_showing_all_left_panels();
        if self.window_state.show_all_left_panels != Some(show_all_left) {
            self.window_state.show_all_left_panels = Some(show_all_left);
            let _ = window_state::save(&self.state_dir, &self.window_state);
        }
        if self.shell.quit {
            self.quit = true;
        }
    }

    // ---- input -------------------------------------------------------------------------

    fn handle_input(&mut self, events: Vec<InputEvent>) {
        for event in events {
            match event {
                InputEvent::Key(raw) => {
                    // The raw event first: held modifiers are what the keyboard says, not
                    // what the layer makes of it.
                    self.held.observe(&raw);
                    let now = Instant::now();
                    let nav_keys = self.nav.process(raw, now);
                    for nav_key in nav_keys {
                        let keys = if self.nav.active() { vec![nav_key] } else { self.num.process(nav_key, now) };
                        for key in keys {
                            self.key(key);
                        }
                    }
                }
                InputEvent::Mouse(mouse) => {
                    let extra = self.held.mods() | self.nav.virtual_mods();
                    self.shell.held_mods = extra;
                    let event = MouseEvent { mods: mouse.mods | extra, ..mouse };
                    self.dirty |= self.shell.handle_mouse(&event);
                }
                InputEvent::Paste(text) => {
                    self.shell.handle_paste(&text);
                    self.dirty = true;
                }
                InputEvent::FocusGained => self.window_focus(true),
                InputEvent::FocusLost => self.window_focus(false),
                InputEvent::Reply(reply) => self.on_reply(reply),
            }
        }
    }

    fn key(&mut self, key: KeyEvent) {
        self.shell.held_mods = self.held.mods() | self.nav.virtual_mods();
        self.dirty = true;
        // Text from an input method and modifier keys on their own are never bindings, but
        // a program in the terminal may want them.
        if key.is_text_only() || matches!(key.key, Key::Modifier(_)) {
            self.shell.handle_key(&key);
        } else if key.is_release() {
            self.router.release(&mut self.shell, &key);
        } else {
            self.router.key(&mut self.shell, &key);
        }
    }

    fn window_focus(&mut self, focused: bool) {
        self.dirty = true;
        if focused {
            if let Some(ipc) = &mut self.ipc {
                ipc.touch_focus();
            }
        } else {
            self.window_geometry_dirty = true;
            // Nothing held now will be reported released: let go of all of it.
            let releases = self.nav.reset();
            for key in releases {
                self.key(key);
            }
            let num_releases = self.num.reset();
            for key in num_releases {
                self.key(key);
            }
            self.held.clear();
            self.router.forget_held_keys();
            if matches!(self.shell.settings.autosave, Autosave::OnWindowChange | Autosave::OnFocusChange)
                && self.shell.pane.slots().iter().any(|slot| slot.item.is_dirty() && slot.item.path().is_some())
            {
                self.shell.dispatch(&Action::new("workspace::SaveAll"));
            }
        }
        self.dots_overlay.on_focus_changed(focused, &self.shell.theme);
        self.shell.with_focused_view(|view, cx| view.focus_changed(focused, cx));
    }

    fn on_reply(&mut self, reply: TerminalReply) {
        match reply {
            TerminalReply::KittyKeyboardFlags(flags) => {
                log::info!("the terminal reports kitty keyboard flags {flags}");
                self.kitty_flags = Some(flags);
                self.parser.set_kitty_flags(flags);
                self.apply_nav_layer(false);
            }
            // The last reply of the round: no keyboard flags by now means a legacy terminal.
            TerminalReply::DeviceAttributes(_) if self.kitty_flags.is_none() => {
                log::info!("the terminal has no kitty keyboard protocol");
                self.kitty_flags = Some(0);
                self.parser.set_kitty_flags(0);
                self.shell.cmd_keys = false;
                self.apply_nav_layer(false);
                self.shell.set_status("No cmd keys from this terminal: use the ctrl-] prefix (ctrl-] ? lists every key)", false);
                self.dirty = true;
            }
            TerminalReply::Color { slot, rgb } => {
                if self.terminal_palette.set(slot, rgb) && self.shell.theme.name == themes::TERMINAL {
                    let (theme, _) = themes::load_with_palette(&self.shell.settings, &self.terminal_palette);
                    self.shell.theme = theme;
                    self.dirty = true;
                }
            }
            TerminalReply::DarkMode(dark) => {
                if self.terminal_palette.set_dark(dark) && self.shell.theme.name == themes::TERMINAL {
                    let (theme, _) = themes::load_with_palette(&self.shell.settings, &self.terminal_palette);
                    self.shell.theme = theme;
                    self.dirty = true;
                }
            }
            other => log::debug!("terminal reply: {other:?}"),
        }
    }

    /// Switch the nav layer to what is wanted and possible, delivering the releases of keys
    /// it was holding. With `announce`, say what happened in the status bar.
    fn apply_nav_layer(&mut self, announce: bool) {
        let works = self.kitty_flags.is_some_and(NavLayer::works_with);
        let on = self.nav_wanted && works;
        if on != self.nav.enabled() {
            let releases = self.nav.set_enabled(on);
            for key in releases {
                self.key(key);
            }
        }
        if on != self.num.enabled() {
            let releases = self.num.set_enabled(on);
            for key in releases {
                self.key(key);
            }
        }
        self.update_nav_indicator();
        if announce {
            let text = match (self.nav_wanted, works) {
                (true, true) => "Nav layer on: hold ; for arrows on e s d f, modifiers on j k l h",
                (true, false) => "Nav layer on, but this terminal does not report key releases, so it cannot work here",
                (false, _) => "Nav layer off",
            };
            self.shell.set_status(text, false);
        }
        self.dirty = true;
    }

    fn update_nav_indicator(&mut self) {
        let known = self.kitty_flags.is_some();
        let works = self.kitty_flags.is_some_and(NavLayer::works_with);
        let indicator = match (self.nav_wanted, known, works) {
            (false, _, _) => NavLayerIndicator::Off,
            (true, true, false) => NavLayerIndicator::Unavailable,
            _ if self.nav.active() => NavLayerIndicator::Held,
            _ => NavLayerIndicator::Ready,
        };
        if indicator != self.shell.nav_layer {
            self.shell.nav_layer = indicator;
            self.dirty = true;
        }
    }

    fn handle_ipc(&mut self, request: ipc::Request) {
        match request {
            ipc::Request::Open { targets, focus, speak } => {
                let focus = focus.unwrap_or(true);
                for target in targets {
                    if target.path.is_dir() {
                        self.shell.set_status(format!("{} is a folder: open it with projects::OpenRecent or a new bakedit", target.path.display()), true);
                        continue;
                    }
                    self.shell.open_file_range(&target.path, target.line, target.end_line, target.column, false, focus);
                }
                if let Some(text) = speak {
                    self.speaker.speak(text);
                }
            }
            ipc::Request::Speak { text } => match text {
                Some(text) => self.speaker.speak(text),
                None => self.speaker.stop(),
            },
            ipc::Request::Action { name, args } => {
                if !self.shell.dispatch(&Action { name: name.clone(), args }) {
                    self.shell.set_status(format!("Nothing here handles {name}"), true);
                }
            }
            ipc::Request::Ping => {}
        }
    }

    // ---- requests --------------------------------------------------------------------------

    fn apply_request(&mut self, request: Request) {
        match request {
            Request::ToggleNavLayer => {
                self.nav_wanted = !self.nav_wanted;
                self.apply_nav_layer(true);
            }
            Request::ReloadConfig => self.reload_config(true),
            Request::ShowKeybindings => {
                let text = self.router.listing();
                self.shell.open_page("Key Bindings", &text, None);
            }
            Request::OpenProject(root) => self.open_project(root),
            Request::PreviewTheme(name) => {
                let preview_settings = Settings { theme: Some(name), ..self.shell.settings.clone() };
                let (theme, _) = themes::load_with_palette(&preview_settings, &self.terminal_palette);
                self.shell.theme = theme;
                self.dots_overlay.sync_theme(&self.shell.theme);
            }
            Request::SelectTheme(name) => {
                self.shell.settings.theme = Some(name.clone());
                let (theme, warning) = themes::load_with_palette(&self.shell.settings, &self.terminal_palette);
                self.shell.theme = theme;
                self.dots_overlay.sync_theme(&self.shell.theme);
                let _ = settings::save_values(&settings::settings_path(), &[("theme", serde_json::Value::String(name.clone()))]);
                self.config.changed();
                if let Some(w) = warning {
                    self.shell.set_status(w, true);
                } else {
                    self.shell.set_status(format!("Theme set to {name}"), false);
                }
            }
            Request::ToggleAgentDots => {
                let enabled = !self.shell.settings.agent_dots;
                self.set_agent_dots(enabled);
                let _ = settings::save_values(&settings::settings_path(), &[("agent_dots", serde_json::Value::Bool(enabled))]);
                self.config.changed();
                self.shell.set_status(
                    if enabled {
                        "Agent Dots enabled (floating bar appears when you leave bakedit)"
                    } else {
                        "Agent Dots disabled"
                    },
                    false,
                );
            }
            Request::SaveProjectPanelGitDiff(enabled) => {
                let _ = settings::save_nested_value(
                    &settings::settings_path(),
                    "project_panel",
                    "git_diff_on_select",
                    serde_json::Value::Bool(enabled),
                );
                self.config.changed();
            }
            Request::SaveZoomOffset(offset) => {
                self.window_state.zoom_offset = offset;
                window_state::spawn_capture_and_save(self.state_dir.clone(), self.window_state.clone());
            }
            Request::ApplyOnboarding { agent_dots, theme } => {
                self.shell.settings.theme = Some(theme.clone());
                let (loaded_theme, _) = themes::load_with_palette(&self.shell.settings, &self.terminal_palette);
                self.shell.theme = loaded_theme;
                self.set_agent_dots(agent_dots);
                let _ = settings::save_values(
                    &settings::settings_path(),
                    &[
                        ("agent_dots", serde_json::Value::Bool(agent_dots)),
                        ("theme", serde_json::Value::String(theme.clone())),
                    ],
                );
                self.config.changed();
                onboarding::mark_onboarded(&self.state_dir);
                self.shell.set_status(
                    format!(
                        "Setup saved · theme: {theme} · Agent Dots: {}",
                        if agent_dots { "on" } else { "off" }
                    ),
                    false,
                );
            }
            Request::CancelOnboarding { restore_theme } => {
                self.shell.settings.theme = restore_theme;
                let (loaded_theme, _) = themes::load_with_palette(&self.shell.settings, &self.terminal_palette);
                self.shell.theme = loaded_theme;
                self.dots_overlay.sync_theme(&self.shell.theme);
                onboarding::mark_onboarded(&self.state_dir);
            }
        }
        self.dirty = true;
    }

    fn set_agent_dots(&mut self, enabled: bool) {
        self.shell.settings.agent_dots = enabled;
        self.dots_overlay.set_enabled(enabled, &self.shell.theme);
        self.dots_overlay.sync_theme(&self.shell.theme);
        match (enabled, self.dots_watcher.is_some()) {
            (true, false) => {
                let waker = self.waker.clone();
                self.dots_watcher = Some(DotsWatcher::start(move || waker.wake()));
            }
            (false, true) => {
                self.dots_watcher = None;
                self.shell.dots = None;
            }
            _ => {}
        }
    }

    /// Read settings.json, keymap.json and the theme again. Settings that build the window
    /// (where the docks are, the terminal's command) take effect at the next start.
    fn reload_config(&mut self, announce: bool) {
        let loaded = load_config(Some(&self.terminal_palette));
        let nav_setting_changed = loaded.settings.nav_layer != self.shell.settings.nav_layer;
        self.router.clear(&mut self.shell);
        self.router = loaded.router;
        self.shell.binding_hint = self.router.binding_hint();
        self.shell.settings = loaded.settings;
        self.shell.theme = loaded.theme;
        self.set_agent_dots(self.shell.settings.agent_dots);
        if nav_setting_changed {
            self.nav_wanted = self.shell.settings.nav_layer;
            self.apply_nav_layer(false);
        }
        match (self.shell.settings.usage_meter, self.usage.is_some()) {
            (true, false) => {
                let waker = self.waker.clone();
                self.usage = Some(UsageWatcher::start(move || waker.wake()));
            }
            (false, true) => self.usage = None,
            _ => {}
        }
        self.config.changed();
        match loaded.warnings.as_slice() {
            [] if announce => self.shell.set_status("Settings and keymap reloaded", false),
            [] => self.shell.set_status("Settings changed: reloaded", false),
            [one] => self.shell.set_status(one.clone(), true),
            [first, rest @ ..] => self.shell.set_status(format!("{first} (and {} more: see --log)", rest.len()), true),
        }
        self.dirty = true;
    }

    /// Leave this project for `root`: its session is saved and the new one's restored.
    fn open_project(&mut self, root: PathBuf) {
        if !root.is_dir() {
            self.shell.set_status(format!("{} is not a folder", root.display()), true);
            return;
        }
        self.save_session();
        let clipboard = std::mem::replace(&mut self.shell.clipboard, Clipboard::with_backend(Box::new(MemoryBackend::default())));
        let settings = self.shell.settings.clone();
        let theme = self.shell.theme.clone();
        let usage = std::mem::take(&mut self.shell.usage);
        let mut shell = build_shell(&root, (settings, theme), clipboard, &self.waker, &self.terminal_command, &self.state_dir, &self.router);
        shell.usage = usage;
        shell.nav_layer = self.shell.nav_layer;
        shell.cmd_keys = self.shell.cmd_keys;
        shell.zoom_offset = self.shell.zoom_offset;
        open_session(&mut shell, &self.sessions_dir, true);
        self.shell = shell;
        self.saved_session = None;
        self.router.clear(&mut self.shell);
        if let Some(ipc) = &mut self.ipc {
            ipc.set_root(&root);
        }
        if let Err(error) = session::remember_project(&self.state_dir, &root) {
            log::warn!("could not update the recent projects: {error}");
        }
        let title = window_title(&root);
        let _ = self.screen.backend_mut().write_all(title.as_bytes());
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.shell.set_status(format!("Opened {name}"), false);
    }

    /// Write the session if it changed since it was last written.
    fn save_session(&mut self) {
        self.session_deadline = None;
        if self.window_geometry_dirty {
            self.window_geometry_dirty = false;
            window_state::spawn_capture_and_save(self.state_dir.clone(), self.window_state.clone());
        }
        let current = session::capture(&self.shell);
        let Ok(text) = serde_json::to_string(&current) else { return };
        if self.saved_session.as_deref() == Some(text.as_str()) {
            return;
        }
        match session::save(&self.sessions_dir, self.shell.root(), &current) {
            Ok(()) => self.saved_session = Some(text),
            Err(error) => log::warn!("could not save the session: {error}"),
        }
    }

    // ---- drawing ---------------------------------------------------------------------------

    fn draw(&mut self) -> io::Result<()> {
        let shell = &mut self.shell;
        let mut cursor = None;
        self.screen.backend_mut().write_all(seq::SYNC_BEGIN.as_bytes())?;
        self.screen.draw(|frame| {
            cursor = shell.render(frame.buffer_mut());
            if let Some(cursor) = cursor {
                frame.set_cursor_position(Position::new(cursor.col, cursor.row));
            }
        })?;
        if let Some(cursor) = cursor {
            let blink = self.shell.settings.cursor_blink;
            let shape = match (cursor.shape, blink) {
                (CursorShape::Block, true) => seq::CursorShape::BlinkingBlock,
                (CursorShape::Block, false) => seq::CursorShape::SteadyBlock,
                (CursorShape::Bar, true) => seq::CursorShape::BlinkingBar,
                (CursorShape::Bar, false) => seq::CursorShape::SteadyBar,
                (CursorShape::Underline, true) => seq::CursorShape::BlinkingUnderline,
                (CursorShape::Underline, false) => seq::CursorShape::SteadyUnderline,
            };
            if self.cursor_shape != Some(shape) {
                self.screen.backend_mut().write_all(seq::cursor_shape(shape).as_bytes())?;
                self.cursor_shape = Some(shape);
            }
        }
        let placement = self.shell.kitty_image_placement(crate::items::image::terminal_cell_pixels());
        match placement {
            Some(p) => {
                if self.kitty_image_key.as_ref() != Some(&p.key) {
                    let seq = crate::items::image::encode_kitty_graphics(&p);
                    self.screen.backend_mut().write_all(&seq)?;
                    self.kitty_image_key = Some(p.key);
                }
            }
            None => {
                if self.kitty_image_key.take().is_some() {
                    self.screen.backend_mut().write_all(crate::items::image::delete_kitty_graphics())?;
                }
            }
        }
        let backend = self.screen.backend_mut();
        backend.write_all(seq::SYNC_END.as_bytes())?;
        backend.flush()
    }
}

/// For `--action`: send an action to the window that was used last. Returns the line to
/// print on failure.
pub fn send_action(name: &str, args: Option<serde_json::Value>) -> Result<(), String> {
    let instances = ipc::live_instances(&ipc::instances_dir());
    let target = instances.first().ok_or("No bakedit window is running")?;
    let request = ipc::Request::Action { name: name.to_string(), args };
    match ipc::send(&target.socket, &request) {
        Ok(response) if response.ok => Ok(()),
        Ok(response) => Err(response.error.unwrap_or_else(|| "the window refused the action".to_string())),
        Err(error) => Err(format!("could not reach the window (pid {}): {error}", target.pid)),
    }
}

/// For `bakedit file.rs:12` while a window is up: the running window that should open the
/// files (see [`ipc::pick_instance`]), or `None` when no window is running.
pub fn window_for_files(files: &[PathSpec]) -> Option<ipc::InstanceInfo> {
    let instances = ipc::live_instances(&ipc::instances_dir());
    let paths: Vec<PathBuf> = files.iter().map(|file| file.path.clone()).collect();
    ipc::pick_instance(&instances, &paths).cloned()
}

/// Hand the files to `target`, a window [`window_for_files`] picked.
pub fn send_files(
    target: &ipc::InstanceInfo,
    files: &[PathSpec],
    highlight: bool,
    focus: bool,
    speak: Option<String>,
) -> Result<(), String> {
    let targets = files
        .iter()
        .map(|file| ipc::OpenTarget {
            path: file.path.clone(),
            line: file.line,
            end_line: file.end_line.or_else(|| highlight.then_some(file.line).flatten()),
            column: file.column,
        })
        .collect();
    let request = ipc::Request::Open { targets, focus: (!focus).then_some(false), speak };
    match ipc::send(&target.socket, &request) {
        Ok(response) if response.ok => Ok(()),
        Ok(response) => Err(response.error.unwrap_or_else(|| "the window refused the files".to_string())),
        Err(error) => Err(format!("could not reach the window (pid {}): {error}", target.pid)),
    }
}

/// Ask the most recently focused running window to speak `text` (or stop speaking when `None`).
/// If no window is running and `text` is `Some`, speaks synchronously in this process.
pub fn send_speak(text: Option<String>) -> Result<(), String> {
    let instances = ipc::live_instances(&ipc::instances_dir());
    let Some(target) = instances.first() else {
        if let Some(text) = text {
            crate::tts::speak_blocking(&text);
        }
        return Ok(());
    };
    let request = ipc::Request::Speak { text };
    match ipc::send(&target.socket, &request) {
        Ok(response) if response.ok => Ok(()),
        Ok(response) => Err(response.error.unwrap_or_else(|| "the window refused the speak request".to_string())),
        Err(error) => Err(format!("could not reach the window (pid {}): {error}", target.pid)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modal_names_tell_the_modals_apart() {
        assert_eq!(modal_name(&None), None);
        let picker = Some(Modal::Picker { kind: crate::view::PickerKind::FileFinder, picker: crate::picker::Picker::new("") });
        assert_eq!(modal_name(&picker).as_deref(), Some("FileFinder"));
        let prompt = Some(Modal::Prompt { prompt: crate::chrome::Prompt::new("?", &["Cancel"]), answers: vec![Vec::new()] });
        assert_eq!(modal_name(&prompt).as_deref(), Some("prompt"));
    }

    #[test]
    fn config_files_that_appear_change_or_go_are_noticed_once() {
        let dir = tempfile::tempdir().unwrap();
        let keymap = dir.path().join("keymap.json");
        let mut files = ConfigFiles::watching(vec![dir.path().join("settings.json"), keymap.clone()]);
        assert!(!files.changed(), "nothing happened yet");
        std::fs::write(&keymap, "[]").unwrap();
        assert!(files.changed(), "a new file");
        assert!(!files.changed(), "noticed once");
        // A later write moves the modification time (set it explicitly: file systems round).
        let later = std::time::SystemTime::now() + Duration::from_secs(5);
        std::fs::File::options().write(true).open(&keymap).unwrap().set_modified(later).unwrap();
        assert!(files.changed());
        std::fs::remove_file(&keymap).unwrap();
        assert!(files.changed(), "a removed file");
    }

    #[test]
    fn titles_name_the_project() {
        assert_eq!(window_title(Path::new("/Users/me/Code/bakedit")), "\x1b]2;bakedit · bakedit\x1b\\");
    }

    #[test]
    fn a_fresh_project_opens_the_terminal_and_the_tree_with_the_focus_in_the_centre() {
        let (mut shell, _) = crate::shell_tests::shell();
        let sessions = tempfile::tempdir().unwrap();
        open_session(&mut shell, sessions.path(), true);
        assert_eq!(shell.workspace.focus(), Focus::Center);
        assert_eq!(shell.workspace.visible_panel(crate::workspace::DockPosition::Left), Some(PanelId::Terminal));
        assert_eq!(shell.workspace.visible_panel(crate::workspace::DockPosition::Right), Some(PanelId::Project));
    }

    #[test]
    fn a_saved_session_is_put_back_unless_restoring_is_off() {
        let sessions = tempfile::tempdir().unwrap();
        let (mut shell, _) = crate::shell_tests::shell();
        shell.dispatch(&Action::new("git_panel::Toggle"));
        session::save(sessions.path(), shell.root(), &session::capture(&shell)).unwrap();
        let (mut restored, _) = crate::shell_tests::shell();
        open_session(&mut restored, sessions.path(), true);
        assert_eq!(restored.workspace.visible_panel(crate::workspace::DockPosition::Left), Some(PanelId::Git));
        assert_eq!(restored.workspace.visible_panel(crate::workspace::DockPosition::Right), None);
        let (mut fresh, _) = crate::shell_tests::shell();
        open_session(&mut fresh, sessions.path(), false);
        assert_eq!(fresh.workspace.visible_panel(crate::workspace::DockPosition::Left), Some(PanelId::Terminal), "--no-restore");
    }
}
