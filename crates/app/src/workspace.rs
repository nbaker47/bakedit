//! The workspace model: which docks are open, which panel each shows, what is zoomed and
//! where the focus is. Pure state, no drawing.
//!
//! The rules are Zed's (`crates/workspace/src/workspace.rs` and `dock.rs`), because the
//! user's keybindings were written against them:
//!
//! * A dock shows one panel at a time. Opening a panel in a dock replaces the one shown.
//! * `<panel>::Toggle` focuses the panel if it is not focused; if it is, focus returns to
//!   the centre and the panel's dock closes.
//! * `<panel>::ToggleFocus` does the same but leaves the dock open.
//! * A zoomed panel covers the whole workspace. Zoom belongs to the panel and survives the
//!   dock being closed and reopened.
//! * Focusing the centre closes any dock whose panel is zoomed (so the editor is revealed).
//!   Focusing a panel un-zooms the centre and closes other docks whose panel is zoomed.
//! * `ToggleAllDocks` closes every open dock and remembers them, or reopens the remembered
//!   ones when none is open.

use std::fmt;

/// Where a dock sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DockPosition {
    Left,
    Right,
    Bottom,
}

impl DockPosition {
    pub const ALL: [DockPosition; 3] = [DockPosition::Left, DockPosition::Bottom, DockPosition::Right];

    fn index(self) -> usize {
        match self {
            DockPosition::Left => 0,
            DockPosition::Bottom => 1,
            DockPosition::Right => 2,
        }
    }

    /// Parse the value of a Zed `"dock"` setting.
    pub fn from_setting(value: &str) -> Option<DockPosition> {
        match value {
            "left" => Some(DockPosition::Left),
            "right" => Some(DockPosition::Right),
            "bottom" => Some(DockPosition::Bottom),
            _ => None,
        }
    }
}

/// The panels bakedit has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PanelId {
    Terminal,
    Git,
    Project,
}

impl PanelId {
    pub const ALL: [PanelId; 3] = [PanelId::Terminal, PanelId::Git, PanelId::Project];

    pub fn title(self) -> &'static str {
        match self {
            PanelId::Terminal => "Terminal",
            PanelId::Git => "Git",
            PanelId::Project => "Project",
        }
    }
}

impl fmt::Display for PanelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.title())
    }
}

/// What has keyboard focus (modals sit on top of this and are tracked by the app).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Focus {
    Center,
    Panel(PanelId),
}

/// What covers the whole workspace, if anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Zoomed {
    Center,
    Panel(PanelId),
}

#[derive(Debug, Clone, PartialEq)]
struct PanelState {
    id: PanelId,
    /// Size along the dock's axis in cells; `None` until the user resizes it.
    size: Option<u16>,
    zoomed: bool,
}

#[derive(Debug, Clone, PartialEq)]
struct Dock {
    open: bool,
    panels: Vec<PanelState>,
    active: Option<usize>,
}

impl Dock {
    fn active_panel(&self) -> Option<&PanelState> {
        self.panels.get(self.active?)
    }

    fn active_panel_mut(&mut self) -> Option<&mut PanelState> {
        let active = self.active?;
        self.panels.get_mut(active)
    }

    fn index_of(&self, id: PanelId) -> Option<usize> {
        self.panels.iter().position(|p| p.id == id)
    }
}

/// Dock state as saved in a session file.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DockSnapshot {
    pub open: bool,
    pub active: Option<String>,
}

fn default_true() -> bool {
    true
}

/// Workspace state as saved in a session file.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceSnapshot {
    #[serde(default)]
    pub left: Option<DockSnapshot>,
    #[serde(default)]
    pub right: Option<DockSnapshot>,
    #[serde(default)]
    pub bottom: Option<DockSnapshot>,
    /// Panel title -> size in cells.
    #[serde(default)]
    pub sizes: std::collections::BTreeMap<String, u16>,
    /// Titles of zoomed panels.
    #[serde(default)]
    pub zoomed: Vec<String>,
    #[serde(default)]
    pub focus: Option<String>,
    /// When true, the right dock (file tree) sits immediately to the right of the left
    /// dock (git/terminal) instead of at the far right of the window.
    #[serde(default = "default_true")]
    pub right_dock_inner_left: bool,
    /// When true, every panel in the left dock (e.g. terminal/herdr and git) is shown
    /// side-by-side at once alongside the file tree on the left.
    #[serde(default)]
    pub show_all_left_panels: bool,
}

