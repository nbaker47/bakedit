//! The contract between the app shell and the things it hosts: the panels in the docks and
//! the items in the centre's tabs.
//!
//! A view draws itself, answers actions and input, and says what key context it is in. It
//! never reaches into another view. When it needs something outside itself (open a file,
//! move the focus, ask a yes/no question) it pushes an [`Effect`] and the shell does it
//! once the view has returned. That keeps every view testable on its own and means two
//! views can never be borrowed at once.

use std::any::Any;
use std::path::{Path, PathBuf};
use std::time::Instant;

use bakedit_core::{Action, KeyEvent, Mods, MouseEvent, Theme};
use ratatui::layout::Rect;

use crate::chrome::Tab;
use crate::clipboard::Clipboard;
use crate::picker::PickerItem;
use crate::project::Project;
use crate::settings::Settings;
use crate::ui::Canvas;
use crate::workspace::{Focus, PanelId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Bar,
    Underline,
}

/// Where the terminal's text cursor goes this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub col: u16,
    pub row: u16,
    pub shape: CursorShape,
}

/// One level of the key context stack, as a view describes it. The shell turns these into
/// the keymap's own type; views do not depend on the keymap crate.
///
/// `Context::new("Editor").value("mode", "full").flag("multibuffer")` matches the keymap
/// predicates `Editor`, `mode == full` and `multibuffer`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    pub name: &'static str,
    pub flags: Vec<&'static str>,
    pub values: Vec<(&'static str, String)>,
}

impl Context {
    pub fn new(name: &'static str) -> Context {
        Context { name, flags: Vec::new(), values: Vec::new() }
    }

    pub fn flag(mut self, flag: &'static str) -> Context {
        self.flags.push(flag);
        self
    }

    pub fn flag_if(self, condition: bool, flag: &'static str) -> Context {
        if condition {
            self.flag(flag)
        } else {
            self
        }
    }

    pub fn value(mut self, key: &'static str, value: impl Into<String>) -> Context {
        self.values.push((key, value.into()));
        self
    }
}

/// A question for the user with one action per answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Question {
    pub message: String,
    pub detail: Option<String>,
    /// (button label, what to do when it is chosen). The last button is the cancel button:
    /// it is preselected and escape chooses it; give it no actions to make it a plain cancel.
    pub answers: Vec<(String, Vec<Effect>)>,
}

/// Something a view asks the shell to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Open a file in the centre. `line` and `column` are 1-based. A `preview` tab is
    /// replaced by the next preview; `focus` moves the keyboard to the editor.
    OpenFile { path: PathBuf, line: Option<u32>, column: Option<u32>, preview: bool, focus: bool },
    /// Move the keyboard focus.
    Focus(Focus),
    /// Show a panel without moving the focus.
    ShowPanel(PanelId),
    /// Run an action as if a key bound to it had been pressed.
    Dispatch(Action),
    /// Select and scroll to a path in the project panel (opening the panel if needed).
    RevealInProjectPanel(PathBuf),
    /// Show the project diff, scrolled to a file if one is given.
    ShowProjectDiff { path: Option<PathBuf>, focus: bool },
    /// A line in the status bar. Errors stay until the next message; others fade.
    Status { text: String, error: bool },
    /// Ask a question.
    Ask(Question),
    /// Hand a URL or file to the operating system (`open` on macOS).
    OpenExternally(String),
    /// Git state changed outside the normal file-watching path: refresh it now.
    RefreshGit,
    /// The file at this path was created, renamed or removed by a view: refresh the tree
    /// and any open tab for it.
    PathsChanged(Vec<PathBuf>),
    /// A view moved or renamed a file or folder from `from` to `to`: the tabs showing it
    /// (or a file inside it) follow it there. Sent before the `PathsChanged` for the same
    /// move.
    Moved { from: PathBuf, to: PathBuf },
    /// Close the tab this view lives in (sent by an item about itself).
    CloseItem,
    /// Leave this project and open another directory in this window. Unsaved files are
    /// asked about first, as when quitting.
    OpenProject(PathBuf),
    /// Switch the active git repository in a multi-repo workspace (without changing the
    /// open project root).
    SelectRepo(PathBuf),
    /// Switch the git diff base between uncommitted changes (`None`) and a branch/ref
    /// (`Some(ref_name)`).
    SetDiffBase(Option<String>),
    Quit,
}

