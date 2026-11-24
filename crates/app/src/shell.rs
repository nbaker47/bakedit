//! The shell: the workspace, the centre's tabs, the docked panels and whatever modal is
//! open, wired together. It routes actions and input to the right view, applies the
//! effects views ask for, and draws the frame.
//!
//! The shell knows views only through the traits in `view.rs`. What a panel or an editor
//! actually is, where picker rows come from and how items are made are all supplied by the
//! app, which is why everything here can be tested with stand-ins.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bakedit_core::{Action, KeyEvent, Mods, MouseButton, MouseEvent, MouseKind, Theme};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::json;

use crate::actions;
use crate::chrome::{self, ModeIndicator, NavLayerIndicator, Prompt, Status, TabHit};
use crate::clipboard::Clipboard;
use crate::dots::{self, DotsSnapshot};
use crate::fuzzy;
use crate::hints;
use crate::layout::{self, Layout};
use crate::line_input::{InputOutcome, LineInput};
use crate::onboarding::{KeymapsGuide, OnboardingModal, OnboardingOutcome};
use crate::pane::{CloseSet, Location, Pane};
use crate::picker::{Picker, PickerEvent, PickerItem};
pub use crate::project::GitSummary;
use crate::project::{Project, ProjectEvent};
use crate::settings::Settings;
use crate::themes;
use crate::ui::{self, Canvas, Style};
use crate::usage::{self, Vendor};
use crate::view::{
    Context, Cursor, CursorShape, Cx, Effect, FsChange, Item, ItemFactory, PickerKind, PickerSource, RenderCx, View,
};
use crate::workspace::{DockPosition, Focus, PanelId, Workspace};

/// How long an ordinary status message stays up.
pub const MESSAGE_LIFETIME: Duration = Duration::from_secs(6);

/// What the one-line input modal is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputPurpose {
    GoToLine,
    SaveAs,
}

pub enum Modal {
    Picker { kind: PickerKind, picker: Picker },
    Prompt { prompt: Prompt, answers: Vec<Vec<Effect>> },
    Input { purpose: InputPurpose, input: LineInput, title: String },
    Onboarding(OnboardingModal),
    KeymapsGuide(KeymapsGuide),
}

/// Things only the app can do, collected for it to pick up after each event. (Git refreshes
/// and file changes reported by views are handled here: the shell owns the project.)
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// `bakedit::ToggleNavLayer`: switch the hold-`;` layer on or off.
    ToggleNavLayer,
    /// `bakedit::ReloadConfig`: read settings.json and keymap.json again.
    ReloadConfig,
    /// `bakedit::ShowKeybindings`: open a page listing every binding (the app has the keymap).
    ShowKeybindings,
    /// Replace this project with another directory. Unsaved files have been dealt with.
    OpenProject(PathBuf),
    /// Live-preview a built-in theme during onboarding or theme selection.
    PreviewTheme(String),
    /// Persist a chosen theme from the theme picker.
    SelectTheme(String),
    /// Toggle Agent Dots on or off and persist to settings.json.
    ToggleAgentDots,
    /// Save onboarding choices (`agent_dots` + `theme`) and write the onboarded marker.
    ApplyOnboarding { agent_dots: bool, theme: String },
    /// Cancel onboarding (restoring any previewed theme) and write the onboarded marker.
    CancelOnboarding { restore_theme: Option<String> },
}

/// Looks up the keys bound to an action, for display.
pub type BindingHint = Box<dyn Fn(&Action) -> Option<String>>;

pub struct Shell {
    pub settings: Settings,
    pub theme: Theme,
    /// What every view shares; see `project.rs`. The app calls [`Shell::poll_project`]
    /// after each wake to collect what its background work produced.
    pub project: Project,
    pub workspace: Workspace,
    pub pane: Pane,
    pub clipboard: Clipboard,
    pub modal: Option<Modal>,
    pub held_mods: Mods,
    pub quit: bool,
    pub requests: Vec<Request>,
    pub git: GitSummary,
    pub usage: Vec<Vendor>,
    pub dots: Option<DotsSnapshot>,
    pub dots_pulse: bool,
    pub nav_layer: NavLayerIndicator,
    /// The terminal sends cmd chords (the kitty keyboard protocol). Without them, hints name
    /// the `ctrl-]` prefix keys instead.
    pub cmd_keys: bool,
    /// Keys of an unfinished chord, shown in the status bar.
    pub pending_keys: Option<String>,
    /// Completions of the unfinished chord, shown once the user has paused.
    pub which_key: Vec<(String, String)>,
    /// Looks up the key binding to show beside a command palette row.
    pub binding_hint: BindingHint,
    /// bakedit's configuration folder, where `zed::OpenSettingsFile` and `zed::OpenKeymap`
    /// find (or start) the files: [`crate::settings::config_dir`] unless a test moves it.
    pub config_dir: PathBuf,
    panels: HashMap<PanelId, Box<dyn View>>,
    factory: Box<dyn ItemFactory>,
    source: Box<dyn PickerSource>,
    effects: Vec<Effect>,
    message: Option<(String, bool, Instant)>,
    layout: Layout,
    screen: Rect,
    tab_hits: Vec<TabHit>,
    /// Where the last frame drew the mode (NAV, EDIT) in the status bar: a click
    /// there switches the editor in front between nav and edit mode.
    mode_pill: Option<Rect>,
    dragging: Option<(DockPosition, PanelId)>,
    /// The view a mouse button went down in, and where it was drawn: it gets the drags and
    /// the release that follow.
    mouse_capture: Option<(Focus, Rect)>,
    palette: Vec<actions::Command>,
    /// For each picker row, the index into the list it was filtered from.
    picker_rows: Vec<usize>,
    /// Every row of an open picker that a view filled ([`View::picker_rows`]), before the
    /// query filtered them.
    view_rows: Vec<PickerItem>,
    /// The row last handed to [`View::picker_preview`], so it is only told about changes.
    previewed: Option<usize>,
    /// [`PickerSource::version`] when the open picker was last filled.
    source_version: u64,
    /// The path of the front tab, kept so a `Cx` can lend it while the pane is borrowed.
    active_path: Option<PathBuf>,
}

/// Build a `Cx` from the shell's fields without borrowing the views.
macro_rules! cx {
    ($shell:expr) => {{
        $shell.active_path = $shell.pane.active().and_then(|item| item.path()).map(Path::to_path_buf);
        Cx {
            settings: &$shell.settings,
            theme: &$shell.theme,
            clipboard: &mut $shell.clipboard,
            effects: &mut $shell.effects,
            project: &$shell.project,
            root: $shell.project.root(),
            active_path: $shell.active_path.as_deref(),
            now: Instant::now(),
            held_mods: $shell.held_mods,
        }
    }};
}

/// Which view fills a picker's rows itself, rather than the [`PickerSource`].
fn view_backed(kind: PickerKind) -> bool {
    matches!(kind, PickerKind::Links | PickerKind::Outline)
}

impl Shell {
    pub fn new(
        settings: Settings,
        theme: Theme,
        project: Project,
        clipboard: Clipboard,
        factory: Box<dyn ItemFactory>,
        source: Box<dyn PickerSource>,
    ) -> Shell {
        let workspace = Workspace::new(&[
            (PanelId::Terminal, settings.terminal.dock),
            (PanelId::Git, settings.git_panel.dock),
            (PanelId::Project, settings.project_panel.dock),
        ]);
        Shell {
            settings,
            theme,
            project,
            workspace,
            pane: Pane::new(),
            clipboard,
            modal: None,
            held_mods: Mods::empty(),
            quit: false,
            requests: Vec::new(),
            git: GitSummary::default(),
            usage: Vec::new(),
            dots: None,
            dots_pulse: false,
            nav_layer: NavLayerIndicator::Off,
            cmd_keys: true,
            pending_keys: None,
            which_key: Vec::new(),
            binding_hint: Box::new(|_| None),
            config_dir: crate::settings::config_dir(),
            panels: HashMap::new(),
            factory,
            source,
            effects: Vec::new(),
            message: None,
            layout: Layout::default(),
            screen: Rect::default(),
            tab_hits: Vec::new(),
            mode_pill: None,
            dragging: None,
            mouse_capture: None,
            palette: actions::palette(),
            picker_rows: Vec::new(),
            view_rows: Vec::new(),
            previewed: None,
            source_version: 0,
            active_path: None,
        }
    }

    /// The project directory.
    pub fn root(&self) -> &Path {
        self.project.root()
    }

    pub fn set_panel(&mut self, id: PanelId, view: Box<dyn View>) {
        self.panels.insert(id, view);
    }