impl Default for WorkspaceSnapshot {
    fn default() -> Self {
        Self {
            left: None,
            right: None,
            bottom: None,
            sizes: std::collections::BTreeMap::new(),
            zoomed: Vec::new(),
            focus: None,
            right_dock_inner_left: true,
            show_all_left_panels: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Workspace {
    docks: [Dock; 3],
    focus: Focus,
    center_zoomed: bool,
    last_open: Vec<DockPosition>,
    right_dock_inner_left: bool,
    show_all_left_panels: bool,
}

impl Workspace {
    /// A workspace with each panel placed in the given dock. Panels sharing a dock are
    /// listed in the order given; the first is the one the dock shows initially. Everything
    /// starts closed with the focus in the centre.
    pub fn new(placement: &[(PanelId, DockPosition)]) -> Workspace {
        let empty = || Dock { open: false, panels: Vec::new(), active: None };
        let mut docks = [empty(), empty(), empty()];
        for &(id, position) in placement {
            let dock = &mut docks[position.index()];
            if dock.index_of(id).is_none() {
                dock.panels.push(PanelState { id, size: None, zoomed: false });
            }
        }
        for dock in &mut docks {
            if !dock.panels.is_empty() {
                dock.active = Some(0);
            }
        }
        Workspace {
            docks,
            focus: Focus::Center,
            center_zoomed: false,
            last_open: Vec::new(),
            right_dock_inner_left: true,
            show_all_left_panels: false,
        }
    }

    /// True when the right dock (file tree) is placed immediately to the right of the left
    /// dock (git/terminal) instead of the far right edge.
    pub fn is_right_dock_inner_left(&self) -> bool {
        self.right_dock_inner_left
    }

    /// Swap the right dock (file tree) between far right and immediately right of the left
    /// dock (git/terminal). Returns the new `inner_left` state.
    pub fn toggle_right_dock_inner_left(&mut self) -> bool {
        self.right_dock_inner_left = !self.right_dock_inner_left;
        self.right_dock_inner_left
    }

    /// True when all left-dock panels (herdr/terminal, git) are shown side-by-side at once.
    pub fn is_showing_all_left_panels(&self) -> bool {
        self.show_all_left_panels
    }

    /// Toggle showing all left-side panes simultaneously (herdr/terminal, git, and file tree
    /// on the left). Returns the new state.
    pub fn toggle_show_all_left_panels(&mut self) -> bool {
        self.show_all_left_panels = !self.show_all_left_panels;
        if self.show_all_left_panels {
            self.center_zoomed = false;
            for dock in &mut self.docks {
                for panel in &mut dock.panels {
                    panel.zoomed = false;
                }
            }
            if self.dock(DockPosition::Left).active.is_some() {
                self.dock_mut(DockPosition::Left).open = true;
            }
            self.right_dock_inner_left = true;
            if self.dock(DockPosition::Right).active.is_some() {
                self.dock_mut(DockPosition::Right).open = true;
            }
        }
        self.show_all_left_panels
    }

    fn dock(&self, position: DockPosition) -> &Dock {
        &self.docks[position.index()]
    }

    fn dock_mut(&mut self, position: DockPosition) -> &mut Dock {
        &mut self.docks[position.index()]
    }

    /// The dock a panel lives in.
    pub fn position_of(&self, id: PanelId) -> Option<DockPosition> {
        DockPosition::ALL.into_iter().find(|&p| self.dock(p).index_of(id).is_some())
    }

    pub fn focus(&self) -> Focus {
        self.focus
    }

    pub fn is_dock_open(&self, position: DockPosition) -> bool {
        self.dock(position).open
    }

    /// The panel an open dock is showing.
    pub fn visible_panel(&self, position: DockPosition) -> Option<PanelId> {
        let dock = self.dock(position);
        if dock.open {
            dock.active_panel().map(|p| p.id)
        } else {
            None
        }
    }

    /// The panel a dock would show if it were open.
    pub fn active_panel(&self, position: DockPosition) -> Option<PanelId> {
        self.dock(position).active_panel().map(|p| p.id)
    }

    /// Every panel in a dock, in order.
    pub fn panels_in(&self, position: DockPosition) -> Vec<PanelId> {
        self.dock(position).panels.iter().map(|p| p.id).collect()
    }

    /// True when the panel is currently shown in its open dock.
    pub fn is_panel_visible(&self, id: PanelId) -> bool {
        self.position_of(id).is_some_and(|p| {
            if self.show_all_left_panels && p == DockPosition::Left && self.is_dock_open(DockPosition::Left) {
                true
            } else {
                self.visible_panel(p) == Some(id)
            }
        })
    }

    pub fn is_panel_zoomed(&self, id: PanelId) -> bool {
        self.panel(id).is_some_and(|p| p.zoomed)
    }

    pub fn is_center_zoomed(&self) -> bool {
        self.center_zoomed
    }

    fn panel(&self, id: PanelId) -> Option<&PanelState> {
        self.docks.iter().flat_map(|d| d.panels.iter()).find(|p| p.id == id)
    }

    fn panel_mut(&mut self, id: PanelId) -> Option<&mut PanelState> {
        self.docks.iter_mut().flat_map(|d| d.panels.iter_mut()).find(|p| p.id == id)
    }

    /// What currently covers the whole workspace: an open dock whose visible panel is
    /// zoomed, else the centre if it is zoomed.
    pub fn zoomed(&self) -> Option<Zoomed> {
        for position in DockPosition::ALL {
            let dock = self.dock(position);
            if dock.open {
                if let Some(panel) = dock.active_panel() {
                    if panel.zoomed {
                        return Some(Zoomed::Panel(panel.id));
                    }
                }
            }
        }
        if self.center_zoomed {
            Some(Zoomed::Center)
        } else {
            None
        }
    }

    /// The size the user gave a panel, if they resized it.
    pub fn panel_size(&self, id: PanelId) -> Option<u16> {
        self.panel(id).and_then(|p| p.size)
    }

    pub fn set_panel_size(&mut self, id: PanelId, size: Option<u16>) {
        if let Some(panel) = self.panel_mut(id) {
            panel.size = size;
        }
    }

    // ---- focus -------------------------------------------------------------------------

    /// Show a panel and give it the focus.
    pub fn focus_panel(&mut self, id: PanelId) {
        let Some(position) = self.position_of(id) else { return };
        self.activate(id);
        self.dock_mut(position).open = true;
        self.focus = Focus::Panel(id);
        self.reveal_dock(position);
    }

    /// Give the focus to the centre. Docks whose panel is zoomed close so the centre shows.
    pub fn focus_center(&mut self) {
        self.focus = Focus::Center;
        for position in DockPosition::ALL {
            let dock = self.dock_mut(position);
            if dock.open && dock.active_panel().is_some_and(|p| p.zoomed) {
                dock.open = false;
            }
        }
    }

    /// Make `id` the panel its dock shows, without opening the dock or moving focus.
    fn activate(&mut self, id: PanelId) {
        let Some(position) = self.position_of(id) else { return };
        let dock = self.dock_mut(position);
        dock.active = dock.index_of(id);
    }

    /// A dock is about to be the thing in focus: un-zoom the centre and close other docks
    /// whose panel is zoomed (they would cover it).
    fn reveal_dock(&mut self, revealed: DockPosition) {
        self.center_zoomed = false;
        for position in DockPosition::ALL {
            if position == revealed {
                continue;
            }
            let dock = self.dock_mut(position);
            if dock.open && dock.active_panel().is_some_and(|p| p.zoomed) {
                dock.open = false;
            }
        }
    }

    /// Open a panel in its dock without moving the focus (Zed's `open_panel`).
    pub fn open_panel(&mut self, id: PanelId) {
        let Some(position) = self.position_of(id) else { return };
        self.activate(id);
        self.dock_mut(position).open = true;
    }

    /// Close `id`. When all left-dock panels are shown side-by-side, closing one left
    /// panel hides only that panel and keeps the other left panel open. Otherwise closes
    /// the dock `id` lives in (Zed's `close_panel`). If focus was on the closed panel it
    /// returns to the centre.
    pub fn close_panel(&mut self, id: PanelId) {
        let Some(position) = self.position_of(id) else { return };
        if self.show_all_left_panels && position == DockPosition::Left && self.is_dock_open(DockPosition::Left) {
            if let Some(other) = self.dock(DockPosition::Left).panels.iter().find(|p| p.id != id).map(|p| p.id) {
                self.show_all_left_panels = false;
                self.activate(other);
                if self.focus == Focus::Panel(id) {
                    self.focus_center();
                }
                return;
            }
        }
        self.close_dock(position);
    }

    fn close_dock(&mut self, position: DockPosition) {
        let focus_was_here = self.focus_position() == Some(position);
        self.dock_mut(position).open = false;
        if focus_was_here {
            self.focus_center();
        }
    }

    fn focus_position(&self) -> Option<DockPosition> {
        match self.focus {
            Focus::Panel(id) => self.position_of(id),
            Focus::Center => None,
        }
    }

    /// `<panel>::ToggleFocus`: focus the panel, or if it already has the focus, the centre.
    /// Returns true if the panel ended up focused.
    pub fn toggle_panel_focus(&mut self, id: PanelId) -> bool {
        if self.focus == Focus::Panel(id) {
            self.activate(id);
            self.focus_center();
            false
        } else {
            self.focus_panel(id);
            true
        }
    }

    /// `<panel>::Toggle`: focus the panel, or if it already has the focus, return to the
    /// centre and close the panel's dock.
    pub fn toggle_panel(&mut self, id: PanelId) {
        if !self.toggle_panel_focus(id) {
            self.close_panel(id);
        }
    }

    // ---- docks -------------------------------------------------------------------------

    fn open_positions(&self) -> Vec<DockPosition> {
        DockPosition::ALL.into_iter().filter(|&p| self.dock(p).open).collect()
    }

    fn remember_open_docks(&mut self) {
        let open = self.open_positions();
        if !open.is_empty() {
            self.last_open = open;
        }
    }

    /// `workspace::ToggleLeftDock` and friends.
    pub fn toggle_dock(&mut self, position: DockPosition) {
        if self.dock(position).panels.is_empty() {
            return;
        }
        let other_is_zoomed = match self.zoomed() {
            Some(Zoomed::Panel(id)) => self.position_of(id) != Some(position),
            Some(Zoomed::Center) => true,
            None => false,
        };
        let was_visible = self.dock(position).open && !other_is_zoomed;
        if was_visible {
            self.remember_open_docks();
            self.close_dock(position);
        } else {
            let dock = self.dock_mut(position);
            if dock.active.is_none() {
                dock.active = Some(0);
            }
            dock.open = true;
            if let Some(id) = self.dock(position).active_panel().map(|p| p.id) {
                self.focus = Focus::Panel(id);
            }
            self.reveal_dock(position);
        }
    }

    /// `workspace::CloseActiveDock`: close the dock that has the focus. False if the focus
    /// is not in a dock.
    pub fn close_active_dock(&mut self) -> bool {
        match self.focus_position() {
            Some(position) if self.dock(position).open => {
                self.remember_open_docks();
                self.close_dock(position);
                true
            }
            _ => false,
        }
    }

    /// Close every dock, remembering which were open, and focus the centre.
    pub fn close_all_docks(&mut self) {
        self.remember_open_docks();
        for position in DockPosition::ALL {
            self.dock_mut(position).open = false;
        }
        self.focus = Focus::Center;
    }

    /// `workspace::ToggleAllDocks`: close all open docks, or reopen the remembered ones when
    /// none is open. Focus goes to the centre, unless a reopened dock shows a zoomed panel:
    /// that panel covers the centre, so it takes the focus.
    pub fn toggle_all_docks(&mut self) {
        if !self.open_positions().is_empty() {
            self.close_all_docks();
            return;
        }
        let positions = std::mem::take(&mut self.last_open);
        if positions.is_empty() {
            return;
        }
        for &position in &positions {
            let dock = self.dock_mut(position);
            if dock.active.is_some() {
                dock.open = true;
            }
        }
        self.focus = match self.zoomed() {
            Some(Zoomed::Panel(id)) => Focus::Panel(id),
            _ => Focus::Center,
        };
    }

    // ---- zoom --------------------------------------------------------------------------

    /// `workspace::ToggleZoom`: zoom or un-zoom whatever has the focus.
    pub fn toggle_zoom(&mut self) {
        match self.focus {
            Focus::Center => {
                self.center_zoomed = !self.center_zoomed;
            }
            Focus::Panel(id) => {
                let zoomed = !self.is_panel_zoomed(id);
                self.set_panel_zoomed(id, zoomed);
            }
        }
    }

    /// Zoom or un-zoom a panel. Zooming one panel un-zooms the others in its dock.
    pub fn set_panel_zoomed(&mut self, id: PanelId, zoomed: bool) {
        let Some(position) = self.position_of(id) else { return };
        for panel in &mut self.dock_mut(position).panels {
            if panel.id == id {
                panel.zoomed = zoomed;
            } else if zoomed {
                panel.zoomed = false;
            }
        }
    }

    /// bakedit's `terminal_panel::ToggleZoom` (and the same for any panel): if the panel has
    /// the focus toggle its zoom, otherwise bring it up zoomed.
    pub fn toggle_panel_zoom(&mut self, id: PanelId) {
        if self.focus == Focus::Panel(id) {
            let zoomed = !self.is_panel_zoomed(id);
            self.set_panel_zoomed(id, zoomed);
        } else {
            self.set_panel_zoomed(id, true);
            self.focus_panel(id);
        }
    }

    // ---- sizes -------------------------------------------------------------------------

    /// The dock whose size the resize actions change: the focused one, else the first open.
    pub fn active_dock(&self) -> Option<DockPosition> {
        self.focus_position()
            .filter(|&p| self.dock(p).open)
            .or_else(|| self.open_positions().into_iter().next())
    }

    /// Change the size of `id` by `delta` cells, starting from `current` and clamped to `min..=max`.
    pub fn resize_panel(&mut self, id: PanelId, current: u16, delta: i32, min: u16, max: u16) {
        if let Some(panel) = self.panel_mut(id) {
            let wanted = (current as i32 + delta).clamp(min as i32, max.max(min) as i32);
            panel.size = Some(wanted as u16);
        }
    }

    /// Forget the user's size for `id`.
    pub fn reset_panel_size(&mut self, id: PanelId) {
        if let Some(panel) = self.panel_mut(id) {
            panel.size = None;
        }
    }

    /// Change the size of the panel shown in `position` by `delta` cells, starting from
    /// `current` (the size it is drawn at now) and clamped to `min..=max`.
    pub fn resize_dock(&mut self, position: DockPosition, current: u16, delta: i32, min: u16, max: u16) {
        if let Some(panel) = self.dock_mut(position).active_panel_mut() {
            let wanted = (current as i32 + delta).clamp(min as i32, max.max(min) as i32);
            panel.size = Some(wanted as u16);
        }
    }

    /// Forget the user's size for the panel shown in `position`.
    pub fn reset_dock_size(&mut self, position: DockPosition) {
        if let Some(panel) = self.dock_mut(position).active_panel_mut() {
            panel.size = None;
        }
    }

    // ---- persistence -------------------------------------------------------------------

    pub fn snapshot(&self) -> WorkspaceSnapshot {
        let dock = |position: DockPosition| {
            let dock = self.dock(position);
            if dock.panels.is_empty() {
                return None;
            }
            Some(DockSnapshot {
                open: dock.open,
                active: dock.active_panel().map(|p| p.id.title().to_string()),
            })
        };
        let panels = || self.docks.iter().flat_map(|d| d.panels.iter());
        WorkspaceSnapshot {
            left: dock(DockPosition::Left),
            right: dock(DockPosition::Right),
            bottom: dock(DockPosition::Bottom),
            sizes: panels().filter_map(|p| Some((p.id.title().to_string(), p.size?))).collect(),
            zoomed: panels().filter(|p| p.zoomed).map(|p| p.id.title().to_string()).collect(),
            focus: match self.focus {
                Focus::Center => None,
                Focus::Panel(id) => Some(id.title().to_string()),
            },
            right_dock_inner_left: self.right_dock_inner_left,
            show_all_left_panels: self.show_all_left_panels,
        }
    }

    /// Apply a saved snapshot. Unknown names are ignored, and the result is always a
    /// consistent state (the focus never lands on a hidden panel).
    pub fn restore(&mut self, snapshot: &WorkspaceSnapshot) {
        self.right_dock_inner_left = snapshot.right_dock_inner_left;
        self.show_all_left_panels = snapshot.show_all_left_panels;
        let by_title = |title: &str| PanelId::ALL.into_iter().find(|id| id.title() == title);
        for (title, &size) in &snapshot.sizes {
            if let Some(id) = by_title(title) {
                self.set_panel_size(id, Some(size));
            }
        }
        for id in PanelId::ALL {
            let zoomed = snapshot.zoomed.iter().any(|t| t == id.title());
            if let Some(panel) = self.panel_mut(id) {
                panel.zoomed = zoomed;
            }
        }
        for (position, saved) in [
            (DockPosition::Left, &snapshot.left),
            (DockPosition::Right, &snapshot.right),
            (DockPosition::Bottom, &snapshot.bottom),
        ] {
            let Some(saved) = saved else { continue };
            if let Some(id) = saved.active.as_deref().and_then(by_title) {
                if self.position_of(id) == Some(position) {
                    self.activate(id);
                }
            }
            let dock = self.dock_mut(position);
            dock.open = saved.open && dock.active.is_some();
        }
        self.focus = Focus::Center;
        match snapshot.focus.as_deref().and_then(by_title) {
            Some(id) if self.is_panel_visible(id) => self.focus = Focus::Panel(id),
            _ => {
                // The centre cannot hold the focus underneath a zoomed panel.
                if let Some(Zoomed::Panel(id)) = self.zoomed() {
                    self.focus = Focus::Panel(id);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use DockPosition::{Left, Right};
    use PanelId::{Git, Project, Terminal};

    /// The user's layout: terminal and git on the left, the file tree on the right.
    fn workspace() -> Workspace {
        Workspace::new(&[(Terminal, Left), (Git, Left), (Project, Right)])
    }

    #[test]
    fn starts_closed_with_focus_in_the_centre() {
        let ws = workspace();
        assert_eq!(ws.focus(), Focus::Center);
        assert_eq!(ws.visible_panel(Left), None);
        assert_eq!(ws.active_panel(Left), Some(Terminal));
        assert_eq!(ws.position_of(Git), Some(Left));
        assert_eq!(ws.position_of(Project), Some(Right));
        assert_eq!(ws.zoomed(), None);
    }

    #[test]
    fn toggle_shows_and_focuses_then_hides() {
        let mut ws = workspace();
        ws.toggle_panel(Terminal);
        assert_eq!(ws.focus(), Focus::Panel(Terminal));
        assert_eq!(ws.visible_panel(Left), Some(Terminal));
        ws.toggle_panel(Terminal);
        assert_eq!(ws.focus(), Focus::Center);
        assert_eq!(ws.visible_panel(Left), None);
    }

    #[test]
    fn toggle_from_elsewhere_focuses_without_closing() {
        let mut ws = workspace();
        ws.toggle_panel(Terminal);
        ws.focus_center();
        assert_eq!(ws.visible_panel(Left), Some(Terminal), "an un-zoomed dock stays open");
        ws.toggle_panel(Terminal);
        assert_eq!(ws.focus(), Focus::Panel(Terminal));
        assert_eq!(ws.visible_panel(Left), Some(Terminal));
    }

    #[test]
    fn panels_in_one_dock_replace_each_other() {
        let mut ws = workspace();
        ws.toggle_panel(Terminal);
        ws.toggle_panel(Git);
        assert_eq!(ws.visible_panel(Left), Some(Git));
        assert_eq!(ws.focus(), Focus::Panel(Git));
        assert!(!ws.is_panel_visible(Terminal));
        ws.toggle_panel(Terminal);
        assert_eq!(ws.visible_panel(Left), Some(Terminal));
        assert_eq!(ws.focus(), Focus::Panel(Terminal));
    }

    #[test]
    fn toggle_focus_keeps_the_dock_open() {
        let mut ws = workspace();
        assert!(ws.toggle_panel_focus(Project));
        assert_eq!(ws.focus(), Focus::Panel(Project));
        assert!(!ws.toggle_panel_focus(Project));
        assert_eq!(ws.focus(), Focus::Center);
        assert_eq!(ws.visible_panel(Right), Some(Project));
    }

    #[test]
    fn zoomed_terminal_covers_everything_and_survives_closing() {
        let mut ws = workspace();
        ws.toggle_panel(Terminal);
        ws.toggle_zoom();
        assert_eq!(ws.zoomed(), Some(Zoomed::Panel(Terminal)));
        // cmd-e: focusing the editor closes the zoomed dock to reveal it.
        ws.focus_center();
        assert_eq!(ws.zoomed(), None);
        assert_eq!(ws.visible_panel(Left), None);
        // cmd-t: the terminal comes back still zoomed.
        ws.toggle_panel(Terminal);
        assert_eq!(ws.zoomed(), Some(Zoomed::Panel(Terminal)));
        assert_eq!(ws.focus(), Focus::Panel(Terminal));
        // cmd-alt-t again un-zooms it in place.
        ws.toggle_zoom();
        assert_eq!(ws.zoomed(), None);
        assert_eq!(ws.visible_panel(Left), Some(Terminal));
    }

    #[test]
    fn focusing_another_dock_closes_a_zoomed_one() {
        let mut ws = workspace();
        ws.toggle_panel(Terminal);
        ws.toggle_zoom();
        ws.focus_panel(Project);
        assert_eq!(ws.focus(), Focus::Panel(Project));
        assert_eq!(ws.visible_panel(Left), None, "the zoomed terminal would cover the tree");
        assert_eq!(ws.zoomed(), None);
        assert!(ws.is_panel_zoomed(Terminal), "but it remembers that it is zoomed");
    }

    #[test]
    fn switching_panel_within_a_zoomed_dock_shows_the_other_panel_unzoomed() {
        let mut ws = workspace();
        ws.toggle_panel(Terminal);
        ws.toggle_zoom();
        ws.toggle_panel(Git);
        assert_eq!(ws.visible_panel(Left), Some(Git));
        assert_eq!(ws.zoomed(), None);
        ws.toggle_panel(Terminal);
        assert_eq!(ws.zoomed(), Some(Zoomed::Panel(Terminal)));
    }

    #[test]
    fn centre_zoom_is_dropped_when_a_panel_takes_focus() {
        let mut ws = workspace();
        ws.toggle_panel(Project);
        ws.focus_center();
        ws.toggle_zoom();
        assert_eq!(ws.zoomed(), Some(Zoomed::Center));
        ws.focus_panel(Project);
        assert_eq!(ws.zoomed(), None);
        assert!(!ws.is_center_zoomed());
    }

    #[test]
    fn toggle_all_docks_closes_then_restores() {
        let mut ws = workspace();
        ws.toggle_panel(Terminal);
        ws.toggle_panel(Project);
        ws.focus_center();
        ws.toggle_all_docks();
        assert_eq!(ws.visible_panel(Left), None);
        assert_eq!(ws.visible_panel(Right), None);
        assert_eq!(ws.focus(), Focus::Center);
        ws.toggle_all_docks();
        assert_eq!(ws.visible_panel(Left), Some(Terminal));
        assert_eq!(ws.visible_panel(Right), Some(Project));
        assert_eq!(ws.focus(), Focus::Center);
    }

    #[test]
    fn toggle_all_docks_with_nothing_remembered_does_nothing() {
        let mut ws = workspace();
        ws.toggle_all_docks();
        assert_eq!(ws.visible_panel(Left), None);
        assert_eq!(ws.visible_panel(Right), None);
    }

    #[test]
    fn toggle_all_docks_from_a_panel_moves_focus_to_the_centre() {
        let mut ws = workspace();
        ws.toggle_panel(Project);
        ws.toggle_all_docks();
        assert_eq!(ws.focus(), Focus::Center);
        assert_eq!(ws.visible_panel(Right), None);
    }

    #[test]
    fn restoring_a_zoomed_dock_focuses_it() {
        let mut ws = workspace();
        ws.toggle_panel(Terminal);
        ws.toggle_zoom();
        ws.toggle_all_docks();
        assert_eq!(ws.focus(), Focus::Center);
        ws.toggle_all_docks();
        assert_eq!(ws.zoomed(), Some(Zoomed::Panel(Terminal)));
        assert_eq!(ws.focus(), Focus::Panel(Terminal));
    }

    #[test]
    fn close_active_dock_only_acts_from_inside_a_dock() {
        let mut ws = workspace();
        ws.toggle_panel(Project);
        ws.focus_center();
        assert!(!ws.close_active_dock());
        assert_eq!(ws.visible_panel(Right), Some(Project));
        ws.focus_panel(Project);
        assert!(ws.close_active_dock());
        assert_eq!(ws.visible_panel(Right), None);
        assert_eq!(ws.focus(), Focus::Center);
        // and ToggleAllDocks can bring it back
        ws.toggle_all_docks();
        assert_eq!(ws.visible_panel(Right), Some(Project));
    }

    #[test]
    fn toggle_dock_opens_focused_and_closes_back_to_the_centre() {
        let mut ws = workspace();
        ws.toggle_dock(Left);
        assert_eq!(ws.visible_panel(Left), Some(Terminal));
        assert_eq!(ws.focus(), Focus::Panel(Terminal));
        ws.toggle_dock(Left);
        assert_eq!(ws.visible_panel(Left), None);
        assert_eq!(ws.focus(), Focus::Center);
        // An empty dock ignores the request.
        ws.toggle_dock(DockPosition::Bottom);
        assert_eq!(ws.visible_panel(DockPosition::Bottom), None);
    }

    #[test]
    fn toggle_dock_under_a_zoomed_panel_reveals_it_instead_of_closing() {
        let mut ws = workspace();
        ws.open_panel(Project);
        ws.toggle_panel(Terminal);
        ws.toggle_zoom();
        // The right dock is open but hidden behind the zoomed terminal: toggling reveals it.
        ws.toggle_dock(Right);
        assert_eq!(ws.visible_panel(Right), Some(Project));
        assert_eq!(ws.focus(), Focus::Panel(Project));
        assert_eq!(ws.zoomed(), None);
    }

    #[test]
    fn panel_zoom_shortcut_brings_the_terminal_up_zoomed_from_anywhere() {
        let mut ws = workspace();
        ws.toggle_panel_zoom(Terminal);
        assert_eq!(ws.focus(), Focus::Panel(Terminal));
        assert_eq!(ws.zoomed(), Some(Zoomed::Panel(Terminal)));
        ws.toggle_panel_zoom(Terminal);
        assert_eq!(ws.zoomed(), None);
        assert_eq!(ws.visible_panel(Left), Some(Terminal));
        ws.focus_center();
        ws.toggle_panel_zoom(Terminal);
        assert_eq!(ws.zoomed(), Some(Zoomed::Panel(Terminal)));
    }

    #[test]
    fn zooming_one_panel_unzooms_its_dock_mates() {
        let mut ws = workspace();
        ws.set_panel_zoomed(Terminal, true);
        ws.set_panel_zoomed(Git, true);
        assert!(ws.is_panel_zoomed(Git));
        assert!(!ws.is_panel_zoomed(Terminal));
    }

    #[test]
    fn sizes_are_per_panel_and_clamped() {
        let mut ws = workspace();
        ws.toggle_panel(Terminal);
        assert_eq!(ws.active_dock(), Some(Left));
        ws.resize_dock(Left, 80, 10, 20, 120);
        assert_eq!(ws.panel_size(Terminal), Some(90));
        ws.resize_dock(Left, 90, 500, 20, 120);
        assert_eq!(ws.panel_size(Terminal), Some(120));
        ws.resize_dock(Left, 120, -500, 20, 120);
        assert_eq!(ws.panel_size(Terminal), Some(20));
        assert_eq!(ws.panel_size(Git), None);
        ws.reset_dock_size(Left);
        assert_eq!(ws.panel_size(Terminal), None);
    }

    #[test]
    fn snapshot_round_trips() {
        let mut ws = workspace();
        ws.toggle_panel(Project);
        ws.toggle_panel(Git);
        ws.set_panel_zoomed(Terminal, true);
        ws.set_panel_size(Project, Some(31));
        let snapshot = ws.snapshot();
        let json = serde_json::to_string(&snapshot).unwrap();
        let parsed: WorkspaceSnapshot = serde_json::from_str(&json).unwrap();
        let mut restored = workspace();
        restored.restore(&parsed);
        assert_eq!(restored, ws);
    }

    #[test]
    fn restore_never_leaves_focus_on_a_hidden_panel() {
        let mut ws = workspace();
        let snapshot = WorkspaceSnapshot {
            left: Some(DockSnapshot { open: false, active: Some("Git".into()) }),
            focus: Some("Git".into()),
            ..Default::default()
        };
        ws.restore(&snapshot);
        assert_eq!(ws.focus(), Focus::Center);
        assert_eq!(ws.active_panel(Left), Some(Git));

        let snapshot = WorkspaceSnapshot {
            left: Some(DockSnapshot { open: true, active: Some("Terminal".into()) }),
            zoomed: vec!["Terminal".into()],
            focus: None,
            ..Default::default()
        };
        ws.restore(&snapshot);
        assert_eq!(ws.focus(), Focus::Panel(Terminal), "a zoomed panel covers the centre");

        // Garbage in the file is ignored.
        let snapshot = WorkspaceSnapshot {
            left: Some(DockSnapshot { open: true, active: Some("Nope".into()) }),
            focus: Some("Nope".into()),
            zoomed: vec!["Nope".into()],
            ..Default::default()
        };
        let mut ws = workspace();
        ws.restore(&snapshot);
        assert_eq!(ws.visible_panel(Left), Some(Terminal));
        assert_eq!(ws.focus(), Focus::Center);
    }
}
