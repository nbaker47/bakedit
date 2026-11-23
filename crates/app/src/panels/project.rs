//! The file tree in the right dock (Zed's project panel).
//!
//! # What the app calls (these signatures stay)
//!
//! * `ProjectPanel::new(cx: &mut Cx<'_>) -> ProjectPanel`: the tree of `cx.root` with the
//!   root open.
//! * `ProjectPanel::expanded(&self) -> Vec<PathBuf>` and
//!   `ProjectPanel::restore_expanded(&mut self, paths: &[PathBuf], cx: &mut Cx<'_>)`: the
//!   open folders. The session file reaches them through `View::session_state` and
//!   `View::restore_session_state` (`{"expanded": [paths]}`).
//!
//! Everything else goes through `View`: `menu::Select*` and the `project_panel::*` actions
//! of the keymap (`RevealPath {"path"}` is what the shell sends for "reveal in the tree"),
//! clicks and the wheel, and `paths_changed` from the file watcher. When the front tab
//! changes and `project_panel.auto_reveal_entries` is on, its file is selected.
//!
//! What it does now: browse, open (a preview tab on space and on a click, a kept tab on
//! enter), expand and collapse, reveal, copy paths, with git colours on names. Creating,
//! renaming, moving and deleting files are for the panel's next owner.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bakedit_core::{Action, KeyEvent, MouseButton, MouseEvent, MouseKind, Rgb, Theme};
use bakedit_git::{DisplayStatus, Status};
use bakedit_project::{FileTree, Row};
use ratatui::layout::Rect;

use crate::icons::file_icon;
use crate::ui::{Canvas, Style};
use crate::view::{Context, Cursor, Cx, Effect, FsChange, RenderCx, View};

/// Two clicks on one row within this time open it for good.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// Columns each level of the tree is indented by.
const INDENT: u16 = 2;
/// Rows a wheel step scrolls.
const WHEEL_ROWS: usize = 3;

/// The file tree.
pub struct ProjectPanel {
    tree: FileTree,
    /// First row shown.
    scroll: usize,
    /// Rows shown at the last draw.
    height: usize,
    area: Rect,
    /// Keep the selection in view at the next draw (centred when true).
    reveal: Option<bool>,
    /// The file of the front tab last revealed.
    last_active: Option<PathBuf>,
    last_click: Option<(usize, Instant)>,
    /// Git colours by path, worked out when the status changes.
    colors: GitColors,
}

#[derive(Default)]
struct GitColors {
    generation: Option<u64>,
    /// Files with a status, by absolute path.
    files: std::collections::HashMap<PathBuf, DisplayStatus>,
    /// Folders that hold a changed file somewhere below.
    dirs: HashSet<PathBuf>,
}

impl GitColors {
    fn rebuild(&mut self, project: &crate::project::Project) {
        if self.generation == Some(project.git_generation()) {
            return;
        }
        self.generation = Some(project.git_generation());
        self.files.clear();
        self.dirs.clear();
        let Some(status) = project.status().map(Arc::clone) else { return };
        for entry in &status_entries(&status) {
            let Some(path) = project.abs_path(&entry.0) else { continue };
            for dir in path.ancestors().skip(1) {
                if !self.dirs.insert(dir.to_path_buf()) {
                    break;
                }
            }
            self.files.insert(path, entry.1);
        }
    }

    fn color(&self, row: &Row, theme: &Theme) -> Rgb {
        let status = self.files.get(&row.path).copied();
        match status {
            Some(DisplayStatus::Added | DisplayStatus::Untracked) => theme.created,
            Some(DisplayStatus::Modified | DisplayStatus::Renamed) => theme.modified,
            Some(DisplayStatus::Deleted) => theme.deleted,
            Some(DisplayStatus::Conflict) => theme.conflict,
            None if row.is_dir() && self.dirs.contains(&row.path) => theme.modified,
            None if row.ignored => theme.ignored,
            None => theme.text,
        }
    }
}

