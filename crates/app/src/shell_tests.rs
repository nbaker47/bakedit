//! The shell driven end to end with stand-in views: the user's panel, fullscreen, tab,
//! prompt and picker flows as sequences of actions.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use bakedit_core::{Action, KeyEvent, MouseButton, MouseEvent, MouseKind, Theme};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::json;

use crate::clipboard::{Clipboard, MemoryBackend};
use crate::pane::test_support::FakeItem;
use crate::picker::PickerItem;
use crate::project::Project;
use crate::settings::Settings;
use crate::shell::{Modal, Request, Shell};
use crate::ui::{Canvas, Style};
use crate::view::{Context, Cursor, Cx, Effect, FsChange, Item, ItemFactory, PickerKind, PickerSource, RenderCx, View};
use crate::workspace::{DockPosition, Focus, PanelId, Zoomed};

pub(crate) type Log = Rc<RefCell<Vec<String>>>;

struct FakePanel {
    name: &'static str,
    log: Log,
    handles: Vec<&'static str>,
}

impl View for FakePanel {
    fn render(&mut self, canvas: &mut Canvas<'_>, area: Rect, cx: &RenderCx<'_>) -> Option<Cursor> {
        canvas.fill(area, cx.theme.panel_bg);
        let label = if cx.focused { format!("[{}]", self.name) } else { self.name.to_string() };
        canvas.text(area.x + 1, area.y, &label, Style::fg(cx.theme.text), area.width);
        None
    }
    fn contexts(&self, out: &mut Vec<Context>) {
        // The real tree says whether a name is being edited; the keymap keys off it.
        out.push(Context::new(self.name).flag_if(self.name == "ProjectPanel", "not_editing"));
    }
    fn handle_action(&mut self, action: &Action, _: &mut Cx<'_>) -> bool {
        self.log.borrow_mut().push(format!("{} action {} {}", self.name, action.name, action.args.clone().unwrap_or(json!(null))));
        self.handles.contains(&action.name.as_str())
    }
    fn handle_key(&mut self, key: &KeyEvent, _: &mut Cx<'_>) -> bool {
        self.log.borrow_mut().push(format!("{} key {}", self.name, key.display()));
        true
    }
    fn handle_mouse(&mut self, event: &MouseEvent, _: Rect, _: &mut Cx<'_>) -> bool {
        self.log.borrow_mut().push(format!("{} mouse {:?}", self.name, event.kind));
        true
    }
    fn focus_changed(&mut self, focused: bool, _: &mut Cx<'_>) {
        self.log.borrow_mut().push(format!("{} focus {focused}", self.name));
    }
    fn paths_changed(&mut self, change: &FsChange, _: &mut Cx<'_>) {
        let paths: Vec<String> = change.paths.iter().map(|p| p.display().to_string()).collect();
        self.log.borrow_mut().push(format!("{} paths {}", self.name, paths.join(",")));
    }
}

/// A terminal stand-in that knows the links on its screen.
struct LinkPanel {
    log: Log,
    links: Vec<&'static str>,
}

impl View for LinkPanel {
    fn render(&mut self, canvas: &mut Canvas<'_>, area: Rect, cx: &RenderCx<'_>) -> Option<Cursor> {
        canvas.fill(area, cx.theme.panel_bg);
        None
    }
    fn contexts(&self, out: &mut Vec<Context>) {
        out.push(Context::new("Terminal"));
    }
    fn handle_action(&mut self, _: &Action, _: &mut Cx<'_>) -> bool {
        false
    }
    fn handle_key(&mut self, _: &KeyEvent, _: &mut Cx<'_>) -> bool {
        false
    }
    fn picker_rows(&mut self, kind: PickerKind, _: &mut Cx<'_>) -> Option<Vec<PickerItem>> {
        (kind == PickerKind::Links).then(|| self.links.iter().map(|link| PickerItem::new(*link)).collect())
    }
    fn picker_preview(&mut self, _: PickerKind, row: Option<usize>, _: &mut Cx<'_>) {
        self.log.borrow_mut().push(format!("preview {row:?}"));
    }
    fn picker_confirm(&mut self, _: PickerKind, row: usize, secondary: bool, cx: &mut Cx<'_>) {
        self.log.borrow_mut().push(format!("confirm {row} {secondary}"));
        cx.push(Effect::open(format!("/project/{}", self.links[row])));
    }
}

struct Factory;

impl ItemFactory for Factory {
    fn open_path(&mut self, path: &Path, _: &mut Cx<'_>) -> Result<Box<dyn Item>, String> {
        if path.to_string_lossy().contains("missing") {
            return Err(format!("{} does not exist", path.display()));
        }
        Ok(FakeItem::file(&path.to_string_lossy()))
    }
    fn untitled(&mut self, _: &mut Cx<'_>) -> Box<dyn Item> {
        FakeItem::untitled("untitled")
    }
    fn project_diff(&mut self, _: &mut Cx<'_>) -> Box<dyn Item> {
        let mut item = FakeItem::untitled("Project Diff");
        item.kind = "project_diff";
        item.handles = vec!["project_diff::ShowFile"];
        item
    }
    fn project_search(&mut self, _: bool, _: &mut Cx<'_>) -> Box<dyn Item> {
        let mut item = FakeItem::untitled("Search");
        item.kind = "project_search";
        item
    }
    fn read_only(&mut self, title: &str, _: &str, _: Option<&str>, _: &mut Cx<'_>) -> Box<dyn Item> {
        let mut item = FakeItem::untitled(title);
        item.kind = "page";
        item
    }
}

struct Source {
    files: Vec<&'static str>,
}

