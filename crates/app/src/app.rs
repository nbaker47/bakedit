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
use bakedit_input::{HeldMods, NavLayer, NavLayerConfig, Parser};
use crossbeam_channel::{unbounded, Receiver, RecvTimeoutError};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Position;

use crate::background::Waker;
use crate::chrome::NavLayerIndicator;
use crate::cli::PathSpec;
use crate::clipboard::{Clipboard, MemoryBackend};
use crate::factory::{AppFactory, AppSource};
use crate::host::{self, Host, HostEvent};
use crate::ipc;
use crate::keys::KeyRouter;
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
use crate::workspace::{Focus, PanelId};

/// How long the start of an escape sequence waits for the rest before it is read as Escape.
const ESCAPE_TIMEOUT: Duration = Duration::from_millis(25);
/// How long after a change the session is written.
const SESSION_DEBOUNCE: Duration = Duration::from_millis(1500);
/// How often the settings and keymap files are looked at for changes.
const CONFIG_CHECK: Duration = Duration::from_millis(1500);

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
}

/// The files whose changes reload the configuration, and when each last changed.
struct ConfigFiles {
    paths: Vec<PathBuf>,
    stamps: Vec<Option<SystemTime>>,
}

impl ConfigFiles {
    fn new() -> ConfigFiles {
        let config_home = settings::config_dir().parent().map(Path::to_path_buf).unwrap_or_default();
        let paths = vec![settings::settings_path(), settings::keymap_path(), config_home.join("zed").join("settings.json")];
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
    held: HeldMods,
    events: Receiver<AppEvent>,
    waker: Waker,
    screen: Screen,
    ipc: Option<ipc::Server>,
    usage: Option<UsageWatcher>,
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
    config: ConfigFiles,
    /// The session as last written, to skip writing the same again.
    saved_session: Option<String>,
    /// Where the keyboard was after the last event, to notice it moving.
    focus_key: (Focus, Option<String>),
    cursor_shape: Option<seq::CursorShape>,
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

fn load_config() -> Loaded {
    let loaded = settings::load();
    let mut warnings = loaded.warnings;
    let (theme, theme_warning) = themes::load(&loaded.settings);
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
        let loaded = load_config();
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
        let mut shell = build_shell(
            &options.root,
            (loaded.settings, loaded.theme),
            Clipboard::new(),
            &waker,
            &terminal_command,
            &state_dir,
            &loaded.router,
        );
        open_session(&mut shell, &sessions_dir, options.restore);
        for file in &options.files {
            shell.open_file(&file.path, file.line, file.column, false, true);
        }
        match loaded.warnings.as_slice() {
            [] => {}
            [one] => shell.set_status(one.clone(), true),
            [first, rest @ ..] => shell.set_status(format!("{first} (and {} more: see --log)", rest.len()), true),
        }
        for warning in &loaded.warnings {
            log::warn!("{warning}");
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

        let mut app = App {
            focus_key: (shell.workspace.focus(), None),
            shell,
            router: loaded.router,
            parser: Parser::new(),
            // Off until the terminal says it reports key releases (see on_reply).
            nav: NavLayer::new(NavLayerConfig { enabled: false, ..NavLayerConfig::default() }),
            held: HeldMods::new(),
            events,
            waker,
            screen,
            ipc,
            usage,
            terminal_command,
            state_dir,
            sessions_dir,
            kitty_flags: None,
            nav_wanted,
            escape_deadline: None,
            session_deadline: None,
            config_deadline: Instant::now() + CONFIG_CHECK,
            config: ConfigFiles::new(),
            saved_session: None,
            cursor_shape: None,
            dirty: true,
            quit: false,
            _host: host,
        };
        app.update_nav_indicator();
        let title = window_title(&options.root);
        let _ = app.screen.backend_mut().write_all(title.as_bytes());
        let result = app.event_loop();
        app.save_session();
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
            AppEvent::Resized => self.dirty = true,
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
                    let keys = self.nav.process(raw, Instant::now());
                    for key in keys {
                        self.key(key);
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
            // Nothing held now will be reported released: let go of all of it.
            let releases = self.nav.reset();
            for key in releases {
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
                self.shell.set_status(
                    "This terminal sends no cmd keys and no key releases: use ctrl-] as the prefix (ctrl-] t terminal, ctrl-] p files, ctrl-] ? every key)",
                    false,
                );
                self.dirty = true;
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
            ipc::Request::Open { targets } => {
                for target in targets {
                    if target.path.is_dir() {
                        self.shell.set_status(format!("{} is a folder: open it with projects::OpenRecent or a new bakedit", target.path.display()), true);
                        continue;
                    }
                    self.shell.open_file(&target.path, target.line, target.column, false, true);
                }
            }
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
        }
        self.dirty = true;
    }

    /// Read settings.json, keymap.json and the theme again. Settings that build the window
    /// (where the docks are, the terminal's command) take effect at the next start.
    fn reload_config(&mut self, announce: bool) {
        let loaded = load_config();
        let nav_setting_changed = loaded.settings.nav_layer != self.shell.settings.nav_layer;
        self.router.clear(&mut self.shell);
        self.router = loaded.router;
        self.shell.binding_hint = self.router.binding_hint();
        self.shell.settings = loaded.settings;
        self.shell.theme = loaded.theme;
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

/// For `bakedit file.rs:12` while a window is up: hand the files to the window that should
/// open them. Returns `Ok(false)` when no window is running.
pub fn send_files(files: &[PathSpec]) -> Result<bool, String> {
    let instances = ipc::live_instances(&ipc::instances_dir());
    let paths: Vec<PathBuf> = files.iter().map(|file| file.path.clone()).collect();
    let Some(target) = ipc::pick_instance(&instances, &paths) else { return Ok(false) };
    let targets = files.iter().map(|file| ipc::OpenTarget { path: file.path.clone(), line: file.line, column: file.column }).collect();
    match ipc::send(&target.socket, &ipc::Request::Open { targets }) {
        Ok(response) if response.ok => Ok(true),
        Ok(response) => Err(response.error.unwrap_or_else(|| "the window refused the files".to_string())),
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