    /// Make a panel that needs a `Cx` to be made (the project, the settings) and put it in
    /// its dock. Effects it pushes while being made are applied.
    pub fn build_panel(&mut self, id: PanelId, make: impl FnOnce(&mut Cx<'_>) -> Box<dyn View>) {
        let mut cx = cx!(self);
        let view = make(&mut cx);
        self.panels.insert(id, view);
        self.apply_effects();
    }

    pub fn panel_mut(&mut self, id: PanelId) -> Option<&mut (dyn View + 'static)> {
        self.panels.get_mut(&id).map(|view| view.as_mut())
    }

    pub fn set_status(&mut self, text: impl Into<String>, error: bool) {
        self.message = Some((text.into(), error, Instant::now()));
    }

    /// The status bar's message, if one is showing: its text and whether it is an error.
    pub fn status_message(&self) -> Option<(&str, bool)> {
        self.message.as_ref().map(|(text, error, _)| (text.as_str(), *error))
    }

    /// When the status message goes away by itself. Errors stay until the next message.
    pub fn message_deadline(&self) -> Option<Instant> {
        match &self.message {
            Some((_, false, at)) => Some(*at + MESSAGE_LIFETIME),
            _ => None,
        }
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The view in a dock, for the app to inspect (the terminal panel's state, say).
    pub fn panel(&self, id: PanelId) -> Option<&dyn View> {
        self.panels.get(&id).map(|view| view.as_ref())
    }

    /// What each panel wants kept in the session file, by panel title.
    pub fn panel_states(&self) -> std::collections::BTreeMap<String, serde_json::Value> {
        self.panels.iter().filter_map(|(id, view)| Some((id.title().to_string(), view.session_state()?))).collect()
    }

    /// Hand each panel what it saved in the session file.
    pub fn restore_panel_states(&mut self, states: &std::collections::BTreeMap<String, serde_json::Value>) {
        let mut cx = cx!(self);
        for (id, view) in self.panels.iter_mut() {
            if let Some(state) = states.get(id.title()) {
                view.restore_session_state(state, &mut cx);
            }
        }
        self.apply_effects();
    }

    /// Run `f` with the view that has the keyboard focus (the front tab when the centre has
    /// it) and a `Cx`, then apply the effects it pushed. Returns `None` when nothing has the
    /// focus to run it on. For the app's own hooks into the focused view (focus in and out
    /// of the whole window).
    pub fn with_focused_view<R>(&mut self, f: impl FnOnce(&mut dyn View, &mut Cx<'_>) -> R) -> Option<R> {
        let mut cx = cx!(self);
        let result = match self.workspace.focus() {
            Focus::Center => self.pane.active_mut().map(|item| f(item as &mut dyn View, &mut cx)),
            Focus::Panel(id) => self.panels.get_mut(&id).map(|view| f(view.as_mut(), &mut cx)),
        };
        self.apply_effects();
        result
    }

    // ---- focus -------------------------------------------------------------------------

    /// Change the workspace model and tell the views whose focus changed.
    fn with_workspace(&mut self, change: impl FnOnce(&mut Workspace)) {
        let before = self.workspace.focus();
        change(&mut self.workspace);
        let after = self.workspace.focus();
        if before != after {
            self.notify_focus(before, false);
            self.notify_focus(after, true);
        }
    }

    fn notify_focus(&mut self, target: Focus, focused: bool) {
        let autosave = !focused && self.settings.autosave == crate::settings::Autosave::OnFocusChange;
        let mut cx = cx!(self);
        match target {
            Focus::Center => {
                if let Some(item) = self.pane.active_mut() {
                    // Zed's "on_focus_change": a file is saved as the focus leaves it.
                    if autosave && item.is_dirty() && item.path().is_some() {
                        if let Err(message) = item.save(true, &mut cx) {
                            cx.error(message);
                        }
                    }
                    item.focus_changed(focused, &mut cx);
                }
            }
            Focus::Panel(id) => {
                if let Some(view) = self.panels.get_mut(&id) {
                    view.focus_changed(focused, &mut cx);
                }
            }
        }
    }

    pub fn set_focus(&mut self, focus: Focus) {
        self.with_workspace(|ws| match focus {
            Focus::Center => ws.focus_center(),
            Focus::Panel(id) => ws.focus_panel(id),
        });
    }

    /// The key context stack for whatever has the focus, root first.
    pub fn contexts(&self) -> Vec<Context> {
        let mut out = vec![Context::new("Workspace")];
        match &self.modal {
            Some(Modal::Picker { kind, .. }) => {
                out.push(Context::new(match kind {
                    PickerKind::FileFinder => "FileFinder",
                    PickerKind::CommandPalette => "CommandPalette",
                    PickerKind::TabSwitcher => "TabSwitcher",
                    PickerKind::Outline => "Outline",
                    PickerKind::Branches => "GitBranchSelector",
                    PickerKind::Repositories => "GitRepoSelector",
                    PickerKind::DiffBase => "GitDiffBaseSelector",
                    PickerKind::Links => "LinkPicker",
                    PickerKind::RecentProjects => "RecentProjects",
                    PickerKind::Themes => "ThemeSelector",
                }));
                out.push(Context::new("Picker"));
                out.push(Context::new("Editor").value("mode", "single_line"));
            }
            Some(Modal::Prompt { .. }) => out.push(Context::new("Prompt")),
            Some(Modal::Onboarding(_)) => out.push(Context::new("Onboarding")),
            Some(Modal::KeymapsGuide(_)) => out.push(Context::new("KeymapsGuide")),
            Some(Modal::Input { purpose, .. }) => {
                out.push(Context::new(match purpose {
                    InputPurpose::GoToLine => "GoToLine",
                    InputPurpose::SaveAs => "SaveAs",
                }));
                out.push(Context::new("Editor").value("mode", "single_line"));
            }
            None => match self.workspace.focus() {
                Focus::Center => {
                    out.push(Context::new("Pane"));
                    if let Some(item) = self.pane.active() {
                        item.contexts(&mut out);
                    }
                }
                Focus::Panel(id) => {
                    out.push(Context::new("Dock"));
                    if let Some(view) = self.panels.get(&id) {
                        view.contexts(&mut out);
                    }
                }
            },
        }
        out
    }

    // ---- actions -----------------------------------------------------------------------

    /// Run one action. Returns false if nothing wanted it.
    pub fn dispatch(&mut self, action: &Action) -> bool {
        let handled = self.dispatch_inner(action);
        self.apply_effects();
        handled
    }

    fn dispatch_inner(&mut self, action: &Action) -> bool {
        if self.modal.is_some() {
            return self.modal_action(action) || self.workspace_action(action);
        }
        let mut cx = cx!(self);
        let handled = match self.workspace.focus() {
            Focus::Center => self.pane.active_mut().is_some_and(|item| item.handle_action(action, &mut cx)),
            Focus::Panel(id) => self.panels.get_mut(&id).is_some_and(|view| view.handle_action(action, &mut cx)),
        };
        handled || self.pane_action(action) || self.workspace_action(action)
    }

    /// A key press no binding claimed.
    pub fn handle_key(&mut self, key: &KeyEvent) -> bool {
        let handled = match &mut self.modal {
            Some(Modal::Picker { picker, .. }) => match key.text.as_deref() {
                Some(text) if key.is_down() => {
                    let event = picker.insert(text);
                    self.picker_event(event);
                    true
                }
                _ => true,
            },
            Some(Modal::Input { input, .. }) => {
                if let (Some(text), true) = (key.text.as_deref(), key.is_down()) {
                    input.insert(text);
                }
                true
            }
            Some(Modal::Prompt { prompt, .. }) => {
                let choice = key.text.as_deref().and_then(|t| t.chars().next()).and_then(|c| prompt.button_for_key(c));
                if let Some(index) = choice {
                    self.answer_prompt(index);
                }
                true
            }
            Some(Modal::Onboarding(modal)) => {
                let outcome = modal.handle_key(key);
                self.handle_onboarding_outcome(outcome);
                true
            }
            Some(Modal::KeymapsGuide(guide)) => {
                if guide.handle_key(key) {
                    self.modal = None;
                }
                true
            }
            None => {
                let mut cx = cx!(self);
                match self.workspace.focus() {
                    Focus::Center => self.pane.active_mut().is_some_and(|item| item.handle_key(key, &mut cx)),
                    Focus::Panel(id) => self.panels.get_mut(&id).is_some_and(|view| view.handle_key(key, &mut cx)),
                }
            }
        };
        self.apply_effects();
        handled
    }

    pub fn handle_paste(&mut self, text: &str) -> bool {
        let handled = match &mut self.modal {
            Some(Modal::Picker { picker, .. }) => {
                let event = picker.insert(text);
                self.picker_event(event);
                true
            }
            Some(Modal::Input { input, .. }) => {
                input.insert(text);
                true
            }
            Some(Modal::Prompt { .. }) | Some(Modal::Onboarding(_)) | Some(Modal::KeymapsGuide(_)) => true,
            None => {
                let mut cx = cx!(self);
                match self.workspace.focus() {
                    Focus::Center => self.pane.active_mut().is_some_and(|item| item.handle_paste(text, &mut cx)),
                    Focus::Panel(id) => self.panels.get_mut(&id).is_some_and(|view| view.handle_paste(text, &mut cx)),
                }
            }
        };
        self.apply_effects();
        handled
    }

    /// Let every view poll its background work, refill an open picker whose rows came in
    /// late, and let an ordinary status message go once it has been up long enough.
    /// Returns true if anything needs redrawing.
    pub fn tick(&mut self) -> bool {
        let mut dirty = false;
        let mut cx = cx!(self);
        for view in self.panels.values_mut() {
            dirty |= view.tick(&mut cx);
        }
        for item in self.pane.items_mut() {
            dirty |= item.tick(&mut cx);
        }
        dirty |= !self.effects.is_empty();
        self.apply_effects();
        dirty |= self.refresh_source_picker();
        if let Some((_, error, at)) = &self.message {
            if !error && at.elapsed() >= MESSAGE_LIFETIME {
                self.message = None;
                dirty = true;
            }
        }
        dirty
    }

    /// Collect what the project's background work produced and act on it: file changes go
    /// to every view, a new git status to the status bar, finished git jobs and notices to
    /// the message line. Returns true if anything happened.
    pub fn poll_project(&mut self) -> bool {
        let events = self.project.poll();
        let happened = !events.is_empty();
        for event in events {
            match event {
                ProjectEvent::Fs(change) => self.paths_changed(&change),
                ProjectEvent::Git => self.git = self.project.git_summary(),
                ProjectEvent::JobFinished { label, result } => match result {
                    Ok(message) if message.is_empty() => {}
                    Ok(message) => self.set_status(message, false),
                    Err(reason) => self.set_status(format!("{label} failed: {reason}"), true),
                },
                ProjectEvent::Notice { text, error } => self.set_status(text, error),
            }
        }
        happened
    }

    /// While a picker whose rows come from the [`PickerSource`] is open, refill it when
    /// the source says its answer changed (the file index grew, the branches arrived),
    /// keeping the selection.
    fn refresh_source_picker(&mut self) -> bool {
        let Some(Modal::Picker { kind, .. }) = &self.modal else { return false };
        let kind = *kind;
        if view_backed(kind) || matches!(kind, PickerKind::CommandPalette | PickerKind::TabSwitcher) {
            return false;
        }
        let mut cx = cx!(self);
        let version = self.source.version(kind, &mut cx);
        if version == self.source_version {
            return false;
        }
        self.source_version = version;
        let Some(Modal::Picker { picker, .. }) = &self.modal else { return false };
        let query = picker.query().to_string();
        let mut cx = cx!(self);
        let items = self.source.rows(kind, &query, &mut cx);
        self.picker_rows = (0..items.len()).collect();
        if let Some(Modal::Picker { picker, .. }) = &mut self.modal {
            picker.refresh_items(items);
        }
        self.apply_effects();
        true
    }

    fn pane_action(&mut self, action: &Action) -> bool {
        let name = action.name.as_str();
        let skip_save = action.arg_str("save_intent") == Some("skip");
        let save_first = action.arg_str("save_intent") == Some("save");
        match name {
            "pane::ActivateNextItem" => self.switch_tab(|pane| pane.activate_next()),
            "pane::ActivatePreviousItem" => self.switch_tab(|pane| pane.activate_previous()),
            "pane::ActivateLastItem" => self.switch_tab(|pane| pane.activate_last()),
            "pane::ActivateItem" => {
                let index = action.arg_i64("index").unwrap_or(0).max(0) as usize;
                self.switch_tab(|pane| {
                    pane.activate(index);
                });
            }
            "pane::SwapItemLeft" => self.pane.swap_active(false),
            "pane::SwapItemRight" => self.pane.swap_active(true),
            "pane::CloseActiveItem" => {
                if let Some(index) = self.pane.active_index() {
                    self.close_tabs(vec![index], action, save_first, skip_save);
                }
            }
            "pane::CloseOtherItems" => self.close_set(CloseSet::Others, action, save_first, skip_save),
            "pane::CloseItemsToTheLeft" => self.close_set(CloseSet::ToTheLeft, action, save_first, skip_save),
            "pane::CloseItemsToTheRight" => self.close_set(CloseSet::ToTheRight, action, save_first, skip_save),
            "pane::CloseCleanItems" => self.close_set(CloseSet::Clean, action, save_first, skip_save),
            "pane::CloseAllItems" | "workspace::CloseAllItemsAndPanes" => {
                self.close_set(CloseSet::All, action, save_first, skip_save)
            }
            "pane::ReopenClosedItem" => {
                if let Some(location) = self.pane.pop_closed() {
                    self.open_file(&location.path, Some(location.line), Some(location.column), false, true);
                }
            }
            "pane::GoBack" | "pane::GoForward" => {
                let current = self.current_location();
                let target = if name == "pane::GoBack" { self.pane.go_back(current) } else { self.pane.go_forward(current) };
                if let Some(target) = target {
                    self.open_file_no_history(&target.path, Some(target.line), Some(target.column), false, true);
                }
            }
            "pane::RevealInProjectPanel" => match self.pane.active().and_then(|item| item.reveal_path()) {
                Some(path) => self.effects.push(Effect::RevealInProjectPanel(path)),
                None => return false,
            },
            "pane::DeploySearch" => {
                let replace = action.arg_bool("replace_enabled").unwrap_or(false);
                // What to look for first: the selection or the word at the cursor of the tab
                // the search was started from (Zed's seed_search_query_from_cursor).
                let seed = if self.settings.seed_search_query_from_cursor {
                    self.pane.active().and_then(|item| item.query_suggestion())
                } else {
                    None
                };
                self.show_singleton("project_search", |factory, cx| factory.project_search(replace, cx));
                let mut args = serde_json::Map::new();
                if let Some(query) = seed {
                    args.insert("query".into(), json!(query));
                }
                if let Some(include) = action.arg_str("include") {
                    args.insert("include".into(), json!(include));
                }
                if replace {
                    args.insert("replace".into(), json!(true));
                }
                let focus = if args.is_empty() { Action::new("search::FocusSearch") } else { Action::with_args("search::FocusSearch", args.into()) };
                let mut cx = cx!(self);
                if let Some(item) = self.pane.active_mut() {
                    item.handle_action(&focus, &mut cx);
                }
            }
            _ => return false,
        }
        true
    }

    fn switch_tab(&mut self, change: impl FnOnce(&mut Pane)) {
        let focused = self.workspace.focus() == Focus::Center;
        if focused {
            self.notify_focus(Focus::Center, false);
        }
        change(&mut self.pane);
        if focused {
            self.notify_focus(Focus::Center, true);
        }
    }

    fn workspace_action(&mut self, action: &Action) -> bool {
        let name = action.name.as_str();
        let panel_of = |prefix: &str| match prefix {
            "terminal_panel" => Some(PanelId::Terminal),
            "git_panel" => Some(PanelId::Git),
            "project_panel" => Some(PanelId::Project),
            _ => None,
        };
        if let Some((prefix, verb)) = name.split_once("::") {
            if let Some(id) = panel_of(prefix) {
                match verb {
                    // Zed registers these on the workspace: they reach the git panel from
                    // anywhere (the end of Nathan's arrow-key sequence runs while the tree
                    // has the keyboard), showing and focusing it first.
                    "FocusChanges" | "FocusEditor" if id == PanelId::Git => {
                        self.set_focus(Focus::Panel(id));
                        let mut cx = cx!(self);
                        if let Some(view) = self.panels.get_mut(&id) {
                            view.handle_action(action, &mut cx);
                        }
                    }
                    "Toggle" => self.with_workspace(|ws| ws.toggle_panel(id)),
                    "ToggleFocus" => {
                        let close = self.settings.close_panel_on_toggle;
                        self.with_workspace(|ws| {
                            if !ws.toggle_panel_focus(id) && close {
                                ws.close_panel(id);
                            }
                        });
                    }
                    "ToggleZoom" => self.with_workspace(|ws| ws.toggle_panel_zoom(id)),
                    "SwapPosition" if id == PanelId::Project => {
                        let inner_left = self.workspace.toggle_right_dock_inner_left();
                        if !self.workspace.is_dock_open(DockPosition::Right) {
                            self.with_workspace(|ws| ws.toggle_dock(DockPosition::Right));
                        }
                        let msg = if inner_left {
                            "File tree: right of git/terminal"
                        } else {
                            "File tree: far right"
                        };
                        self.set_status(msg, false);
                    }
                    _ => return false,
                }
                return true;
            }
        }
        match name {
            "workspace::ToggleZoom" => self.with_workspace(|ws| ws.toggle_zoom()),
            "workspace::ToggleAllDocks" => self.with_workspace(|ws| ws.toggle_all_docks()),
            "workspace::ToggleAllLeftPanels" => {
                let mut on = false;
                self.with_workspace(|ws| on = ws.toggle_show_all_left_panels());
                let msg = if on {
                    "All left panels: on (herdr, git, file tree)"
                } else {
                    "All left panels: off"
                };
                self.set_status(msg, false);
            }
            "workspace::ToggleLeftDock" => self.with_workspace(|ws| ws.toggle_dock(DockPosition::Left)),
            "workspace::ToggleRightDock" => self.with_workspace(|ws| ws.toggle_dock(DockPosition::Right)),
            "workspace::ToggleBottomDock" => self.with_workspace(|ws| ws.toggle_dock(DockPosition::Bottom)),
            "workspace::CloseActiveDock" => {
                let mut closed = false;
                self.with_workspace(|ws| closed = ws.close_active_dock());
                return closed;
            }
            "workspace::FocusCenterPane" => self.set_focus(Focus::Center),
            "workspace::IncreaseActiveDockSize" | "workspace::DecreaseActiveDockSize" => {
                let delta = if name.contains("Increase") { 4 } else { -4 };
                self.resize_active_dock(delta);
            }
            "workspace::ResetActiveDockSize" => {
                if let Some(position) = self.workspace.active_dock() {
                    self.workspace.reset_dock_size(position);
                }
            }
            "workspace::Save" | "workspace::SaveWithoutFormat" => self.save_active(name == "workspace::Save"),
            "workspace::SaveAll" => {
                self.save_all();
            }
            "workspace::SaveAs" => {
                let current = self.pane.active().and_then(|i| i.path()).map(|p| p.to_string_lossy().into_owned());
                let start = current.unwrap_or_else(|| format!("{}/", self.project.root().display()));
                self.modal = Some(Modal::Input {
                    purpose: InputPurpose::SaveAs,
                    input: LineInput::with_text(&start),
                    title: "Save as".into(),
                });
            }
            "workspace::NewFile" => {
                let mut cx = cx!(self);
                let item = self.factory.untitled(&mut cx);
                self.add_item(item, false);
                self.set_focus(Focus::Center);
            }
            "workspace::Open" | "file_finder::Toggle" => self.toggle_picker(PickerKind::FileFinder),
            "command_palette::Toggle" => self.toggle_picker(PickerKind::CommandPalette),
            "tab_switcher::Toggle" => {
                if self.pane.len() > 1 {
                    self.toggle_picker(PickerKind::TabSwitcher);
                    // ctrl-shift-tab starts at the other end: the tab used longest ago.
                    if action.arg_bool("select_last") == Some(true) {
                        if let Some(Modal::Picker { kind: PickerKind::TabSwitcher, picker }) = &mut self.modal {
                            picker.select(picker.items().len().saturating_sub(1));
                        }
                    }
                } else {
                    self.set_status("Only one tab is open", false);
                }
            }
            "outline::Toggle" => self.toggle_picker(PickerKind::Outline),
            "branches::OpenRecent" => self.toggle_picker(PickerKind::Branches),
            "git::SelectRepository" => self.toggle_picker(PickerKind::Repositories),
            "git::SelectDiffBase" => self.toggle_picker(PickerKind::DiffBase),
            "git::ToggleBranchDiff" => {
                let msg = self.project.toggle_branch_diff();
                self.set_status(msg, false);
            }
            "terminal::OpenLinks" => self.toggle_picker(PickerKind::Links),
            "projects::OpenRecent" => self.toggle_picker(PickerKind::RecentProjects),
            "go_to_line::Toggle" => {
                if self.pane.active().is_some() {
                    self.modal = Some(Modal::Input {
                        purpose: InputPurpose::GoToLine,
                        input: LineInput::new(),
                        title: "Go to line".into(),
                    });
                }
            }
            "git::Diff" => self.effects.push(Effect::ShowProjectDiff { path: None, focus: true }),
            "workspace::CopyPath" | "editor::CopyPath" | "workspace::CopyRelativePath" => {
                let Some(path) = self.pane.active().and_then(|item| item.path()).map(Path::to_path_buf) else { return false };
                let shown = if name == "workspace::CopyRelativePath" {
                    path.strip_prefix(self.project.root()).unwrap_or(&path).to_path_buf()
                } else {
                    path
                };
                let text = shown.to_string_lossy().into_owned();
                self.clipboard.set(&text, false);
                self.set_status(format!("Copied {text}"), false);
            }
            "editor::RevealInFileManager" => {
                let Some(path) = self.pane.active().and_then(|item| item.path()).map(Path::to_path_buf) else { return false };
                crate::external::reveal(&path);
            }
            "bakedit::RevealPath" => {
                let Some(path) = action.arg_str("path") else { return false };
                crate::external::reveal(Path::new(path));
            }
            "zed::OpenSettingsFile" | "zed::OpenSettings" => self.open_config_file("settings.json"),
            "zed::OpenKeymap" => self.open_config_file("keymap.json"),
            "zed::OpenDefaultKeymap" => self.open_page("Default Keymap", crate::keys::DEFAULT_KEYMAP, Some("JSON")),
            "zed::Quit" => self.request_quit(action.arg_str("save_intent")),
            // From outside the terminal (ctrl-~ in the editor): show the panel and add a terminal
            // to it, as Zed does. The panel itself handles it while it has the keyboard.
            "workspace::NewTerminal" => {
                self.set_focus(Focus::Panel(PanelId::Terminal));
                let mut cx = cx!(self);
                if let Some(view) = self.panels.get_mut(&PanelId::Terminal) {
                    view.handle_action(action, &mut cx);
                }
            }
            "bakedit::ToggleNavLayer" => self.requests.push(Request::ToggleNavLayer),
            "bakedit::ReloadConfig" => self.requests.push(Request::ReloadConfig),
            "bakedit::ShowKeybindings" => self.requests.push(Request::ShowKeybindings),
            "bakedit::ShowKeymapsGuide" => {
                self.modal = Some(Modal::KeymapsGuide(KeymapsGuide::new(self.cmd_keys)));
            }
            "bakedit::Onboarding" => self.open_onboarding(),
            "bakedit::SelectTheme" => self.toggle_picker(PickerKind::Themes),
            "bakedit::ToggleAgentDots" => self.requests.push(Request::ToggleAgentDots),
            "bakedit::ToggleEditMode" => {
                self.settings.vim_mode = !self.settings.vim_mode;
                let text = if self.settings.vim_mode {
                    "Nav and edit modes on: files open in nav mode, cmd-i to edit and to go back"
                } else {
                    "Nav and edit modes off: keys type everywhere"
                };
                self.set_status(text, false);
            }
            // The editor takes it while it has the keys. Anywhere else the key is used up here,
            // so ctrl-] i types no stray i into the terminal, a picker or a text field.
            "bakedit::ToggleMode" => self.set_status("Nav and edit modes are the editor's: no editor has the keys", false),
            "bakedit::OpenProject" => {
                let Some(path) = action.arg_str("path").map(PathBuf::from) else { return false };
                self.request_open_project(path, action.arg_str("save_intent"));
            }
            _ => return false,
        }
        true
    }

    /// Open `settings.json` or `keymap.json` from bakedit's configuration folder, writing a
    /// first version when there is none yet (see [`crate::settings::first_version`]).
    fn open_config_file(&mut self, name: &str) {
        let path = self.config_dir.join(name);
        if !path.exists() {
            let zed_settings = self.config_dir.parent().map(|home| home.join("zed").join("settings.json"));
            let text = crate::settings::first_version(name, zed_settings.as_deref());
            let written = std::fs::create_dir_all(&self.config_dir).and_then(|()| std::fs::write(&path, text));
            if let Err(error) = written {
                self.set_status(format!("Could not create {}: {error}", path.display()), true);
                return;
            }
        }
        self.effects.push(Effect::open(path));
    }

    fn resize_active_dock(&mut self, delta: i32) {
        let Some(position) = self.workspace.active_dock() else { return };
        let panel = match self.workspace.focus() {
            Focus::Panel(id) if self.workspace.position_of(id) == Some(position) => id,
            _ => match self.workspace.visible_panel(position) {
                Some(id) => id,
                None => return,
            },
        };
        let Some(area) = self.layout.panel(panel) else { return };
        let (current, max) = match position {
            DockPosition::Bottom => (area.height, self.screen.height.saturating_sub(layout::MIN_CENTER_HEIGHT + 2)),
            _ => (area.width, self.screen.width.saturating_sub(layout::MIN_CENTER_WIDTH + 2)),
        };
        self.workspace.resize_panel(panel, current, delta, layout::MIN_DOCK_WIDTH, max);
    }

    // ---- files and tabs ----------------------------------------------------------------

    fn current_location(&self) -> Option<Location> {
        let item = self.pane.active()?;
        let (line, column) = item.position()?;
        Some(Location { path: item.path()?.to_path_buf(), line, column })
    }

    /// Add a tab, telling a preview it replaced that it is gone.
    fn add_item(&mut self, item: Box<dyn Item>, preview: bool) {
        let (_, replaced) = self.pane.add(item, preview);
        if let Some(mut replaced) = replaced {
            let mut cx = cx!(self);
            replaced.closed(&mut cx);
        }
    }

    /// Close tab `index` and tell its item.
    fn close_item(&mut self, index: usize) {
        if let Some(mut item) = self.pane.close(index) {
            let mut cx = cx!(self);
            item.closed(&mut cx);
        }
    }

    /// Open a file in the centre (1-based position), remembering where the jump started.
    pub fn open_file(&mut self, path: &Path, line: Option<u32>, column: Option<u32>, preview: bool, focus: bool) {
        if let Some(from) = self.current_location() {
            self.pane.record_jump(from);
        }
        self.open_file_no_history(path, line, column, preview, focus);
    }

    fn open_file_no_history(&mut self, path: &Path, line: Option<u32>, column: Option<u32>, preview: bool, focus: bool) {
        let was_center = self.workspace.focus() == Focus::Center;
        if was_center {
            self.notify_focus(Focus::Center, false);
        }
        match self.pane.find_path(path) {
            Some(index) => {
                self.pane.activate(index);
                if !preview {
                    self.pane.keep(index);
                }
            }
            None => {
                let mut cx = cx!(self);
                match self.factory.open_path(path, &mut cx) {
                    Ok(item) => {
                        let preview = preview && self.settings.preview_tabs;
                        self.add_item(item, preview);
                    }
                    Err(message) => {
                        self.set_status(message, true);
                        if was_center {
                            self.notify_focus(Focus::Center, true);
                        }
                        return;
                    }
                }
            }
        }
        if let (Some(line), Some(item)) = (line, self.pane.active_mut()) {
            item.go_to(line, column);
        }
        // The file finder lists what was opened lately first.
        self.project.touch_recent(path);
        if focus {
            if was_center {
                self.notify_focus(Focus::Center, true);
            } else {
                self.set_focus(Focus::Center);
            }
        } else if was_center {
            self.notify_focus(Focus::Center, true);
        }
    }

    /// Bring the one item of a kind to the front, creating it if there is none.
    fn show_singleton(&mut self, kind: &str, make: impl FnOnce(&mut dyn ItemFactory, &mut Cx<'_>) -> Box<dyn Item>) {
        let existing = self.pane.slots().iter().position(|slot| slot.item.kind() == kind);
        match existing {
            Some(index) => {
                self.pane.activate(index);
            }
            None => {
                let mut cx = cx!(self);
                let item = make(self.factory.as_mut(), &mut cx);
                self.add_item(item, false);
            }
        }
        self.set_focus(Focus::Center);
    }

    fn save_active(&mut self, format: bool) {
        let mut cx = cx!(self);
        let Some(item) = self.pane.active_mut() else { return };
        let name = item.path().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned());
        match item.save(format, &mut cx) {
            Ok(()) => {
                if let Some(name) = name {
                    self.set_status(format!("Saved {name}"), false);
                }
            }
            Err(message) => self.set_status(message, true),
        }
    }

    fn save_all(&mut self) -> bool {
        let mut cx = cx!(self);
        let mut failures = Vec::new();
        let mut saved = 0;
        for item in self.pane.items_mut() {
            if item.is_dirty() {
                match item.save(true, &mut cx) {
                    Ok(()) => saved += 1,
                    Err(message) => failures.push(message),
                }
            }
        }
        match failures.first() {
            Some(message) => {
                self.set_status(message.clone(), true);
                false
            }
            None => {
                if saved > 0 {
                    self.set_status(format!("Saved {saved} file{}", if saved == 1 { "" } else { "s" }), false);
                }
                true
            }
        }
    }

    fn close_set(&mut self, set: CloseSet, action: &Action, save_first: bool, skip_save: bool) {
        let indices = self.pane.close_candidates(set);
        self.close_tabs(indices, action, save_first, skip_save);
    }

    /// Close tabs (indices highest first). Unsaved ones are asked about once, together.
    fn close_tabs(&mut self, indices: Vec<usize>, action: &Action, save_first: bool, skip_save: bool) {
        let dirty: Vec<usize> = indices.iter().copied().filter(|&i| self.pane.item(i).is_some_and(|item| item.is_dirty())).collect();
        if !dirty.is_empty() && !skip_save {
            if save_first {
                let mut cx = cx!(self);
                for &index in &dirty {
                    if let Some(item) = self.pane.item_mut(index) {
                        if let Err(message) = item.save(true, &mut cx) {
                            self.set_status(message, true);
                            return;
                        }
                    }
                }
            } else {
                let what = if dirty.len() == 1 {
                    let name = self.pane.item(dirty[0]).and_then(|i| i.path()).and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned());
                    format!("Save changes to {}?", name.unwrap_or_else(|| "this file".into()))
                } else {
                    format!("Save changes to {} files?", dirty.len())
                };
                let again = |intent: &str| {
                    let mut args = action.args.clone().filter(|a| a.is_object()).unwrap_or_else(|| json!({}));
                    args["save_intent"] = json!(intent);
                    vec![Effect::Dispatch(Action::with_args(action.name.clone(), args))]
                };
                self.ask(what, Some("Your changes will be lost if you don't save them.".into()), vec![
                    ("Save".into(), again("save")),
                    ("Don't Save".into(), again("skip")),
                    ("Cancel".into(), Vec::new()),
                ]);
                return;
            }
        }
        let was_center = self.workspace.focus() == Focus::Center;
        if was_center {
            self.notify_focus(Focus::Center, false);
        }
        for index in indices {
            self.close_item(index);
        }
        if was_center {
            self.notify_focus(Focus::Center, true);
        }
    }

    fn request_quit(&mut self, intent: Option<&str>) {
        let dirty = self.pane.slots().iter().filter(|slot| slot.item.is_dirty()).count();
        match intent {
            _ if dirty == 0 => self.quit = true,
            Some("skip") => self.quit = true,
            Some("save") => {
                if self.save_all() {
                    self.quit = true;
                }
            }
            _ => {
                let quit = |intent: &str| vec![Effect::Dispatch(Action::with_args("zed::Quit", json!({ "save_intent": intent })))];
                let what = format!("{dirty} file{} unsaved changes", if dirty == 1 { " has" } else { "s have" });
                self.ask(what, None, vec![
                    ("Save All and Quit".into(), quit("save")),
                    ("Quit Without Saving".into(), quit("skip")),
                    ("Cancel".into(), Vec::new()),
                ]);
            }
        }
    }

    /// Leave this project for `root`, asking about unsaved files first as quitting does.
    fn request_open_project(&mut self, root: PathBuf, intent: Option<&str>) {
        let dirty = self.pane.slots().iter().filter(|slot| slot.item.is_dirty()).count();
        let open = |shell: &mut Shell, root: PathBuf| shell.requests.push(Request::OpenProject(root));
        match intent {
            _ if dirty == 0 => open(self, root),
            Some("skip") => open(self, root),
            Some("save") => {
                if self.save_all() {
                    open(self, root);
                }
            }
            _ => {
                let again = |intent: &str| {
                    let args = json!({ "path": root.to_string_lossy(), "save_intent": intent });
                    vec![Effect::Dispatch(Action::with_args("bakedit::OpenProject", args))]
                };
                let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| root.display().to_string());
                let what = format!("{dirty} file{} unsaved changes", if dirty == 1 { " has" } else { "s have" });
                self.ask(what, Some(format!("Save before opening {name}?")), vec![
                    ("Save All and Open".into(), again("save")),
                    ("Open Without Saving".into(), again("skip")),
                    ("Cancel".into(), Vec::new()),
                ]);
            }
        }
    }

    // ---- modals ------------------------------------------------------------------------

    fn ask(&mut self, message: String, detail: Option<String>, answers: Vec<(String, Vec<Effect>)>) {
        let labels: Vec<&str> = answers.iter().map(|(label, _)| label.as_str()).collect();
        let mut prompt = Prompt::new(message, &labels);
        prompt.detail = detail;
        self.modal = Some(Modal::Prompt { prompt, answers: answers.into_iter().map(|(_, effects)| effects).collect() });
    }

    fn answer_prompt(&mut self, index: usize) {
        if let Some(Modal::Prompt { mut answers, .. }) = self.modal.take() {
            if index < answers.len() {
                self.effects.extend(answers.swap_remove(index));
            }
        }
    }

    fn toggle_picker(&mut self, kind: PickerKind) {
        if matches!(&self.modal, Some(Modal::Picker { kind: open, .. }) if *open == kind) {
            self.close_picker(false);
            return;
        }
        if self.modal.is_some() {
            self.close_picker(false);
        }
        if view_backed(kind) {
            // These rows come from one view and do not depend on what is typed: ask once.
            let mut cx = cx!(self);
            let rows = match kind {
                PickerKind::Links => self.panels.get_mut(&PanelId::Terminal).and_then(|view| view.picker_rows(kind, &mut cx)),
                _ => self.pane.active_mut().and_then(|item| item.picker_rows(kind, &mut cx)),
            };
            self.apply_effects();
            let Some(rows) = rows else {
                let why = match kind {
                    PickerKind::Links => "The terminal is not running",
                    _ if self.pane.is_empty() => "No file open to list the symbols of",
                    _ => "This tab has no outline",
                };
                self.set_status(why, false);
                return;
            };
            self.view_rows = rows;
        } else {
            self.view_rows.clear();
        }
        let include_ignored = self.project.index().is_some_and(|index| index.include_ignored());
        let placeholder = match kind {
            PickerKind::FileFinder => finder_placeholder(include_ignored),
            PickerKind::CommandPalette => "Execute a command",
            PickerKind::TabSwitcher => "Switch tab",
            PickerKind::Outline => "Go to symbol",
            PickerKind::Branches => "Switch branch",
            PickerKind::Repositories => "Select repository",
            PickerKind::DiffBase => "Diff base (uncommitted or branch)",
            PickerKind::Links => "Open a path or link from the terminal",
            PickerKind::RecentProjects => "Open a recent project",
            PickerKind::Themes => "Select a built-in theme",
        };
        let mut picker = Picker::new(placeholder);
        picker.empty_message = match kind {
            PickerKind::FileFinder => "No matching files",
            PickerKind::CommandPalette => "No matching commands",
            PickerKind::TabSwitcher => "No other tabs",
            PickerKind::Outline if self.view_rows.is_empty() => "No symbols in this file",
            PickerKind::Outline => "No matching symbols",
            PickerKind::Branches => "No matching branches",
            PickerKind::Repositories => "No git repositories found in this folder",
            PickerKind::DiffBase => "No matching branches",
            PickerKind::Links if self.view_rows.is_empty() => "No paths or links on screen: they appear here when a program prints one",
            PickerKind::Links => "No matching paths or links",
            PickerKind::RecentProjects => "No recent projects",
            PickerKind::Themes => "No matching themes",
        }
        .to_string();
        self.modal = Some(Modal::Picker { kind, picker });
        self.previewed = None;
        if !view_backed(kind) && !matches!(kind, PickerKind::CommandPalette | PickerKind::TabSwitcher | PickerKind::Themes) {
            let mut cx = cx!(self);
            self.source_version = self.source.version(kind, &mut cx);
            self.apply_effects();
        }
        self.refill_picker();
        // The tab switcher starts on the tab used before this one, so a quick tap flips
        // between the last two tabs.
        if let (PickerKind::TabSwitcher, Some(Modal::Picker { picker, .. })) = (kind, &mut self.modal) {
            picker.select(1);
        }
    }

    /// Close the open picker. A picker a view fills is told when nothing was chosen, so it
    /// can put back what its preview moved.
    fn close_picker(&mut self, chose: bool) {
        let Some(Modal::Picker { kind, .. }) = self.modal.take() else { return };
        if view_backed(kind) && !chose && self.previewed.is_some() {
            self.with_picker_view(kind, |view, cx| view.picker_preview(kind, None, cx));
        }
        self.view_rows.clear();
        self.picker_rows.clear();
        self.previewed = None;
    }

    /// Run `f` on the view that fills a picker of this kind.
    fn with_picker_view(&mut self, kind: PickerKind, f: impl FnOnce(&mut dyn View, &mut Cx<'_>)) {
        let mut cx = cx!(self);
        match kind {
            PickerKind::Links => {
                if let Some(view) = self.panels.get_mut(&PanelId::Terminal) {
                    f(view.as_mut(), &mut cx);
                }
            }
            _ => {
                if let Some(item) = self.pane.active_mut() {
                    f(item as &mut dyn View, &mut cx);
                }
            }
        }
        self.apply_effects();
    }

    /// Tell the view behind an open picker which row is highlighted now, if that changed.
    fn preview_selection(&mut self) {
        let Some(Modal::Picker { kind, picker }) = &self.modal else { return };
        let kind = *kind;
        if !view_backed(kind) {
            return;
        }
        let row = picker.selected().and_then(|selected| self.picker_rows.get(selected).copied());
        if row.is_none() || row == self.previewed {
            return;
        }
        self.previewed = row;
        self.with_picker_view(kind, |view, cx| view.picker_preview(kind, row, cx));
    }

    fn refill_picker(&mut self) {
        let Some(Modal::Picker { kind, picker }) = &self.modal else { return };
        let (kind, query) = (*kind, picker.query().to_string());
        let (items, rows): (Vec<PickerItem>, Vec<usize>) = match kind {
            PickerKind::CommandPalette => {
                let titles: Vec<&str> = self.palette.iter().map(|c| c.title.as_str()).collect();
                fuzzy::filter(titles, &query)
                    .into_iter()
                    .map(|m| {
                        let command = &self.palette[m.index];
                        let mut item = PickerItem::new(command.title.clone()).matched(m.positions);
                        item.hint = (self.binding_hint)(&command.action);
                        (item, m.index)
                    })
                    .unzip()
            }
            PickerKind::TabSwitcher => {
                let order = self.pane.by_recency();
                let cx = self.render_cx(false);
                let root = self.project.root();
                let tabs: Vec<(usize, chrome::Tab, Option<String>)> = order
                    .iter()
                    .filter_map(|&index| {
                        let item = self.pane.item(index)?;
                        let dir = item.path().and_then(|p| p.parent()).map(|d| d.strip_prefix(root).unwrap_or(d).to_string_lossy().into_owned());
                        Some((index, item.tab(&cx), dir))
                    })
                    .collect();
                let titles: Vec<&str> = tabs.iter().map(|(_, tab, _)| tab.title.as_str()).collect();
                fuzzy::filter(titles, &query)
                    .into_iter()
                    .map(|m| {
                        let (index, tab, dir) = &tabs[m.index];
                        let mut item = PickerItem::new(tab.title.clone()).matched(m.positions);
                        item.detail = dir.clone().filter(|d| !d.is_empty());
                        item.icon = tab.icon.clone();
                        item.hint = tab.dirty.then(|| "●".to_string());
                        (item, *index)
                    })
                    .unzip()
            }
            PickerKind::Themes => {
                let names: Vec<&str> = themes::BUILTIN_CHOICES.iter().map(|(n, _)| *n).collect();
                fuzzy::filter(names, &query)
                    .into_iter()
                    .map(|m| {
                        let (name, desc) = themes::BUILTIN_CHOICES[m.index];
                        let mut item = PickerItem::new(name).matched(m.positions);
                        item.detail = Some(desc.to_string());
                        if self.theme.name == name {
                            item.hint = Some("active".to_string());
                        }
                        (item, m.index)
                    })
                    .unzip()
            }
            kind if view_backed(kind) => {
                let labels: Vec<&str> = self.view_rows.iter().map(|row| row.label.as_str()).collect();
                fuzzy::filter(labels, &query)
                    .into_iter()
                    .map(|m| {
                        let mut item = self.view_rows[m.index].clone();
                        if !query.trim().is_empty() {
                            item.matched = m.positions;
                        }
                        (item, m.index)
                    })
                    .unzip()
            }
            _ => {
                let mut cx = cx!(self);
                let items = self.source.rows(kind, &query, &mut cx);
                let rows = (0..items.len()).collect();
                (items, rows)
            }
        };
        self.picker_rows = rows;
        if let Some(Modal::Picker { picker, .. }) = &mut self.modal {
            picker.set_items(items);
        }
        self.apply_effects();
        self.preview_selection();
    }

    fn picker_event(&mut self, event: PickerEvent) {
        match event {
            PickerEvent::Handled => self.preview_selection(),
            PickerEvent::QueryChanged => self.refill_picker(),
            PickerEvent::Cancelled => self.close_picker(false),
            PickerEvent::Copy => {
                if let Some(Modal::Picker { picker, .. }) = &mut self.modal {
                    let text = picker.take_copied();
                    self.clipboard.set(&text, false);
                }
            }
            PickerEvent::WantsPaste => {
                if let (Some(clip), Some(Modal::Picker { picker, .. })) = (self.clipboard.get(), &mut self.modal) {
                    picker.insert(&clip.text);
                    self.refill_picker();
                }
            }
            PickerEvent::Confirmed { secondary } => {
                let Some(Modal::Picker { kind, picker }) = &self.modal else { return };
                let kind = *kind;
                let query = picker.query().to_string();
                let row = picker.selected().and_then(|selected| self.picker_rows.get(selected).copied());
                match (kind, row) {
                    (PickerKind::CommandPalette, Some(index)) => {
                        self.close_picker(true);
                        let action = self.palette[index].action.clone();
                        self.effects.push(Effect::Dispatch(action));
                    }
                    (PickerKind::TabSwitcher, Some(index)) => {
                        self.close_picker(true);
                        self.switch_tab(|pane| {
                            pane.activate(index);
                        });
                    }
                    (PickerKind::Themes, Some(index)) => {
                        self.close_picker(true);
                        if let Some((name, _)) = themes::BUILTIN_CHOICES.get(index) {
                            self.requests.push(Request::SelectTheme((*name).to_string()));
                        }
                    }
                    (PickerKind::CommandPalette | PickerKind::TabSwitcher | PickerKind::Themes, None) => self.close_picker(true),
                    (kind, Some(row)) if view_backed(kind) => {
                        self.close_picker(true);
                        self.with_picker_view(kind, |view, cx| view.picker_confirm(kind, row, secondary, cx));
                    }
                    // Nothing chosen from a view's list: as if it were dismissed.
                    (kind, None) if view_backed(kind) => self.close_picker(false),
                    (kind, row) => {
                        self.close_picker(true);
                        let mut cx = cx!(self);
                        self.source.confirm(kind, row, &query, secondary, &mut cx);
                    }
                }
            }
        }
    }

    fn modal_action(&mut self, action: &Action) -> bool {
        let name = action.name.as_str();
        if let Some(Modal::Picker { kind, .. }) = &self.modal {
            let kind = *kind;
            match name {
                "picker::ConfirmCompletion" => return self.complete_query(),
                "picker::ConfirmInput" => return self.confirm_query(kind, action.arg_bool("secondary").unwrap_or(false)),
                "search::ToggleIncludeIgnored" if kind == PickerKind::FileFinder => {
                    self.toggle_ignored_files();
                    return true;
                }
                _ => {}
            }
        }
        match &mut self.modal {
            Some(Modal::Picker { picker, .. }) => match picker.handle_action(action) {
                Some(event) => {
                    self.picker_event(event);
                    true
                }
                None => name == "tab_switcher::CloseSelectedItem" && self.close_switcher_selection(),
            },
            Some(Modal::Prompt { prompt, .. }) => {
                match name {
                    "menu::SelectNext" => prompt.select_next(),
                    "menu::SelectPrevious" => prompt.select_previous(),
                    "menu::Confirm" | "menu::SecondaryConfirm" => {
                        let index = prompt.selected;
                        self.answer_prompt(index);
                    }
                    "menu::Cancel" => {
                        let index = prompt.cancel;
                        self.answer_prompt(index);
                    }
                    // A prompt is a question that must be answered: nothing else gets through.
                    _ => {}
                }
                true
            }
            Some(Modal::Input { purpose, input, .. }) => match name {
                "menu::Cancel" => {
                    self.modal = None;
                    true
                }
                "menu::Confirm" | "menu::SecondaryConfirm" => {
                    let (purpose, text) = (*purpose, input.text().to_string());
                    self.modal = None;
                    self.confirm_input(purpose, &text);
                    true
                }
                _ => match input.handle_action(action) {
                    InputOutcome::Ignored => false,
                    InputOutcome::Copied { text, .. } => {
                        self.clipboard.set(&text, false);
                        true
                    }
                    InputOutcome::WantsPaste => {
                        if let Some(clip) = self.clipboard.get() {
                            input.insert(&clip.text);
                        }
                        true
                    }
                    _ => true,
                },
            },
            Some(Modal::Onboarding(modal)) => {
                let synthetic = match name {
                    "menu::Cancel" => Some(bakedit_core::Key::Escape),
                    "menu::Confirm" | "menu::SecondaryConfirm" => Some(bakedit_core::Key::Enter),
                    "menu::SelectNext" => Some(bakedit_core::Key::Down),
                    "menu::SelectPrevious" => Some(bakedit_core::Key::Up),
                    _ => None,
                };
                if let Some(key) = synthetic {
                    let ev = KeyEvent::new(key, Mods::empty());
                    let outcome = modal.handle_key(&ev);
                    self.handle_onboarding_outcome(outcome);
                }
                true
            }
            Some(Modal::KeymapsGuide(guide)) => {
                match name {
                    "menu::Cancel" => self.modal = None,
                    "menu::SelectNext" => guide.scroll_by(1),
                    "menu::SelectPrevious" => guide.scroll_by(-1),
                    _ => {}
                }
                true
            }
            None => false,
        }
    }

    /// Open the 3-page interactive onboarding wizard.
    pub fn open_onboarding(&mut self) {
        self.modal = Some(Modal::Onboarding(OnboardingModal::new(
            self.settings.agent_dots,
            self.settings.theme.as_deref(),
            self.cmd_keys,
        )));
    }

    fn handle_onboarding_outcome(&mut self, outcome: OnboardingOutcome) {
        match outcome {
            OnboardingOutcome::Continue => {}
            OnboardingOutcome::PreviewTheme(name) => {
                self.requests.push(Request::PreviewTheme(name));
            }
            OnboardingOutcome::Finish { agent_dots, theme } => {
                self.modal = None;
                self.requests.push(Request::ApplyOnboarding { agent_dots, theme });
            }
            OnboardingOutcome::Cancel { restore_theme } => {
                self.modal = None;
                self.requests.push(Request::CancelOnboarding { restore_theme });
            }
        }
    }

    /// Tab in a picker: the query becomes what the selected row completes to (a file's
    /// whole path). False when the row has nothing to complete to, so that the next binding
    /// of the key (moving down the list) has it, as in Zed.
    fn complete_query(&mut self) -> bool {
        let Some(Modal::Picker { picker, .. }) = &mut self.modal else { return false };
        let Some(completion) = picker.selected().and_then(|row| picker.items()[row].completion.clone()) else { return false };
        if completion == picker.query() {
            return true;
        }
        picker.input.set_text(&completion);
        self.refill_picker();
        true
    }

    /// Alt-enter in a picker: act on the query as typed rather than on the selected row (a
    /// path that is not in the index, a branch to create). Lists that only choose among
    /// their rows have nothing to do with it.
    fn confirm_query(&mut self, kind: PickerKind, secondary: bool) -> bool {
        if view_backed(kind) || matches!(kind, PickerKind::CommandPalette | PickerKind::TabSwitcher) {
            return false;
        }
        let Some(Modal::Picker { picker, .. }) = &self.modal else { return false };
        let query = picker.query().to_string();
        self.close_picker(true);
        let mut cx = cx!(self);
        self.source.confirm(kind, None, &query, secondary, &mut cx);
        true
    }

    /// cmd-shift-i in the file finder: list the files git ignores too, or stop listing them.
    /// The index walks the project again; the open finder refills when it is done.
    fn toggle_ignored_files(&mut self) {
        let Some(index) = self.project.index() else {
            self.set_status("This project has no file index", true);
            return;
        };
        let include = !index.include_ignored();
        index.set_include_ignored(include);
        if let Some(Modal::Picker { picker, .. }) = &mut self.modal {
            picker.placeholder = finder_placeholder(include).to_string();
        }
        self.set_status(if include { "The finder lists ignored files too" } else { "The finder leaves out ignored files" }, false);
    }

    fn close_switcher_selection(&mut self) -> bool {
        let Some(Modal::Picker { kind: PickerKind::TabSwitcher, picker }) = &self.modal else { return false };
        let Some(index) = picker.selected().and_then(|selected| self.picker_rows.get(selected).copied()) else { return true };
        if !self.pane.item(index).is_some_and(|item| item.is_dirty()) {
            self.close_item(index);
            self.refill_picker();
        }
        true
    }

    fn confirm_input(&mut self, purpose: InputPurpose, text: &str) {
        match purpose {
            InputPurpose::GoToLine => match hints::parse_line_target(text) {
                Some(target) => {
                    if let Some(from) = self.current_location() {
                        self.pane.record_jump(from);
                    }
                    if let Some(item) = self.pane.active_mut() {
                        let count = item.line_count().unwrap_or(u32::MAX as usize);
                        item.go_to(target.resolve(count) as u32, target.column.map(|c| c as u32));
                    }
                }
                None => self.set_status(format!("Not a line number: {text}"), true),
            },
            InputPurpose::SaveAs => {
                let text = text.trim();
                if text.is_empty() {
                    return;
                }
                let path = crate::cli::parse_path_spec(text, self.project.root()).path;
                let mut cx = cx!(self);
                let action = Action::with_args("workspace::SaveAsPath", json!({ "path": path.to_string_lossy() }));
                let handled = self.pane.active_mut().is_some_and(|item| item.handle_action(&action, &mut cx));
                if !handled {
                    self.set_status("This tab cannot be saved to a file", true);
                }
            }
        }
    }

    // ---- effects -----------------------------------------------------------------------

    fn apply_effects(&mut self) {
        // Effects can cause more effects; a bound keeps a misbehaving view from looping.
        for _ in 0..200 {
            if self.effects.is_empty() {
                return;
            }
            let batch = std::mem::take(&mut self.effects);
            for effect in batch {
                self.apply_effect(effect);
            }
        }
        self.effects.clear();
    }

    fn apply_effect(&mut self, effect: Effect) {
        match effect {
            Effect::OpenFile { path, line, column, preview, focus } => self.open_file(&path, line, column, preview, focus),
            Effect::Focus(focus) => self.set_focus(focus),
            Effect::ShowPanel(id) => self.with_workspace(|ws| ws.open_panel(id)),
            Effect::Dispatch(action) => {
                self.dispatch_inner(&action);
            }
            Effect::RevealInProjectPanel(path) => {
                self.set_focus(Focus::Panel(PanelId::Project));
                let action = Action::with_args("project_panel::RevealPath", json!({ "path": path.to_string_lossy() }));
                let mut cx = cx!(self);
                if let Some(view) = self.panels.get_mut(&PanelId::Project) {
                    view.handle_action(&action, &mut cx);
                }
            }
            Effect::ShowProjectDiff { path, focus } => {
                let before = self.workspace.focus();
                self.show_singleton("project_diff", |factory, cx| factory.project_diff(cx));
                if let Some(path) = path {
                    let action = Action::with_args("project_diff::ShowFile", json!({ "path": path.to_string_lossy() }));
                    let mut cx = cx!(self);
                    if let Some(item) = self.pane.active_mut() {
                        item.handle_action(&action, &mut cx);
                    }
                }
                if !focus {
                    self.set_focus(before);
                }
            }
            Effect::Status { text, error } => self.set_status(text, error),
            Effect::Ask(question) => self.ask(question.message, question.detail, question.answers),
            Effect::OpenExternally(target) => crate::external::open(&target),
            Effect::RefreshGit => self.project.refresh_git(),
            Effect::Moved { from, to } => {
                let mut cx = cx!(self);
                for item in self.pane.items_mut() {
                    item.moved(&from, &to, &mut cx);
                }
            }
            Effect::PathsChanged(paths) => {
                // The view that made the change knows of it already; the index, the status
                // and every other view hear of it now rather than when the watcher does.
                self.project.paths_changed(&paths);
                self.paths_changed(&FsChange::of(paths));
            }
            Effect::CloseItem => {
                if let Some(index) = self.pane.active_index() {
                    let was_center = self.workspace.focus() == Focus::Center;
                    if was_center {
                        self.notify_focus(Focus::Center, false);
                    }
                    self.close_item(index);
                    if was_center {
                        self.notify_focus(Focus::Center, true);
                    }
                }
            }
            Effect::OpenProject(root) => self.request_open_project(root, None),
            Effect::SelectRepo(root) => {
                self.project.select_repo(&root);
                self.git = self.project.git_summary();
            }
            Effect::SetDiffBase(base) => {
                let (target, msg) = match base {
                    Some(name) => (crate::project::DiffBase::Branch(name.clone()), format!("Diff: branch vs {name}")),
                    None => (crate::project::DiffBase::Uncommitted, "Diff: uncommitted changes".to_string()),
                };
                self.project.set_diff_base(target);
                self.set_status(msg, false);
            }
            Effect::SetCommitDiff { sha, label } => {
                let msg = format!("Diff: commit {label}");
                self.project.set_diff_base(crate::project::DiffBase::Commit { sha, label });
                self.set_status(msg, false);
            }
            Effect::Quit => self.request_quit(None),
        }
    }

    /// Tell every view (the panels and every open tab) that files changed on disk.
    pub fn paths_changed(&mut self, change: &FsChange) {
        if change.is_empty() {
            return;
        }
        let mut cx = cx!(self);
        for view in self.panels.values_mut() {
            view.paths_changed(change, &mut cx);
        }
        for item in self.pane.items_mut() {
            item.paths_changed(change, &mut cx);
        }
        self.apply_effects();
    }

    /// Open a page of text nobody edits (the key bindings, the built-in keymap), or bring
    /// back the one with the same title.
    pub fn open_page(&mut self, title: &str, text: &str, language: Option<&str>) {
        let cx = self.render_cx(false);
        let existing = self
            .pane
            .slots()
            .iter()
            .position(|slot| slot.item.kind() == "page" && slot.item.tab(&cx).title == title);
        match existing {
            Some(index) => self.switch_tab(|pane| {
                pane.activate(index);
            }),
            None => {
                let mut cx = cx!(self);
                let item = self.factory.read_only(title, text, language, &mut cx);
                self.switch_tab(|pane| {
                    pane.add(item, false);
                });
            }
        }
        self.set_focus(Focus::Center);
        self.apply_effects();
    }

    /// A `RenderCx` for asking items how they look outside of drawing (tab titles).
    fn render_cx(&self, focused: bool) -> RenderCx<'_> {
        RenderCx {
            settings: &self.settings,
            theme: &self.theme,
            project: &self.project,
            root: self.project.root(),
            active_path: self.pane.active().and_then(|item| item.path()),
            now: Instant::now(),
            focused,
            zoomed: false,
            held_mods: self.held_mods,
        }
    }

    // ---- mouse -------------------------------------------------------------------------

    pub fn handle_mouse(&mut self, event: &MouseEvent) -> bool {
        let (col, row) = (event.col, event.row);
        let inside = |rect: Rect| col >= rect.x && col < rect.right() && row >= rect.y && row < rect.bottom();
        let handled = match &mut self.modal {
            Some(Modal::Picker { picker, .. }) => {
                match event.kind {
                    MouseKind::Press(MouseButton::Left) => {
                        let outcome = picker.click(col, row);
                        self.picker_event(outcome);
                    }
                    MouseKind::ScrollDown => {
                        picker.scroll_by(1);
                        self.preview_selection();
                    }
                    MouseKind::ScrollUp => {
                        picker.scroll_by(-1);
                        self.preview_selection();
                    }
                    _ => {}
                }
                true
            }
            Some(Modal::Prompt { prompt, .. }) => {
                if event.kind == MouseKind::Press(MouseButton::Left) {
                    match prompt.click(col, row) {
                        Ok(index) => self.answer_prompt(index),
                        Err(true) => {
                            let cancel = prompt.cancel;
                            self.answer_prompt(cancel);
                        }
                        Err(false) => {}
                    }
                }
                true
            }
            Some(Modal::Input { .. }) => {
                if event.kind == MouseKind::Press(MouseButton::Left) {
                    self.modal = None;
                }
                true
            }
            Some(Modal::Onboarding(modal)) => {
                modal.handle_mouse(event);
                true
            }
            Some(Modal::KeymapsGuide(guide)) => {
                guide.handle_mouse(event);
                true
            }
            None => {
                // Dragging a divider resizes its dock panel.
                if let Some((position, panel)) = self.dragging {
                    match event.kind {
                        MouseKind::Drag(MouseButton::Left) => {
                            self.drag_divider(position, panel, col, row);
                            return true;
                        }
                        MouseKind::Release(_) => {
                            self.dragging = None;
                            return true;
                        }
                        _ => {}
                    }
                }
                // Once a button went down in a view, its drags and release go there too,
                // wherever the pointer is now.
                if let Some((target, area)) = self.mouse_capture {
                    if matches!(event.kind, MouseKind::Drag(_) | MouseKind::Release(_)) {
                        if matches!(event.kind, MouseKind::Release(_)) {
                            self.mouse_capture = None;
                        }
                        let handled = self.mouse_to(target, area, event);
                        self.apply_effects();
                        return handled;
                    }
                }
                if event.kind == MouseKind::Press(MouseButton::Left) {
                    if let Some(divider) = self.layout.dividers.iter().find(|d| inside(d.rect)) {
                        self.dragging = Some((divider.position, divider.panel));
                        return true;
                    }
                    // The mode in the status bar belongs to the editor in front: a click on
                    // it switches that editor between nav and edit mode, as cmd-i does.
                    if self.mode_pill.is_some_and(inside) {
                        let action = Action::new("bakedit::ToggleMode");
                        let mut cx = cx!(self);
                        if let Some(item) = self.pane.active_mut() {
                            item.handle_action(&action, &mut cx);
                        }
                        self.apply_effects();
                        return true;
                    }
                }
                if let Some(hit) = self.tab_hits.iter().find(|hit| inside(hit.rect)).copied() {
                    match event.kind {
                        MouseKind::Press(MouseButton::Left) => {
                            self.switch_tab(|pane| {
                                pane.activate(hit.index);
                            });
                            self.set_focus(Focus::Center);
                        }
                        MouseKind::Press(MouseButton::Middle) => {
                            self.close_tabs(vec![hit.index], &Action::new("pane::CloseActiveItem"), false, false);
                        }
                        _ => {}
                    }
                    self.apply_effects();
                    return true;
                }
                let target = self
                    .layout
                    .docks
                    .iter()
                    .find(|dock| inside(dock.rect))
                    .map(|dock| (Focus::Panel(dock.panel), dock.rect))
                    .or_else(|| {
                        // The item draws below the tab bar (all of the centre when no tab is open).
                        self.layout.center.filter(|rect| inside(*rect)).map(|rect| {
                            let tabs = u16::from(!self.pane.is_empty()).min(rect.height);
                            (Focus::Center, Rect::new(rect.x, rect.y + tabs, rect.width, rect.height - tabs))
                        })
                    });
                let Some((target, area)) = target else { return false };
                if let MouseKind::Press(_) = event.kind {
                    // A click moves the focus to what was clicked; wheel and hover do not.
                    if self.workspace.focus() != target {
                        self.set_focus(target);
                    }
                    self.mouse_capture = Some((target, area));
                }
                self.mouse_to(target, area, event)
            }
        };
        self.apply_effects();
        handled
    }

    /// Hand a mouse event to the view at `target`, drawn in `area`.
    fn mouse_to(&mut self, target: Focus, area: Rect, event: &MouseEvent) -> bool {
        let mut cx = cx!(self);
        match target {
            Focus::Center => self.pane.active_mut().is_some_and(|item| item.handle_mouse(event, area, &mut cx)),
            Focus::Panel(id) => self.panels.get_mut(&id).is_some_and(|view| view.handle_mouse(event, area, &mut cx)),
        }
    }

    fn drag_divider(&mut self, position: DockPosition, panel: PanelId, col: u16, row: u16) {
        let Some(area) = self.layout.panel(panel) else { return };
        let (current, wanted, max) = match position {
            DockPosition::Left => (area.width, col.saturating_sub(area.x), self.screen.width.saturating_sub(layout::MIN_CENTER_WIDTH + 2)),
            DockPosition::Right => {
                let want = if self.workspace.is_right_dock_inner_left() {
                    col.saturating_sub(area.x)
                } else {
                    area.right().saturating_sub(col + 1)
                };
                (area.width, want, self.screen.width.saturating_sub(layout::MIN_CENTER_WIDTH + 2))
            }
            DockPosition::Bottom => (area.height, area.bottom().saturating_sub(row + 1), self.screen.height.saturating_sub(layout::MIN_CENTER_HEIGHT + 2)),
        };
        self.workspace.resize_panel(panel, current, wanted as i32 - current as i32, layout::MIN_DOCK_WIDTH, max);
    }

    // ---- drawing -----------------------------------------------------------------------

    /// Draw the whole frame. Returns where the terminal's cursor should be, if anywhere.
    pub fn render(&mut self, buf: &mut Buffer) -> Option<Cursor> {
        let screen = *buf.area();
        self.screen = screen;
        self.active_path = self.pane.active().and_then(|item| item.path()).map(Path::to_path_buf);
        let default_width = |panel: PanelId| match panel {
            PanelId::Terminal => self.settings.terminal.default_width,
            PanelId::Git => self.settings.git_panel.default_width,
            PanelId::Project => self.settings.project_panel.default_width,
        };
        self.layout = layout::compute_with(&self.workspace, screen, default_width, self.settings.usage_meter);
        let layout = self.layout.clone();
        let focus = self.workspace.focus();
        let modal_open = self.modal.is_some();
        let now = Instant::now();
        let mut cursor = None;
        let mut canvas = Canvas::new(buf, screen);
        canvas.fill(screen, self.theme.background);

        for divider in &layout.dividers {
            let focused = matches!(focus, Focus::Panel(id) if id == divider.panel);
            let color = if focused { self.theme.panel_focused_border } else { self.theme.border };
            let style = Style::colors(color, self.theme.background);
            if divider.rect.width == 1 {
                canvas.vline(divider.rect.x, divider.rect.y, divider.rect.height, "│", style);
            } else {
                canvas.hline(divider.rect.x, divider.rect.y, divider.rect.width, "─", style);
            }
        }

        for dock in &layout.docks {
            let focused = focus == Focus::Panel(dock.panel) && !modal_open;
            let cx = RenderCx {
                settings: &self.settings,
                theme: &self.theme,
                project: &self.project,
                root: self.project.root(),
                active_path: self.active_path.as_deref(),
                now,
                focused,
                zoomed: self.workspace.is_panel_zoomed(dock.panel),
                held_mods: self.held_mods,
            };
            let mut sub = canvas.sub(dock.rect);
            match self.panels.get_mut(&dock.panel) {
                Some(view) => {
                    let view_cursor = view.render(&mut sub, dock.rect, &cx);
                    if focused {
                        cursor = view_cursor;
                    }
                }
                None => sub.fill(dock.rect, self.theme.panel_bg),
            }
        }

        self.tab_hits.clear();
        if let Some(center) = layout.center {
            let focused = focus == Focus::Center && !modal_open;
            let cx = RenderCx {
                settings: &self.settings,
                theme: &self.theme,
                project: &self.project,
                root: self.project.root(),
                active_path: self.active_path.as_deref(),
                now,
                focused,
                zoomed: self.workspace.is_center_zoomed(),
                held_mods: self.held_mods,
            };
            if self.pane.is_empty() {
                draw_empty_center(&mut canvas, center, &self.theme, self.cmd_keys);
            } else {
                let tabs: Vec<chrome::Tab> = self
                    .pane
                    .slots()
                    .iter()
                    .map(|slot| {
                        let mut tab = slot.item.tab(&cx);
                        tab.preview = slot.preview;
                        tab
                    })
                    .collect();
                let active = self.pane.active_index().unwrap_or(0);
                let bar = Rect::new(center.x, center.y, center.width, 1.min(center.height));
                self.tab_hits = chrome::tab_bar(&mut canvas, bar, &tabs, active, &mut self.pane.first_tab, focused, &self.theme);
                let body = Rect::new(center.x, center.y + bar.height, center.width, center.height - bar.height);
                if let Some(item) = self.pane.active_mut() {
                    let mut sub = canvas.sub(body);
                    let item_cursor = item.render(&mut sub, body, &cx);
                    if focused {
                        cursor = item_cursor;
                    }
                }
            }
        }

        let item_status = match focus {
            Focus::Center => self.pane.active().map(|item| item.status()),
            Focus::Panel(_) => None,
        }
        .unwrap_or_default();
        let status = Status {
            mode: item_status.mode.unwrap_or(ModeIndicator::None),
            branch: self.git.branch.clone(),
            ahead: self.git.ahead,
            behind: self.git.behind,
            changes: self.git.changes,
            // A git job that is still running says so when nothing else is being said.
            message: self
                .message
                .as_ref()
                .map(|(text, error, _)| (text.clone(), *error))
                .or_else(|| self.project.busy().map(|label| (format!("{label}…"), false))),
            pending_keys: self.pending_keys.clone(),
            position: item_status.position,
            cursors: item_status.cursors.max(1),
            language: item_status.language,
            indent: item_status.indent,
            line_ending: item_status.line_ending,
            nav_layer: self.nav_layer,
            nerd_font: self.settings.icons == crate::settings::Icons::Nerd,
        };
        self.mode_pill = chrome::status_bar(&mut canvas, layout.status, &status, &self.theme);
        if let Some(strip) = layout.usage {
            let now_unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
            if self.settings.agent_dots {
                if let Some(snap) = &self.dots {
                    let dots_w = (strip.width / 2).min(60);
                    let used = dots::render_inline_strip(
                        &mut canvas,
                        Rect::new(strip.x, strip.y, dots_w, strip.height),
                        snap,
                        self.dots_pulse,
                        &self.theme,
                    );
                    let rest = Rect::new(strip.x + used, strip.y, strip.width.saturating_sub(used), strip.height);
                    usage::render(&mut canvas, rest, &self.usage, now_unix, &self.theme);
                } else {
                    usage::render(&mut canvas, strip, &self.usage, now_unix, &self.theme);
                }
            } else {
                usage::render(&mut canvas, strip, &self.usage, now_unix, &self.theme);
            }
        }

        let body = Rect::new(screen.x, screen.y, screen.width, layout.status.y.saturating_sub(screen.y));
        if !self.which_key.is_empty() && !modal_open {
            hints::which_key(&mut canvas, body, self.pending_keys.as_deref().unwrap_or(""), &self.which_key, &self.theme);
        }
        match &mut self.modal {
            Some(Modal::Picker { picker, .. }) => {
                let (col, row) = picker.render(&mut canvas, body, &self.theme);
                cursor = Some(Cursor { col, row, shape: CursorShape::Bar });
            }
            Some(Modal::Prompt { prompt, .. }) => {
                prompt.render(&mut canvas, body, &self.theme);
                cursor = None;
            }
            Some(Modal::Input { input, title, .. }) => {
                cursor = Some(draw_input_modal(&mut canvas, body, title, input, &self.theme));
            }
            Some(Modal::Onboarding(modal)) => {
                modal.pulse_on = self.dots_pulse;
                modal.render(&mut canvas, body, &self.theme);
                cursor = None;
            }
            Some(Modal::KeymapsGuide(guide)) => {
                guide.render_modal(&mut canvas, body, &self.theme);
                cursor = None;
            }
            None => {}
        }
        cursor
    }
}

/// The file finder's placeholder, which says whether ignored files are listed.
fn finder_placeholder(include_ignored: bool) -> &'static str {
    if include_ignored {
        "Search project files, ignored ones too"
    } else {
        "Search project files"
    }
}

/// What the centre says with no tab open: that nothing is open, and the keys that open
/// something. On a narrow centre the list gives way to the one key that matters most.
fn draw_empty_center(canvas: &mut Canvas<'_>, area: Rect, theme: &Theme, cmd_keys: bool) {
    canvas.fill(area, theme.editor_bg);
    let hints: [(&str, &str); 5] = if cmd_keys {
        [("cmd-p", "find a file"), ("cmd-shift-f", "search the project"), ("cmd-shift-e", "file tree"), ("cmd-t", "terminal"), ("cmd-shift-p", "all commands")]
    } else {
        [("ctrl-] p", "find a file"), ("ctrl-] f", "search the project"), ("ctrl-] e", "file tree"), ("ctrl-] t", "terminal"), ("ctrl-] shift-p", "all commands")]
    };
    let heading = Style::colors(theme.text, theme.editor_bg).bold();
    let key = Style::colors(theme.text, theme.editor_bg);
    let muted = Style::colors(theme.text_muted, theme.editor_bg);
    let key_width = hints.iter().map(|(keys, _)| ui::width(keys)).max().unwrap_or(0) as u16 + 3;
    let text_width = hints.iter().map(|(_, what)| ui::width(what)).max().unwrap_or(0) as u16;
    let block = key_width + text_width;
    if area.width >= block + 4 && area.height >= hints.len() as u16 + 4 {
        let top = area.y + (area.height - hints.len() as u16 - 2) / 3;
        let left = area.x + (area.width - block) / 2;
        canvas.text(left, top, "No file open", heading, block);
        for (i, (keys, what)) in hints.iter().enumerate() {
            let y = top + 2 + i as u16;
            canvas.text(left, y, keys, key, key_width);
            canvas.text(left + key_width, y, what, muted, text_width);
        }
    } else if area.width >= 8 && area.height >= 3 {
        // Too narrow for the list: the heading and the first key, cut cleanly.
        let room = area.width - 2;
        let top = area.y + area.height / 3;
        canvas.text_truncated(area.x + 1, top, "No file open", heading, room);
        canvas.text_truncated(area.x + 1, top + 2, &format!("{} {}", hints[0].0, hints[0].1), muted, room);
    }
}

fn draw_input_modal(canvas: &mut Canvas<'_>, screen: Rect, title: &str, input: &mut LineInput, theme: &Theme) -> Cursor {
    let bg = theme.elevated_surface;
    let width = screen.width.saturating_sub(4).min(70).max(screen.width.min(20));
    let x = screen.x + (screen.width - width) / 2;
    let y = screen.y + (screen.height / 8).min(screen.height.saturating_sub(2));
    let panel = Rect::new(x, y, width, 2.min(screen.height));
    canvas.fill(panel, bg);
    canvas.text_truncated(panel.x + 2, panel.y, title, Style::colors(theme.text_muted, bg), width.saturating_sub(4));
    let field = Rect::new(panel.x + 2, panel.y + 1, width.saturating_sub(4), 1);
    let col = input.render(
        canvas,
        field,
        Style::colors(theme.text, bg),
        Style::colors(theme.text, theme.selection_bg),
        "",
        Style::colors(theme.text_muted, bg),
    );
    Cursor { col, row: field.y, shape: CursorShape::Bar }
}