impl PickerSource for Source {
    fn rows(&mut self, _: PickerKind, query: &str, _: &mut Cx<'_>) -> Vec<PickerItem> {
        self.files.iter().filter(|f| f.contains(query)).map(|f| PickerItem::new(*f).completion(*f)).collect()
    }
    fn confirm(&mut self, _: PickerKind, row: Option<usize>, query: &str, _: bool, cx: &mut Cx<'_>) {
        let matching: Vec<&&str> = self.files.iter().filter(|f| f.contains(query)).collect();
        match row {
            Some(row) => {
                if let Some(file) = matching.get(row) {
                    cx.push(Effect::open(format!("/project/{file}")));
                }
            }
            None => cx.status(format!("confirmed the query {query:?}")),
        }
    }
}

pub(crate) fn shell() -> (Shell, Log) {
    let log: Log = Rc::default();
    let mut shell = Shell::new(
        Settings::default(),
        Theme::builtin(),
        Project::detached("/project"),
        Clipboard::with_backend(Box::new(MemoryBackend::default())),
        Box::new(Factory),
        Box::new(Source { files: vec!["src/main.rs", "src/lib.rs", "README.md"] }),
    );
    for (id, name, handles) in [
        (PanelId::Terminal, "Terminal", vec![]),
        (PanelId::Git, "GitPanel", vec![]),
        (PanelId::Project, "ProjectPanel", vec!["project_panel::ScrollCursorCenter", "project_panel::RevealPath"]),
    ] {
        shell.set_panel(id, Box::new(FakePanel { name, log: log.clone(), handles }));
    }
    (shell, log)
}

fn act(shell: &mut Shell, name: &str) -> bool {
    shell.dispatch(&Action::new(name))
}

fn frame(shell: &mut Shell, width: u16, height: u16) -> String {
    let mut buf = Buffer::empty(Rect::new(0, 0, width, height));
    shell.render(&mut buf);
    crate::dump::to_text(&buf)
}

pub(crate) fn fake(shell: &Shell, index: usize) -> &FakeItem {
    shell.pane.item(index).unwrap().as_any().downcast_ref::<FakeItem>().unwrap()
}

pub(crate) fn fake_mut(shell: &mut Shell, index: usize) -> &mut FakeItem {
    shell.pane.item_mut(index).unwrap().as_any_mut().downcast_mut::<FakeItem>().unwrap()
}

fn names(shell: &Shell) -> Vec<String> {
    shell.pane.slots().iter().map(|slot| slot.item.as_any().downcast_ref::<FakeItem>().unwrap().name.clone()).collect()
}

#[test]
fn cmd_t_shows_the_terminal_and_cmd_t_again_hides_it() {
    let (mut shell, log) = shell();
    assert!(act(&mut shell, "terminal_panel::Toggle"));
    assert_eq!(shell.workspace.focus(), Focus::Panel(PanelId::Terminal));
    assert_eq!(shell.workspace.visible_panel(DockPosition::Left), Some(PanelId::Terminal));
    assert!(act(&mut shell, "terminal_panel::Toggle"));
    assert_eq!(shell.workspace.focus(), Focus::Center);
    assert_eq!(shell.workspace.visible_panel(DockPosition::Left), None);
    let log = log.borrow();
    assert!(log.contains(&"Terminal focus true".to_string()));
    assert!(log.contains(&"Terminal focus false".to_string()));
}

#[test]
fn the_fullscreen_terminal_flow() {
    let (mut shell, _) = shell();
    // cmd-alt-t from the editor: the terminal comes up fullscreen.
    act(&mut shell, "terminal_panel::ToggleZoom");
    assert_eq!(shell.workspace.zoomed(), Some(Zoomed::Panel(PanelId::Terminal)));
    let text = frame(&mut shell, 100, 20);
    assert!(text.lines().next().unwrap().starts_with(" [Terminal]"));
    assert_eq!(shell.layout().panel(PanelId::Terminal), Some(Rect::new(0, 0, 100, 18)));
    // cmd-e: back to the editor, the zoomed terminal gets out of the way.
    act(&mut shell, "workspace::FocusCenterPane");
    assert_eq!(shell.workspace.zoomed(), None);
    assert_eq!(shell.workspace.visible_panel(DockPosition::Left), None);
    // cmd-t: it returns still fullscreen; cmd-alt-f (ToggleZoom in the terminal) un-zooms it.
    act(&mut shell, "terminal_panel::Toggle");
    assert_eq!(shell.workspace.zoomed(), Some(Zoomed::Panel(PanelId::Terminal)));
    act(&mut shell, "workspace::ToggleZoom");
    assert_eq!(shell.workspace.zoomed(), None);
    assert_eq!(shell.workspace.visible_panel(DockPosition::Left), Some(PanelId::Terminal));
}

#[test]
fn git_and_terminal_share_the_left_dock() {
    let (mut shell, _) = shell();
    act(&mut shell, "terminal_panel::Toggle");
    act(&mut shell, "git_panel::Toggle");
    assert_eq!(shell.workspace.visible_panel(DockPosition::Left), Some(PanelId::Git));
    act(&mut shell, "project_panel::ToggleFocus");
    assert_eq!(shell.workspace.visible_panel(DockPosition::Right), Some(PanelId::Project));
    let text = frame(&mut shell, 160, 20);
    let first = text.lines().next().unwrap();
    assert!(first.starts_with(" GitPanel"), "{first:?}");
    assert!(first.contains("[ProjectPanel]"), "the focused panel is on the right: {first:?}");
}

