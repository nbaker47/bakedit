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
//! # What it does
//!
//! * The first row is a pinned header with the project name (the tree's root entry, so it
//!   can be selected, collapsed and used as a paste target); the entries scroll below it.
//! * Browse and open: a preview tab on space and on a click, a kept tab on enter and on a
//!   second click, expand and collapse, reveal, typing letters to jump to an entry.
//! * Change: new file and new folder (the name is typed in a row of its own), rename,
//!   duplicate, trash and delete (asked about first unless `skip_prompt`), cut, copy and
//!   paste. An error from the file system goes to the status bar and leaves the name field
//!   open so it can be corrected.
//! * Paths: copy the absolute or relative path, reveal in the file manager, open with the
//!   system, start a project search inside the folder.
//!
//! Key contexts: `ProjectPanel{not_editing}`; while a name is being typed
//! `ProjectPanel{editing} > Editor{mode=single_line}`.
//!
//! The panel asks the shell for two things the keymap has no name for: `bakedit::RevealPath
//! {"path"}` (show a path in Finder) and `search::FocusSearch {"include": "dir/"}` right
//! after `pane::DeploySearch` (the search view may use the folder to narrow the search).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bakedit_core::{Action, KeyEvent, Mods, MouseButton, MouseEvent, MouseKind, Rgb, Theme};
use bakedit_git::{DiffStatus, DisplayStatus, Status};
use bakedit_project::{FileTree, Row};
use ratatui::layout::Rect;
use serde_json::json;

use crate::background::Task;
use crate::icons::file_icon;
use crate::line_input::{InputOutcome, LineInput};
use crate::project::DiffBase;
use crate::settings::Icons;
use crate::ui::{Canvas, Style};
use crate::view::{Context, Cursor, CursorShape, Cx, Effect, FsChange, Question, RenderCx, View};

/// Two clicks on one row within this time open it for good.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// Typed letters that follow each other within this time are one search.
const TYPE_AHEAD: Duration = Duration::from_millis(900);
/// Columns each level of the tree is indented by.
const INDENT: u16 = 2;
/// Rows a wheel step scrolls.
const WHEEL_ROWS: usize = 3;
/// A name keeps at least this many columns however deep the entry is; the indent gives way.
const MIN_NAME: u16 = 8;

/// What the name field in the tree is for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum EditKind {
    NewFile { dir: PathBuf },
    NewDir { dir: PathBuf },
    Rename { path: PathBuf },
}

/// A name being typed in a row of the tree.
struct Edit {
    kind: EditKind,
    input: LineInput,
}

/// One row of the list under the header, as it is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Line {
    /// The tree's row with this index.
    Entry(usize),
    /// The name field of a new entry, at this depth.
    Input(usize),
}

/// The file tree.
pub struct ProjectPanel {
    tree: FileTree,
    /// First list line shown (the header is not part of the list).
    scroll: usize,
    /// List lines that fit at the last draw.
    height: usize,
    area: Rect,
    /// What the list showed at the last draw, for mouse hits.
    shown: Vec<Line>,
    /// Keep the selection in view at the next draw (centred when true).
    reveal: Option<bool>,
    /// The file of the front tab last revealed.
    last_active: Option<PathBuf>,
    last_click: Option<(usize, Instant)>,
    /// Git colours by path, worked out when the status changes.
    colors: GitColors,
    edit: Option<Edit>,
    /// Letters typed to jump to an entry, and when the last one came.
    typed: Option<(String, Instant)>,
    /// In-tree find bar (`Ctrl+F` / `Cmd+F`).
    find: Option<PanelFindBar>,
}

struct PanelFindBar {
    query: LineInput,
    focused: bool,
    matches: Vec<usize>,
    current: Option<usize>,
}

#[derive(Default)]
struct GitColors {
    generation: Option<u64>,
    diff_base: DiffBase,
    /// Files with a status, by absolute path.
    files: HashMap<PathBuf, DisplayStatus>,
    /// Folders that hold a changed file somewhere below.
    dirs: HashSet<PathBuf>,
    /// Background diff load when `diff_base` is a branch or historical commit.
    diff_task: Option<Task<(u64, DiffBase, Vec<(PathBuf, DisplayStatus)>)>>,
}

impl GitColors {
    fn apply_entries(&mut self, entries: Vec<(PathBuf, DisplayStatus)>) {
        self.files.clear();
        self.dirs.clear();
        for (path, status) in entries {
            for dir in path.ancestors().skip(1) {
                if !self.dirs.insert(dir.to_path_buf()) {
                    break;
                }
            }
            self.files.insert(path, status);
        }
    }

    /// Poll a pending branch/commit diff load. Returns true when new colours arrived.
    fn poll(&mut self) -> bool {
        let Some((generation, base, entries)) = self.diff_task.as_mut().and_then(Task::poll) else {
            if self.diff_task.as_ref().is_some_and(Task::is_finished) {
                self.diff_task = None;
            }
            return false;
        };
        self.diff_task = None;
        if self.generation == Some(generation) && self.diff_base == base {
            self.apply_entries(entries);
            return true;
        }
        false
    }

