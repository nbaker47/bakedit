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
//!   for the app's messages and tests (both about the terminal tab in front).
//!
//! Everything else goes through `View`: keys and pastes go to the program (a key the keymap
//! did not take, releases included, in the encoding the program asked for); the mouse goes
//! to the program when it asked for it, else selects and scrolls; the link modifier
//! (`link_click_modifier`, cmd by default) plus a click on a path:line or URL opens it,
//! whatever the program does with the mouse, and underlines it while hovering;
//! `picker_rows(PickerKind::Links)` lists the paths and URLs on screen for
//! `terminal::OpenLinks`; `terminal::*` and `herdr::*` actions.
//!
//! # Layout
//!
//! One header row (the terminal tabs, a `+` button, and "fullscreen" while the panel is
//! zoomed), then the program. `workspace::NewTerminal` adds a terminal (a shell: herdr is
//! one program, started once), `pane::ActivateItem` / `ActivateNextItem` /
//! `ActivatePreviousItem` / `ActivateLastItem` switch between them and
//! `pane::CloseActiveItem` closes the one in front (the last one closes the panel).
//!
//! # herdr
//!
//! `herdr::*` actions run natively through `bakedit_herdr` on a worker thread (see
//! `herdr.rs`) so the UI never waits. When the socket is not there they type the user's
//! ctrl-b chord into the terminal instead, as the Zed keymap did.

mod header;
mod herdr;
mod launch;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use bakedit_core::{Action, Key, KeyEvent, Mods, MouseButton, MouseEvent, MouseKind, Rgb, Theme};
use bakedit_term::{
    default_search_bases, LinkTarget, ResolvedPath, Scroll, SelectionKind, TermColors, TermConfig, TermError, TermEvent, Terminal,
};
use ratatui::layout::Rect;
use ratatui::style::Modifier;

use crate::background::Waker;
use crate::picker::PickerItem;
use crate::settings::{LinkModifier, Settings};
use crate::ui::{self, Canvas, Style};
use crate::view::{Context, Cursor, CursorShape, Cx, Effect, PickerKind, RenderCx, View};

pub use launch::{LaunchEnv, TerminalLaunch};

/// Lines one wheel step scrolls the scrollback.
const WHEEL_LINES: i32 = 3;

/// A link the link picker offered.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LinkChoice {
    File(ResolvedPath),
    Url(String),
}

/// One terminal: a program on a pty, and what the panel remembers about it.
struct Tab {
    /// Runs herdr's interface (the `herdr::*` actions are about this one).
    herdr: bool,
    /// A short name for what runs in it.
    program: String,
    /// What to start in it.
    config: Option<TermConfig>,
    fallback: Option<TermConfig>,
    terminal: Option<Terminal>,
    /// Events from the terminal's thread, collected in `tick`.
    events: Arc<Mutex<Vec<TermEvent>>>,
    /// Why the program could not be started at all.
    failed: Option<String>,
    /// The program exited, with this status (`None` when killed by a signal).
    exited: Option<Option<i32>>,
    /// The working directory the program last reported (OSC 7).
    cwd: Option<PathBuf>,
    /// What the program set as its title.
    title: Option<String>,
    /// The terminal's generation at the last draw.
    drawn: Option<u64>,
}

impl Tab {
    fn new(program: String, config: TermConfig, fallback: Option<TermConfig>, herdr: bool) -> Tab {
        Tab {
            herdr,
            program,
            config: Some(config),
            fallback,
            terminal: None,
            events: Arc::new(Mutex::new(Vec::new())),
            failed: None,
            exited: None,
            cwd: None,
            title: None,
            drawn: None,
        }
    }

    fn title(&self) -> &str {
        self.title.as_deref().filter(|title| !title.trim().is_empty()).unwrap_or(&self.program)
    }

    /// Whether the tab was never started (and did not fail or end): the next draw starts it.
    fn pending(&self) -> bool {
        self.terminal.is_none() && self.failed.is_none() && self.exited.is_none()
    }
}