fn status_entries(status: &Status) -> Vec<(PathBuf, DisplayStatus)> {
    status.entries.iter().map(|entry| (entry.path.clone(), entry.display_status())).collect()
}

impl ProjectPanel {
    /// The tree of the project directory, with the root open and nothing selected.
    pub fn new(cx: &mut Cx<'_>) -> ProjectPanel {
        ProjectPanel {
            tree: FileTree::new(cx.root),
            scroll: 0,
            height: 0,
            area: Rect::default(),
            reveal: None,
            last_active: None,
            last_click: None,
            colors: GitColors::default(),
        }
    }

    /// The folders that are open, for the session file.
    pub fn expanded(&self) -> Vec<PathBuf> {
        self.tree.expanded_paths()
    }

    /// Open these folders again (from the session file). Ones that are gone are skipped.
    pub fn restore_expanded(&mut self, paths: &[PathBuf], cx: &mut Cx<'_>) {
        for path in paths {
            self.tree.expand(path);
        }
        self.watch_ignored(cx);
    }

    /// An ignored folder open in the tree (target/) is watched too, or it goes stale.
    fn watch_ignored(&self, cx: &Cx<'_>) {
        cx.project.watch_ignored_dirs(&self.tree.expanded_ignored_dirs());
    }

    fn selected_row(&self) -> Option<&Row> {
        self.tree.selected_row()
    }

    /// Open the selected entry: a folder opens or closes, a file opens in the centre.
    fn open_selected(&mut self, preview: bool, cx: &mut Cx<'_>) -> bool {
        let Some(row) = self.selected_row().cloned() else { return false };
        if row.is_dir() {
            self.tree.toggle_selected();
            self.watch_ignored(cx);
        } else {
            cx.push(Effect::OpenFile { path: row.path, line: None, column: None, preview, focus: !preview });
        }
        true
    }

    fn row_at(&self, row: u16) -> Option<usize> {
        let index = self.scroll + row.checked_sub(self.area.y)? as usize;
        (index < self.tree.rows().len()).then_some(index)
    }
}

impl View for ProjectPanel {
    fn render(&mut self, canvas: &mut Canvas<'_>, area: Rect, cx: &RenderCx<'_>) -> Option<Cursor> {
        let theme = cx.theme;
        let bg = theme.panel_bg;
        canvas.fill(area, bg);
        self.area = area;
        self.height = area.height as usize;
        if area.width == 0 || area.height == 0 {
            return None;
        }
        self.colors.rebuild(cx.project);
        let rows = self.tree.rows();
        // Keep the selection in view.
        if let (Some(centre), Some(selected)) = (self.reveal.take(), self.tree.selected()) {
            if centre {
                self.scroll = selected.saturating_sub(self.height / 2);
            } else if selected < self.scroll {
                self.scroll = selected;
            } else if selected >= self.scroll + self.height {
                self.scroll = selected + 1 - self.height;
            }
        }
        self.scroll = self.scroll.min(rows.len().saturating_sub(self.height.max(1)));

        for (offset, row) in rows.iter().enumerate().skip(self.scroll).take(self.height) {
            let y = area.y + (offset - self.scroll) as u16;
            let selected = self.tree.selected() == Some(offset);
            let row_bg = if selected { theme.element_selected } else { bg };
            if selected {
                canvas.fill(Rect::new(area.x, y, area.width, 1), row_bg);
            }
            let depth = row.depth.saturating_sub(1) as u16;
            let mut x = area.x + 1 + depth * INDENT;
            if x >= area.right() {
                continue;
            }
            let (glyph, icon_color) = file_icon(cx.settings, &row.name, row.is_dir(), row.is_expanded());
            let icon_color = if row.ignored { icon_color.lerp(bg, 0.45) } else { icon_color };
            if !row.is_root {
                let used = canvas.text(x, y, &glyph, Style::colors(icon_color, row_bg), area.right().saturating_sub(x));
                x += used + 1;
            }
            let mut style = Style::colors(self.colors.color(row, theme), row_bg);
            if row.is_root {
                style = Style::colors(theme.text, row_bg).bold();
            }
            let room = area.right().saturating_sub(x + 1);
            canvas.text_truncated(x, y, &row.name, style, room);
        }
        if rows.len() == 1 && self.height > 2 {
            let message = "This folder is empty";
            canvas.text_truncated(area.x + 1 + INDENT, area.y + 1, message, Style::colors(theme.text_muted, bg), area.width.saturating_sub(2 + INDENT));
        }
        None
    }

