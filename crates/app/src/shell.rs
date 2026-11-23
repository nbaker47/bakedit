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
use crate::fuzzy;
use crate::hints;
use crate::layout::{self, Layout};
use crate::line_input::{InputOutcome, LineInput};
use crate::pane::{CloseSet, Location, Pane};
use crate::picker::{Picker, PickerEvent, PickerItem};
use crate::settings::Settings;
use crate::ui::{Canvas, Style};
use crate::usage::{self, Vendor};
use crate::view::{Context, Cursor, CursorShape, Cx, Effect, Item, ItemFactory, PickerKind, PickerSource, RenderCx, View};
use crate::workspace::{DockPosition, Focus, PanelId, Workspace};

/// How long an ordinary status message stays up.
const MESSAGE_LIFETIME: Duration = Duration::from_secs(6);

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
}

/// Things only the app can do, collected for it to pick up after each event.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    RefreshGit,
    PathsChanged(Vec<PathBuf>),
    ToggleNavLayer,
    ReloadConfig,
    ShowKeybindings,
}

/// Git facts for the status bar, supplied by the app.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GitSummary {
    pub branch: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub changes: usize,
}

pub struct Shell {
    pub settings: Settings,
    pub theme: Theme,
    pub root: PathBuf,
    pub workspace: Workspace,
    pub pane: Pane,
    pub clipboard: Clipboard,
    pub modal: Option<Modal>,
    pub held_mods: Mods,
    pub quit: bool,
    pub requests: Vec<Request>,
    pub git: GitSummary,
    pub usage: Vec<Vendor>,
    pub nav_layer: NavLayerIndicator,
    /// Keys of an unfinished chord, shown in the status bar.
    pub pending_keys: Option<String>,
    /// Completions of the unfinished chord, shown once the user has paused.
    pub which_key: Vec<(String, String)>,
    /// Looks up the key binding to show beside a command palette row.
    pub binding_hint: Box<dyn Fn(&Action) -> Option<String>>,
    panels: HashMap<PanelId, Box<dyn View>>,
    factory: Box<dyn ItemFactory>,
    source: Box<dyn PickerSource>,
    effects: Vec<Effect>,
    message: Option<(String, bool, Instant)>,
    layout: Layout,
    screen: Rect,
    tab_hits: Vec<TabHit>,
    dragging: Option<DockPosition>,
    palette: Vec<actions::Command>,
    /// For each picker row, the index into the list it was filtered from.
    picker_rows: Vec<usize>,
}

/// Build a `Cx` from the shell's fields without borrowing the views.
macro_rules! cx {
    ($shell:expr) => {
        Cx {
            settings: &$shell.settings,
            theme: &$shell.theme,
            clipboard: &mut $shell.clipboard,
            effects: &mut $shell.effects,
            root: &$shell.root,
            now: Instant::now(),
            held_mods: $shell.held_mods,
        }
    };
}