/// The terminal panel.
pub struct TerminalPanel {
    launch: TerminalLaunch,
    tabs: Vec<Tab>,
    active: usize,
    /// The workspace shows the panel fullscreen (as told at the last draw).
    zoomed: bool,
    /// Where the panel was last drawn, header included.
    area: Rect,
    /// Where the program was last drawn.
    body: Rect,
    header_hits: header::Hits,
    focused: bool,
    /// A press went down that is not the program's (a link, the header): the drags and the
    /// release that follow are ours too.
    swallow_release: bool,
    /// The mouse went down for a local selection (the program does not want the mouse).
    selecting: bool,
    /// The cell (in the program's coordinates) the pointer is over.
    hover: Option<(u16, u16)>,
    /// Whether what is under the pointer opens: asked once per link, not per frame.
    hover_opens: Option<(LinkTarget, bool)>,
    links: Vec<LinkChoice>,
    worker: Option<herdr::Worker>,
    pane_dirs: herdr::PaneDirs,
    /// Why the panel does not run what the settings ask for: shown in the header.
    note: Option<String>,
    /// A line for the status bar, pushed at the next chance.
    notice: Option<(String, bool)>,
}

impl TerminalPanel {
    /// A panel that will run `launch` when it is first drawn.
    pub fn new(launch: TerminalLaunch) -> TerminalPanel {
        let note = launch.notice.clone();
        let notice = launch.notice.clone().map(|text| (text, false));
        let first = Tab::new(launch.program.clone(), launch.config.clone(), launch.fallback.clone(), launch.herdr.is_some());
        TerminalPanel {
            launch,
            tabs: vec![first],
            active: 0,
            zoomed: false,
            area: Rect::default(),
            body: Rect::default(),
            header_hits: Vec::new(),
            focused: false,
            swallow_release: false,
            selecting: false,
            hover: None,
            hover_opens: None,
            links: Vec::new(),
            worker: None,
            pane_dirs: herdr::PaneDirs::default(),
            note,
            notice,
        }
    }

    /// Whether the terminal in front shows a running program: it started and its exit has
    /// not been seen (by `tick`) yet.
    pub fn is_running(&self) -> bool {
        let tab = self.tab();
        tab.terminal.is_some() && tab.exited.is_none()
    }

    /// A short name for what the terminal in front runs ("herdr", "zsh").
    pub fn program(&self) -> &str {
        &self.tab().program
    }

    /// The text on the screen of the terminal in front, for tests and diagnostics. Empty
    /// before it starts.
    pub fn screen_text(&self) -> String {
        self.tab().terminal.as_ref().map(Terminal::screen_text).unwrap_or_default()
    }

    /// How many terminals the panel holds.
    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    /// Which terminal is in front (0-based).
    pub fn active_tab(&self) -> usize {
        self.active
    }

    fn tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    fn tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    fn terminal(&self) -> Option<&Terminal> {
        self.tab().terminal.as_ref()
    }

    /// Where the program is drawn in a panel drawn in `area`: below the header row.
    fn body_of(area: Rect) -> Rect {
        let header = area.height.min(1);
        Rect::new(area.x, area.y + header, area.width, area.height - header)
    }

    // ---- starting and stopping ---------------------------------------------------------

    /// Start the program of tab `index` at `cols` x `rows`.
    fn start(&mut self, index: usize, cols: u16, rows: u16, waker: &Waker) {
        let focused = self.focused && index == self.active;
        let tab = &mut self.tabs[index];
        let Some(config) = tab.config.clone() else { return };
        let events = tab.events.clone();
        let wake = waker.clone();
        let on_event = Box::new(move |event: TermEvent| {
            if let Ok(mut queue) = events.lock() {
                queue.push(event);
            }
            wake.wake();
        });
        let sized = |config: &TermConfig| TermConfig { cols: cols.max(2), rows: rows.max(1), ..config.clone() };
        let started = match &tab.fallback {
            Some(fallback) => Terminal::spawn_with_fallback(sized(&config), sized(fallback), on_event),
            None => Terminal::spawn(sized(&config), on_event).map(|terminal| (terminal, None)),
        };
        match started {
            Ok((terminal, problem)) => {
                if let Some(problem) = problem {
                    let text = format!("{}: the terminal runs your shell instead", describe_spawn_error(&problem));
                    self.note = Some(text.clone());
                    self.notice = Some((text, false));
                    tab.herdr = false;
                    self.launch.herdr = None;
                }
                if focused {
                    terminal.send_focus(true);
                }
                tab.terminal = Some(terminal);
                tab.failed = None;
                tab.exited = None;
            }
            Err(error) => {
                let text = format!("The terminal could not start: {}", describe_spawn_error(&error));
                self.notice = Some((text.clone(), true));
                tab.failed = Some(text);
            }
        }
        tab.drawn = None;
    }