#[test]
fn cmd_shift_e_focuses_the_tree_and_closes_it_from_inside() {
    let (mut shell, log) = shell();
    // Workspace binding: [project_panel::ToggleFocus, project_panel::ScrollCursorCenter]
    act(&mut shell, "project_panel::ToggleFocus");
    assert!(act(&mut shell, "project_panel::ScrollCursorCenter"), "the panel takes the second action");
    assert_eq!(shell.workspace.focus(), Focus::Panel(PanelId::Project));
    assert!(log.borrow().iter().any(|l| l.starts_with("ProjectPanel action project_panel::ScrollCursorCenter")));
    // In the tree, cmd-shift-e is workspace::CloseActiveDock.
    assert!(act(&mut shell, "workspace::CloseActiveDock"));
    assert_eq!(shell.workspace.focus(), Focus::Center);
    assert_eq!(shell.workspace.visible_panel(DockPosition::Right), None);
    assert!(!act(&mut shell, "workspace::CloseActiveDock"), "from the editor there is no dock to close");
}

#[test]
fn cmd_alt_e_focuses_the_editor_and_hides_every_dock() {
    let (mut shell, _) = shell();
    act(&mut shell, "terminal_panel::Toggle");
    act(&mut shell, "project_panel::ToggleFocus");
    act(&mut shell, "workspace::FocusCenterPane");
    act(&mut shell, "workspace::ToggleAllDocks");
    frame(&mut shell, 120, 20);
    assert!(shell.layout().docks.is_empty());
    assert_eq!(shell.layout().center, Some(Rect::new(0, 0, 120, 18)));
    act(&mut shell, "workspace::ToggleAllDocks");
    frame(&mut shell, 120, 20);
    assert_eq!(shell.layout().docks.len(), 2);
}

#[test]
fn opening_files_previews_and_reveals() {
    let (mut shell, log) = shell();
    shell.open_file(Path::new("/project/a.rs"), None, None, false, true);
    shell.open_file(Path::new("/project/b.rs"), None, None, true, false);
    shell.open_file(Path::new("/project/c.rs"), Some(12), None, true, false);
    assert_eq!(names(&shell), ["a.rs", "c.rs"], "the preview tab was reused");
    assert_eq!(fake(&shell, 1).line, 12);
    // Opening the previewed file for real keeps it.
    shell.open_file(Path::new("/project/c.rs"), None, None, false, true);
    assert!(!shell.pane.is_preview(1));
    shell.open_file(Path::new("/project/missing.rs"), None, None, false, true);
    let text = frame(&mut shell, 100, 12);
    assert!(text.contains("/project/missing.rs does not exist"), "{text}");
    // pane::RevealInProjectPanel sends the path to the tree and focuses it.
    assert!(act(&mut shell, "pane::RevealInProjectPanel"));
    assert_eq!(shell.workspace.focus(), Focus::Panel(PanelId::Project));
    assert!(log.borrow().iter().any(|l| l.contains("project_panel::RevealPath") && l.contains("/project/c.rs")), "{:?}", log.borrow());
}

#[test]
fn closing_an_unsaved_tab_asks_first() {
    let (mut shell, _) = shell();
    shell.open_file(Path::new("/project/a.rs"), None, None, false, true);
    fake_mut(&mut shell, 0).dirty = true;
    act(&mut shell, "pane::CloseActiveItem");
    assert!(matches!(shell.modal, Some(Modal::Prompt { .. })));
    assert_eq!(shell.pane.len(), 1);
    let text = frame(&mut shell, 100, 20);
    assert!(text.contains("Save changes to a.rs?"), "{text}");
    assert!(text.contains("Don't Save"));
    // Escape cancels and the tab stays.
    act(&mut shell, "menu::Cancel");
    assert!(shell.modal.is_none());
    assert_eq!(shell.pane.len(), 1);
    // "Save" (first button) saves and closes.
    act(&mut shell, "pane::CloseActiveItem");
    act(&mut shell, "menu::SelectNext");
    act(&mut shell, "menu::Confirm");
    assert!(shell.pane.is_empty());
    // "Don't Save", chosen by its first letter.
    shell.open_file(Path::new("/project/b.rs"), None, None, false, true);
    fake_mut(&mut shell, 0).dirty = true;
    act(&mut shell, "pane::CloseActiveItem");
    shell.handle_key(&KeyEvent::typed('d'));
    assert!(shell.pane.is_empty());
    assert!(shell.modal.is_none());
}

#[test]
fn bulk_close_and_reopen() {
    let (mut shell, _) = shell();
    for name in ["a.rs", "b.rs", "c.rs"] {
        shell.open_file(Path::new(&format!("/project/{name}")), None, None, false, true);
    }
    shell.dispatch(&Action::with_args("pane::ActivateItem", json!(0)));
    act(&mut shell, "pane::CloseOtherItems");
    assert_eq!(names(&shell), ["a.rs"]);
    act(&mut shell, "pane::ReopenClosedItem");
    assert_eq!(names(&shell).len(), 2);
    act(&mut shell, "pane::ActivateNextItem");
    act(&mut shell, "pane::ActivatePreviousItem");
    act(&mut shell, "pane::CloseAllItems");
    assert!(shell.pane.is_empty());
    let text = frame(&mut shell, 100, 20);
    assert!(text.contains("No file open"), "{text}");
    assert!(text.contains("cmd-p         find a file"));
}

#[test]
fn quitting_with_unsaved_work_asks() {
    let (mut shell, _) = shell();
    act(&mut shell, "zed::Quit");
    assert!(shell.quit, "nothing unsaved: quit at once");
    let (mut shell, _) = shell_with_dirty();
    act(&mut shell, "zed::Quit");
    assert!(!shell.quit);
    assert!(matches!(shell.modal, Some(Modal::Prompt { .. })));
    act(&mut shell, "menu::Confirm");
    assert!(!shell.quit, "the default button is Cancel");
    act(&mut shell, "zed::Quit");
    shell.handle_key(&KeyEvent::typed('s'));
    assert!(shell.quit);
    assert_eq!(fake(&shell, 0).saves, 1);
}