    fn contexts(&self, out: &mut Vec<Context>) {
        out.push(Context::new("ProjectPanel").flag("not_editing"));
    }

    fn handle_action(&mut self, action: &Action, cx: &mut Cx<'_>) -> bool {
        let page = self.height.max(1);
        match action.name.as_str() {
            "menu::SelectNext" => self.tree.select_next(),
            "menu::SelectPrevious" => self.tree.select_prev(),
            "menu::SelectFirst" => self.tree.select_first(),
            "menu::SelectLast" => self.tree.select_last(),
            "project_panel::SelectNextPage" => self.tree.select_page_down(page),
            "project_panel::SelectPreviousPage" => self.tree.select_page_up(page),
            "project_panel::ExpandSelectedEntry" => {
                self.tree.expand_selected();
                self.watch_ignored(cx);
            }
            "project_panel::CollapseSelectedEntry" => {
                self.tree.collapse_selected();
                self.watch_ignored(cx);
            }
            "project_panel::CollapseAllEntries" => {
                self.tree.collapse_all();
                self.watch_ignored(cx);
            }
            "project_panel::ExpandAllEntries" => {
                self.tree.expand_all_selected();
                self.watch_ignored(cx);
            }
            "project_panel::Open" => return self.open_selected(true, cx),
            "project_panel::OpenPermanent" => return self.open_selected(false, cx),
            "project_panel::RevealPath" => {
                let Some(path) = action.arg_str("path").map(PathBuf::from) else { return false };
                if self.tree.reveal(&path).is_some() {
                    self.watch_ignored(cx);
                    self.reveal = Some(true);
                }
            }
            "project_panel::ScrollCursorCenter" => self.reveal = Some(true),
            "workspace::CopyPath" | "workspace::CopyRelativePath" => {
                let Some(path) = self.selected_row().map(|row| row.path.clone()) else { return false };
                let shown = if action.name == "workspace::CopyRelativePath" {
                    path.strip_prefix(cx.root).unwrap_or(&path).to_path_buf()
                } else {
                    path
                };
                let text = shown.to_string_lossy().into_owned();
                cx.clipboard.set(&text, false);
                cx.status(format!("Copied {text}"));
                return true;
            }
            "project_panel::RevealInFileManager" => {
                let Some(path) = self.selected_row().map(|row| row.path.to_string_lossy().into_owned()) else { return false };
                cx.push(Effect::OpenExternally(path));
                return true;
            }
            _ => return false,
        }
        if self.reveal.is_none() {
            self.reveal = Some(false);
        }
        true
    }

    fn handle_key(&mut self, _key: &KeyEvent, _cx: &mut Cx<'_>) -> bool {
        false
    }

    fn handle_mouse(&mut self, event: &MouseEvent, _area: Rect, cx: &mut Cx<'_>) -> bool {
        match event.kind {
            MouseKind::ScrollDown => {
                let last = self.tree.rows().len().saturating_sub(self.height.max(1));
                self.scroll = (self.scroll + WHEEL_ROWS).min(last);
            }
            MouseKind::ScrollUp => self.scroll = self.scroll.saturating_sub(WHEEL_ROWS),
            MouseKind::Press(MouseButton::Left) => {
                let Some(index) = self.row_at(event.row) else { return true };
                self.tree.select(index);
                let double = self.last_click.is_some_and(|(row, at)| row == index && at.elapsed() < DOUBLE_CLICK);
                self.last_click = Some((index, Instant::now()));
                // A click opens a folder or previews a file; a second click keeps the tab.
                return self.open_selected(!double, cx);
            }
            _ => return false,
        }
        true
    }