    /// Start the program of the terminal in front again after it exited.
    fn restart(&mut self, waker: &Waker) {
        let (cols, rows) = (self.body.width, self.body.height);
        let index = self.active;
        let tab = self.tab_mut();
        tab.terminal = None;
        tab.exited = None;
        if cols > 0 && rows > 0 {
            self.start(index, cols, rows, waker);
        }
    }

    /// Add a terminal running the user's shell, in the directory the one in front is in,
    /// and bring it to the front. It starts at the next draw.
    fn new_tab(&mut self, cx: &Cx<'_>) {
        let mut config = self.launch.shell.clone();
        config.cwd = Some(self.tab().cwd.clone().unwrap_or_else(|| cx.root.to_path_buf()));
        self.tabs.push(Tab::new(self.launch.shell_name.clone(), config, None, false));
        let last = self.tabs.len() - 1;
        self.activate(last);
    }

    /// Bring terminal `index` to the front, moving the focus report with it.
    fn activate(&mut self, index: usize) {
        if index >= self.tabs.len() || index == self.active {
            return;
        }
        if let Some(terminal) = self.terminal() {
            terminal.send_focus(false);
            terminal.selection_clear();
        }
        self.active = index;
        self.selecting = false;
        self.swallow_release = false;
        self.hover = None;
        self.hover_opens = None;
        if self.focused {
            if let Some(terminal) = self.terminal() {
                terminal.send_focus(true);
            }
        }
    }

    /// Close the terminal in front. The last one is not kept: the panel closes, and its
    /// next opening starts a fresh program.
    fn close_active(&mut self, cx: &mut Cx<'_>) {
        if self.tabs.len() > 1 {
            // Dropping the terminal hangs up on its program.
            self.tabs.remove(self.active);
            self.active = self.active.min(self.tabs.len() - 1);
            self.selecting = false;
            self.swallow_release = false;
            self.hover = None;
            self.hover_opens = None;
            if self.focused {
                if let Some(terminal) = self.terminal() {
                    terminal.send_focus(true);
                }
            }
            return;
        }
        let herdr = self.launch.herdr.is_some();
        let fresh = Tab::new(self.launch.program.clone(), self.launch.config.clone(), self.launch.fallback.clone(), herdr);
        self.tabs[0] = fresh;
        self.hover = None;
        self.hover_opens = None;
        cx.push(Effect::Dispatch(Action::new("terminal_panel::Toggle")));
    }

    // ---- herdr -------------------------------------------------------------------------

    fn herdr_action(&mut self, action: &Action, cx: &mut Cx<'_>) -> bool {
        let Some(operation) = herdr::operation(action) else { return false };
        let Some(client) = self.launch.herdr.clone() else {
            // No session of ours to drive: the user's own herdr, if any, is in the terminal.
            self.type_chord(operation.chord());
            return true;
        };
        // The actions are about the terminal that runs herdr: show it.
        if let Some(index) = self.tabs.iter().position(|tab| tab.herdr) {
            self.activate(index);
        }
        if self.worker.is_none() {
            self.worker = herdr::Worker::start(client, cx.project.waker().clone());
        }
        let chord = operation.chord().to_string();
        match &self.worker {
            Some(worker) if worker.send(operation) => {}
            _ => self.type_chord(&chord),
        }
        true
    }