fn shell_with_dirty() -> (Shell, Log) {
    let (mut shell, log) = shell();
    shell.open_file(Path::new("/project/a.rs"), None, None, false, true);
    fake_mut(&mut shell, 0).dirty = true;
    (shell, log)
}

#[test]
fn saving() {
    let (mut shell, _) = shell_with_dirty();
    act(&mut shell, "workspace::Save");
    assert_eq!(fake(&shell, 0).saves, 1);
    assert!(frame(&mut shell, 100, 12).contains("Saved a.rs"));
    shell.open_file(Path::new("/project/readonly.rs"), None, None, false, true);
    fake_mut(&mut shell, 1).dirty = true;
    act(&mut shell, "workspace::SaveAll");
    assert!(frame(&mut shell, 100, 12).contains("readonly.rs is read-only"));
}

#[test]
fn the_command_palette_runs_commands() {
    let (mut shell, _) = shell_with_dirty();
    act(&mut shell, "command_palette::Toggle");
    for c in "wssave".chars() {
        shell.handle_key(&KeyEvent::typed(c));
    }
    let text = frame(&mut shell, 120, 24);
    assert!(text.contains("workspace: save"), "{text}");
    act(&mut shell, "menu::Confirm");
    assert!(shell.modal.is_none());
    assert_eq!(fake(&shell, 0).saves, 1, "the chosen command ran");
    // The same key closes it again.
    act(&mut shell, "command_palette::Toggle");
    act(&mut shell, "command_palette::Toggle");
    assert!(shell.modal.is_none());
}

#[test]
fn the_file_finder_opens_what_is_picked() {
    let (mut shell, _) = shell();
    act(&mut shell, "file_finder::Toggle");
    let text = frame(&mut shell, 120, 24);
    assert!(text.contains("Search project files"));
    assert!(text.contains("src/main.rs") && text.contains("README.md"));
    for c in "lib".chars() {
        shell.handle_key(&KeyEvent::typed(c));
    }
    act(&mut shell, "menu::Confirm");
    assert_eq!(names(&shell), ["lib.rs"]);
    assert_eq!(shell.workspace.focus(), Focus::Center);
    // Nothing matches: the picker says so and confirming does nothing.
    act(&mut shell, "file_finder::Toggle");
    shell.handle_paste("zzz");
    assert!(frame(&mut shell, 120, 24).contains("No matching files"));
    act(&mut shell, "menu::Confirm");
    assert_eq!(names(&shell), ["lib.rs"]);
}

#[test]
fn the_tab_switcher_flips_to_the_previous_tab() {
    let (mut shell, _) = shell();
    for name in ["a.rs", "b.rs", "c.rs"] {
        shell.open_file(Path::new(&format!("/project/{name}")), None, None, false, true);
    }
    shell.dispatch(&Action::with_args("pane::ActivateItem", json!(0)));
    // Most recent: a, then c, then b. ctrl-tab then enter goes to c.
    act(&mut shell, "tab_switcher::Toggle");
    act(&mut shell, "menu::Confirm");
    assert_eq!(shell.pane.active_index(), Some(2));
    act(&mut shell, "tab_switcher::Toggle");
    act(&mut shell, "menu::Confirm");
    assert_eq!(shell.pane.active_index(), Some(0), "and back again");
    // ctrl-shift-tab starts at the tab used longest ago: b.
    shell.dispatch(&Action::with_args("tab_switcher::Toggle", json!({ "select_last": true })));
    act(&mut shell, "menu::Confirm");
    assert_eq!(shell.pane.active_index(), Some(1));
}

#[test]
fn closing_a_tab_tells_its_item() {
    use crate::pane::test_support::take_closed;
    let (mut shell, _) = shell();
    take_closed();
    shell.open_file(Path::new("/project/a.rs"), None, None, false, true);
    shell.open_file(Path::new("/project/b.rs"), None, None, true, true);
    // A preview replaced by the next preview is closed too.
    shell.open_file(Path::new("/project/c.rs"), None, None, true, true);
    assert_eq!(take_closed(), ["b.rs"]);
    act(&mut shell, "pane::CloseActiveItem");
    assert_eq!(take_closed(), ["c.rs"]);
    act(&mut shell, "pane::CloseAllItems");
    assert_eq!(take_closed(), ["a.rs"]);
}

#[test]
fn go_to_line_and_back() {
    let (mut shell, _) = shell();
    shell.open_file(Path::new("/project/a.rs"), None, None, false, true);
    act(&mut shell, "go_to_line::Toggle");
    assert!(frame(&mut shell, 100, 20).contains("Go to line"));
    shell.handle_paste("120:4");
    act(&mut shell, "menu::Confirm");
    assert_eq!(fake(&shell, 0).line, 120);
    act(&mut shell, "pane::GoBack");
    assert_eq!(fake(&shell, 0).line, 1);
    act(&mut shell, "pane::GoForward");
    assert_eq!(fake(&shell, 0).line, 120);
    act(&mut shell, "go_to_line::Toggle");
    shell.handle_paste("abc");
    act(&mut shell, "menu::Confirm");
    assert!(frame(&mut shell, 100, 20).contains("Not a line number: abc"));
}