impl Shell {
    pub fn new(
        settings: Settings,
        theme: Theme,
        root: PathBuf,
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
            root,
            workspace,
            pane: Pane::new(),
            clipboard,
            modal: None,
            held_mods: Mods::empty(),
            quit: false,
            requests: Vec::new(),
            git: GitSummary::default(),
            usage: Vec::new(),
            nav_layer: NavLayerIndicator::Off,
            pending_keys: None,
            which_key: Vec::new(),
            binding_hint: Box::new(|_| None),
            panels: HashMap::new(),
            factory,
            source,
            effects: Vec::new(),
            message: None,
            layout: Layout::default(),
            screen: Rect::default(),
            tab_hits: Vec::new(),
            dragging: None,
            palette: actions::palette(),
            picker_rows: Vec::new(),
        }
    }

    pub fn set_panel(&mut self, id: PanelId, view: Box<dyn View>) {
        self.panels.insert(id, view);
    }

    pub fn panel_mut(&mut self, id: PanelId) -> Option<&mut (dyn View + 'static)> {
        self.panels.get_mut(&id).map(|view| view.as_mut())
    }

    pub fn set_status(&mut self, text: impl Into<String>, error: bool) {
        self.message = Some((text.into(), error, Instant::now()));
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
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
        let mut cx = cx!(self);
        match target {
            Focus::Center => {
                if let Some(item) = self.pane.active_mut() {
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
                    PickerKind::Links => "LinkPicker",
                    PickerKind::RecentProjects => "RecentProjects",
                }));
                out.push(Context::new("Picker"));
                out.push(Context::new("Editor").value("mode", "single_line"));
            }
            Some(Modal::Prompt { .. }) => out.push(Context::new("Prompt")),
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
            Some(Modal::Prompt { .. }) => true,
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

    /// Let every view poll its background work. Returns true if anything needs redrawing.
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
        if let Some((_, error, at)) = &self.message {
            if !error && at.elapsed() > MESSAGE_LIFETIME {
                self.message = None;
                dirty = true;
            }
        }
        dirty
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
            "pane::RevealInProjectPanel" => match self.pane.active().and_then(|item| item.path()).map(Path::to_path_buf) {
                Some(path) => self.effects.push(Effect::RevealInProjectPanel(path)),
                None => return false,
            },
            "pane::DeploySearch" => {
                let replace = action.arg_bool("replace_enabled").unwrap_or(false);
                self.show_singleton("project_search", |factory, cx| factory.project_search(replace, cx));
                if let Some(item) = self.pane.active_mut() {
                    let mut cx = cx!(self);
                    item.handle_action(&Action::new("search::FocusSearch"), &mut cx);
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
                    _ => return false,
                }
                return true;
            }
        }
        match name {
            "workspace::ToggleZoom" => self.with_workspace(|ws| ws.toggle_zoom()),
            "workspace::ToggleAllDocks" => self.with_workspace(|ws| ws.toggle_all_docks()),
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
                let start = current.unwrap_or_else(|| format!("{}/", self.root.display()));
                self.modal = Some(Modal::Input {
                    purpose: InputPurpose::SaveAs,
                    input: LineInput::with_text(&start),
                    title: "Save as".into(),
                });
            }
            "workspace::NewFile" => {
                let mut cx = cx!(self);
                let item = self.factory.untitled(&mut cx);
                self.pane.add(item, false);
                self.set_focus(Focus::Center);
            }
            "workspace::Open" | "file_finder::Toggle" => self.toggle_picker(PickerKind::FileFinder),
            "command_palette::Toggle" => self.toggle_picker(PickerKind::CommandPalette),
            "tab_switcher::Toggle" => {
                if self.pane.len() > 1 {
                    self.toggle_picker(PickerKind::TabSwitcher);
                }
            }
            "outline::Toggle" => self.toggle_picker(PickerKind::Outline),
            "branches::OpenRecent" => self.toggle_picker(PickerKind::Branches),
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
                    path.strip_prefix(&self.root).unwrap_or(&path).to_path_buf()
                } else {
                    path
                };
                let text = shown.to_string_lossy().into_owned();
                self.clipboard.set(&text, false);
                self.set_status(format!("Copied {text}"), false);
            }
            "editor::RevealInFileManager" => {
                let Some(path) = self.pane.active().and_then(|item| item.path()).map(Path::to_path_buf) else { return false };
                reveal_in_file_manager(&path);
            }
            "zed::OpenSettingsFile" | "zed::OpenSettings" => self.effects.push(Effect::open(crate::settings::settings_path())),
            "zed::OpenKeymap" => self.effects.push(Effect::open(crate::settings::keymap_path())),
            "zed::Quit" => self.request_quit(action.arg_str("save_intent")),
            "bakedit::ToggleNavLayer" => self.requests.push(Request::ToggleNavLayer),
            "bakedit::ReloadConfig" => self.requests.push(Request::ReloadConfig),
            "bakedit::ShowKeybindings" => self.requests.push(Request::ShowKeybindings),
            _ => return false,
        }
        true
    }

    fn resize_active_dock(&mut self, delta: i32) {
        let Some(position) = self.workspace.active_dock() else { return };
        let Some(area) = self.layout.dock(position).map(|dock| dock.rect) else { return };
        let (current, max) = match position {
            DockPosition::Bottom => (area.height, self.screen.height.saturating_sub(layout::MIN_CENTER_HEIGHT + 2)),
            _ => (area.width, self.screen.width.saturating_sub(layout::MIN_CENTER_WIDTH + 2)),
        };
        self.workspace.resize_dock(position, current, delta, layout::MIN_DOCK_WIDTH, max);
    }

    // ---- files and tabs ----------------------------------------------------------------

    fn current_location(&self) -> Option<Location> {
        let item = self.pane.active()?;
        let (line, column) = item.position()?;
        Some(Location { path: item.path()?.to_path_buf(), line, column })
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
                        self.pane.add(item, preview && self.settings.preview_tabs);
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
                self.pane.add(item, false);
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
            self.pane.close(index);
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
            self.modal = None;
            return;
        }
        let placeholder = match kind {
            PickerKind::FileFinder => "Search project files",
            PickerKind::CommandPalette => "Execute a command",
            PickerKind::TabSwitcher => "Switch tab",
            PickerKind::Outline => "Go to symbol",
            PickerKind::Branches => "Switch branch",
            PickerKind::Links => "Open a path or link from the terminal",
            PickerKind::RecentProjects => "Open a recent project",
        };
        let mut picker = Picker::new(placeholder);
        picker.empty_message = match kind {
            PickerKind::FileFinder => "No matching files",
            PickerKind::CommandPalette => "No matching commands",
            PickerKind::TabSwitcher => "No other tabs",
            PickerKind::Outline => "No symbols in this file",
            PickerKind::Branches => "No matching branches",
            PickerKind::Links => "No paths or links on screen",
            PickerKind::RecentProjects => "No recent projects",
        }
        .to_string();
        self.modal = Some(Modal::Picker { kind, picker });
        self.refill_picker();
        // The tab switcher starts on the tab used before this one, so a quick tap flips
        // between the last two tabs.
        if let (PickerKind::TabSwitcher, Some(Modal::Picker { picker, .. })) = (kind, &mut self.modal) {
            picker.select(1);
        }
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
                let cx = RenderCx {
                    settings: &self.settings,
                    theme: &self.theme,
                    root: &self.root,
                    now: Instant::now(),
                    focused: false,
                    held_mods: self.held_mods,
                };
                let tabs: Vec<(usize, chrome::Tab, Option<String>)> = order
                    .iter()
                    .filter_map(|&index| {
                        let item = self.pane.item(index)?;
                        let dir = item.path().and_then(|p| p.parent()).map(|d| d.strip_prefix(&self.root).unwrap_or(d).to_string_lossy().into_owned());
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
    }

    fn picker_event(&mut self, event: PickerEvent) {
        match event {
            PickerEvent::Handled => {}
            PickerEvent::QueryChanged => self.refill_picker(),
            PickerEvent::Cancelled => self.modal = None,
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
                let Some(Modal::Picker { kind, picker }) = self.modal.take() else { return };
                let row = picker.selected().and_then(|selected| self.picker_rows.get(selected).copied());
                match (kind, row) {
                    (PickerKind::CommandPalette, Some(index)) => {
                        let action = self.palette[index].action.clone();
                        self.effects.push(Effect::Dispatch(action));
                    }
                    (PickerKind::TabSwitcher, Some(index)) => self.switch_tab(|pane| {
                        pane.activate(index);
                    }),
                    (PickerKind::CommandPalette | PickerKind::TabSwitcher, None) => {}
                    (kind, row) => {
                        let mut cx = cx!(self);
                        self.source.confirm(kind, row, picker.query(), secondary, &mut cx);
                    }
                }
            }
        }
    }

    fn modal_action(&mut self, action: &Action) -> bool {
        let name = action.name.as_str();
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
            None => false,
        }
    }

    fn close_switcher_selection(&mut self) -> bool {
        let Some(Modal::Picker { kind: PickerKind::TabSwitcher, picker }) = &self.modal else { return false };
        let Some(index) = picker.selected().and_then(|selected| self.picker_rows.get(selected).copied()) else { return true };
        if !self.pane.item(index).is_some_and(|item| item.is_dirty()) {
            self.pane.close(index);
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
                        item.go_to(target.line as u32, target.column.map(|c| c as u32));
                    }
                }
                None => self.set_status(format!("Not a line number: {text}"), true),
            },
            InputPurpose::SaveAs => {
                let text = text.trim();
                if text.is_empty() {
                    return;
                }
                let path = crate::cli::parse_path_spec(text, &self.root).path;
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
            Effect::OpenExternally(target) => open_externally(&target),
            Effect::RefreshGit => self.requests.push(Request::RefreshGit),
            Effect::PathsChanged(paths) => self.requests.push(Request::PathsChanged(paths)),
            Effect::CloseItem => {
                if let Some(index) = self.pane.active_index() {
                    self.pane.close(index);
                }
            }
            Effect::Quit => self.request_quit(None),
        }
    }

    /// Tell every open item that files changed on disk.
    pub fn paths_changed(&mut self, paths: &[PathBuf]) {
        let mut cx = cx!(self);
        for item in self.pane.items_mut() {
            item.paths_changed(paths, &mut cx);
        }
        self.apply_effects();
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
                    MouseKind::ScrollDown => picker.scroll_by(1),
                    MouseKind::ScrollUp => picker.scroll_by(-1),
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
            None => {
                // Dragging a divider resizes its dock.
                if let Some(position) = self.dragging {
                    match event.kind {
                        MouseKind::Drag(MouseButton::Left) => {
                            self.drag_divider(position, col, row);
                            return true;
                        }
                        MouseKind::Release(_) => {
                            self.dragging = None;
                            return true;
                        }
                        _ => {}
                    }
                }
                if event.kind == MouseKind::Press(MouseButton::Left) {
                    if let Some(divider) = self.layout.dividers.iter().find(|d| inside(d.rect)) {
                        self.dragging = Some(divider.position);
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
                    .or_else(|| self.layout.center.filter(|rect| inside(*rect)).map(|rect| (Focus::Center, rect)));
                let Some((target, area)) = target else { return false };
                // A click moves the focus to what was clicked; wheel and hover do not.
                if matches!(event.kind, MouseKind::Press(_)) && self.workspace.focus() != target {
                    self.set_focus(target);
                }
                let mut cx = cx!(self);
                match target {
                    Focus::Center => {
                        let below_tabs = Rect::new(area.x, area.y + 1, area.width, area.height.saturating_sub(1));
                        self.pane.active_mut().is_some_and(|item| item.handle_mouse(event, below_tabs, &mut cx))
                    }
                    Focus::Panel(id) => self.panels.get_mut(&id).is_some_and(|view| view.handle_mouse(event, area, &mut cx)),
                }
            }
        };
        self.apply_effects();
        handled
    }

    fn drag_divider(&mut self, position: DockPosition, col: u16, row: u16) {
        let Some(area) = self.layout.dock(position).map(|dock| dock.rect) else { return };
        let (current, wanted, max) = match position {
            DockPosition::Left => (area.width, col.saturating_sub(area.x), self.screen.width.saturating_sub(layout::MIN_CENTER_WIDTH + 2)),
            DockPosition::Right => (area.width, area.right().saturating_sub(col + 1), self.screen.width.saturating_sub(layout::MIN_CENTER_WIDTH + 2)),
            DockPosition::Bottom => (area.height, area.bottom().saturating_sub(row + 1), self.screen.height.saturating_sub(layout::MIN_CENTER_HEIGHT + 2)),
        };
        self.workspace.resize_dock(position, current, wanted as i32 - current as i32, layout::MIN_DOCK_WIDTH, max);
    }

    // ---- drawing -----------------------------------------------------------------------

    /// Draw the whole frame. Returns where the terminal's cursor should be, if anywhere.
    pub fn render(&mut self, buf: &mut Buffer) -> Option<Cursor> {
        let screen = *buf.area();
        self.screen = screen;
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
            let focused = matches!(focus, Focus::Panel(id) if self.workspace.position_of(id) == Some(divider.position));
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
            let cx = RenderCx { settings: &self.settings, theme: &self.theme, root: &self.root, now, focused, held_mods: self.held_mods };
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
            let cx = RenderCx { settings: &self.settings, theme: &self.theme, root: &self.root, now, focused, held_mods: self.held_mods };
            if self.pane.is_empty() {
                draw_empty_center(&mut canvas, center, &self.theme);
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
            message: self.message.as_ref().map(|(text, error, _)| (text.clone(), *error)),
            pending_keys: self.pending_keys.clone(),
            position: item_status.position,
            cursors: item_status.cursors.max(1),
            language: item_status.language,
            indent: item_status.indent,
            line_ending: item_status.line_ending,
            nav_layer: self.nav_layer,
            nerd_font: self.settings.icons == crate::settings::Icons::Nerd,
        };
        chrome::status_bar(&mut canvas, layout.status, &status, &self.theme);
        if let Some(strip) = layout.usage {
            let now_unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
            usage::render(&mut canvas, strip, &self.usage, now_unix, &self.theme);
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
            None => {}
        }
        cursor
    }
}