impl Effect {
    pub fn status(text: impl Into<String>) -> Effect {
        Effect::Status { text: text.into(), error: false }
    }

    pub fn error(text: impl Into<String>) -> Effect {
        Effect::Status { text: text.into(), error: true }
    }

    pub fn open(path: impl Into<PathBuf>) -> Effect {
        Effect::OpenFile { path: path.into(), line: None, column: None, preview: false, focus: true }
    }
}

/// Files that changed on disk: what the file watcher saw, or what a view did itself and
/// reported with [`Effect::PathsChanged`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FsChange {
    /// Absolute paths that were created, written, renamed or removed.
    pub paths: Vec<PathBuf>,
    /// So much changed that the paths were not listed: treat everything as changed.
    pub rescan: bool,
}

impl FsChange {
    /// A change of exactly these paths.
    pub fn of(paths: Vec<PathBuf>) -> FsChange {
        FsChange { paths, rescan: false }
    }

    /// Whether `path` may have changed: it is listed, something above it is listed (its
    /// folder was renamed or removed), or everything is to be re-read.
    pub fn touches(&self, path: &Path) -> bool {
        self.rescan || self.paths.iter().any(|changed| path.starts_with(changed))
    }

    pub fn is_empty(&self) -> bool {
        !self.rescan && self.paths.is_empty()
    }
}

/// What a view may read and use while it handles input.
pub struct Cx<'a> {
    pub settings: &'a Settings,
    pub theme: &'a Theme,
    pub clipboard: &'a mut Clipboard,
    pub effects: &'a mut Vec<Effect>,
    /// What every view shares: the git repository and its status, the file index, recent
    /// files, the waker for background work. See `project.rs`.
    pub project: &'a Project,
    /// The project directory (`project.root()`).
    pub root: &'a Path,
    /// The file in the front tab of the centre, if that tab shows a file.
    pub active_path: Option<&'a Path>,
    pub now: Instant,
    /// Modifier keys physically held right now, including the command key, which mouse
    /// events cannot report.
    pub held_mods: Mods,
}

impl Cx<'_> {
    pub fn push(&mut self, effect: Effect) {
        self.effects.push(effect);
    }

    pub fn status(&mut self, text: impl Into<String>) {
        self.effects.push(Effect::status(text));
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.effects.push(Effect::error(text));
    }
}

/// What a view may read while it draws.
pub struct RenderCx<'a> {
    pub settings: &'a Settings,
    pub theme: &'a Theme,
    pub project: &'a Project,
    pub root: &'a Path,
    /// The file in the front tab of the centre, if that tab shows a file.
    pub active_path: Option<&'a Path>,
    pub now: Instant,
    /// This view has the keyboard focus.
    pub focused: bool,
    /// This view's panel (or the centre) is zoomed: it covers the whole workspace.
    pub zoomed: bool,
    pub held_mods: Mods,
}

/// Anything drawn in a dock or a tab.
pub trait View {
    /// Draw into `area`. Return where the text cursor belongs if this view wants one shown
    /// (the shell only shows it when the view has the focus).
    fn render(&mut self, canvas: &mut Canvas<'_>, area: Rect, cx: &RenderCx<'_>) -> Option<Cursor>;

    /// The key contexts this view adds below its container, outermost first.
    fn contexts(&self, out: &mut Vec<Context>);