#[test]
fn the_project_diff_is_one_tab() {
    let (mut shell, _) = shell();
    act(&mut shell, "git_panel::Toggle");
    act(&mut shell, "git::Diff");
    assert_eq!(names(&shell), ["Project Diff"]);
    assert_eq!(shell.workspace.focus(), Focus::Center);
    act(&mut shell, "git::Diff");
    assert_eq!(names(&shell).len(), 1, "it is reused, not opened twice");
    // From the git panel, without taking the focus, scrolled to a file.
    shell.set_focus(Focus::Panel(PanelId::Git));
    shell.dispatch(&Action::new("noop"));
    let effect = Effect::ShowProjectDiff { path: Some("/project/a.rs".into()), focus: false };
    shell.dispatch(&Action::with_args("test::Effect", json!(null)));
    // Effects arrive from views; push one the way a view would, through a dispatch cycle.
    struct Pusher(Option<Effect>);
    impl View for Pusher {
        fn render(&mut self, _: &mut Canvas<'_>, _: Rect, _: &RenderCx<'_>) -> Option<Cursor> {
            None
        }
        fn contexts(&self, _: &mut Vec<Context>) {}
        fn handle_action(&mut self, _: &Action, cx: &mut Cx<'_>) -> bool {
            if let Some(effect) = self.0.take() {
                cx.push(effect);
            }
            true
        }
        fn handle_key(&mut self, _: &KeyEvent, _: &mut Cx<'_>) -> bool {
            false
        }
    }
    shell.set_panel(PanelId::Git, Box::new(Pusher(Some(effect))));
    act(&mut shell, "menu::Confirm");
    assert_eq!(shell.workspace.focus(), Focus::Panel(PanelId::Git), "the focus stays in the git panel");
    assert!(fake(&shell, 0).seen.contains(&"project_diff::ShowFile".to_string()));
}

#[test]
fn key_contexts_follow_the_focus() {
    let (mut shell, _) = shell();
    let names = |shell: &Shell| shell.contexts().iter().map(|c| c.name).collect::<Vec<_>>();
    assert_eq!(names(&shell), ["Workspace", "Pane"]);
    shell.open_file(Path::new("/project/a.rs"), None, None, false, true);
    assert_eq!(names(&shell), ["Workspace", "Pane", "Fake"]);
    act(&mut shell, "terminal_panel::Toggle");
    assert_eq!(names(&shell), ["Workspace", "Dock", "Terminal"]);
    act(&mut shell, "file_finder::Toggle");
    assert_eq!(names(&shell), ["Workspace", "FileFinder", "Picker", "Editor"]);
    assert_eq!(shell.contexts()[3].values, vec![("mode", "single_line".to_string())]);
    act(&mut shell, "menu::Cancel");
    act(&mut shell, "go_to_line::Toggle");
    assert_eq!(names(&shell), ["Workspace", "GoToLine", "Editor"]);
}

#[test]
fn unclaimed_keys_and_actions_go_to_the_focused_view_only() {
    let (mut shell, log) = shell();
    shell.open_file(Path::new("/project/a.rs"), None, None, false, true);
    shell.handle_key(&KeyEvent::typed('x'));
    assert!(fake(&shell, 0).seen.contains(&"typed x".to_string()));
    act(&mut shell, "terminal_panel::Toggle");
    shell.handle_key(&KeyEvent::typed('y'));
    assert!(log.borrow().contains(&"Terminal key y".to_string()));
    assert!(!fake(&shell, 0).seen.contains(&"typed y".to_string()));
    assert!(!act(&mut shell, "editor::MoveUp"), "nobody in the terminal handles an editor action");
    assert!(!act(&mut shell, "no::SuchAction"));
}

#[test]
fn requests_for_the_app_are_collected() {
    let (mut shell, _) = shell();
    act(&mut shell, "bakedit::ToggleNavLayer");
    act(&mut shell, "bakedit::ReloadConfig");
    assert_eq!(shell.requests, vec![Request::ToggleNavLayer, Request::ReloadConfig]);
}

#[test]
fn the_frame_has_every_part() {
    let (mut shell, _) = shell();
    shell.open_file(Path::new("/project/a.rs"), None, None, false, true);
    shell.open_file(Path::new("/project/b.rs"), None, None, false, true);
    act(&mut shell, "terminal_panel::Toggle");
    act(&mut shell, "project_panel::ToggleFocus");
    shell.git.branch = Some("main".into());
    shell.usage = vec![crate::usage::Vendor {
        short: "cld".into(),
        name: "Claude".into(),
        meters: vec![crate::usage::Meter { window: "7d".into(), percent: 28.0, reset_at: None }],
        error: None,
    }];
    let text = frame(&mut shell, 200, 30);
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].starts_with(" Terminal"), "{:?}", lines[0]);
    assert!(lines[0].contains(" a.rs "), "the tab bar: {:?}", lines[0]);
    assert!(lines[0].contains(" b.rs "));
    assert!(lines[0].contains("[ProjectPanel]"));
    assert!(lines[1].contains("b.rs"), "the active item draws below the tabs: {:?}", lines[1]);
    assert_eq!(lines[0].matches('│').count(), 2, "one divider each side of the centre");
    assert!(lines[28].contains("main"), "the status bar is the row above the usage strip: {:?}", lines[28]);
    assert_eq!(lines[29], " cld 7d ▰▰▱▱▱▱ 28%");
    // Without the strip the status bar is the last row.
    shell.settings.usage_meter = false;
    let text = frame(&mut shell, 200, 30);
    assert!(text.lines().nth(29).unwrap().contains("main"));
    for (w, h) in [(1, 1), (10, 3), (40, 8), (20, 40)] {
        frame(&mut shell, w, h);
    }
}