    fn tick(&mut self, cx: &mut Cx<'_>) -> bool {
        let mut dirty = self.colors.generation != Some(cx.project.git_generation());
        if cx.settings.project_panel.auto_reveal_entries {
            let active = cx.active_path.map(Path::to_path_buf);
            if active != self.last_active {
                if let Some(path) = &active {
                    if self.tree.reveal(path).is_some() {
                        self.watch_ignored(cx);
                        self.reveal = Some(false);
                        dirty = true;
                    }
                }
                self.last_active = active;
            }
        }
        dirty
    }

    fn paths_changed(&mut self, change: &FsChange, cx: &mut Cx<'_>) {
        if change.rescan {
            self.tree.refresh_all();
        } else {
            self.tree.refresh(&change.paths);
        }
        self.watch_ignored(cx);
    }

    fn session_state(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({ "expanded": self.expanded() }))
    }

    fn restore_session_state(&mut self, state: &serde_json::Value, cx: &mut Cx<'_>) {
        let paths: Vec<PathBuf> = state
            .get("expanded")
            .and_then(|v| v.as_array())
            .map(|list| list.iter().filter_map(|p| p.as_str()).map(PathBuf::from).collect())
            .unwrap_or_default();
        self.restore_expanded(&paths, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::test_support::Harness;
    use serde_json::json;

    fn project_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/nested")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.path().join("src/nested/deep.rs"), "").unwrap();
        std::fs::write(dir.path().join("README.md"), "# hi\n").unwrap();
        dir
    }

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }

    fn act(panel: &mut ProjectPanel, harness: &mut Harness, name: &str) -> bool {
        panel.handle_action(&Action::new(name), &mut harness.cx())
    }

    #[test]
    fn shows_the_root_and_its_entries_with_icons_and_a_gap() {
        let dir = project_dir();
        let mut harness = Harness::new(dir.path());
        harness.settings.icons = crate::settings::Icons::Ascii;
        let mut panel = ProjectPanel::new(&mut harness.cx());
        let (text, cursor, _) = harness.draw(&mut panel, 30, 6);
        let rows = lines(&text);
        let root = dir.path().file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(rows[0].trim(), root);
        assert_eq!(rows[1], " ▸ src", "folders first, the icon then one space");
        assert_eq!(rows[2], " · README.md");
        assert_eq!(cursor, None);
    }

    #[test]
    fn keys_move_open_and_reveal() {
        let dir = project_dir();
        let mut harness = Harness::new(dir.path());
        harness.settings.icons = crate::settings::Icons::Ascii;
        let mut panel = ProjectPanel::new(&mut harness.cx());
        act(&mut panel, &mut harness, "menu::SelectNext"); // the root
        act(&mut panel, &mut harness, "menu::SelectNext"); // src
        assert!(act(&mut panel, &mut harness, "project_panel::Open"), "a folder opens");
        let rows = lines(&harness.draw(&mut panel, 30, 8).0);
        assert_eq!(rows[1], " ▾ src");
        assert_eq!(rows[2], "   ▸ nested", "children are indented two cells");
        assert_eq!(rows[3], "   · main.rs");
        act(&mut panel, &mut harness, "menu::SelectNext");
        act(&mut panel, &mut harness, "menu::SelectNext");
        assert!(act(&mut panel, &mut harness, "project_panel::Open"));
        let main = dir.path().join("src/main.rs");
        assert_eq!(
            harness.take_effects(),
            vec![Effect::OpenFile { path: main.clone(), line: None, column: None, preview: true, focus: false }],
            "space previews and leaves the focus in the tree"
        );
        act(&mut panel, &mut harness, "project_panel::OpenPermanent");
        assert!(matches!(&harness.take_effects()[..], [Effect::OpenFile { preview: false, focus: true, .. }]));
        // Reveal a path deep in a closed folder.
        let deep = dir.path().join("src/nested/deep.rs");
        act(&mut panel, &mut harness, "project_panel::CollapseAllEntries");
        let reveal = Action::with_args("project_panel::RevealPath", json!({ "path": deep.to_string_lossy() }));
        assert!(panel.handle_action(&reveal, &mut harness.cx()));
        let rows = lines(&harness.draw(&mut panel, 30, 8).0);
        assert!(rows.iter().any(|r| r == "     · deep.rs"), "{rows:?}");
        assert_eq!(panel.expanded().len(), 3, "the root, src and nested are open");
        // Copy the selected path.
        assert!(act(&mut panel, &mut harness, "workspace::CopyRelativePath"));
        assert_eq!(harness.clipboard.get().unwrap().text, "src/nested/deep.rs");
        assert!(!act(&mut panel, &mut harness, "editor::MoveUp"));
    }

    #[test]
    fn the_selected_row_is_highlighted_across_the_whole_width() {
        let dir = project_dir();
        let mut harness = Harness::new(dir.path());
        let mut panel = ProjectPanel::new(&mut harness.cx());
        act(&mut panel, &mut harness, "menu::SelectNext");
        act(&mut panel, &mut harness, "menu::SelectNext");
        let (_, _, buf) = harness.draw(&mut panel, 30, 6);
        let selected = crate::ui::color(harness.theme.element_selected);
        assert_eq!(buf[(0, 1)].bg, selected);
        assert_eq!(buf[(29, 1)].bg, selected);
        assert_ne!(buf[(0, 2)].bg, selected);
    }

    #[test]
    fn clicks_select_open_and_a_second_click_keeps_the_tab() {
        let dir = project_dir();
        let mut harness = Harness::new(dir.path());
        let mut panel = ProjectPanel::new(&mut harness.cx());
        harness.draw(&mut panel, 30, 6);
        let click = MouseEvent::new(MouseKind::Press(MouseButton::Left), 4, 2);
        panel.handle_mouse(&click, Rect::new(0, 0, 30, 6), &mut harness.cx());
        assert!(matches!(&harness.take_effects()[..], [Effect::OpenFile { preview: true, .. }]));
        panel.handle_mouse(&click, Rect::new(0, 0, 30, 6), &mut harness.cx());
        assert!(matches!(&harness.take_effects()[..], [Effect::OpenFile { preview: false, .. }]));
    }

    #[test]
    fn new_files_appear_and_the_front_tab_is_revealed() {
        let dir = project_dir();
        let mut harness = Harness::new(dir.path());
        harness.settings.icons = crate::settings::Icons::Ascii;
        let mut panel = ProjectPanel::new(&mut harness.cx());
        std::fs::write(dir.path().join("added.txt"), "").unwrap();
        panel.paths_changed(&FsChange::of(vec![dir.path().join("added.txt")]), &mut harness.cx());
        assert!(harness.draw(&mut panel, 30, 8).0.contains("added.txt"));
        // The front tab's file is selected without moving the focus.
        let deep = dir.path().join("src/nested/deep.rs");
        let mut cx = harness.cx();
        cx.active_path = Some(&deep);
        assert!(panel.tick(&mut cx));
        let text = harness.draw(&mut panel, 30, 8).0;
        assert!(text.contains("deep.rs"), "{text}");
        assert!(harness.take_effects().is_empty());
    }

    #[test]
    fn an_empty_folder_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let mut harness = Harness::new(dir.path());
        let mut panel = ProjectPanel::new(&mut harness.cx());
        assert!(harness.draw(&mut panel, 30, 6).0.contains("This folder is empty"));
    }
}