    /// Offer an action. Return false to let the next candidate or the shell have it.
    fn handle_action(&mut self, action: &Action, cx: &mut Cx<'_>) -> bool;

    /// A key event no binding claimed. Mostly that is a press that types text
    /// (`key.text`), but the releases of such presses and modifier keys pressed on their
    /// own arrive here too, so that a terminal can pass them to a program that asked for
    /// them: check [`KeyEvent::is_down`] before acting. Return false if unused.
    fn handle_key(&mut self, key: &KeyEvent, cx: &mut Cx<'_>) -> bool;

    /// Pasted text from the host terminal.
    fn handle_paste(&mut self, text: &str, cx: &mut Cx<'_>) -> bool {
        let _ = (text, cx);
        false
    }

    /// A mouse event for this view, drawn in `area` (coordinates are screen cells).
    /// `event.mods` already includes the command key when it is held. Once a button goes
    /// down in a view, the drags and the release that follow come to that view even when
    /// the pointer has left `area`.
    fn handle_mouse(&mut self, event: &MouseEvent, area: Rect, cx: &mut Cx<'_>) -> bool {
        let _ = (event, area, cx);
        false
    }

    /// The view gained or lost the keyboard focus. Losing it includes the whole window
    /// losing the focus to another application.
    fn focus_changed(&mut self, focused: bool, cx: &mut Cx<'_>) {
        let _ = (focused, cx);
    }

    /// Called after background work signals ([`Waker::wake`](crate::background::Waker::wake))
    /// and at any time asked for with `wake_at`: poll tasks and channels, advance timers.
    /// Return true if the view needs redrawing.
    fn tick(&mut self, cx: &mut Cx<'_>) -> bool {
        let _ = cx;
        false
    }

    /// Files changed on disk: reload or re-read what this view shows of them.
    fn paths_changed(&mut self, change: &FsChange, cx: &mut Cx<'_>) {
        let _ = (change, cx);
    }

    /// Every row for a picker only this view can fill: [`PickerKind::Links`] is asked of
    /// the terminal panel, [`PickerKind::Outline`] of the tab in front. The shell filters
    /// the rows by what is typed and draws them. `None` means this view has no such list.
    fn picker_rows(&mut self, kind: PickerKind, cx: &mut Cx<'_>) -> Option<Vec<PickerItem>> {
        let _ = (kind, cx);
        None
    }

    /// The row highlighted in that picker changed (`row` indexes what
    /// [`View::picker_rows`] returned), or with `None` the picker was dismissed without a
    /// choice: show a preview, or put back whatever a preview moved.
    fn picker_preview(&mut self, kind: PickerKind, row: Option<usize>, cx: &mut Cx<'_>) {
        let _ = (kind, row, cx);
    }

    /// A row of that picker was chosen (`secondary` for cmd-enter).
    fn picker_confirm(&mut self, kind: PickerKind, row: usize, secondary: bool, cx: &mut Cx<'_>) {
        let _ = (kind, row, secondary, cx);
    }

    /// What a panel wants back after a restart (the folders open in the tree, say), kept in
    /// the session file. `None` keeps nothing. Asked of the panels only; tabs are saved by
    /// path and position.
    fn session_state(&self) -> Option<serde_json::Value> {
        None
    }

    /// Put back what [`View::session_state`] gave when the session was saved. The value
    /// may come from an older bakedit: ignore what is not understood.
    fn restore_session_state(&mut self, state: &serde_json::Value, cx: &mut Cx<'_>) {
        let _ = (state, cx);
    }
}

/// What the status bar shows about the item in front.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ItemStatus {
    pub mode: Option<crate::chrome::ModeIndicator>,
    /// 1-based line and column, and the number of selected characters.
    pub position: Option<(usize, usize, usize)>,
    pub cursors: usize,
    pub language: Option<String>,
    pub indent: Option<String>,
    pub line_ending: Option<&'static str>,
}

/// Makes the items the shell cannot make itself. The app supplies the real one; tests
/// supply a fake.
pub trait ItemFactory {
    /// An editor (or other viewer) for a file.
    fn open_path(&mut self, path: &Path, cx: &mut Cx<'_>) -> Result<Box<dyn Item>, String>;
    /// An empty, unsaved buffer.
    fn untitled(&mut self, cx: &mut Cx<'_>) -> Box<dyn Item>;
    /// The project diff view.
    fn project_diff(&mut self, cx: &mut Cx<'_>) -> Box<dyn Item>;
    /// The project search view.
    fn project_search(&mut self, replace: bool, cx: &mut Cx<'_>) -> Box<dyn Item>;
    /// A page of text that cannot be edited (the list of key bindings, the built-in
    /// keymap). `language` is a language name to colour it as ("JSON"), if any.
    fn read_only(&mut self, title: &str, text: &str, language: Option<&str>, cx: &mut Cx<'_>) -> Box<dyn Item>;
}

/// The pickers whose rows come from outside the shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PickerKind {
    FileFinder,
    CommandPalette,
    TabSwitcher,
    Outline,
    Branches,
    Repositories,
    DiffBase,
    /// Paths and URLs visible in the terminal.
    Links,
    RecentProjects,
    Themes,
}

/// Supplies rows for the pickers whose rows depend on what is typed and come from the
/// project rather than from one view (files, branches, recent projects), and acts on the
/// choice.
pub trait PickerSource {
    fn rows(&mut self, kind: PickerKind, query: &str, cx: &mut Cx<'_>) -> Vec<PickerItem>;
    /// `row` is the selected row's index in what `rows` last returned, or `None` when the
    /// list was empty and the bare query was confirmed.
    fn confirm(&mut self, kind: PickerKind, row: Option<usize>, query: &str, secondary: bool, cx: &mut Cx<'_>);
    /// A number that changes when `rows` would answer differently for the same query (the
    /// file index grew, the branch list arrived). The shell asks while the picker is open
    /// and refills it, keeping the selection, when the number moves.
    fn version(&mut self, kind: PickerKind, cx: &mut Cx<'_>) -> u64 {
        let _ = (kind, cx);
        0
    }
}

/// A view that lives in a tab of the centre.
pub trait Item: View {
    /// What the status bar should say about this item.
    fn status(&self) -> ItemStatus {
        ItemStatus::default()
    }

    /// A stable name for what this item is ("editor", "project_diff", "project_search",
    /// "page" for read-only text).
    fn kind(&self) -> &'static str {
        "editor"
    }

    /// How the tab bar shows it.
    fn tab(&self, cx: &RenderCx<'_>) -> Tab;

    /// The file it shows, if it shows a file.
    fn path(&self) -> Option<&Path> {
        None
    }

    fn is_dirty(&self) -> bool {
        false
    }

    /// Write it to disk. `format` is false for "save without formatting".
    fn save(&mut self, format: bool, cx: &mut Cx<'_>) -> Result<(), String> {
        let _ = (format, cx);
        Ok(())
    }

    /// Move the cursor to a 1-based position (used when a file is opened at a line).
    fn go_to(&mut self, line: u32, column: Option<u32>) {
        let _ = (line, column);
    }

    /// The 1-based cursor position, for the navigation history and the session file.
    fn position(&self) -> Option<(u32, u32)> {
        None
    }

    /// How many lines the item has, for "go to line" counting from the end.
    fn line_count(&self) -> Option<usize> {
        None
    }

    /// The first line shown (0-based), for the session file.
    fn scroll_top(&self) -> Option<u32> {
        None
    }

    /// Scroll so that `line` (0-based) is the first one shown, when a session is restored.
    fn set_scroll_top(&mut self, line: u32) {
        let _ = line;
    }

    /// What a search started from this tab should look for first: the selection, or the
    /// word at the cursor.
    fn query_suggestion(&self) -> Option<String> {
        None
    }

    /// The file `pane::RevealInProjectPanel` shows in the tree: the one this tab shows, or
    /// for a view of many files (the project diff) the one at its cursor.
    fn reveal_path(&self) -> Option<std::path::PathBuf> {
        self.path().map(Path::to_path_buf)
    }

    /// The tab is closed, or a preview took its place: let go of what is kept outside the
    /// item for it (an editor's colours in the project's syntax store).
    fn closed(&mut self, cx: &mut Cx<'_>) {
        let _ = cx;
    }

    /// bakedit moved or renamed `from` to `to` (a file, or a folder the file is in): show
    /// and save the file at its new place.
    fn moved(&mut self, from: &Path, to: &Path, cx: &mut Cx<'_>) {
        let _ = (from, to, cx);
    }

    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

/// What a view's own tests need to drive it without a shell: the things a `Cx` borrows.
#[cfg(test)]
pub(crate) mod test_support {
    use std::path::PathBuf;
    use std::time::Instant;

    use bakedit_core::{Mods, Theme};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    use super::{Cursor, Cx, Effect, RenderCx, View};
    use crate::clipboard::{Clipboard, MemoryBackend};
    use crate::project::Project;
    use crate::settings::Settings;
    use crate::ui::Canvas;

    pub struct Harness {
        pub settings: Settings,
        pub theme: Theme,
        pub clipboard: Clipboard,
        pub effects: Vec<Effect>,
        pub project: Project,
        pub held_mods: Mods,
    }

    impl Harness {
        /// A harness over a project that touches nothing on disk.
        pub fn new(root: impl Into<PathBuf>) -> Harness {
            Harness::with_project(Project::detached(root))
        }

        pub fn with_project(project: Project) -> Harness {
            Harness {
                settings: Settings::default(),
                theme: Theme::builtin(),
                clipboard: Clipboard::with_backend(Box::new(MemoryBackend::default())),
                effects: Vec::new(),
                project,
                held_mods: Mods::empty(),
            }
        }

        pub fn cx(&mut self) -> Cx<'_> {
            Cx {
                settings: &self.settings,
                theme: &self.theme,
                clipboard: &mut self.clipboard,
                effects: &mut self.effects,
                project: &self.project,
                root: self.project.root(),
                active_path: None,
                now: Instant::now(),
                held_mods: self.held_mods,
            }
        }

        pub fn render_cx(&self, focused: bool) -> RenderCx<'_> {
            RenderCx {
                settings: &self.settings,
                theme: &self.theme,
                project: &self.project,
                root: self.project.root(),
                active_path: None,
                now: Instant::now(),
                focused,
                zoomed: false,
                held_mods: self.held_mods,
            }
        }

        /// Draw `view` focused on a `width` x `height` screen: the frame as text (one line
        /// per row, trailing blanks dropped), the cursor it asked for, and the cells.
        pub fn draw(&self, view: &mut dyn View, width: u16, height: u16) -> (String, Option<Cursor>, Buffer) {
            let area = Rect::new(0, 0, width, height);
            let mut buf = Buffer::empty(area);
            let cursor = {
                let mut canvas = Canvas::new(&mut buf, area);
                view.render(&mut canvas, area, &self.render_cx(true))
            };
            (crate::dump::to_text(&buf), cursor, buf)
        }

        /// The effects pushed so far, taken out.
        pub fn take_effects(&mut self) -> Vec<Effect> {
            std::mem::take(&mut self.effects)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_builder() {
        let context = Context::new("Editor").value("mode", "full").flag("multibuffer").flag_if(false, "nope").flag_if(true, "yes");
        assert_eq!(context.name, "Editor");
        assert_eq!(context.flags, vec!["multibuffer", "yes"]);
        assert_eq!(context.values, vec![("mode", "full".to_string())]);
    }

    #[test]
    fn fs_changes_touch_listed_paths_and_what_is_below_them() {
        let change = FsChange::of(vec!["/p/src".into(), "/p/a.rs".into()]);
        assert!(change.touches(Path::new("/p/a.rs")));
        assert!(change.touches(Path::new("/p/src/deep/b.rs")), "a renamed folder touches its files");
        assert!(!change.touches(Path::new("/p/src2/b.rs")), "a name that merely starts the same is another folder");
        assert!(!change.touches(Path::new("/p")));
        assert!(!change.is_empty());
        let rescan = FsChange { paths: Vec::new(), rescan: true };
        assert!(rescan.touches(Path::new("/anything")));
        assert!(!rescan.is_empty());
        assert!(FsChange::default().is_empty());
    }

    #[test]
    fn effect_shorthands() {
        assert_eq!(Effect::status("ok"), Effect::Status { text: "ok".into(), error: false });
        assert_eq!(Effect::error("bad"), Effect::Status { text: "bad".into(), error: true });
        assert_eq!(
            Effect::open("/a.rs"),
            Effect::OpenFile { path: "/a.rs".into(), line: None, column: None, preview: false, focus: true }
        );
    }
}