#[test]
fn clicks_focus_switch_tabs_and_resize() {
    let (mut shell, log) = shell();
    shell.open_file(Path::new("/project/a.rs"), None, None, false, true);
    shell.open_file(Path::new("/project/b.rs"), None, None, false, true);
    act(&mut shell, "terminal_panel::Toggle");
    act(&mut shell, "workspace::FocusCenterPane");
    let text = frame(&mut shell, 200, 30);
    let click = |kind: MouseKind, col: u16, row: u16| MouseEvent::new(kind, col, row);
    // A click in the terminal focuses it and is passed on.
    shell.handle_mouse(&click(MouseKind::Press(MouseButton::Left), 5, 5));
    assert_eq!(shell.workspace.focus(), Focus::Panel(PanelId::Terminal));
    assert!(log.borrow().iter().any(|l| l.starts_with("Terminal mouse Press")));
    // The wheel over the editor does not steal the focus.
    let center = shell.layout().center.unwrap();
    shell.handle_mouse(&click(MouseKind::ScrollDown, center.x + 5, center.y + 5));
    assert_eq!(shell.workspace.focus(), Focus::Panel(PanelId::Terminal));
    // Clicking the first tab activates it and focuses the editor.
    let tab_col = text.lines().next().unwrap().find(" a.rs").unwrap() as u16;
    shell.handle_mouse(&click(MouseKind::Press(MouseButton::Left), tab_col + 2, 0));
    assert_eq!(shell.pane.active_index(), Some(0));
    assert_eq!(shell.workspace.focus(), Focus::Center);
    // Middle click closes a tab.
    shell.handle_mouse(&click(MouseKind::Press(MouseButton::Middle), tab_col + 2, 0));
    assert_eq!(names(&shell), ["b.rs"]);
    // Dragging the divider resizes the dock.
    let divider = shell.layout().dividers[0].rect;
    shell.handle_mouse(&click(MouseKind::Press(MouseButton::Left), divider.x, 4));
    shell.handle_mouse(&click(MouseKind::Drag(MouseButton::Left), divider.x - 20, 4));
    shell.handle_mouse(&click(MouseKind::Release(MouseButton::Left), divider.x - 20, 4));
    frame(&mut shell, 200, 30);
    assert_eq!(shell.layout().panel(PanelId::Terminal).unwrap().width, divider.x - 20);
}

#[test]
fn a_prompt_blocks_everything_else() {
    let (mut shell, _) = shell_with_dirty();
    act(&mut shell, "pane::CloseActiveItem");
    assert!(act(&mut shell, "terminal_panel::Toggle"), "swallowed");
    assert_eq!(shell.workspace.focus(), Focus::Center, "but not acted on");
    // Clicking outside the prompt cancels it.
    frame(&mut shell, 100, 24);
    shell.handle_mouse(&MouseEvent::new(MouseKind::Press(MouseButton::Left), 0, 0));
    assert!(shell.modal.is_none());
    assert_eq!(shell.pane.len(), 1);
}

#[test]
fn the_terminal_fills_the_link_picker_and_hears_the_choice() {
    let (mut shell, log) = shell();
    shell.set_panel(PanelId::Terminal, Box::new(LinkPanel { log: log.clone(), links: vec!["src/main.rs:12", "README.md", "src/lib.rs:3"] }));
    act(&mut shell, "terminal::OpenLinks");
    let text = frame(&mut shell, 120, 24);
    assert!(text.contains("Open a path or link from the terminal"), "{text}");
    assert!(text.contains("README.md") && text.contains("src/lib.rs:3"));
    // The shell filters what the view gave it; the view hears the row it gave, not the
    // position in the filtered list.
    shell.handle_paste("lib");
    assert!(log.borrow().contains(&"preview Some(2)".to_string()), "{:?}", log.borrow());
    act(&mut shell, "menu::SecondaryConfirm");
    assert!(shell.modal.is_none());
    assert!(log.borrow().contains(&"confirm 2 true".to_string()), "{:?}", log.borrow());
    assert_eq!(names(&shell), ["lib.rs:3"]);
    // Moving through the rows previews each; escape tells the view nothing was chosen.
    log.borrow_mut().clear();
    act(&mut shell, "terminal::OpenLinks");
    act(&mut shell, "menu::SelectNext");
    act(&mut shell, "menu::Cancel");
    assert_eq!(*log.borrow(), ["preview Some(0)", "preview Some(1)", "preview None"]);
}

#[test]
fn a_picker_with_nobody_to_fill_it_says_why_instead() {
    let (mut shell, _) = shell();
    act(&mut shell, "outline::Toggle");
    assert!(shell.modal.is_none());
    assert_eq!(shell.status_message(), Some(("No file open to list the symbols of", false)));
    shell.open_file(Path::new("/project/a.rs"), None, None, false, true);
    act(&mut shell, "outline::Toggle");
    assert!(shell.modal.is_none());
    assert_eq!(shell.status_message(), Some(("This tab has no outline", false)));
    act(&mut shell, "terminal::OpenLinks");
    assert_eq!(shell.status_message(), Some(("The terminal is not running", false)), "the stand-in terminal has no rows");
}

#[test]
fn file_changes_reach_every_panel_and_tab() {
    let (mut shell, logged) = shell();
    shell.open_file(Path::new("/project/a.rs"), None, None, false, true);
    shell.paths_changed(&FsChange::of(vec!["/project/a.rs".into()]));
    let log = logged.borrow();
    for panel in ["Terminal", "GitPanel", "ProjectPanel"] {
        assert!(log.contains(&format!("{panel} paths /project/a.rs")), "{panel}: {log:?}");
    }
    let before = log.len();
    drop(log);
    assert!(fake(&shell, 0).seen.contains(&"reload".to_string()), "the open tab heard of its file");
    // Nothing is sent for an empty change.
    shell.paths_changed(&FsChange::default());
    assert_eq!(logged.borrow().len(), before);
}