    /// Type a herdr chord into the terminal that runs herdr, or the one in front when none
    /// does.
    fn type_chord(&mut self, chord: &str) {
        let index = self.tabs.iter().position(|tab| tab.herdr).unwrap_or(self.active);
        let tab = &self.tabs[index];
        if tab.exited.is_none() {
            if let Some(terminal) = &tab.terminal {
                terminal.send_text(chord);
            }
        }
    }

    // ---- links -------------------------------------------------------------------------

    /// Where to look for a relative path printed in the terminal: the directory of the
    /// herdr pane under the cell when there is one, the program's reported directory, the
    /// project, and the usual places (`default_search_bases`).
    fn search_bases(&mut self, root: &Path, cell: Option<(u16, u16)>) -> Vec<PathBuf> {
        let mut bases = Vec::new();
        let tab = &self.tabs[self.active];
        if let (true, Some(client), Some((col, row)), Some(terminal)) = (tab.herdr, &self.launch.herdr, cell, &tab.terminal) {
            if let Some(dir) = self.pane_dirs.dir_at(client, terminal.size(), col, row) {
                bases.push(dir);
            }
        }
        let start = self.tab().cwd.clone().unwrap_or_else(|| root.to_path_buf());
        bases.push(start.clone());
        bases.push(root.to_path_buf());
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        bases.extend(default_search_bases(&start, &home));
        let mut seen = std::collections::HashSet::new();
        bases.retain(|base| seen.insert(base.clone()));
        bases
    }

    fn resolve(&mut self, target: &LinkTarget, cx: &Cx<'_>, cell: Option<(u16, u16)>) -> Option<LinkChoice> {
        match target {
            LinkTarget::Url(url) => is_openable_url(url).then(|| LinkChoice::Url(url.clone())),
            LinkTarget::Path { .. } => {
                let bases = self.search_bases(cx.root, cell);
                target.resolve(&bases).map(LinkChoice::File)
            }
        }
    }