fn draw_empty_center(canvas: &mut Canvas<'_>, area: Rect, theme: &Theme) {
    canvas.fill(area, theme.editor_bg);
    let lines = [
        ("No file open", true),
        ("", false),
        ("cmd-p         find a file", false),
        ("cmd-shift-f   search the project", false),
        ("cmd-shift-e   file tree", false),
        ("cmd-t         terminal", false),
        ("cmd-shift-p   all commands", false),
    ];
    if area.height < lines.len() as u16 + 2 || area.width < 34 {
        return;
    }
    let top = area.y + (area.height - lines.len() as u16) / 3;
    let left = area.x + (area.width - 32) / 2;
    for (i, (text, heading)) in lines.iter().enumerate() {
        let style = if *heading { Style::colors(theme.text, theme.editor_bg).bold() } else { Style::colors(theme.text_muted, theme.editor_bg) };
        canvas.text(left, top + i as u16, text, style, 32);
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

fn open_externally(target: &str) {
    let program = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let _ = std::process::Command::new(program)
        .arg(target)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn reveal_in_file_manager(path: &Path) {
    if cfg!(target_os = "macos") {
        let _ = std::process::Command::new("open").arg("-R").arg(path).spawn();
    } else if let Some(parent) = path.parent() {
        open_externally(&parent.to_string_lossy());
    }
}