#[test]
fn a_drag_stays_with_the_view_it_started_in() {
    let (mut shell, log) = shell();
    shell.open_file(Path::new("/project/a.rs"), None, None, false, true);
    act(&mut shell, "terminal_panel::Toggle");
    frame(&mut shell, 200, 30);
    let terminal = shell.layout().panel(PanelId::Terminal).unwrap();
    let center = shell.layout().center.unwrap();
    let at = |kind: MouseKind, col: u16| MouseEvent::new(kind, col, 5);
    shell.handle_mouse(&at(MouseKind::Press(MouseButton::Left), terminal.x + 2));
    // Dragged out over the editor and let go there: still the terminal's.
    shell.handle_mouse(&at(MouseKind::Drag(MouseButton::Left), center.x + 10));
    shell.handle_mouse(&at(MouseKind::Release(MouseButton::Left), center.x + 10));
    let terminal_events: Vec<String> = log.borrow().iter().filter(|l| l.starts_with("Terminal mouse")).cloned().collect();
    assert_eq!(terminal_events, ["Terminal mouse Press(Left)", "Terminal mouse Drag(Left)", "Terminal mouse Release(Left)"]);
    // After the release, the next event goes by position again: the editor's, not the terminal's.
    shell.handle_mouse(&at(MouseKind::ScrollDown, center.x + 10));
    assert!(!log.borrow().iter().any(|l| l == "Terminal mouse ScrollDown"), "{:?}", log.borrow());
    // And a press in the editor is not captured by the terminal either.
    shell.handle_mouse(&at(MouseKind::Press(MouseButton::Left), center.x + 10));
    shell.handle_mouse(&at(MouseKind::Drag(MouseButton::Left), terminal.x + 2));
    assert_eq!(log.borrow().iter().filter(|l| l.starts_with("Terminal mouse")).count(), 3);
}

#[test]
fn opening_another_project_asks_about_unsaved_files_first() {
    let (mut shell, _) = shell();
    shell.dispatch(&Action::with_args("bakedit::OpenProject", json!({ "path": "/elsewhere" })));
    assert_eq!(shell.requests, vec![Request::OpenProject("/elsewhere".into())], "nothing unsaved: at once");
    let (mut shell, _) = shell_with_dirty();
    shell.dispatch(&Action::with_args("bakedit::OpenProject", json!({ "path": "/elsewhere" })));
    assert!(shell.requests.is_empty());
    let text = frame(&mut shell, 100, 24);
    assert!(text.contains("1 file has unsaved changes") && text.contains("Save before opening elsewhere?"), "{text}");
    // "Save All and Open" saves, then asks the app to switch.
    act(&mut shell, "menu::SelectNext");
    act(&mut shell, "menu::Confirm");
    assert_eq!(fake(&shell, 0).saves, 1);
    assert_eq!(shell.requests, vec![Request::OpenProject("/elsewhere".into())]);
}

#[test]
fn a_failed_git_job_shows_in_the_status_bar() {
    let (mut shell, _) = shell();
    let _task = shell.project.git_job("Commit", |_| Ok(String::new()));
    assert!(shell.poll_project());
    assert_eq!(shell.status_message(), Some(("Commit failed: this folder is not in a git repository", true)));
    assert!(frame(&mut shell, 120, 12).contains("Commit failed: this folder is not in a git repository"));
    assert!(!shell.poll_project(), "nothing more to collect");
}

/// A source whose answer changes while the picker is open, like a file index still walking.
struct GrowingSource {
    files: Rc<RefCell<Vec<&'static str>>>,
}

impl PickerSource for GrowingSource {
    fn rows(&mut self, _: PickerKind, query: &str, _: &mut Cx<'_>) -> Vec<PickerItem> {
        self.files.borrow().iter().filter(|f| f.contains(query)).map(|f| PickerItem::new(*f)).collect()
    }
    fn confirm(&mut self, _: PickerKind, _: Option<usize>, _: &str, _: bool, _: &mut Cx<'_>) {}
    fn version(&mut self, _: PickerKind, _: &mut Cx<'_>) -> u64 {
        self.files.borrow().len() as u64
    }
}

#[test]
fn an_open_finder_fills_in_as_the_index_grows_and_keeps_the_selection() {
    let files: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(vec!["a.rs", "b.rs"]));
    let mut shell = Shell::new(
        Settings::default(),
        Theme::builtin(),
        Project::detached("/project"),
        Clipboard::with_backend(Box::new(MemoryBackend::default())),
        Box::new(Factory),
        Box::new(GrowingSource { files: files.clone() }),
    );
    act(&mut shell, "file_finder::Toggle");
    act(&mut shell, "menu::SelectNext");
    assert!(!shell.tick(), "nothing new yet");
    files.borrow_mut().push("c.rs");
    assert!(shell.tick(), "the source moved: the picker is refilled");
    let Some(Modal::Picker { picker, .. }) = &shell.modal else { panic!("the finder is open") };
    assert_eq!(picker.items().len(), 3);
    assert_eq!(picker.selected(), Some(1), "the selection stayed on b.rs");
}

#[test]
fn pages_open_once_and_are_brought_back() {
    let (mut shell, _) = shell();
    act(&mut shell, "zed::OpenDefaultKeymap");
    act(&mut shell, "zed::OpenDefaultKeymap");
    assert_eq!(names(&shell), ["Default Keymap"]);
    assert_eq!(fake(&shell, 0).kind, "page");
    assert_eq!(shell.workspace.focus(), Focus::Center);
}

#[test]
fn a_status_message_fades_but_an_error_stays() {
    let (mut shell, _) = shell();
    shell.set_status("Saved a.rs", false);
    let deadline = shell.message_deadline().expect("an ordinary message has a deadline");
    assert!(deadline > std::time::Instant::now());
    shell.set_status("Could not save", true);
    assert_eq!(shell.message_deadline(), None, "errors stay until the next message");
}

