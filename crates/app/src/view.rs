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
    /// Close the tab this view lives in (sent by an item about itself).
    CloseItem,
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

/// What a view may read and use while it handles input.
pub struct Cx<'a> {
    pub settings: &'a Settings,
    pub theme: &'a Theme,
    pub clipboard: &'a mut Clipboard,
    pub effects: &'a mut Vec<Effect>,
    /// The project directory.
    pub root: &'a Path,
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
    pub root: &'a Path,
    pub now: Instant,
    /// This view has the keyboard focus.
    pub focused: bool,
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

    /// A key press no binding claimed (typed text, mostly). Return false if unused.
    fn handle_key(&mut self, key: &KeyEvent, cx: &mut Cx<'_>) -> bool;

    /// Pasted text from the host terminal.
    fn handle_paste(&mut self, text: &str, cx: &mut Cx<'_>) -> bool {
        let _ = (text, cx);
        false
    }

    /// A mouse event inside `area` (coordinates are screen cells).
    fn handle_mouse(&mut self, event: &MouseEvent, area: Rect, cx: &mut Cx<'_>) -> bool {
        let _ = (event, area, cx);
        false
    }

    /// The view gained or lost the keyboard focus.
    fn focus_changed(&mut self, focused: bool, cx: &mut Cx<'_>) {
        let _ = (focused, cx);
    }

    /// Called regularly (and after background work signals): poll channels, advance timers.
    /// Return true if the view needs redrawing.
    fn tick(&mut self, cx: &mut Cx<'_>) -> bool {
        let _ = cx;
        false
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
}

/// The pickers whose rows come from outside the shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PickerKind {
    FileFinder,
    CommandPalette,
    TabSwitcher,
    Outline,
    Branches,
    /// Paths and URLs visible in the terminal.
    Links,
    RecentProjects,
}

/// Supplies rows for the pickers the shell does not fill itself, and acts on the choice.
pub trait PickerSource {
    fn rows(&mut self, kind: PickerKind, query: &str, cx: &mut Cx<'_>) -> Vec<crate::picker::PickerItem>;
    /// `row` is the selected row's index in what `rows` last returned, or `None` when the
    /// list was empty and the bare query was confirmed.
    fn confirm(&mut self, kind: PickerKind, row: Option<usize>, query: &str, secondary: bool, cx: &mut Cx<'_>);
}

/// A view that lives in a tab of the centre.
pub trait Item: View {
    /// What the status bar should say about this item.
    fn status(&self) -> ItemStatus {
        ItemStatus::default()
    }

    /// A stable name for what this item is ("editor", "project_diff", "project_search").
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

    /// Files on disk changed: reload if this item shows one of them.
    fn paths_changed(&mut self, paths: &[PathBuf], cx: &mut Cx<'_>) {
        let _ = (paths, cx);
    }

    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
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
    fn effect_shorthands() {
        assert_eq!(Effect::status("ok"), Effect::Status { text: "ok".into(), error: false });
        assert_eq!(Effect::error("bad"), Effect::Status { text: "bad".into(), error: true });
        assert_eq!(
            Effect::open("/a.rs"),
            Effect::OpenFile { path: "/a.rs".into(), line: None, column: None, preview: false, focus: true }
        );
    }
}