    fn open(choice: &LinkChoice, cx: &mut Cx<'_>) {
        match choice {
            // Focusing the editor also un-zooms the panel, so the file is visible.
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

    fn link_modifier_held(mods: Mods, settings: &Settings) -> bool {
        match settings.link_click_modifier {
            LinkModifier::Cmd => mods.contains(Mods::SUPER),
            LinkModifier::Alt => mods.contains(Mods::ALT),
            LinkModifier::Ctrl => mods.contains(Mods::CTRL),
            LinkModifier::None => true,
        }
    }

    /// The cells to underline: every row of the link under the pointer, when the link
    /// modifier is held and what is there would open.
    fn hovered_link_cells(&mut self, cx: &RenderCx<'_>) -> Vec<(u16, u16, u16)> {
        let Some((col, row)) = self.hover else { return Vec::new() };
        if !TerminalPanel::link_modifier_held(cx.held_mods, cx.settings) {
            return Vec::new();
        }
        let Some(link) = self.terminal().and_then(|terminal| terminal.link_at(col, row)) else { return Vec::new() };
        let opens = match &self.hover_opens {
            Some((target, opens)) if *target == link.target => *opens,
            _ => {
                let opens = match &link.target {
                    LinkTarget::Url(url) => is_openable_url(url),
                    target @ LinkTarget::Path { .. } => {
                        let bases = self.search_bases(cx.root, Some((link.col_start, link.row)));
                        target.resolve(&bases).is_some()
                    }
                };
                self.hover_opens = Some((link.target.clone(), opens));
                opens
            }
        };
        if !opens {
            return Vec::new();
        }
        let Some(terminal) = self.terminal() else { return Vec::new() };
        terminal.visible_links().into_iter().filter(|other| other.id == link.id).map(|other| (other.row, other.col_start, other.col_end)).collect()
    }

    // ---- drawing -----------------------------------------------------------------------

    fn draw_message(canvas: &mut Canvas<'_>, area: Rect, text: &str, theme: &Theme) {
        canvas.fill(area, theme.terminal_bg);
        if area.height == 0 || area.width < 4 {
            return;
        }
        let y = area.y + area.height / 3;
        canvas.text_truncated(area.x + 2, y, text, Style::colors(theme.text_muted, theme.terminal_bg), area.width.saturating_sub(4));
    }

    fn draw_header(&mut self, canvas: &mut Canvas<'_>, area: Rect, theme: &Theme) {
        let tabs: Vec<header::HeaderTab> =
            self.tabs.iter().map(|tab| header::HeaderTab { title: tab.title().to_string(), exited: tab.exited.is_some() }).collect();
        let row = Rect::new(area.x, area.y, area.width, area.height.min(1));
        self.header_hits = header::draw(canvas, row, &tabs, self.active, self.zoomed, self.note.as_deref(), theme);
    }

    /// Switch to the terminal an action asks for. False for an action about tabs that is
    /// not for one of them.
    fn tab_action(&mut self, name: &str, action: &Action, cx: &mut Cx<'_>) -> bool {
        let count = self.tabs.len();
        match name {
            // A terminal that has not started yet starts when the panel is shown: that one is
            // the new terminal, rather than a second one beside a program never seen.
            "workspace::NewTerminal" if self.tab().pending() => {}
            "workspace::NewTerminal" => self.new_tab(cx),
            "pane::CloseActiveItem" => self.close_active(cx),
            "pane::ActivateNextItem" => self.activate((self.active + 1) % count),
            "pane::ActivatePreviousItem" => self.activate((self.active + count - 1) % count),
            "pane::ActivateLastItem" => self.activate(count - 1),
            "pane::ActivateItem" => {
                let index = action.arg_i64("index").or_else(|| action.args.as_ref().and_then(|args| args.as_i64())).unwrap_or(0);
                self.activate((index.max(0) as usize).min(count - 1));
            }
            _ => return false,
        }
        true
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

/// Draw every cell of `terminal` into `body`. `underlined` lists `(row, first col, end col)`
/// spans to underline on top of what the program drew.
fn draw_cells(canvas: &mut Canvas<'_>, body: Rect, terminal: &Terminal, theme: &Theme, underlined: &[(u16, u16, u16)]) {
    canvas.fill(body, theme.terminal_bg);
    let colors = TermColors::from_theme(theme);
    terminal.render(&mut |col, row, cell| {
        if cell.wide_spacer {
            return;
        }
        let (x, y) = (body.x + col, body.y + row);
        if x >= body.right() || y >= body.bottom() {
            return;
        }
        let mut fg: Rgb = colors.resolve(cell.fg, colors.fg);
        let mut bg: Rgb = colors.resolve(cell.bg, colors.bg);
        if cell.inverse {
            std::mem::swap(&mut fg, &mut bg);
        }
        if cell.selected {
            bg = theme.selection_bg;
        }
        if cell.hidden {
            fg = bg;
        }
        let mut symbol = String::new();
        symbol.push(if cell.ch == '\0' { ' ' } else { cell.ch });
        symbol.extend(cell.zerowidth.iter());
        // A wide character needs its second cell inside the panel.
        if cell.wide && x + 1 >= body.right() {
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
            let hovered = underlined.iter().any(|&(r, start, end)| r == row && col >= start && col < end);
            if cell.underline != bakedit_term::Underline::None || hovered {
                modifier |= Modifier::UNDERLINED;
            }
            if cell.strikeout {
                modifier |= Modifier::CROSSED_OUT;
            }
            target.modifier = modifier;
        }
    });
}

impl View for TerminalPanel {
    fn render(&mut self, canvas: &mut Canvas<'_>, area: Rect, cx: &RenderCx<'_>) -> Option<Cursor> {
        let theme = cx.theme;
        self.area = area;
        self.zoomed = cx.zoomed;
        self.focused = cx.focused;
        if area.width == 0 || area.height == 0 {
            return None;
        }
        let body = TerminalPanel::body_of(area);
        self.body = body;
        self.draw_header(canvas, area, theme);
        if body.height == 0 {
            return None;
        }
        if self.tab().pending() {
            let index = self.active;
            self.start(index, body.width, body.height, cx.project.waker());
        }
        let underlined = self.hovered_link_cells(cx);
        let program = self.tab().program.clone();
        let exited = self.tab().exited;
        let failed = self.tab().failed.clone();
        let tab = &mut self.tabs[self.active];
        let Some(terminal) = &tab.terminal else {
            let text = failed.unwrap_or_else(|| "Starting the terminal".to_string());
            TerminalPanel::draw_message(canvas, body, &text, theme);
            return None;
        };
        terminal.resize(body.width, body.height);
        tab.drawn = Some(terminal.generation());
        draw_cells(canvas, body, terminal, theme, &underlined);

        if let Some(code) = exited {
            // The last screen stays; one quiet row at the bottom says what happened.
            let bar = Rect::new(body.x, body.bottom() - 1, body.width, 1);
            let bg = theme.elevated_surface;
            canvas.fill(bar, bg);
            let how = match code {
                Some(0) => format!("{program} exited"),
                Some(code) => format!("{program} exited with code {code}"),
                None => format!("{program} was stopped"),
            };
            let text = format!("{how}  ·  enter starts it again");
            canvas.text_truncated(bar.x + 1, bar.y, &text, Style::colors(theme.text_muted, bg), bar.width.saturating_sub(2));
            return None;
        }

        if !cx.focused {
            return None;
        }
        let cursor = terminal.cursor()?;
        let (col, row) = (body.x + cursor.col, body.y + cursor.row);
        if col >= body.right() || row >= body.bottom() {
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
        if self.tab_action(name, action, cx) {
            return true;
        }
        if self.tab().exited.is_some() && name == "menu::Confirm" {
            let waker = cx.project.waker().clone();
            self.restart(&waker);
            return true;
        }
        let Some(terminal) = self.terminal() else { return false };
        match name {
            // Nothing selected: nothing happens, and cmd-c does not reach the program either
            // (Zed's terminal, and every Mac terminal, keep cmd-c for themselves).
            "terminal::Copy" => {
                if let Some(text) = terminal.selection_text() {
                    cx.clipboard.set(&text, false);
                    terminal.selection_clear();
                }
            }
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
        if self.tab().exited.is_some() {
            if key.is_down() && key.key == Key::Enter {
                let waker = cx.project.waker().clone();
                self.restart(&waker);
            }
            return key.is_down();
        }
        let Some(terminal) = self.terminal() else { return false };
        if key.is_text_only() {
            return key.text.as_deref().is_some_and(|text| terminal.send_text(text));
        }
        terminal.send_key(key) || key.is_down()
    }

    fn handle_paste(&mut self, text: &str, _cx: &mut Cx<'_>) -> bool {
        match self.terminal() {
            Some(terminal) if self.tab().exited.is_none() => {
                terminal.send_paste(text);
                true
            }
            _ => false,
        }
    }

    fn handle_mouse(&mut self, event: &MouseEvent, area: Rect, cx: &mut Cx<'_>) -> bool {
        let body = TerminalPanel::body_of(area);
        if self.swallow_release && matches!(event.kind, MouseKind::Release(_) | MouseKind::Drag(_)) {
            if matches!(event.kind, MouseKind::Release(_)) {
                self.swallow_release = false;
            }
            return true;
        }
        // The header: tabs switch on a click and close on a middle click.
        if event.row < body.y && !self.selecting {
            return self.header_mouse(event, cx);
        }
        let local = MouseEvent {
            col: event.col.saturating_sub(body.x).min(body.width.saturating_sub(1)),
            row: event.row.saturating_sub(body.y).min(body.height.saturating_sub(1)),
            ..*event
        };
        if self.terminal().is_none() {
            return false;
        }
        let modifier = TerminalPanel::link_modifier_held(event.mods, cx.settings);
        // A click with the link modifier opens what is under it, whatever the program does
        // with the mouse: that is how a path an agent printed gets into the editor.
        if event.kind == MouseKind::Press(MouseButton::Left) && modifier {
            let link = self.terminal().and_then(|terminal| terminal.link_at(local.col, local.row));
            if let Some(link) = link {
                if let Some(choice) = self.resolve(&link.target, cx, Some((link.col_start, link.row))) {
                    TerminalPanel::open(&choice, cx);
                    self.swallow_release = true;
                    return true;
                }
            }
        }
        if event.kind == MouseKind::Move {
            let cell = Some((local.col, local.row)).filter(|_| event.row >= body.y && event.row < body.bottom());
            let changed = self.hover != cell;
            self.hover = cell;
            if self.terminal().is_some_and(|terminal| terminal.send_mouse(&local)) {
                return true;
            }
            // Only the underline depends on where the pointer is.
            return changed && modifier;
        }
        let Some(terminal) = self.tabs[self.active].terminal.as_ref() else { return false };
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
        if let Some(terminal) = self.terminal() {
            terminal.send_focus(focused);
        }
    }

    fn tick(&mut self, cx: &mut Cx<'_>) -> bool {
        let mut dirty = false;
        for tab in &mut self.tabs {
            let events = match tab.events.lock() {
                Ok(mut queue) => std::mem::take(&mut *queue),
                Err(_) => Vec::new(),
            };
            for event in events {
                match event {
                    TermEvent::Wakeup | TermEvent::Bell => {}
                    TermEvent::Title(title) => {
                        tab.title = Some(title);
                        dirty = true;
                    }
                    TermEvent::ResetTitle => {
                        tab.title = None;
                        dirty = true;
                    }
                    TermEvent::Exit(code) => {
                        tab.exited = Some(code);
                        dirty = true;
                    }
                    TermEvent::ClipboardStore(text) => {
                        cx.clipboard.set(&text, false);
                    }
                    // Reading the clipboard is not offered to programs in the terminal.
                    TermEvent::ClipboardLoad => {}
                    TermEvent::Cwd(path) => tab.cwd = Some(path),
                    TermEvent::Notification { title, body } => {
                        let text = if body.is_empty() { title } else if title.is_empty() { body } else { format!("{title}: {body}") };
                        cx.status(text);
                    }
                }
            }
        }
        if let Some(terminal) = &self.tab().terminal {
            dirty |= Some(terminal.generation()) != self.tab().drawn;
        }
        if let Some((text, error)) = self.notice.take() {
            if error {
                cx.error(text);
            } else {
                cx.status(text);
            }
        }
        let mut chords = Vec::new();
        if let Some(worker) = &self.worker {
            while let Ok(report) = worker.reports.try_recv() {
                match report {
                    herdr::Report::Failure(text) => cx.error(text),
                    herdr::Report::Fallback(chord) => chords.push(chord),
                }
            }
        }
        for chord in chords {
            self.type_chord(&chord);
        }
        dirty
    }

    fn picker_rows(&mut self, kind: PickerKind, cx: &mut Cx<'_>) -> Option<Vec<PickerItem>> {
        if kind != PickerKind::Links {
            return None;
        }
        let found = self.terminal()?.visible_links();
        let mut seen = std::collections::HashSet::new();
        let mut rows = Vec::new();
        self.links.clear();
        // Newest output is at the bottom: list from there up, each link once.
        for link in found.into_iter().rev() {
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
            let Some(choice) = self.resolve(&link.target, cx, Some((link.col_start, link.row))) else { continue };
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

impl TerminalPanel {
    /// A mouse event over the header.
    fn header_mouse(&mut self, event: &MouseEvent, cx: &mut Cx<'_>) -> bool {
        let hit = header::hit(&self.header_hits, event.col);
        match event.kind {
            MouseKind::Press(MouseButton::Left) => {
                match hit {
                    Some(header::Hit::Tab(index)) => self.activate(index),
                    Some(header::Hit::New) => self.new_tab(cx),
                    None => {}
                }
                self.swallow_release = true;
            }
            MouseKind::Press(MouseButton::Middle) => {
                if let Some(header::Hit::Tab(index)) = hit {
                    self.activate(index);
                    self.close_active(cx);
                }
                self.swallow_release = true;
            }
            _ => {}
        }
        true
    }
}

#[cfg(test)]
mod tests;