#[test]
fn the_empty_centre_says_what_to_press_at_any_width() {
    let (mut shell, _) = shell();
    let text = frame(&mut shell, 120, 30);
    assert!(text.contains("No file open") && text.contains("cmd-shift-p   all commands"), "{text}");
    // A terminal without cmd keys: the ctrl-] prefix spellings instead.
    shell.cmd_keys = false;
    let text = frame(&mut shell, 120, 30);
    assert!(text.contains("ctrl-] p         find a file"), "{text}");
    assert!(!text.contains("cmd-p"));
    // Squeezed between two docks: the heading and the first key, cut cleanly.
    shell.cmd_keys = true;
    act(&mut shell, "terminal_panel::Toggle");
    act(&mut shell, "project_panel::ToggleFocus");
    let text = frame(&mut shell, 150, 30);
    let center = shell.layout().center.unwrap();
    assert!(center.width < 34, "the centre is narrow here: {center:?}");
    assert!(text.contains("No file open"), "{text}");
    assert!(text.lines().any(|line| line.contains("cmd-p find")), "{text}");
}

#[test]
fn opening_the_settings_or_keymap_starts_the_file_when_there_is_none() {
    let config_home = tempfile::tempdir().unwrap();
    std::fs::create_dir(config_home.path().join("zed")).unwrap();
    std::fs::write(config_home.path().join("zed/settings.json"), "{ \"vim_mode\": true }\n").unwrap();
    let (mut shell, _) = shell();
    shell.config_dir = config_home.path().join("bakedit");
    act(&mut shell, "zed::OpenSettingsFile");
    let settings = std::fs::read_to_string(shell.config_dir.join("settings.json")).unwrap();
    assert!(settings.ends_with("{ \"vim_mode\": true }\n"), "a copy of Zed's, which was read until now: {settings}");
    assert!(crate::settings::Settings::parse(&settings, &mut Vec::new()).vim_mode, "the copy reads the same");
    let opened = shell.pane.active().and_then(|item| item.path()).map(Path::to_path_buf);
    assert_eq!(opened, Some(shell.config_dir.join("settings.json")));
    act(&mut shell, "zed::OpenKeymap");
    let keymap = std::fs::read_to_string(shell.config_dir.join("keymap.json")).unwrap();
    let mut loaded = bakedit_keymap::Keymap::new();
    assert!(loaded.load(&keymap, bakedit_keymap::KeymapSource::User).is_empty(), "{keymap}");
    assert!(loaded.is_empty());
    // An existing file is opened as it is.
    std::fs::write(shell.config_dir.join("keymap.json"), "[]").unwrap();
    act(&mut shell, "zed::OpenKeymap");
    assert_eq!(std::fs::read_to_string(shell.config_dir.join("keymap.json")).unwrap(), "[]");
}

#[test]
fn tab_completes_a_finder_row_and_alt_enter_takes_the_query_as_typed() {
    let (mut shell, _) = shell();
    act(&mut shell, "file_finder::Toggle");
    shell.handle_paste("lib");
    assert!(act(&mut shell, "picker::ConfirmCompletion"));
    let Some(Modal::Picker { picker, .. }) = &shell.modal else { panic!("the finder stays open") };
    assert_eq!(picker.query(), "src/lib.rs", "tab puts the whole path in the query");
    // alt-enter: the query as typed, not the row.
    shell.handle_paste("x");
    let confirm_input = Action::with_args("picker::ConfirmInput", json!({ "secondary": false }));
    assert!(shell.dispatch(&confirm_input));
    assert!(shell.modal.is_none());
    assert_eq!(shell.status_message(), Some(("confirmed the query \"src/lib.rsx\"", false)));
    // Lists that only choose among their rows leave tab to the next binding (move down).
    act(&mut shell, "command_palette::Toggle");
    assert!(!act(&mut shell, "picker::ConfirmCompletion"));
    assert!(!shell.dispatch(&confirm_input));
}

#[test]
fn cmd_shift_i_in_the_finder_lists_ignored_files_too() {
    let dir = tempfile::tempdir().unwrap();
    crate::project::test_support::git(dir.path(), &["init", "--quiet"]);
    std::fs::write(dir.path().join(".gitignore"), "build/\n").unwrap();
    std::fs::create_dir(dir.path().join("build")).unwrap();
    std::fs::write(dir.path().join("build/out.txt"), "x").unwrap();
    std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    let mut project = Project::open(dir.path(), crate::background::Waker::detached());
    project.settle(std::time::Duration::from_secs(20));
    let state = tempfile::tempdir().unwrap();
    let mut shell = Shell::new(
        Settings::default(),
        Theme::builtin(),
        project,
        Clipboard::with_backend(Box::new(MemoryBackend::default())),
        Box::new(Factory),
        Box::new(crate::factory::AppSource::new(state.path().to_path_buf())),
    );
    let finder_labels = |shell: &Shell| -> Vec<String> {
        let Some(Modal::Picker { picker, .. }) = &shell.modal else { panic!("the finder is open") };
        picker.items().iter().map(|item| item.label.clone()).collect()
    };
    act(&mut shell, "file_finder::Toggle");
    shell.handle_paste("out");
    assert!(finder_labels(&shell).is_empty(), "ignored files are left out at first");
    assert!(act(&mut shell, "search::ToggleIncludeIgnored"));
    let Some(Modal::Picker { picker, .. }) = &shell.modal else { panic!("the finder stays open") };
    assert_eq!(picker.placeholder, "Search project files, ignored ones too");
    let started = std::time::Instant::now();
    while finder_labels(&shell).is_empty() && started.elapsed() < std::time::Duration::from_secs(20) {
        shell.tick();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(finder_labels(&shell), ["out.txt"], "the open finder refills once the index has walked again");
    assert!(act(&mut shell, "search::ToggleIncludeIgnored"));
    assert_eq!(shell.status_message(), Some(("The finder leaves out ignored files", false)));
}