    /// Work the colours out again when the status or active diff base moved; `enabled` is
    /// the `git_status` setting, and with it off nothing is coloured.
    fn rebuild(&mut self, project: &crate::project::Project, enabled: bool) {
        let wanted = enabled.then(|| project.git_generation());
        let base = project.diff_base().clone();
        if self.generation == wanted && (!enabled || self.diff_base == base) {
            return;
        }
        self.generation = wanted;
        self.diff_base = base.clone();
        self.diff_task = None;
        self.files.clear();
        self.dirs.clear();
        if !enabled {
            return;
        }
        match base {
            DiffBase::Uncommitted => {
                let uncommitted: Vec<(PathBuf, DisplayStatus)> = project
                    .status()
                    .map(|status| {
                        status_entries(status)
                            .into_iter()
                            .filter_map(|(rel, st)| project.abs_path(&rel).map(|abs| (abs, st)))
                            .collect()
                    })
                    .unwrap_or_default();
                self.apply_entries(uncommitted.clone());
                // Also colour files changed on the current branch (`Since <base>`) so
                // clean worktrees with committed branch work still show green/orange/red
                // diff colours in the file tree. Uncommitted status takes priority.
                if let (Some(repo), Some(top)) = (project.repo().cloned(), project.git_top().map(Path::to_path_buf)) {
                    let generation = project.git_generation();
                    self.diff_task = Some(project.spawn(move || {
                        let mut merged: HashMap<PathBuf, DisplayStatus> = HashMap::new();
                        if let Some(base_ref) = repo.default_base_ref() {
                            if let Ok(files) = repo.diff_against_ref(&base_ref) {
                                for file in files {
                                    merged.insert(top.join(&file.path), diff_to_display(file.status));
                                }
                            }
                        }
                        for (path, status) in uncommitted {
                            merged.insert(path, status);
                        }
                        (generation, DiffBase::Uncommitted, merged.into_iter().collect())
                    }));
                }
            }
            DiffBase::Branch(ref_name) => {
                let (Some(repo), Some(top)) = (project.repo().cloned(), project.git_top().map(Path::to_path_buf)) else { return };
                let generation = project.git_generation();
                let target = DiffBase::Branch(ref_name.clone());
                self.diff_task = Some(project.spawn(move || {
                    let entries = repo
                        .diff_against_ref(&ref_name)
                        .unwrap_or_default()
                        .into_iter()
                        .map(|file| (top.join(&file.path), diff_to_display(file.status)))
                        .collect();
                    (generation, target, entries)
                }));
            }
            DiffBase::Commit { sha, label } => {
                let (Some(repo), Some(top)) = (project.repo().cloned(), project.git_top().map(Path::to_path_buf)) else { return };
                let generation = project.git_generation();
                let target = DiffBase::Commit { sha: sha.clone(), label };
                self.diff_task = Some(project.spawn(move || {
                    let entries = repo
                        .diff_commit(&sha)
                        .unwrap_or_default()
                        .into_iter()
                        .map(|file| (top.join(&file.path), diff_to_display(file.status)))
                        .collect();
                    (generation, target, entries)
                }));
            }
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

fn diff_to_display(status: DiffStatus) -> DisplayStatus {
    match status {
        DiffStatus::Added | DiffStatus::Untracked => DisplayStatus::Added,
        DiffStatus::Deleted => DisplayStatus::Deleted,
        DiffStatus::Modified => DisplayStatus::Modified,
        DiffStatus::Renamed | DiffStatus::Copied => DisplayStatus::Renamed,
        DiffStatus::Conflicted => DisplayStatus::Conflict,
    }
}

fn status_entries(status: &Status) -> Vec<(PathBuf, DisplayStatus)> {
    status.entries.iter().map(|entry| (entry.path.clone(), entry.display_status())).collect()
}

/// What the file system said, without the project's own path in front of every name.
fn plain_error(error: &impl std::fmt::Display, root: &Path) -> String {
    let prefix = format!("{}/", root.display());
    error.to_string().replace(&prefix, "")
}

impl ProjectPanel {
    /// The tree of the project directory, with the root open and nothing selected.
    pub fn new(cx: &mut Cx<'_>) -> ProjectPanel {
        ProjectPanel {
            tree: FileTree::new(cx.root),
            scroll: 0,
            height: 0,
            area: Rect::default(),
            shown: Vec::new(),
            reveal: None,
            last_active: None,
            last_click: None,
            colors: GitColors::default(),
            edit: None,
            typed: None,
            find: None,
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

    /// The selected entry, unless it is the root (which cannot be renamed, moved or removed).
    fn selected_entry(&self) -> Option<PathBuf> {
        self.selected_row().filter(|row| !row.is_root).map(|row| row.path.clone())
    }

    /// The list under the header: every entry that is shown, with the name field of a new
    /// entry first among the children of its folder.
    fn lines(&self, hide_ignored: bool) -> Vec<Line> {
        let rows = self.tree.rows();
        let new_in = match self.edit.as_ref().map(|edit| &edit.kind) {
            Some(EditKind::NewFile { dir } | EditKind::NewDir { dir }) => Some(dir.as_path()),
            _ => None,
        };
        let mut lines = Vec::with_capacity(rows.len() + 1);
        if rows.first().is_some_and(|root| new_in == Some(root.path.as_path())) {
            lines.push(Line::Input(1));
        }
        let mut hidden_below: Option<usize> = None;
        for (index, row) in rows.iter().enumerate().skip(1) {
            if let Some(depth) = hidden_below {
                if row.depth > depth {
                    continue;
                }
                hidden_below = None;
            }
            if hide_ignored && row.ignored && self.tree.selected() != Some(index) {
                hidden_below = Some(row.depth);
                continue;
            }
            lines.push(Line::Entry(index));
            if new_in == Some(row.path.as_path()) {
                lines.push(Line::Input(row.depth + 1));
            }
        }
        lines
    }

    fn hides_ignored(cx: &Cx<'_>) -> bool {
        cx.settings.project_panel.hide_gitignore
    }

    /// Select the next or previous entry that is not hidden, or the last one.
    fn step(&mut self, forward: bool, hide: bool) {
        let before = self.tree.selected();
        for _ in 0..self.tree.rows().len() {
            if forward {
                self.tree.select_next();
            } else {
                self.tree.select_prev();
            }
            let now = self.tree.selected();
            let hidden = hide && now.and_then(|i| self.tree.rows().get(i)).is_some_and(|row| row.ignored && !row.is_root);
            if !hidden || now == before {
                break;
            }
        }
    }

    fn select_last_shown(&mut self, hide: bool) {
        self.tree.select_last();
        let hidden = |panel: &ProjectPanel| {
            hide && panel.selected_row().is_some_and(|row| row.ignored && !row.is_root)
        };
        for _ in 0..self.tree.rows().len() {
            if !hidden(self) {
                break;
            }
            self.tree.select_prev();
        }
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
        self.sync_git_repo(cx);
        true
    }

    /// Switch the active git repository when the selected file tree entry belongs to a
    /// different discovered repository.
    fn sync_git_repo(&self, cx: &mut Cx<'_>) {
        let Some(row) = self.selected_row() else { return };
        let Some(repo_root) = cx.project.repo_root_for_path(&row.path) else { return };
        if cx.project.repo().map(|r| r.root()) == Some(repo_root.as_path()) {
            return;
        }
        cx.push(Effect::SelectRepo(repo_root));
    }

    /// The tree row under screen row `y`: 0 is the header, then the list lines.
    fn hit(&self, y: u16) -> Option<Line> {
        let offset = y.checked_sub(self.area.y)? as usize;
        if offset == 0 {
            return (!self.tree.rows().is_empty()).then_some(Line::Entry(0));
        }
        let header_rows = 1 + usize::from(self.find.is_some());
        if offset < header_rows {
            return None;
        }
        self.shown.get(self.scroll + offset - header_rows).copied()
    }

    // ---- find bar (ctrl-f / cmd-f) ------------------------------------------------------

    fn open_find(&mut self) {
        self.edit = None;
        self.typed = None;
        if let Some(bar) = &mut self.find {
            bar.focused = true;
            bar.query.select_all();
        } else {
            self.find = Some(PanelFindBar {
                query: LineInput::new(),
                focused: true,
                matches: Vec::new(),
                current: None,
            });
        }
    }

    fn find_focused(&self) -> bool {
        self.find.as_ref().is_some_and(|bar| bar.focused)
    }

    fn recompute_find_matches(&mut self, hide: bool, jump: bool, cx: &mut Cx<'_>) {
        let Some(bar) = &self.find else { return };
        let raw = bar.query.text();
        if raw.is_empty() {
            if let Some(bar) = &mut self.find {
                bar.matches.clear();
                bar.current = None;
            }
            return;
        }
        let needle = raw.to_lowercase();
        let root = self.tree.rows().first().map(|r| r.path.clone());
        let matches: Vec<usize> = self
            .lines(hide)
            .into_iter()
            .filter_map(|line| match line {
                Line::Entry(index) => {
                    let row = self.tree.rows().get(index)?;
                    let rel = root
                        .as_deref()
                        .and_then(|r| row.path.strip_prefix(r).ok())
                        .map(|p| p.to_string_lossy())
                        .unwrap_or_else(|| row.name.as_str().into());
                    (row.name.to_lowercase().contains(&needle) || rel.to_lowercase().contains(&needle)).then_some(index)
                }
                Line::Input(_) => None,
            })
            .collect();
        let current = if matches.is_empty() {
            None
        } else {
            let selected = self.tree.selected().unwrap_or(0);
            let pos = matches.iter().position(|&idx| idx >= selected).unwrap_or(0);
            if jump {
                self.tree.select(matches[pos]);
                self.reveal = Some(true);
                self.sync_git_repo(cx);
            }
            Some(pos)
        };
        if let Some(bar) = &mut self.find {
            bar.matches = matches;
            bar.current = current;
        }
    }

    fn step_find_match(&mut self, forward: bool, cx: &mut Cx<'_>) {
        let target = {
            let Some(bar) = &mut self.find else { return };
            if bar.matches.is_empty() {
                return;
            }
            let len = bar.matches.len();
            let next = match bar.current {
                Some(i) if forward => (i + 1) % len,
                Some(i) => (i + len - 1) % len,
                None => 0,
            };
            bar.current = Some(next);
            bar.matches[next]
        };
        self.tree.select(target);
        self.reveal = Some(true);
        self.sync_git_repo(cx);
    }

    fn find_input_action(&mut self, action: &Action, hide: bool, cx: &mut Cx<'_>) -> bool {
        let Some(bar) = &mut self.find else { return false };
        match bar.query.handle_action(action) {
            InputOutcome::Ignored => false,
            InputOutcome::Moved => true,
            InputOutcome::Edited => {
                self.recompute_find_matches(hide, true, cx);
                true
            }
            InputOutcome::Copied { text, edited } => {
                cx.clipboard.set(&text, false);
                if edited {
                    self.recompute_find_matches(hide, true, cx);
                }
                true
            }
            InputOutcome::WantsPaste => {
                if let Some(clip) = cx.clipboard.get() {
                    bar.query.insert(&clip.text);
                    self.recompute_find_matches(hide, true, cx);
                }
                true
            }
        }
    }

    fn render_find_bar(&mut self, canvas: &mut Canvas<'_>, area: Rect, cx: &RenderCx<'_>) -> Option<Cursor> {
        let bar = self.find.as_mut()?;
        let theme = cx.theme;
        let bar_bg = theme.surface;
        let field_bg = theme.editor_bg;
        canvas.fill(area, bar_bg);

        let count_text = if bar.query.is_empty() {
            String::new()
        } else if let Some(current) = bar.current {
            format!(" {}/{} ", current + 1, bar.matches.len())
        } else {
            " 0/0 ".to_string()
        };
        let right_w = count_text.len() as u16;
        let field_x = area.x + 1;
        let field_w = area.width.saturating_sub(1 + right_w + 1);
        let mut cursor = None;
        if field_w >= 4 {
            let field = Rect::new(field_x, area.y, field_w, 1);
            canvas.fill(field, field_bg);
            let inner = Rect::new(field.x + 1, field.y, field.width.saturating_sub(2), 1);
            let input_style = Style::colors(theme.text, field_bg);
            let selection_style = Style::colors(theme.text, theme.selection_bg);
            let placeholder = Style::colors(theme.text_placeholder, field_bg);
            let col = bar.query.render(canvas, inner, input_style, selection_style, "Find in tree", placeholder);
            if bar.focused && cx.focused {
                cursor = Some(Cursor { col, row: area.y, shape: CursorShape::Bar });
            }
        }
        if !count_text.is_empty() {
            let color = if bar.matches.is_empty() { theme.deleted } else { theme.text_muted };
            let rx = area.right().saturating_sub(right_w);
            canvas.text(rx, area.y, &count_text, Style::colors(color, bar_bg), right_w);
        }
        cursor
    }

    // ---- typing to find an entry --------------------------------------------------------

    /// Jump to the next entry whose name starts with the letters typed so far. One letter
    /// typed again moves on to the next entry with that initial.
    fn type_ahead(&mut self, text: &str, now: Instant, hide: bool) -> bool {
        let mut typed = match self.typed.take() {
            Some((typed, at)) if now.duration_since(at) < TYPE_AHEAD => typed,
            _ => String::new(),
        };
        typed.push_str(&text.to_lowercase());
        let repeated = typed.chars().count() > 1 && typed.chars().all(|c| typed.starts_with(c));
        let needle: String = if repeated { typed.chars().take(1).collect() } else { typed.clone() };
        let names: Vec<(usize, bool)> = self
            .lines(hide)
            .iter()
            .filter_map(|line| match line {
                Line::Entry(index) => Some(*index),
                Line::Input(_) => None,
            })
            .map(|index| (index, self.tree.rows()[index].name.to_lowercase().starts_with(&needle)))
            .collect();
        let current = self.tree.selected();
        let at = names.iter().position(|(index, _)| Some(*index) == current);
        // With several letters the current entry may still be the match; with a repeated
        // single letter the search starts after it.
        let start = match at {
            Some(at) if typed.chars().count() > 1 && !repeated => at,
            Some(at) => at + 1,
            None => 0,
        };
        let found = (0..names.len()).map(|step| (start + step) % names.len().max(1)).find(|&i| names[i].1);
        self.typed = Some((typed, now));
        match found {
            Some(i) => {
                self.tree.select(names[i].0);
                self.reveal = Some(false);
                true
            }
            None => false,
        }
    }

    // ---- changing files -----------------------------------------------------------------

    fn start_new(&mut self, directory: bool) -> bool {
        let dir = self.tree.creation_dir();
        self.tree.expand(&dir);
        let kind = if directory { EditKind::NewDir { dir } } else { EditKind::NewFile { dir } };
        self.edit = Some(Edit { kind, input: LineInput::new() });
        self.reveal = Some(false);
        true
    }

    fn start_rename(&mut self) -> bool {
        let Some(path) = self.selected_entry() else { return false };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut input = LineInput::with_text(&name);
        // Select the name without its extension, as Zed does, so typing replaces just that.
        let stem_len = match name.rfind('.') {
            Some(dot) if dot > 0 && !path.is_dir() => name[..dot].chars().count(),
            _ => name.chars().count(),
        };
        input.handle_action(&Action::new("editor::MoveToBeginningOfLine"));
        for _ in 0..stem_len {
            input.handle_action(&Action::new("editor::SelectRight"));
        }
        self.edit = Some(Edit { kind: EditKind::Rename { path }, input });
        self.reveal = Some(false);
        true
    }

    /// Do what the name field was opened for. On a refusal the field stays, with the
    /// reason in the status bar.
    fn confirm_edit(&mut self, cx: &mut Cx<'_>) {
        let Some(edit) = self.edit.take() else { return };
        let name = edit.input.text().trim().to_string();
        if name.is_empty() {
            return;
        }
        let result: Result<(PathBuf, Vec<PathBuf>), String> = match &edit.kind {
            EditKind::NewFile { dir } => self
                .tree
                .create_file(dir, &name)
                .map(|path| (path.clone(), vec![path]))
                .map_err(|e| plain_error(&e, cx.root)),
            EditKind::NewDir { dir } => self
                .tree
                .create_dir(dir, &name)
                .map(|path| (path.clone(), vec![path]))
                .map_err(|e| plain_error(&e, cx.root)),
            EditKind::Rename { path } => {
                if path.file_name().is_some_and(|old| old.to_string_lossy() == name) {
                    return;
                }
                self.tree
                    .rename(path, &name)
                    .map(|new| (new.clone(), vec![path.clone(), new]))
                    .map_err(|e| plain_error(&e, cx.root))
            }
        };
        match result {
            Ok((path, changed)) => {
                self.watch_ignored(cx);
                self.reveal = Some(false);
                if let EditKind::Rename { path: from } = &edit.kind {
                    cx.push(Effect::Moved { from: from.clone(), to: path.clone() });
                }
                cx.push(Effect::PathsChanged(changed));
                if matches!(edit.kind, EditKind::NewFile { .. }) && path.is_file() {
                    cx.push(Effect::open(path));
                }
            }
            Err(message) => {
                cx.error(message);
                self.edit = Some(edit);
            }
        }
    }

    /// Keys and actions while a name is being typed. `None` lets the shell have it.
    fn edit_action(&mut self, action: &Action, cx: &mut Cx<'_>) -> Option<bool> {
        let name = match action.name.as_str() {
            "project_panel::Copy" => "editor::Copy",
            "project_panel::Cut" => "editor::Cut",
            "project_panel::Paste" => "editor::Paste",
            other => other,
        };
        match name {
            "menu::Confirm" | "project_panel::Rename" | "project_panel::OpenPermanent" | "project_panel::Open" => {
                self.confirm_edit(cx);
                return Some(true);
            }
            "menu::Cancel" => {
                self.edit = None;
                return Some(true);
            }
            "menu::SelectNext" | "menu::SelectPrevious" | "menu::SelectFirst" | "menu::SelectLast" => return Some(true),
            _ => {}
        }
        if !name.starts_with("editor::") {
            return None;
        }
        let edit = self.edit.as_mut()?;
        match edit.input.handle_action(&Action { name: name.to_string(), ..action.clone() }) {
            InputOutcome::Copied { text, .. } => {
                cx.clipboard.set(&text, false);
            }
            InputOutcome::WantsPaste => {
                if let Some(clip) = cx.clipboard.get() {
                    edit.input.insert(&clip.text);
                }
            }
            InputOutcome::Moved | InputOutcome::Edited | InputOutcome::Ignored => {}
        }
        Some(true)
    }

    /// Trash or delete the entry. Asks first unless the keymap said `skip_prompt`; the
    /// answer comes back as the same action with the path and `skip_prompt` set.
    fn remove(&mut self, action: &Action, permanent: bool, cx: &mut Cx<'_>) -> bool {
        let path = match action.arg_str("path") {
            Some(path) => PathBuf::from(path),
            None => match self.selected_entry() {
                Some(path) => path,
                None => {
                    if self.selected_row().is_some() {
                        cx.error("The project folder cannot be removed from here");
                    }
                    return self.selected_row().is_some();
                }
            },
        };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if action.arg_bool("skip_prompt") != Some(true) {
            let (verb, detail) = if permanent {
                ("Delete", "This cannot be undone.")
            } else {
                ("Trash", "It can be restored from the trash.")
            };
            let name_action = if permanent { "project_panel::Delete" } else { "project_panel::Trash" };
            let confirmed = Action::with_args(name_action, json!({ "skip_prompt": true, "path": path.to_string_lossy() }));
            cx.push(Effect::Ask(Question {
                message: format!("{verb} {name}{}?", if path.is_dir() { " and everything in it" } else { "" }),
                detail: Some(detail.to_string()),
                answers: vec![(verb.to_string(), vec![Effect::Dispatch(confirmed)]), ("Cancel".to_string(), Vec::new())],
            }));
            return true;
        }
        let result = if permanent { self.tree.delete(&path) } else { self.tree.trash(&path) };
        match result {
            Ok(()) => {
                self.watch_ignored(cx);
                self.reveal = Some(false);
                cx.push(Effect::PathsChanged(vec![path]));
                cx.status(format!("{} {name}", if permanent { "Deleted" } else { "Moved to trash:" }));
            }
            Err(error) => cx.error(plain_error(&error, cx.root)),
        }
        true
    }

    fn duplicate(&mut self, cx: &mut Cx<'_>) -> bool {
        let Some(path) = self.selected_entry() else { return false };
        match self.tree.duplicate(&path) {
            Ok(copy) => {
                self.reveal = Some(false);
                cx.push(Effect::PathsChanged(vec![copy]));
            }
            Err(error) => cx.error(plain_error(&error, cx.root)),
        }
        true
    }

    fn cut_or_copy(&mut self, cut: bool, cx: &mut Cx<'_>) -> bool {
        let done = if cut { self.tree.cut_selected() } else { self.tree.copy_selected() };
        if let (true, Some(row)) = (done, self.selected_row()) {
            let rel = row.path.strip_prefix(cx.root).unwrap_or(&row.path).to_string_lossy().into_owned();
            let shown = if rel.is_empty() { ".".to_string() } else { rel };
            if !cut {
                cx.clipboard.set(&shown, false);
            }
            let message = format!("{} {}", if cut { "Cut" } else { "Copied" }, if cut { row.name.clone() } else { shown });
            cx.status(message);
        }
        done
    }

    fn paste(&mut self, cx: &mut Cx<'_>) -> bool {
        let sources = self.tree.clipboard().filter(|clip| clip.cut).map(|clip| clip.paths.clone()).unwrap_or_default();
        match self.tree.paste_at_selection() {
            Ok(pasted) if pasted.is_empty() => cx.status("Nothing to paste: copy or cut an entry first"),
            Ok(mut changed) => {
                // Cut entries were moved, in order: the tabs showing them follow.
                for (from, to) in sources.iter().zip(&changed) {
                    if from != to {
                        cx.push(Effect::Moved { from: from.clone(), to: to.clone() });
                    }
                }
                changed.extend(sources);
                self.watch_ignored(cx);
                self.reveal = Some(false);
                cx.push(Effect::PathsChanged(changed));
            }
            Err(error) => {
                cx.push(Effect::PathsChanged(sources));
                cx.error(plain_error(&error, cx.root));
            }
        }
        true
    }

    fn search_in_directory(&mut self, cx: &mut Cx<'_>) -> bool {
        if self.selected_row().is_none() {
            return false;
        }
        let dir = self.tree.creation_dir();
        let relative = dir.strip_prefix(cx.root).unwrap_or(&dir).to_string_lossy().into_owned();
        cx.push(Effect::Dispatch(Action::new("pane::DeploySearch")));
        if !relative.is_empty() {
            cx.push(Effect::Dispatch(Action::with_args("search::FocusSearch", json!({ "include": format!("{relative}/") }))));
        }
        true
    }

    fn copy_path(&mut self, relative: bool, cx: &mut Cx<'_>) -> bool {
        let Some(path) = self.selected_row().map(|row| row.path.clone()) else { return false };
        let shown = if relative { path.strip_prefix(cx.root).unwrap_or(&path).to_path_buf() } else { path };
        let mut text = shown.to_string_lossy().into_owned();
        if text.is_empty() {
            text = ".".to_string();
        }
        cx.clipboard.set(&text, false);
        cx.status(format!("Copied {text}"));
        true
    }
}

/// Columns the marker takes before the name: with Nerd icons a chevron column and the
/// icon, with ascii icons just the icon (which is itself a chevron for folders).
fn marker_width(icons: Icons, glyph_width: u16) -> u16 {
    match icons {
        Icons::Ascii => glyph_width + 1,
        Icons::Nerd => 2 + glyph_width + 1,
    }
}

/// What one line of the list looks like.
struct LineLook<'a> {
    depth: usize,
    glyph: &'a str,
    glyph_color: Rgb,
    is_dir: bool,
    expanded: bool,
    bg: Rgb,
}

/// Draw one line: indent guides, marker, then the name or the name field. Returns the
/// cursor column when it drew a field.
fn draw_line(canvas: &mut Canvas<'_>, area: Rect, y: u16, look: &LineLook<'_>, name: NameDraw<'_>, cx: &RenderCx<'_>) -> Option<u16> {
    let LineLook { depth, glyph, glyph_color, is_dir, expanded, bg: row_bg } = *look;
    let glyph = (glyph, glyph_color);
    let theme = cx.theme;
    let glyph_width = crate::ui::width(glyph.0).max(1) as u16;
    let marker = marker_width(cx.settings.icons, glyph_width);
    let wanted = depth.saturating_sub(1) as u16 * INDENT;
    let room = area.width.saturating_sub(1 + marker + MIN_NAME + 1);
    let indent = wanted.min(room);
    let mut x = area.x + 1 + indent;
    // Indent guides under each folder above this entry, where the indent is whole.
    let guide = Style::colors(theme.indent_guide, row_bg);
    for level in 1..depth {
        let gx = area.x + 1 + (level as u16 - 1) * INDENT;
        if gx + INDENT <= x {
            canvas.put(gx, y, "│", guide);
        }
    }
    if cx.settings.icons == Icons::Nerd {
        if is_dir {
            let chevron = if expanded { "▾" } else { "▸" };
            canvas.put(x, y, chevron, Style::colors(theme.icon_muted, row_bg));
        }
        x += 2;
    }
    let used = canvas.text(x, y, glyph.0, Style::colors(glyph.1, row_bg), area.right().saturating_sub(x));
    x += used + 1;
    let max = area.right().saturating_sub(x + 1);
    match name {
        NameDraw::Text(text, style) => {
            canvas.text_truncated(x, y, text, style, max);
            None
        }
        NameDraw::Field(input) => {
            let field = Rect::new(x, y, max + 1, 1);
            canvas.fill(field, theme.editor_bg);
            let base = Style::colors(theme.text, theme.editor_bg);
            let selection = Style::colors(theme.text, theme.selection_bg);
            let placeholder = Style::colors(theme.text_placeholder, theme.editor_bg);
            let label = if is_dir { "folder name" } else { "file name" };
            Some(input.render(canvas, Rect::new(x, y, max.max(1), 1), base, selection, label, placeholder))
        }
    }
}

/// What goes in the name column of a line.
enum NameDraw<'a> {
    Text(&'a str, Style),
    Field(&'a mut LineInput),
}

impl View for ProjectPanel {
    fn render(&mut self, canvas: &mut Canvas<'_>, area: Rect, cx: &RenderCx<'_>) -> Option<Cursor> {
        let theme = cx.theme;
        let bg = theme.panel_bg;
        canvas.fill(area, bg);
        self.area = area;
        let has_find = self.find.is_some() && area.height >= 2;
        let header_rows = 1 + usize::from(has_find);
        self.height = (area.height as usize).saturating_sub(header_rows);
        if area.width == 0 || area.height == 0 {
            return None;
        }
        let hide = cx.settings.project_panel.hide_gitignore;
        self.colors.rebuild(cx.project, cx.settings.project_panel.git_status);
        let lines = self.lines(hide);
        self.shown = lines.clone();

        // Keep the selection in view.
        let focus_line = if self.edit.is_some() {
            lines.iter().position(|line| match (line, self.edit.as_ref().map(|e| &e.kind)) {
                (Line::Input(_), Some(EditKind::NewFile { .. } | EditKind::NewDir { .. })) => true,
                (Line::Entry(index), Some(EditKind::Rename { path })) => self.tree.rows().get(*index).is_some_and(|r| &r.path == path),
                _ => false,
            })
        } else {
            self.tree.selected().and_then(|sel| lines.iter().position(|line| *line == Line::Entry(sel)))
        };
        if focus_line.is_none() && self.tree.selected() == Some(0) && self.reveal.is_some() {
            // The header is pinned; selecting it brings the top of the list back.
            self.scroll = 0;
        }
        if let (Some(centre), Some(selected)) = (self.reveal.take(), focus_line) {
            if centre {
                self.scroll = selected.saturating_sub(self.height / 2);
            } else if selected < self.scroll {
                self.scroll = selected;
            } else if self.height > 0 && selected >= self.scroll + self.height {
                self.scroll = selected + 1 - self.height;
            }
        }
        self.scroll = self.scroll.min(lines.len().saturating_sub(self.height.max(1)));

        // The header: the project's name, pinned.
        let rows = self.tree.rows();
        if let Some(root) = rows.first() {
            let selected = self.tree.selected() == Some(0);
            let row_bg = if selected { theme.element_selected } else { bg };
            if selected {
                canvas.fill(Rect::new(area.x, area.y, area.width, 1), row_bg);
            }
            let chevron = if root.is_expanded() { "▾" } else { "▸" };
            canvas.put(area.x + 1, area.y, chevron, Style::colors(theme.icon_muted, row_bg));
            let name = if root.name.is_empty() { root.path.to_string_lossy().into_owned() } else { root.name.clone() };
            let room = area.width.saturating_sub(4);
            canvas.text_truncated(area.x + 3, area.y, &name, Style::colors(theme.text, row_bg).bold(), room);
        }

        let mut cursor = if has_find {
            self.render_find_bar(canvas, Rect::new(area.x, area.y + 1, area.width, 1), cx)
        } else {
            None
        };
        let list_y = area.y + header_rows as u16;
        let rows = self.tree.rows();
        let cut: HashSet<&Path> = match self.tree.clipboard() {
            Some(clip) if clip.cut => clip.paths.iter().map(PathBuf::as_path).collect(),
            _ => HashSet::new(),
        };
        let renaming = match self.edit.as_ref().map(|e| &e.kind) {
            Some(EditKind::Rename { path }) => Some(path.clone()),
            _ => None,
        };
        let adding = matches!(self.edit.as_ref().map(|e| &e.kind), Some(EditKind::NewFile { .. } | EditKind::NewDir { .. }));
        let new_is_dir = matches!(self.edit.as_ref().map(|e| &e.kind), Some(EditKind::NewDir { .. }));
        let mut field = self.edit.as_mut().map(|edit| &mut edit.input);
        for (offset, line) in lines.iter().enumerate().skip(self.scroll).take(self.height) {
            let y = list_y + (offset - self.scroll) as u16;
            match *line {
                Line::Input(depth) => {
                    let Some(input) = field.as_deref_mut() else { continue };
                    let text = input.text().to_string();
                    let shown_name = if text.is_empty() { " " } else { text.trim_end_matches('/') };
                    let is_dir = new_is_dir || text.ends_with('/');
                    let (glyph, color) = file_icon(cx.settings, shown_name, is_dir, false);
                    canvas.fill(Rect::new(area.x, y, area.width, 1), theme.element_selected);
                    let look = LineLook { depth, glyph: &glyph, glyph_color: color, is_dir, expanded: false, bg: theme.element_selected };
                    let col = draw_line(canvas, area, y, &look, NameDraw::Field(input), cx);
                    cursor = col.map(|col| Cursor { col, row: y, shape: CursorShape::Bar });
                }
                Line::Entry(index) => {
                    let Some(row) = rows.get(index) else { continue };
                    let selected = self.tree.selected() == Some(index) && !adding;
                    let open = cx.active_path == Some(row.path.as_path());
                    let row_bg = if selected {
                        theme.element_selected
                    } else if open {
                        theme.element_hover
                    } else {
                        bg
                    };
                    if row_bg != bg {
                        canvas.fill(Rect::new(area.x, y, area.width, 1), row_bg);
                    }
                    let (glyph, icon_color) = file_icon(cx.settings, &row.name, row.is_dir(), row.is_expanded());
                    let is_cut = cut.contains(row.path.as_path());
                    let icon_color = if row.ignored || is_cut { icon_color.lerp(row_bg, 0.45) } else { icon_color };
                    let mut style = Style::colors(self.colors.color(row, theme), row_bg);
                    if open {
                        style = style.bold();
                    }
                    if is_cut {
                        style = Style::colors(theme.text_muted, row_bg).italic();
                    }
                    let look = LineLook { depth: row.depth, glyph: &glyph, glyph_color: icon_color, is_dir: row.is_dir(), expanded: row.is_expanded(), bg: row_bg };
                    match field.as_deref_mut().filter(|_| renaming.as_ref() == Some(&row.path)) {
                        Some(input) => {
                            let col = draw_line(canvas, area, y, &look, NameDraw::Field(input), cx);
                            cursor = col.map(|col| Cursor { col, row: y, shape: CursorShape::Bar });
                        }
                        None => {
                            draw_line(canvas, area, y, &look, NameDraw::Text(&row.name, style), cx);
                        }
                    }
                }
            }
        }

        if lines.is_empty() && self.height > 0 {
            let (message, hint) = if rows.len() > 1 {
                ("Everything here is ignored", "hide_gitignore is on in settings")
            } else {
                ("This folder is empty", "cmd-n adds a file")
            };
            let width = area.width.saturating_sub(3);
            canvas.text_truncated(area.x + 3, area.y + 1, message, Style::colors(theme.text_muted, bg), width);
            if self.height > 1 {
                canvas.text_truncated(area.x + 3, area.y + 2, hint, Style::colors(theme.text_placeholder, bg), width);
            }
        }
        cursor.filter(|_| cx.focused)
    }

    fn contexts(&self, out: &mut Vec<Context>) {
        if self.find_focused() {
            out.push(Context::new("BufferSearchBar"));
            out.push(Context::new("Editor").value("mode", "single_line"));
            return;
        }
        let editing = self.edit.is_some();
        out.push(Context::new("ProjectPanel").flag(if editing { "editing" } else { "not_editing" }));
        if editing {
            out.push(Context::new("Editor").value("mode", "single_line"));
        }
    }

    fn handle_action(&mut self, action: &Action, cx: &mut Cx<'_>) -> bool {
        let hide = Self::hides_ignored(cx);
        match action.name.as_str() {
            "buffer_search::Deploy" | "search::FocusSearch" => {
                self.open_find();
                return true;
            }
            "buffer_search::Dismiss" => {
                self.find = None;
                return true;
            }
            "buffer_search::FocusEditor" if self.find.is_some() => {
                if let Some(bar) = &mut self.find {
                    bar.focused = false;
                }
                return true;
            }
            "search::SelectNextMatch" if self.find.is_some() => {
                self.step_find_match(true, cx);
                return true;
            }
            "search::SelectPreviousMatch" if self.find.is_some() => {
                self.step_find_match(false, cx);
                return true;
            }
            "menu::Cancel" | "editor::Cancel" if self.find.is_some() => {
                self.find = None;
                return true;
            }
            _ => {}
        }
        if self.find_focused() && self.find_input_action(action, hide, cx) {
            return true;
        }
        if self.edit.is_some() {
            if let Some(handled) = self.edit_action(action, cx) {
                return handled;
            }
        }
        let page = self.height.max(1);
        match action.name.as_str() {
            "menu::SelectNext" => {
                let steps = action.args.as_ref().and_then(|v| v.get("lines")).and_then(|v| v.as_u64()).unwrap_or(1).max(1) as usize;
                for _ in 0..steps {
                    self.step(true, hide);
                }
            }
            "menu::SelectPrevious" => {
                let steps = action.args.as_ref().and_then(|v| v.get("lines")).and_then(|v| v.as_u64()).unwrap_or(1).max(1) as usize;
                for _ in 0..steps {
                    self.step(false, hide);
                }
            }
            "menu::SelectFirst" => self.tree.select_first(),
            "menu::SelectLast" => self.select_last_shown(hide),
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
            "project_panel::OpenPermanent" | "menu::Confirm" => return self.open_selected(false, cx),
            "project_panel::RevealPath" => {
                let Some(path) = action.arg_str("path").map(PathBuf::from) else { return false };
                if self.tree.reveal(&path).is_some() {
                    self.watch_ignored(cx);
                    self.reveal = Some(true);
                }
            }
            "project_panel::ScrollCursorCenter" => self.reveal = Some(true),
            "project_panel::NewFile" => return self.start_new(false),
            "project_panel::NewDirectory" => return self.start_new(true),
            "project_panel::Rename" => return self.start_rename(),
            "project_panel::Duplicate" => return self.duplicate(cx),
            "project_panel::Trash" => return self.remove(action, false, cx),
            "project_panel::Delete" => return self.remove(action, true, cx),
            "project_panel::Cut" => return self.cut_or_copy(true, cx),
            "project_panel::Copy" => return self.cut_or_copy(false, cx),
            "project_panel::Paste" => return self.paste(cx),
            "project_panel::NewSearchInDirectory" => return self.search_in_directory(cx),
            "workspace::CopyPath" => return self.copy_path(false, cx),
            "workspace::CopyRelativePath" => return self.copy_path(true, cx),
            "project_panel::RevealInFileManager" => {
                let Some(path) = self.selected_row().map(|row| row.path.to_string_lossy().into_owned()) else { return false };
                cx.push(Effect::Dispatch(Action::with_args("bakedit::RevealPath", json!({ "path": path }))));
                return true;
            }
            "workspace::OpenWithSystem" => {
                let Some(path) = self.selected_row().map(|row| row.path.to_string_lossy().into_owned()) else { return false };
                cx.push(Effect::OpenExternally(path));
                return true;
            }
            _ => return false,
        }
        if self.reveal.is_none() {
            self.reveal = Some(false);
        }
        self.sync_git_repo(cx);
        true
    }

    fn handle_key(&mut self, key: &KeyEvent, cx: &mut Cx<'_>) -> bool {
        let Some(text) = key.text.as_deref().filter(|_| key.is_down()) else { return false };
        if self.find_focused() {
            if text.chars().all(char::is_control) {
                return false;
            }
            if let Some(bar) = &mut self.find {
                bar.query.insert(text);
            }
            let hide = Self::hides_ignored(cx);
            self.recompute_find_matches(hide, true, cx);
            return true;
        }
        if let Some(edit) = self.edit.as_mut() {
            edit.input.insert(text);
            return true;
        }
        let command = key.mods.intersects(Mods::CTRL | Mods::SUPER | Mods::ALT);
        if command || text.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return false;
        }
        let hide = Self::hides_ignored(cx);
        let moved = self.type_ahead(text, cx.now, hide);
        if moved {
            self.sync_git_repo(cx);
        }
        moved
    }

    fn handle_paste(&mut self, text: &str, cx: &mut Cx<'_>) -> bool {
        if self.find_focused() {
            if let Some(bar) = &mut self.find {
                bar.query.insert(text);
            }
            let hide = Self::hides_ignored(cx);
            self.recompute_find_matches(hide, true, cx);
            return true;
        }
        match self.edit.as_mut() {
            Some(edit) => {
                edit.input.insert(text);
                true
            }
            None => false,
        }
    }

    fn handle_mouse(&mut self, event: &MouseEvent, _area: Rect, cx: &mut Cx<'_>) -> bool {
        match event.kind {
            MouseKind::ScrollDown => {
                let last = self.shown.len().saturating_sub(self.height.max(1));
                self.scroll = (self.scroll + WHEEL_ROWS).min(last);
            }
            MouseKind::ScrollUp => self.scroll = self.scroll.saturating_sub(WHEEL_ROWS),
            MouseKind::Press(MouseButton::Left) => {
                if self.find.is_some() && event.row == self.area.y + 1 {
                    if let Some(bar) = &mut self.find {
                        bar.focused = true;
                    }
                    return true;
                }
                if let Some(bar) = &mut self.find {
                    bar.focused = false;
                }
                let Some(line) = self.hit(event.row) else { return true };
                let Line::Entry(index) = line else { return true };
                self.edit = None;
                self.tree.select(index);
                let double = self.last_click.is_some_and(|(row, at)| row == index && at.elapsed() < DOUBLE_CLICK);
                self.last_click = Some((index, Instant::now()));
                let is_dir = self.selected_row().is_some_and(Row::is_dir);
                // A click opens a folder or previews a file; a second click keeps the tab.
                // Folders were already toggled by the first click of a double click.
                if double && is_dir {
                    self.sync_git_repo(cx);
                    return true;
                }
                return self.open_selected(!double, cx);
            }
            _ => return false,
        }
        true
    }

    fn tick(&mut self, cx: &mut Cx<'_>) -> bool {
        let mut dirty = self.colors.poll();
        if cx.settings.project_panel.git_status {
            let stale = self.colors.generation != Some(cx.project.git_generation()) || self.colors.diff_base != *cx.project.diff_base();
            if stale {
                self.colors.rebuild(cx.project, true);
                dirty = true;
            }
        } else if self.colors.generation.is_some() {
            self.colors.rebuild(cx.project, false);
            dirty = true;
        }
        if cx.settings.project_panel.auto_reveal_entries {
            let active = cx.active_path.map(Path::to_path_buf);
            if active != self.last_active {
                if let Some(path) = &active {
                    if self.tree.reveal(path).is_some() {
                        self.watch_ignored(cx);
                        self.reveal = Some(false);
                        self.sync_git_repo(cx);
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
        // A rename in progress on something that is gone has nothing left to rename.
        if let Some(Edit { kind: EditKind::Rename { path }, .. }) = &self.edit {
            if self.tree.index_of(path).is_none() {
                self.edit = None;
            }
        }
    }

    fn session_state(&self) -> Option<serde_json::Value> {
        Some(json!({ "expanded": self.expanded() }))
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
        assert_eq!(rows[0].trim(), format!("▾ {root}"), "the pinned header: chevron, then the project's name");
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
        assert_eq!(rows[2], " │ ▸ nested", "children are indented two cells, with a guide under the folder");
        assert_eq!(rows[3], " │ · main.rs");
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
        assert!(rows.iter().any(|r| r == " │ │ · deep.rs"), "{rows:?}");
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

    // ---- helpers for the tests below ----

    fn press(panel: &mut ProjectPanel, harness: &mut Harness, text: &str) -> bool {
        let key = KeyEvent::new(bakedit_core::Key::Char(text.chars().next().unwrap()), Mods::empty()).with_text(text);
        panel.handle_key(&key, &mut harness.cx())
    }

    fn type_text(panel: &mut ProjectPanel, harness: &mut Harness, text: &str) {
        for c in text.chars() {
            assert!(press(panel, harness, &c.to_string()));
        }
    }

    fn select(panel: &mut ProjectPanel, harness: &mut Harness, path: &Path) {
        let reveal = Action::with_args("project_panel::RevealPath", json!({ "path": path.to_string_lossy() }));
        assert!(panel.handle_action(&reveal, &mut harness.cx()), "{path:?} is in the tree");
    }

    fn rm_handler(panel: &mut ProjectPanel) {
        panel.tree.set_trash_handler(|path| {
            let result = if path.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) };
            result.map_err(|source| bakedit_project::FileOpError::Io { path: path.to_path_buf(), source })
        });
    }

    fn many_files(count: usize) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..count {
            std::fs::write(dir.path().join(format!("file{i:02}.txt")), "").unwrap();
        }
        dir
    }

    fn status_errors(harness: &mut Harness) -> Vec<String> {
        harness
            .take_effects()
            .into_iter()
            .filter_map(|e| match e {
                Effect::Status { text, error: true } => Some(text),
                _ => None,
            })
            .collect()
    }

    // ---- header, scrolling, rows ----

    #[test]
    fn the_header_stays_while_the_list_scrolls_and_the_selection_stays_in_view() {
        let dir = many_files(30);
        let mut harness = Harness::new(dir.path());
        harness.settings.icons = crate::settings::Icons::Ascii;
        let mut panel = ProjectPanel::new(&mut harness.cx());
        harness.draw(&mut panel, 30, 8);
        act(&mut panel, &mut harness, "menu::SelectLast");
        let rows = lines(&harness.draw(&mut panel, 30, 8).0);
        let root = dir.path().file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(rows[0].trim(), format!("▾ {root}"));
        assert_eq!(rows.last().unwrap(), " · file29.txt", "the last entry is in view: {rows:?}");
        assert!(!rows.iter().any(|r| r.contains("file00")), "the top has scrolled away");
        act(&mut panel, &mut harness, "menu::SelectFirst");
        let rows = lines(&harness.draw(&mut panel, 30, 8).0);
        assert!(rows[0].contains(&root));
        assert!(rows[1].contains("file00"), "selecting the header scrolls back");
    }

    #[test]
    fn the_wheel_scrolls_without_moving_the_selection() {
        let dir = many_files(30);
        let mut harness = Harness::new(dir.path());
        let mut panel = ProjectPanel::new(&mut harness.cx());
        harness.draw(&mut panel, 30, 8);
        let wheel = MouseEvent::new(MouseKind::ScrollDown, 4, 3);
        for _ in 0..3 {
            panel.handle_mouse(&wheel, Rect::new(0, 0, 30, 8), &mut harness.cx());
        }
        let text = harness.draw(&mut panel, 30, 8).0;
        assert!(text.contains("file09") && !text.contains("file00"), "{text}");
        assert_eq!(panel.tree.selected(), None);
        for _ in 0..30 {
            panel.handle_mouse(&wheel, Rect::new(0, 0, 30, 8), &mut harness.cx());
        }
        assert!(harness.draw(&mut panel, 30, 8).0.contains("file29"), "the end of the list is reachable and no further");
        let up = MouseEvent::new(MouseKind::ScrollUp, 4, 3);
        for _ in 0..30 {
            panel.handle_mouse(&up, Rect::new(0, 0, 30, 8), &mut harness.cx());
        }
        assert!(harness.draw(&mut panel, 30, 8).0.contains("file00"));
    }

    #[test]
    fn nerd_icons_get_a_chevron_column_and_a_gap_after_the_glyph() {
        let dir = project_dir();
        let mut harness = Harness::new(dir.path());
        let mut panel = ProjectPanel::new(&mut harness.cx());
        let (text, _, buf) = harness.draw(&mut panel, 30, 6);
        let rows = lines(&text);
        assert!(rows[1].starts_with(" ▸ "), "{:?}", rows[1]);
        let glyph = file_icon(&harness.settings, "src", true, false).0;
        assert!(rows[1].starts_with(&format!(" ▸ {glyph} src")), "chevron, gap, icon, gap, name: {:?}", rows[1]);
        assert!(rows[2].starts_with(&format!("   {}", file_icon(&harness.settings, "README.md", false, false).0)), "files leave the chevron column blank: {:?}", rows[2]);
        drop(buf);
    }

    #[test]
    fn long_names_end_in_an_ellipsis_and_never_reach_the_edge() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a_really_quite_extraordinarily_long_file_name.rs"), "").unwrap();
        let mut harness = Harness::new(dir.path());
        harness.settings.icons = crate::settings::Icons::Ascii;
        let mut panel = ProjectPanel::new(&mut harness.cx());
        let rows = lines(&harness.draw(&mut panel, 24, 4).0);
        assert_eq!(rows[1], " · a_really_quite_extr…", "one blank column is left at the right edge: {rows:?}");
        assert!(crate::ui::width(&rows[1]) < 24);
    }

    #[test]
    fn deep_trees_stay_readable_at_24_columns() {
        let dir = tempfile::tempdir().unwrap();
        let deep = dir.path().join("a/b/c/d/e/f/g/h");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("file.txt"), "").unwrap();
        let mut harness = Harness::new(dir.path());
        harness.settings.icons = crate::settings::Icons::Ascii;
        let mut panel = ProjectPanel::new(&mut harness.cx());
        select(&mut panel, &mut harness, &deep.join("file.txt"));
        let text = harness.draw(&mut panel, 24, 14).0;
        let rows = lines(&text);
        assert!(rows.iter().all(|r| crate::ui::width(r) <= 24), "{rows:?}");
        let last = rows.iter().find(|r| r.contains("file.txt")).unwrap_or_else(|| panic!("the file's whole name is shown: {text}"));
        assert!(last.starts_with(' ') && last.contains("· file.txt"), "{last:?}");
        assert!(rows.iter().any(|r| r.contains("▾ h")), "{text}");
    }

    #[test]
    fn the_open_file_is_marked_and_ignored_and_cut_entries_are_dimmed() {
        let dir = project_dir();
        std::fs::write(dir.path().join(".gitignore"), "ignored.log\n").unwrap();
        std::fs::write(dir.path().join("ignored.log"), "").unwrap();
        let mut harness = Harness::new(dir.path());
        let mut panel = ProjectPanel::new(&mut harness.cx());
        let readme = dir.path().join("README.md");
        let area = Rect::new(0, 0, 30, 8);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        {
            let mut rcx = harness.render_cx(true);
            rcx.active_path = Some(&readme);
            panel.render(&mut Canvas::new(&mut buf, area), area, &rcx);
        }
        let text = crate::dump::to_text(&buf);
        let row = lines(&text).iter().position(|r| r.contains("README.md")).unwrap() as u16;
        let hover = crate::ui::color(harness.theme.element_hover);
        assert_eq!(buf[(0, row)].bg, hover, "the open file's row is tinted across the width");
        assert_eq!(buf[(29, row)].bg, hover);
        let ignored = lines(&text).iter().position(|r| r.contains("ignored.log")).unwrap() as u16;
        let name_x = lines(&text)[ignored as usize].find("ignored.log").unwrap();
        let name_x = lines(&text)[ignored as usize][..name_x].chars().count() as u16;
        assert_eq!(buf[(name_x, ignored)].fg, crate::ui::color(harness.theme.ignored), "ignored files are dimmed");
    }

    #[test]
    fn git_status_colours_names_and_the_folders_above_them() {
        let dir = project_dir();
        let mut harness = Harness::new(dir.path());
        let mut panel = ProjectPanel::new(&mut harness.cx());
        panel.tree.expand(&dir.path().join("src"));
        panel.colors.generation = Some(u64::MAX);
        panel.colors.files.insert(dir.path().join("src/main.rs"), DisplayStatus::Modified);
        panel.colors.files.insert(dir.path().join("README.md"), DisplayStatus::Untracked);
        panel.colors.dirs.insert(dir.path().join("src"));
        // Pretend the status is current so the colours are not rebuilt away.
        let generation = harness.project.git_generation();
        panel.colors.generation = Some(generation);
        let (text, _, buf) = harness.draw(&mut panel, 30, 8);
        let at = |needle: &str| {
            let rows = lines(&text);
            let y = rows.iter().position(|r| r.contains(needle)).unwrap();
            let x = rows[y][..rows[y].find(needle).unwrap()].chars().count() as u16;
            buf[(x, y as u16)].fg
        };
        // draw() enables git_status in the default settings, so the generation check
        // above keeps what was set.
        assert_eq!(at("main.rs"), crate::ui::color(harness.theme.modified));
        assert_eq!(at("README.md"), crate::ui::color(harness.theme.created));
        assert_eq!(at("src"), crate::ui::color(harness.theme.modified), "a folder with a change inside");
        assert_eq!(at("nested"), crate::ui::color(harness.theme.text));
    }

    #[test]
    fn git_colours_go_away_with_the_setting() {
        let dir = project_dir();
        let mut harness = Harness::new(dir.path());
        harness.settings.project_panel.git_status = false;
        let mut panel = ProjectPanel::new(&mut harness.cx());
        panel.colors.generation = Some(0);
        panel.colors.files.insert(dir.path().join("README.md"), DisplayStatus::Modified);
        let (text, _, buf) = harness.draw(&mut panel, 30, 6);
        let y = lines(&text).iter().position(|r| r.contains("README.md")).unwrap() as u16;
        let x = lines(&text)[y as usize].find("README.md").unwrap() as u16;
        assert_eq!(buf[(x, y)].fg, crate::ui::color(harness.theme.text));
    }

    #[test]
    fn hide_gitignore_hides_ignored_entries_and_navigation_skips_them() {
        let dir = project_dir();
        std::fs::write(dir.path().join(".gitignore"), "target/\nz.log\n").unwrap();
        std::fs::create_dir_all(dir.path().join("target/debug")).unwrap();
        std::fs::write(dir.path().join("z.log"), "").unwrap();
        let mut harness = Harness::new(dir.path());
        harness.settings.icons = crate::settings::Icons::Ascii;
        harness.settings.project_panel.hide_gitignore = true;
        let mut panel = ProjectPanel::new(&mut harness.cx());
        let text = harness.draw(&mut panel, 30, 10).0;
        assert!(text.contains("README.md") && !text.contains("target") && !text.contains("z.log"), "{text}");
        act(&mut panel, &mut harness, "menu::SelectLast");
        assert_eq!(panel.selected_row().unwrap().name, "README.md", "the last entry that is shown");
        act(&mut panel, &mut harness, "menu::SelectPrevious");
        assert_eq!(panel.selected_row().unwrap().name, ".gitignore");
        act(&mut panel, &mut harness, "menu::SelectPrevious");
        assert_eq!(panel.selected_row().unwrap().name, "src", "target/ is skipped");
    }

    // ---- empty states ----

    #[test]
    fn the_empty_state_says_what_fills_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut harness = Harness::new(dir.path());
        let mut panel = ProjectPanel::new(&mut harness.cx());
        let text = harness.draw(&mut panel, 30, 6).0;
        assert!(text.contains("This folder is empty") && text.contains("cmd-n adds a file"), "{text}");
        let narrow = lines(&harness.draw(&mut panel, 12, 6).0);
        assert!(narrow.iter().all(|r| crate::ui::width(r) <= 12), "{narrow:?}");
        assert!(narrow[1].ends_with('…'), "{narrow:?}");
        // Typing a new name replaces the message with the field.
        act(&mut panel, &mut harness, "project_panel::NewFile");
        let text = harness.draw(&mut panel, 30, 6).0;
        assert!(!text.contains("This folder is empty"), "{text}");
        assert!(text.contains("file name"), "an empty field shows what it is for: {text}");
    }

    #[test]
    fn a_folder_that_is_all_ignored_says_so() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".gitignore"), "*.log\n").unwrap();
        std::fs::write(dir.path().join("a.log"), "").unwrap();
        let mut harness = Harness::new(dir.path());
        harness.settings.project_panel.hide_gitignore = true;
        let mut panel = ProjectPanel::new(&mut harness.cx());
        let text = harness.draw(&mut panel, 40, 8).0;
        if !text.contains(".gitignore") {
            assert!(text.contains("Everything here is ignored"), "{text}");
        }
    }

    // ---- typing to find ----

    #[test]
    fn typing_letters_jumps_to_the_next_entry_with_them() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["alpha.rs", "apple.rs", "beta.rs", "bravo.rs", "Cargo.toml"] {
            std::fs::write(dir.path().join(name), "").unwrap();
        }
        let mut harness = Harness::new(dir.path());
        let mut panel = ProjectPanel::new(&mut harness.cx());
        let name = |panel: &ProjectPanel| panel.selected_row().map(|r| r.name.clone());
        assert!(press(&mut panel, &mut harness, "b"));
        assert_eq!(name(&panel).as_deref(), Some("beta.rs"));
        std::thread::sleep(TYPE_AHEAD + Duration::from_millis(50));
        assert!(press(&mut panel, &mut harness, "b"));
        assert_eq!(name(&panel).as_deref(), Some("bravo.rs"), "the same initial again moves on");
        std::thread::sleep(TYPE_AHEAD + Duration::from_millis(50));
        assert!(press(&mut panel, &mut harness, "a"));
        assert!(press(&mut panel, &mut harness, "p"));
        assert_eq!(name(&panel).as_deref(), Some("apple.rs"), "letters in a row narrow the match");
        std::thread::sleep(TYPE_AHEAD + Duration::from_millis(50));
        assert!(press(&mut panel, &mut harness, "c"));
        assert_eq!(name(&panel).as_deref(), Some("Cargo.toml"), "case does not matter");
        assert!(!press(&mut panel, &mut harness, "q"), "no match: the key is left for the shell");
        let ctrl = KeyEvent::new(bakedit_core::Key::Char('b'), Mods::CTRL).with_text("b");
        assert!(!panel.handle_key(&ctrl, &mut harness.cx()), "shortcuts are not typed letters");
    }

    // ---- creating, renaming ----

    #[test]
    fn a_new_file_is_typed_in_its_own_row_and_opened() {
        let dir = project_dir();
        let mut harness = Harness::new(dir.path());
        harness.settings.icons = crate::settings::Icons::Ascii;
        let mut panel = ProjectPanel::new(&mut harness.cx());
        select(&mut panel, &mut harness, &dir.path().join("src/main.rs"));
        assert!(act(&mut panel, &mut harness, "project_panel::NewFile"));
        let mut contexts = Vec::new();
        panel.contexts(&mut contexts);
        assert_eq!(contexts[0], Context::new("ProjectPanel").flag("editing"));
        assert_eq!(contexts[1], Context::new("Editor").value("mode", "single_line"));
        type_text(&mut panel, &mut harness, "lib.rs");
        let (text, cursor, buf) = harness.draw(&mut panel, 30, 10);
        let rows = lines(&text);
        let at = rows.iter().position(|r| r.contains("lib.rs")).unwrap();
        let selected = crate::ui::color(harness.theme.element_selected);
        let main = rows.iter().position(|r| r.contains("main.rs")).unwrap() as u16;
        assert_eq!(buf[(1, at as u16)].bg, selected, "the new row is the highlighted one");
        assert_ne!(buf[(1, main)].bg, selected, "the entry the selection is on is not also highlighted");
        assert_eq!(rows[at], " │ · lib.rs", "inside src, first among its children: {rows:?}");
        assert_eq!(rows[at - 1], " ▾ src");
        assert_eq!(cursor.map(|c| (c.col, c.row)), Some((11, at as u16)), "the cursor sits after the typed name");
        assert!(act(&mut panel, &mut harness, "menu::Confirm"));
        assert!(dir.path().join("src/lib.rs").is_file());
        let effects = harness.take_effects();
        assert!(effects.contains(&Effect::PathsChanged(vec![dir.path().join("src/lib.rs")])), "{effects:?}");
        assert!(effects.contains(&Effect::open(dir.path().join("src/lib.rs"))), "the new file opens in the editor");
        let mut contexts = Vec::new();
        panel.contexts(&mut contexts);
        assert_eq!(contexts, vec![Context::new("ProjectPanel").flag("not_editing")]);
        assert_eq!(panel.selected_row().unwrap().name, "lib.rs");
    }

    #[test]
    fn a_new_folder_with_slashes_makes_every_level_and_does_not_open_anything() {
        let dir = project_dir();
        let mut harness = Harness::new(dir.path());
        let mut panel = ProjectPanel::new(&mut harness.cx());
        act(&mut panel, &mut harness, "menu::SelectFirst");
        assert!(act(&mut panel, &mut harness, "project_panel::NewDirectory"));
        type_text(&mut panel, &mut harness, "x/y");
        act(&mut panel, &mut harness, "menu::Confirm");
        assert!(dir.path().join("x/y").is_dir());
        assert!(!harness.take_effects().iter().any(|e| matches!(e, Effect::OpenFile { .. })));
    }

    #[test]
    fn a_refused_name_stays_in_the_field_with_the_reason_in_the_status_bar() {
        let dir = project_dir();
        let mut harness = Harness::new(dir.path());
        let mut panel = ProjectPanel::new(&mut harness.cx());
        act(&mut panel, &mut harness, "menu::SelectFirst");
        act(&mut panel, &mut harness, "project_panel::NewFile");
        type_text(&mut panel, &mut harness, "README.md");
        act(&mut panel, &mut harness, "menu::Confirm");
        let errors = status_errors(&mut harness);
        assert_eq!(errors, vec!["README.md already exists".to_string()], "the project path is left out");
        assert!(panel.edit.is_some(), "still editing, so the name can be fixed");
        // Fix it.
        act(&mut panel, &mut harness, "editor::MoveToEndOfLine");
        type_text(&mut panel, &mut harness, ".bak");
        act(&mut panel, &mut harness, "menu::Confirm");
        assert!(dir.path().join("README.md.bak").is_file());
        assert!(panel.edit.is_none());
    }

    #[test]
    fn escape_and_an_empty_name_cancel() {
        let dir = project_dir();
        let before = std::fs::read_dir(dir.path()).unwrap().count();
        let mut harness = Harness::new(dir.path());
        let mut panel = ProjectPanel::new(&mut harness.cx());
        act(&mut panel, &mut harness, "project_panel::NewFile");
        type_text(&mut panel, &mut harness, "nope.txt");
        assert!(act(&mut panel, &mut harness, "menu::Cancel"));
        assert!(panel.edit.is_none());
        act(&mut panel, &mut harness, "project_panel::NewFile");
        act(&mut panel, &mut harness, "menu::Confirm");
        assert!(panel.edit.is_none(), "an empty name makes nothing");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), before);
        assert!(status_errors(&mut harness).is_empty());
    }

    #[test]
    fn rename_selects_the_stem_and_keeps_the_extension_by_default() {
        let dir = project_dir();
        let mut harness = Harness::new(dir.path());
        let mut panel = ProjectPanel::new(&mut harness.cx());
        let readme = dir.path().join("README.md");
        select(&mut panel, &mut harness, &readme);
        assert!(act(&mut panel, &mut harness, "project_panel::Rename"));
        let (text, cursor, buf) = harness.draw(&mut panel, 30, 6);
        assert!(text.contains("README.md"));
        assert!(cursor.is_some());
        let y = lines(&text).iter().position(|r| r.contains("README.md")).unwrap() as u16;
        let x = lines(&text)[y as usize].find("README").unwrap() as u16;
        assert_eq!(buf[(x, y)].bg, crate::ui::color(harness.theme.selection_bg), "the stem is selected");
        assert_ne!(buf[(x + 7, y)].bg, crate::ui::color(harness.theme.selection_bg), "the extension is not");
        type_text(&mut panel, &mut harness, "CHANGES");
        act(&mut panel, &mut harness, "menu::Confirm");
        assert!(dir.path().join("CHANGES.md").is_file() && !readme.exists());
        let effects = harness.take_effects();
        let moved = Effect::Moved { from: readme.clone(), to: dir.path().join("CHANGES.md") };
        let changed = Effect::PathsChanged(vec![readme, dir.path().join("CHANGES.md")]);
        let at = |wanted: &Effect| effects.iter().position(|effect| effect == wanted);
        assert!(at(&moved).zip(at(&changed)).is_some_and(|(m, c)| m < c), "the tabs follow the file first: {effects:?}");
        assert_eq!(panel.selected_row().unwrap().name, "CHANGES.md");
        assert!(!effects.iter().any(|e| matches!(e, Effect::OpenFile { .. })), "renaming leaves the focus where it is");
    }

    #[test]
    fn rename_onto_an_existing_name_is_refused_and_the_root_cannot_be_renamed() {
        let dir = project_dir();
        std::fs::write(dir.path().join("b.txt"), "").unwrap();
        let mut harness = Harness::new(dir.path());
        let mut panel = ProjectPanel::new(&mut harness.cx());
        act(&mut panel, &mut harness, "menu::SelectFirst");
        assert!(!act(&mut panel, &mut harness, "project_panel::Rename"), "nothing to rename on the header");
        select(&mut panel, &mut harness, &dir.path().join("b.txt"));
        act(&mut panel, &mut harness, "project_panel::Rename");
        act(&mut panel, &mut harness, "editor::SelectAll");
        type_text(&mut panel, &mut harness, "README.md");
        act(&mut panel, &mut harness, "menu::Confirm");
        assert_eq!(status_errors(&mut harness).len(), 1);
        assert!(dir.path().join("b.txt").exists());
        // Leaving the name as it was is not an error.
        act(&mut panel, &mut harness, "menu::Cancel");
        act(&mut panel, &mut harness, "project_panel::Rename");
        act(&mut panel, &mut harness, "menu::Confirm");
        assert!(status_errors(&mut harness).is_empty() && panel.edit.is_none());
    }

    #[test]
    fn the_name_field_edits_like_a_line_input_and_takes_paste_and_the_clipboard() {
        let dir = project_dir();
        let mut harness = Harness::new(dir.path());
        let mut panel = ProjectPanel::new(&mut harness.cx());
        act(&mut panel, &mut harness, "project_panel::NewFile");
        assert!(panel.handle_paste("pasted\nname.txt", &mut harness.cx()));
        assert_eq!(panel.edit.as_ref().unwrap().input.text(), "pasted name.txt");
        act(&mut panel, &mut harness, "editor::Backspace");
        act(&mut panel, &mut harness, "editor::MoveToBeginningOfLine");
        act(&mut panel, &mut harness, "editor::SelectToEndOfLine");
        act(&mut panel, &mut harness, "project_panel::Copy");
        assert_eq!(harness.clipboard.get().unwrap().text, "pasted name.tx", "copy in the field copies text, not entries");
        act(&mut panel, &mut harness, "editor::MoveToEndOfLine");
        act(&mut panel, &mut harness, "project_panel::Paste");
        assert_eq!(panel.edit.as_ref().unwrap().input.text(), "pasted name.txpasted name.tx");
        assert!(act(&mut panel, &mut harness, "menu::SelectNext"), "arrows do not move the tree under the field");
        assert!(!act(&mut panel, &mut harness, "workspace::ToggleZoom"), "other actions reach the shell");
    }

    #[test]
    fn clicking_an_entry_drops_the_name_field() {
        let dir = project_dir();
        let mut harness = Harness::new(dir.path());
        let mut panel = ProjectPanel::new(&mut harness.cx());
        act(&mut panel, &mut harness, "project_panel::NewFile");
        harness.draw(&mut panel, 30, 8);
        let click = MouseEvent::new(MouseKind::Press(MouseButton::Left), 4, 3);
        panel.handle_mouse(&click, Rect::new(0, 0, 30, 8), &mut harness.cx());
        assert!(panel.edit.is_none());
    }

    // ---- removing, copying ----

    #[test]
    fn trash_asks_first_and_the_answer_does_it() {
        let dir = project_dir();
        let mut harness = Harness::new(dir.path());
        let mut panel = ProjectPanel::new(&mut harness.cx());
        rm_handler(&mut panel);
        let readme = dir.path().join("README.md");
        select(&mut panel, &mut harness, &readme);
        let trash = Action::with_args("project_panel::Trash", json!({ "skip_prompt": false }));
        assert!(panel.handle_action(&trash, &mut harness.cx()));
        assert!(readme.exists(), "nothing happens before the answer");
        let effects = harness.take_effects();
        let [Effect::Ask(question)] = &effects[..] else { panic!("{effects:?}") };
        assert_eq!(question.message, "Trash README.md?");
        assert_eq!(question.answers.last().unwrap().0, "Cancel", "the last button is the cancel button");
        assert!(question.answers.last().unwrap().1.is_empty());
        let [Effect::Dispatch(confirmed)] = &question.answers[0].1[..] else { panic!() };
        assert!(panel.handle_action(confirmed, &mut harness.cx()));
        assert!(!readme.exists());
        let effects = harness.take_effects();
        assert!(effects.contains(&Effect::PathsChanged(vec![readme])), "{effects:?}");
        assert!(panel.tree.index_of(&dir.path().join("README.md")).is_none());
    }

    #[test]
    fn skip_prompt_trashes_at_once_and_a_folder_prompt_says_what_goes_with_it() {
        let dir = project_dir();
        let mut harness = Harness::new(dir.path());
        let mut panel = ProjectPanel::new(&mut harness.cx());
        rm_handler(&mut panel);
        select(&mut panel, &mut harness, &dir.path().join("src"));
        let ask = Action::with_args("project_panel::Delete", json!({ "skip_prompt": false }));
        panel.handle_action(&ask, &mut harness.cx());
        let effects = harness.take_effects();
        let [Effect::Ask(question)] = &effects[..] else { panic!("{effects:?}") };
        assert_eq!(question.message, "Delete src and everything in it?");
        let quiet = Action::with_args("project_panel::Trash", json!({ "skip_prompt": true }));
        assert!(panel.handle_action(&quiet, &mut harness.cx()));
        assert!(!dir.path().join("src").exists());
        assert!(harness.take_effects().iter().any(|e| matches!(e, Effect::Status { error: false, .. })));
    }

    #[test]
    fn the_project_folder_cannot_be_trashed() {
        let dir = project_dir();
        let mut harness = Harness::new(dir.path());
        let mut panel = ProjectPanel::new(&mut harness.cx());
        rm_handler(&mut panel);
        act(&mut panel, &mut harness, "menu::SelectFirst");
        let quiet = Action::with_args("project_panel::Trash", json!({ "skip_prompt": true }));
        assert!(panel.handle_action(&quiet, &mut harness.cx()));
        assert!(dir.path().join("README.md").exists());
        assert_eq!(status_errors(&mut harness).len(), 1);
    }

    #[test]
    fn duplicate_cut_copy_and_paste() {
        let dir = project_dir();
        let mut harness = Harness::new(dir.path());
        let mut panel = ProjectPanel::new(&mut harness.cx());
        let readme = dir.path().join("README.md");
        select(&mut panel, &mut harness, &readme);
        assert!(act(&mut panel, &mut harness, "project_panel::Duplicate"));
        assert!(dir.path().join("README copy.md").is_file());
        assert_eq!(panel.selected_row().unwrap().name, "README copy.md", "the copy is selected");
        harness.take_effects();

        // Copy into src.
        select(&mut panel, &mut harness, &readme);
        assert!(act(&mut panel, &mut harness, "project_panel::Copy"));
        select(&mut panel, &mut harness, &dir.path().join("src"));
        assert!(act(&mut panel, &mut harness, "project_panel::Paste"));
        assert!(dir.path().join("src/README.md").is_file() && readme.is_file());

        // Cut main.rs into the project root.
        select(&mut panel, &mut harness, &dir.path().join("src/main.rs"));
        assert!(act(&mut panel, &mut harness, "project_panel::Cut"));
        harness.take_effects();
        select(&mut panel, &mut harness, &readme);
        assert!(act(&mut panel, &mut harness, "project_panel::Paste"));
        assert!(dir.path().join("main.rs").is_file() && !dir.path().join("src/main.rs").exists());
        let effects = harness.take_effects();
        let Some(Effect::PathsChanged(changed)) = effects.iter().find(|e| matches!(e, Effect::PathsChanged(_))) else { panic!("{effects:?}") };
        assert!(changed.contains(&dir.path().join("main.rs")) && changed.contains(&dir.path().join("src/main.rs")), "both ends of a move: {changed:?}");
    }

    #[test]
    fn cut_entries_are_dimmed_until_pasted_and_pasting_nothing_says_so() {
        let dir = project_dir();
        let mut harness = Harness::new(dir.path());
        harness.settings.icons = crate::settings::Icons::Ascii;
        let mut panel = ProjectPanel::new(&mut harness.cx());
        act(&mut panel, &mut harness, "menu::SelectFirst");
        assert!(!act(&mut panel, &mut harness, "project_panel::Cut"), "the header cannot be cut");
        act(&mut panel, &mut harness, "project_panel::Paste");
        let effects = harness.take_effects();
        assert!(effects.iter().any(|e| matches!(e, Effect::Status { text, .. } if text.starts_with("Nothing to paste"))), "{effects:?}");
        select(&mut panel, &mut harness, &dir.path().join("README.md"));
        act(&mut panel, &mut harness, "project_panel::Cut");
        let (text, _, buf) = harness.draw(&mut panel, 30, 6);
        let y = lines(&text).iter().position(|r| r.contains("README.md")).unwrap() as u16;
        assert_eq!(buf[(3, y)].fg, crate::ui::color(harness.theme.text_muted));
    }

    #[test]
    fn paths_open_externally_and_search_in_a_folder() {
        let dir = project_dir();
        let mut harness = Harness::new(dir.path());
        let mut panel = ProjectPanel::new(&mut harness.cx());
        let nested = dir.path().join("src/nested");
        select(&mut panel, &mut harness, &nested);
        assert!(act(&mut panel, &mut harness, "workspace::OpenWithSystem"));
        assert_eq!(harness.take_effects(), vec![Effect::OpenExternally(nested.to_string_lossy().into_owned())]);
        assert!(act(&mut panel, &mut harness, "project_panel::RevealInFileManager"));
        assert_eq!(
            harness.take_effects(),
            vec![Effect::Dispatch(Action::with_args("bakedit::RevealPath", json!({ "path": nested.to_string_lossy() })))]
        );
        assert!(act(&mut panel, &mut harness, "project_panel::NewSearchInDirectory"));
        assert_eq!(
            harness.take_effects(),
            vec![
                Effect::Dispatch(Action::new("pane::DeploySearch")),
                Effect::Dispatch(Action::with_args("search::FocusSearch", json!({ "include": "src/nested/" }))),
            ]
        );
        assert!(act(&mut panel, &mut harness, "workspace::CopyPath"));
        assert_eq!(harness.clipboard.get().unwrap().text, nested.to_string_lossy());
    }

    // ---- mouse, refresh, session ----

    #[test]
    fn clicking_a_folder_toggles_it_once_even_when_double_clicked() {
        let dir = project_dir();
        let mut harness = Harness::new(dir.path());
        let mut panel = ProjectPanel::new(&mut harness.cx());
        harness.draw(&mut panel, 30, 8);
        let click = MouseEvent::new(MouseKind::Press(MouseButton::Left), 4, 1);
        let area = Rect::new(0, 0, 30, 8);
        panel.handle_mouse(&click, area, &mut harness.cx());
        assert!(panel.tree.is_expanded(&dir.path().join("src")));
        harness.draw(&mut panel, 30, 8);
        panel.handle_mouse(&click, area, &mut harness.cx());
        assert!(panel.tree.is_expanded(&dir.path().join("src")), "the second click of a double click keeps it open");
        assert!(harness.take_effects().is_empty());
        // The header toggles the whole tree.
        let header = MouseEvent::new(MouseKind::Press(MouseButton::Left), 6, 0);
        panel.handle_mouse(&header, area, &mut harness.cx());
        assert!(!panel.tree.is_expanded(dir.path()));
    }

    #[test]
    fn a_refresh_keeps_what_is_open_and_selected() {
        let dir = project_dir();
        let mut harness = Harness::new(dir.path());
        let mut panel = ProjectPanel::new(&mut harness.cx());
        let deep = dir.path().join("src/nested/deep.rs");
        select(&mut panel, &mut harness, &deep);
        std::fs::write(dir.path().join("src/nested/zzz.rs"), "").unwrap();
        panel.paths_changed(&FsChange::of(vec![dir.path().join("src/nested/zzz.rs")]), &mut harness.cx());
        assert_eq!(panel.selected_row().unwrap().path, deep);
        assert!(harness.draw(&mut panel, 30, 10).0.contains("zzz.rs"));
        std::fs::remove_file(dir.path().join("src/nested/zzz.rs")).unwrap();
        panel.paths_changed(&FsChange { paths: Vec::new(), rescan: true }, &mut harness.cx());
        let text = harness.draw(&mut panel, 30, 10).0;
        assert!(!text.contains("zzz.rs") && text.contains("deep.rs"), "{text}");
        assert_eq!(panel.expanded().len(), 3);
    }

    #[test]
    fn auto_reveal_follows_the_front_tab_only_when_enabled() {
        let dir = project_dir();
        let mut harness = Harness::new(dir.path());
        harness.settings.project_panel.auto_reveal_entries = false;
        let mut panel = ProjectPanel::new(&mut harness.cx());
        let deep = dir.path().join("src/nested/deep.rs");
        let mut cx = harness.cx();
        cx.active_path = Some(&deep);
        panel.tick(&mut cx);
        assert_eq!(panel.tree.selected(), None);
        harness.settings.project_panel.auto_reveal_entries = true;
        let mut cx = harness.cx();
        cx.active_path = Some(&deep);
        assert!(panel.tick(&mut cx));
        assert_eq!(panel.selected_row().unwrap().path, deep);
        // Moving the selection by hand is not undone while the same tab stays in front.
        act(&mut panel, &mut harness, "menu::SelectFirst");
        let mut cx = harness.cx();
        cx.active_path = Some(&deep);
        panel.tick(&mut cx);
        assert_eq!(panel.tree.selected(), Some(0));
    }

    #[test]
    fn the_session_state_round_trips() {
        let dir = project_dir();
        let mut harness = Harness::new(dir.path());
        let mut panel = ProjectPanel::new(&mut harness.cx());
        select(&mut panel, &mut harness, &dir.path().join("src/nested/deep.rs"));
        let state = panel.session_state().unwrap();
        let mut again = ProjectPanel::new(&mut harness.cx());
        again.restore_session_state(&state, &mut harness.cx());
        assert_eq!(again.expanded().len(), 3);
        again.restore_session_state(&json!("nonsense"), &mut harness.cx());
    }

    #[test]
    fn scroll_cursor_centre_puts_the_selection_mid_list() {
        let dir = many_files(40);
        let mut harness = Harness::new(dir.path());
        harness.settings.icons = crate::settings::Icons::Ascii;
        let mut panel = ProjectPanel::new(&mut harness.cx());
        select(&mut panel, &mut harness, &dir.path().join("file20.txt"));
        act(&mut panel, &mut harness, "project_panel::ScrollCursorCenter");
        let rows = lines(&harness.draw(&mut panel, 30, 11).0);
        assert_eq!(rows[6].trim(), "· file20.txt", "{rows:?}");
    }

    #[test]
    fn find_bar_searches_and_steps_through_tree_entries() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["alpha.rs", "beta.rs", "gamma_alpha.rs"] {
            std::fs::write(dir.path().join(name), "").unwrap();
        }
        let mut harness = Harness::new(dir.path());
        let mut panel = ProjectPanel::new(&mut harness.cx());

        assert!(act(&mut panel, &mut harness, "buffer_search::Deploy"));
        assert!(panel.find_focused());
        let mut ctxs = Vec::new();
        panel.contexts(&mut ctxs);
        assert!(ctxs.iter().all(|c| c.name != "ProjectPanel"), "find bar suppresses ProjectPanel keys: {ctxs:?}");
        assert!(ctxs.iter().any(|c| c.name == "BufferSearchBar"));

        for ch in "alpha".chars() {
            assert!(panel.handle_key(&KeyEvent::typed(ch), &mut harness.cx()));
        }
        assert_eq!(panel.selected_row().unwrap().name, "alpha.rs");
        assert!(act(&mut panel, &mut harness, "search::SelectNextMatch"));
        assert_eq!(panel.selected_row().unwrap().name, "gamma_alpha.rs");
        assert!(act(&mut panel, &mut harness, "search::SelectPreviousMatch"));
        assert_eq!(panel.selected_row().unwrap().name, "alpha.rs");

        let rows = lines(&harness.draw(&mut panel, 30, 8).0);
        assert!(rows[1].contains("alpha"), "find bar renders on row 1: {rows:?}");

        assert!(act(&mut panel, &mut harness, "buffer_search::Dismiss"));
        assert!(panel.find.is_none());
    }
}
