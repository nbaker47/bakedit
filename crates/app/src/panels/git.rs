//! The git panel in the left dock: what changed, and the commands that act on it.
//!
//! # What the app calls (this signature stays)
//!
//! * `GitPanel::new(cx: &mut Cx<'_>) -> GitPanel`: the changes of the project's
//!   repository, kept up to date from `cx.project.status()`.
//!
//! Everything else goes through `View` and the keymap's `GitPanel` sections.
//!
//! # What it shows
//!
//! A header (branch, ahead and behind, the state the repository is in), the changes grouped
//! like Zed does (Conflicts, Tracked, Untracked) with a staged checkbox, a status letter,
//! the file name, its folder and the lines added and removed, and under them the commit
//! message editor with its buttons. All git work goes through `Project::git_job` (off the
//! UI thread, progress and failures in the status bar); the counts of added and removed
//! lines are read by a background task.
//!
//! # Focus inside the panel
//!
//! The panel is either on its list (`GitPanel{ChangesList}`) or on the message editor
//! (`GitPanel{CommitEditor} > CommitEditor > Editor{mode=auto_height}`);
//! `git_panel::FocusEditor` and `git_panel::FocusChanges` move between them.
//!
//! # Shared with the project diff
//!
//! [`request_discard`] and [`move_to_trash`]: the diff has the same "discard this file"
//! command.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bakedit_core::{Action, KeyEvent, Mods, MouseButton, MouseEvent, MouseKind, Rgb, Theme};
use bakedit_git::{Commit, CommitOptions, DiffStatus, DiffTarget, DisplayStatus, FileState, GitError, RepoState, StageState, Status, StatusEntry};
use bakedit_text::{Buffer, Clipboard as TextClipboard, ClipboardPiece, Command, DisplayPosition, EditSettings, Editor, Motion};
use ratatui::layout::Rect;
use serde_json::json;

use crate::background::Task;
use crate::paint::{self, PaintOptions, Row, ScrollMarks};
use crate::project::{DiffBase, Project};
use crate::settings::Icons;
use crate::ui::{self, Canvas, Style};
use crate::view::{Context, Cursor, CursorShape, Cx, Effect, FsChange, Question, RenderCx, View};
use crate::workspace::{Focus, PanelId};

/// Rows a wheel step moves.
const WHEEL_ROWS: usize = 3;
/// The commit message box grows with its text up to this many rows, then scrolls.
const MESSAGE_MAX_ROWS: usize = 8;
/// ...and is never shorter than this, so it reads as a box.
const MESSAGE_MIN_ROWS: usize = 2;
/// The list never gets fewer rows than this when the panel is short.
const MIN_LIST_ROWS: u16 = 2;
/// The counts of added and removed lines are read at most this often.
const COUNTS_INTERVAL: Duration = Duration::from_millis(600);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Group {
    Conflicts,
    Tracked,
    Untracked,
}

impl Group {
    fn title(self) -> &'static str {
        match self {
            Group::Conflicts => "Conflicts",
            Group::Tracked => "Tracked",
            Group::Untracked => "Untracked",
        }
    }
}

/// One changed file as the list shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Change {
    /// Relative to the top of the work tree, as git names it.
    path: PathBuf,
    status: DisplayStatus,
    stage: StageState,
    group: Group,
}

/// One row of the list as drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListRow {
    Header(Group),
    Change(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PanelFocus {
    Changes,
    Editor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListSection {
    Uncommitted,
    Branch,
}

/// What to do when a background job the panel started has finished.
enum After {
    /// A commit: clear the message when it worked.
    Commit,
    /// Files were created, changed or removed: the tree and the tabs need to know.
    Changed(Vec<PathBuf>),
}

struct Pending {
    task: Task<Result<String, String>>,
    after: After,
}

/// The abbreviated hash and message of the last commit, or none before the first one.
type LastCommit = Option<(String, String)>;

/// What the message box held before "amend" replaced it, to put back when it is cancelled.
struct Amending {
    draft: String,
    /// The abbreviated hash of the commit being amended.
    sha: String,
}

/// One row of the message as drawn: a line, or a soft-wrapped part of one.
#[derive(Debug, Clone)]
struct VisualRow {
    line: usize,
    range: Range<usize>,
}

/// Maximum number of recent commits loaded for left/right history browsing in the top pane.
const MAX_HISTORY_COMMITS: usize = 50;

/// The git panel.
pub struct GitPanel {
    changes: Vec<Change>,
    /// The status the list was built from.
    status: Option<Arc<Status>>,
    generation: Option<u64>,
    /// The selected file, kept by path so a rebuilt list does not lose it.
    selected_path: Option<PathBuf>,
    selected: usize,
    /// First row of the list shown.
    scroll: usize,
    /// The selection moved: scroll it into view at the next draw.
    reveal: bool,
    focus: PanelFocus,
    active_section: ListSection,

    /// Commit history for left/right browsing in the top section:
    /// `commit_offset == 0` is Uncommitted (furthest right), `1` is `HEAD`, `2` is `HEAD~1`, ...
    commits: Vec<Commit>,
    commits_task: Option<Task<Vec<Commit>>>,
    commits_stale: bool,
    commit_offset: usize,
    commit_changes: Vec<Change>,
    commit_counts: HashMap<PathBuf, (usize, usize)>,
    commit_selected: usize,
    commit_scroll: usize,
    commit_reveal: bool,
    commit_task: Option<Task<Result<(String, Vec<Change>, HashMap<PathBuf, (usize, usize)>), String>>>,
    commit_loaded_sha: Option<String>,
    commit_error: Option<String>,

    /// Lines added and removed per file, from a background read of the diff.
    counts: HashMap<PathBuf, (usize, usize)>,
    counts_task: Option<Task<HashMap<PathBuf, (usize, usize)>>>,
    counts_stale: bool,
    counts_started: Option<Instant>,
    /// Bottom half: changes since the repo's base branch (e.g. `origin/main` or `main`).
    diff_base: DiffBase,
    branch_repo_root: Option<PathBuf>,
    branch_base: String,
    branch_changes: Vec<Change>,
    branch_counts: HashMap<PathBuf, (usize, usize)>,
    branch_selected_path: Option<PathBuf>,
    branch_selected: usize,
    branch_scroll: usize,
    branch_reveal: bool,
    branch_task: Option<Task<Result<(String, Vec<Change>, HashMap<PathBuf, (usize, usize)>), String>>>,
    branch_stale: bool,
    branch_error: Option<String>,

    message: Editor,
    /// First visual row of the message shown.
    message_top: usize,
    reveal_message: bool,
    amending: Option<Amending>,
    amend_load: Option<Task<Result<LastCommit, String>>>,
    pending: Vec<Pending>,

    // Where things were drawn, for the mouse.
    repo_button: Rect,
    branch_button: Rect,
    mode_button: Rect,
    prev_commit_button: Rect,
    next_commit_button: Rect,
    commit_header_rows: u16,
    list_area: Rect,
    branch_header: Rect,
    branch_list_area: Rect,
    message_area: Rect,
    /// The rows of the message box that hold text (the band minus its padding).
    message_text: Rect,
    commit_button: Rect,
    amend_button: Rect,
    visual: Vec<VisualRow>,
}

fn status_letter(status: DisplayStatus) -> &'static str {
    match status {
        DisplayStatus::Added => "A",
        DisplayStatus::Modified => "M",
        DisplayStatus::Deleted => "D",
        DisplayStatus::Renamed => "R",
        DisplayStatus::Untracked => "U",
        DisplayStatus::Conflict => "!",
    }
}

fn status_color(status: DisplayStatus, theme: &Theme) -> Rgb {
    match status {
        DisplayStatus::Added | DisplayStatus::Untracked => theme.created,
        DisplayStatus::Modified | DisplayStatus::Renamed => theme.modified,
        DisplayStatus::Deleted => theme.deleted,
        DisplayStatus::Conflict => theme.conflict,
    }
}

fn message_editor(text: &str) -> Editor {
    let settings = EditSettings { auto_indent: false, autoclose: false, wrap_width: Some(40), ..EditSettings::default() };
    let mut editor = Editor::with_settings(Buffer::from(text), settings);
    editor.apply(Command::Move(Motion::End));
    editor
}

fn group_of(entry: &StatusEntry) -> Group {
    if entry.conflicted {
        Group::Conflicts
    } else if entry.untracked {
        Group::Untracked
    } else {
        Group::Tracked
    }
}

/// The line of git's output that says there was nothing to do, else `done`.
fn summarize(output: &str, done: String) -> String {
    let nothing = |line: &&str| {
        let line = line.trim();
        line.eq_ignore_ascii_case("Everything up-to-date") || line.eq_ignore_ascii_case("Already up to date.")
    };
    output.lines().find(nothing).map(|line| line.trim().to_string()).unwrap_or(done)
}

fn digits(n: usize) -> u16 {
    n.to_string().len() as u16
}

/// Columns the staged checkbox takes (a space follows it).
fn checkbox_width(icons: Icons) -> u16 {
    match icons {
        Icons::Nerd => 1,
        Icons::Ascii => 3,
    }
}

// ---- trash and discard, shared with the project diff ------------------------------------

#[cfg(test)]
thread_local! {
    /// The Trash of the test running on this thread. Each test sets its own: an
    /// environment variable would be shared by the tests running at the same time.
    static TEST_TRASH: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Make the Trash of the test on this thread `dir`.
#[cfg(test)]
pub(crate) fn set_test_trash(dir: &Path) {
    TEST_TRASH.with(|trash| *trash.borrow_mut() = Some(dir.to_path_buf()));
}

/// The folder "moved to the Trash" files go to: `$BAKEDIT_TRASH_DIR` (scenarios), `~/.Trash`
/// on macOS, else the freedesktop trash. Asked on the thread that starts the job: a test
/// build gives the Trash its test set ([`set_test_trash`]), and never the real one.
fn trash_dir() -> Option<PathBuf> {
    #[cfg(test)]
    {
        TEST_TRASH.with(|trash| trash.borrow().clone())
    }
    #[cfg(not(test))]
    real_trash_dir()
}

#[cfg(not(test))]
fn real_trash_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("BAKEDIT_TRASH_DIR") {
        return Some(PathBuf::from(dir));
    }
    if cfg!(target_os = "macos") {
        return dirs::home_dir().map(|home| home.join(".Trash"));
    }
    dirs::data_dir().map(|data| data.join("Trash").join("files"))
}

/// Move `path` into `dir` under a name that is free there ("a.txt", "a 2.txt", ...).
fn trash_into(dir: &Path, path: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|error| format!("could not open the Trash: {error}"))?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or_else(|| "this path has no name".to_string())?;
    let (stem, extension) = match name.rfind('.') {
        Some(dot) if dot > 0 => (name[..dot].to_string(), name[dot..].to_string()),
        _ => (name.clone(), String::new()),
    };
    let mut target = dir.join(&name);
    let mut n = 2;
    while target.symlink_metadata().is_ok() {
        target = dir.join(format!("{stem} {n}{extension}"));
        n += 1;
    }
    match std::fs::rename(path, &target) {
        Ok(()) => Ok(()),
        // Another volume: a single file can be copied over, a folder cannot.
        Err(error) if error.raw_os_error() == Some(libc::EXDEV) && path.is_file() => std::fs::copy(path, &target)
            .and_then(|_| std::fs::remove_file(path))
            .map_err(|error| format!("could not move {name} to the Trash: {error}")),
        Err(error) => Err(format!("could not move {name} to the Trash: {error}")),
    }
}

/// Move a file or folder into `trash` (from [`trash_dir`], asked before the job started).
/// Nothing is deleted outright.
pub(crate) fn move_to_trash(trash: Option<&Path>, path: &Path) -> Result<(), String> {
    let dir = trash.ok_or_else(|| "there is no Trash folder".to_string())?;
    trash_into(dir, path)
}

fn trash_failure(message: String) -> GitError {
    GitError::Failed { command: "trash".to_string(), code: None, stdout: String::new(), stderr: message }
}

/// What changing the files of an entry back involves.
struct Discard {
    /// Tracked files to put back as they are in the last commit (empty for an untracked one).
    restore: Vec<PathBuf>,
    /// Files git does not have once they are unstaged: they go to the Trash.
    trash: Vec<PathBuf>,
}

fn discard_of(entry: &StatusEntry) -> Option<Discard> {
    if entry.conflicted {
        return None;
    }
    if entry.untracked {
        return Some(Discard { restore: Vec::new(), trash: vec![entry.path.clone()] });
    }
    let brand_new = matches!(entry.index, FileState::Added | FileState::Renamed | FileState::Copied);
    let restore = entry.paths().into_iter().map(Path::to_path_buf).collect();
    Some(Discard { restore, trash: if brand_new { vec![entry.path.clone()] } else { Vec::new() } })
}

fn run_discard(repo: &bakedit_git::Repo, discard: &Discard, trash: Option<&Path>) -> Result<(), GitError> {
    if !discard.restore.is_empty() {
        repo.unstage(&discard.restore)?;
        repo.restore(&discard.restore)?;
    }
    for path in &discard.trash {
        move_to_trash(trash, &repo.root().join(path)).map_err(trash_failure)?;
    }
    Ok(())
}

/// What [`request_discard`] did.
pub(crate) enum Discarding {
    /// Nothing to do, or refused: the status bar already says why.
    Refused,
    /// A question was asked; its answer comes back as an action.
    Asked,
    /// The job is running. When it ends, tell the views about `paths`
    /// (`Effect::PathsChanged`).
    Started { task: Task<Result<String, String>>, paths: Vec<PathBuf> },
}

/// Throw away the changes to one file (`rel` is relative to the top of the work tree).
/// Without `skip_prompt` the user is asked first, and the answer comes back as the action
/// `confirm` with `skip_prompt` and `path` set. A conflicted file is refused: it is
/// resolved by editing it.
pub(crate) fn request_discard(cx: &mut Cx<'_>, rel: &Path, skip_prompt: bool, confirm: &str) -> Discarding {
    let Some(entry) = cx.project.status().and_then(|status| status.entry(rel)) else {
        cx.status(format!("{} has no changes to discard", rel.display()));
        return Discarding::Refused;
    };
    let Some(discard) = discard_of(entry) else {
        cx.error(format!("{} has a conflict: edit it to resolve the conflict, then stage it", rel.display()));
        return Discarding::Refused;
    };
    let name = rel.display().to_string();
    if !skip_prompt {
        let (message, detail, button) = if entry.untracked {
            (format!("Move {name} to the Trash?"), "Git has no copy of it, so the Trash holds the only one.", "Move to Trash")
        } else if discard.trash.is_empty() {
            (format!("Discard the changes to {name}?"), "The file goes back to how the last commit has it. This cannot be undone.", "Discard")
        } else {
            (format!("Discard {name}?"), "The file is new, so it moves to the Trash.", "Discard")
        };
        let again = Action::with_args(confirm, json!({ "skip_prompt": true, "path": rel.to_string_lossy() }));
        cx.push(Effect::Ask(Question {
            message,
            detail: Some(detail.to_string()),
            answers: vec![(button.to_string(), vec![Effect::Dispatch(again)]), ("Cancel".to_string(), Vec::new())],
        }));
        return Discarding::Asked;
    }
    let paths: Vec<PathBuf> = discard.restore.iter().chain(&discard.trash).filter_map(|path| cx.project.abs_path(path)).collect();
    let trash = trash_dir();
    let task = cx.project.git_job(format!("Discard {name}"), move |repo| run_discard(repo, &discard, trash.as_deref()).map(|()| String::new()));
    Discarding::Started { task, paths }
}

// ---- the panel ---------------------------------------------------------------------------

impl GitPanel {
    /// The changes of the project's repository. Empty until the first status arrives.
    pub fn new(cx: &mut Cx<'_>) -> GitPanel {
        let branch_repo_root = cx.project.repo().map(|r| r.root().to_path_buf());
        let branch_base = match cx.project.diff_base() {
            DiffBase::Branch(base) => base.clone(),
            DiffBase::Uncommitted | DiffBase::Commit { .. } => cx.project.repo().and_then(bakedit_git::Repo::default_base_ref).unwrap_or_else(|| "main".to_string()),
        };
        let mut panel = GitPanel {
            changes: Vec::new(),
            status: None,
            generation: None,
            selected_path: None,
            selected: 0,
            scroll: 0,
            reveal: false,
            focus: PanelFocus::Changes,
            active_section: ListSection::Uncommitted,
            commits: Vec::new(),
            commits_task: None,
            commits_stale: true,
            commit_offset: 0,
            commit_changes: Vec::new(),
            commit_counts: HashMap::new(),
            commit_selected: 0,
            commit_scroll: 0,
            commit_reveal: false,
            commit_task: None,
            commit_loaded_sha: None,
            commit_error: None,
            counts: HashMap::new(),
            counts_task: None,
            counts_stale: true,
            counts_started: None,
            diff_base: cx.project.diff_base().clone(),
            branch_repo_root,
            branch_base,
            branch_changes: Vec::new(),
            branch_counts: HashMap::new(),
            branch_selected_path: None,
            branch_selected: 0,
            branch_scroll: 0,
            branch_reveal: false,
            branch_task: None,
            branch_stale: true,
            branch_error: None,
            message: message_editor(""),
            message_top: 0,
            reveal_message: false,
            amending: None,
            amend_load: None,
            pending: Vec::new(),
            repo_button: Rect::default(),
            branch_button: Rect::default(),
            mode_button: Rect::default(),
            prev_commit_button: Rect::default(),
            next_commit_button: Rect::default(),
            commit_header_rows: 0,
            list_area: Rect::default(),
            branch_header: Rect::default(),
            branch_list_area: Rect::default(),
            message_area: Rect::default(),
            message_text: Rect::default(),
            commit_button: Rect::default(),
            amend_button: Rect::default(),
            visual: Vec::new(),
        };
        panel.refresh(cx.project);
        panel
    }

    // ---- the list --------------------------------------------------------------------

    /// Rebuild the uncommitted list and mark the branch diff stale when status, repo or
    /// diff base changes. Returns true if anything changed.
    fn refresh(&mut self, project: &Project) -> bool {
        let generation = project.git_generation();
        let status = project.status().cloned();
        let diff_base = project.diff_base().clone();
        let repo_root = project.repo().map(|r| r.root().to_path_buf());
        let repo_changed = self.branch_repo_root != repo_root;
        let base_changed = self.diff_base != diff_base;
        let same = status.as_ref().map(Arc::as_ptr) == self.status.as_ref().map(Arc::as_ptr);
        if self.generation == Some(generation) && same && !base_changed && !repo_changed {
            return false;
        }
        self.generation = Some(generation);
        self.status = status.clone();
        if repo_changed {
            self.branch_repo_root = repo_root;
            self.branch_base = match &diff_base {
                DiffBase::Branch(base) => base.clone(),
                DiffBase::Uncommitted | DiffBase::Commit { .. } => project.repo().and_then(bakedit_git::Repo::default_base_ref).unwrap_or_else(|| "main".to_string()),
            };
            self.commits.clear();
            self.commits_task = None;
            self.commits_stale = true;
            self.commit_offset = 0;
            self.commit_changes.clear();
            self.commit_counts.clear();
            self.commit_selected = 0;
            self.commit_scroll = 0;
            self.commit_task = None;
            self.commit_loaded_sha = None;
            self.commit_error = None;
            self.branch_changes.clear();
            self.branch_counts.clear();
            self.branch_selected = 0;
            self.branch_selected_path = None;
            self.branch_scroll = 0;
            self.branch_error = None;
            self.branch_task = None;
            self.branch_stale = true;
            self.counts.clear();
            self.counts_stale = true;
        }
        if base_changed {
            self.diff_base = diff_base.clone();
            if let DiffBase::Branch(base) = &diff_base {
                if self.branch_base != *base {
                    self.branch_base = base.clone();
                    self.branch_changes.clear();
                    self.branch_counts.clear();
                    self.branch_selected = 0;
                    self.branch_selected_path = None;
                    self.branch_scroll = 0;
                    self.branch_error = None;
                    self.branch_task = None;
                    self.branch_stale = true;
                }
            }
        }
        if !same || repo_changed {
            self.counts_stale = true;
            self.branch_stale = true;
            self.commits_stale = true;
            let mut changes: Vec<Change> = status
                .as_deref()
                .map(|status| {
                    let entry = |e: &StatusEntry| Change { path: e.path.clone(), status: e.display_status(), stage: e.stage_state(), group: group_of(e) };
                    status.entries.iter().map(entry).collect()
                })
                .unwrap_or_default();
            // Conflicts first, then what git tracks, then what it does not (stable within each).
            changes.sort_by_key(|change| match change.group {
                Group::Conflicts => 0,
                Group::Tracked => 1,
                Group::Untracked => 2,
            });
            self.apply_changes(changes);
        }
        true
    }

    fn apply_changes(&mut self, changes: Vec<Change>) {
        self.changes = changes;
        // The selection stays on its file when the list is rebuilt under it.
        let kept = self.selected_path.as_ref().and_then(|path| self.changes.iter().position(|change| &change.path == path));
        match kept {
            Some(index) => self.selected = index,
            None => self.selected = self.selected.min(self.changes.len().saturating_sub(1)),
        }
        self.selected_path = self.changes.get(self.selected).map(|change| change.path.clone());
        if self.changes.is_empty() && self.commit_offset == 0 && !self.branch_changes.is_empty() {
            self.active_section = ListSection::Branch;
        }
    }

    fn viewing_commit(&self) -> Option<&Commit> {
        self.commit_offset.checked_sub(1).and_then(|i| self.commits.get(i))
    }

    fn top_changes(&self) -> &[Change] {
        if self.commit_offset == 0 { &self.changes } else { &self.commit_changes }
    }

    fn step_commit(&mut self, delta: isize, cx: &mut Cx<'_>) -> bool {
        // Left (+1) steps backwards into commit history; Right (-1) steps forward toward
        // Uncommitted (commit_offset == 0, furthest right).
        if self.commits.is_empty() && delta > 0 {
            self.kick_commits(cx.project);
            cx.status("No commits loaded yet");
            return true;
        }
        let max_offset = self.commits.len();
        let next = (self.commit_offset as isize + delta).clamp(0, max_offset as isize) as usize;
        if next == self.commit_offset {
            if next == 0 {
                self.active_section = ListSection::Uncommitted;
                cx.status("Already at uncommitted changes");
            } else {
                cx.status("Oldest loaded commit");
            }
            return true;
        }
        self.commit_offset = next;
        self.active_section = ListSection::Uncommitted;
        self.commit_selected = 0;
        self.commit_scroll = 0;
        self.commit_reveal = true;
        self.commit_error = None;
        if self.commit_offset == 0 {
            self.commit_task = None;
            self.commit_loaded_sha = None;
            self.commit_changes.clear();
            self.commit_counts.clear();
            if *cx.project.diff_base() != DiffBase::Uncommitted {
                cx.push(Effect::SetDiffBase(None));
            }
            let path = self.changes.get(self.selected).and_then(|c| cx.project.abs_path(&c.path));
            cx.push(Effect::ShowProjectDiff { path, focus: false });
            cx.status("Uncommitted changes");
        } else if let Some(commit) = self.viewing_commit().cloned() {
            self.commit_changes.clear();
            self.commit_counts.clear();
            self.commit_loaded_sha = None;
            self.kick_commit_diff(cx.project);
            let want = DiffBase::Commit { sha: commit.sha.clone(), label: commit.short.clone() };
            if *cx.project.diff_base() != want {
                cx.push(Effect::SetCommitDiff { sha: commit.sha.clone(), label: commit.short.clone() });
            }
            cx.push(Effect::ShowProjectDiff { path: None, focus: false });
            cx.status(format!("{} · {} ({}) — {}", commit.short, commit.author, commit.relative_time, commit.subject));
        }
        true
    }

    fn apply_branch_changes(&mut self, changes: Vec<Change>) {
        self.branch_changes = changes;
        let kept = self.branch_selected_path.as_ref().and_then(|path| self.branch_changes.iter().position(|change| &change.path == path));
        match kept {
            Some(index) => self.branch_selected = index,
            None => self.branch_selected = self.branch_selected.min(self.branch_changes.len().saturating_sub(1)),
        }
        self.branch_selected_path = self.branch_changes.get(self.branch_selected).map(|change| change.path.clone());
        if self.branch_changes.is_empty() && self.active_section == ListSection::Branch {
            self.active_section = ListSection::Uncommitted;
        }
    }

    fn list_rows(&self) -> Vec<ListRow> {
        let mut rows = Vec::with_capacity(self.changes.len() + 3);
        let mut group = None;
        for (index, change) in self.changes.iter().enumerate() {
            if group != Some(change.group) {
                group = Some(change.group);
                rows.push(ListRow::Header(change.group));
            }
            rows.push(ListRow::Change(index));
        }
        rows
    }

    fn selected_change(&self) -> Option<&Change> {
        match self.active_section {
            ListSection::Uncommitted => {
                if self.commit_offset == 0 {
                    self.changes.get(self.selected)
                } else {
                    self.commit_changes.get(self.commit_selected)
                }
            }
            ListSection::Branch => self.branch_changes.get(self.branch_selected),
        }
    }

    fn select(&mut self, index: usize) {
        if self.commit_offset > 0 {
            if self.commit_changes.is_empty() {
                if !self.branch_changes.is_empty() {
                    self.select_branch(index);
                }
                return;
            }
            self.active_section = ListSection::Uncommitted;
            self.commit_selected = index.min(self.commit_changes.len() - 1);
            self.commit_reveal = true;
            return;
        }
        if self.changes.is_empty() {
            if !self.branch_changes.is_empty() {
                self.select_branch(index);
            }
            return;
        }
        self.active_section = ListSection::Uncommitted;
        self.selected = index.min(self.changes.len() - 1);
        self.selected_path = self.changes.get(self.selected).map(|change| change.path.clone());
        self.reveal = true;
    }

    fn select_branch(&mut self, index: usize) {
        if self.branch_changes.is_empty() {
            return;
        }
        self.active_section = ListSection::Branch;
        self.branch_selected = index.min(self.branch_changes.len() - 1);
        self.branch_selected_path = self.branch_changes.get(self.branch_selected).map(|change| change.path.clone());
        self.branch_reveal = true;
    }

    fn move_selection(&mut self, delta: isize) {
        let top_len = self.top_changes().len();
        let top_sel = if self.commit_offset == 0 { self.selected } else { self.commit_selected };
        match self.active_section {
            ListSection::Uncommitted => {
                if top_len == 0 {
                    if !self.branch_changes.is_empty() {
                        self.select_branch(0);
                    }
                    return;
                }
                let next = top_sel as isize + delta;
                if next >= top_len as isize && !self.branch_changes.is_empty() {
                    self.select_branch(0);
                } else {
                    self.select(next.clamp(0, top_len.saturating_sub(1) as isize) as usize);
                }
            }
            ListSection::Branch => {
                if self.branch_changes.is_empty() {
                    if top_len > 0 {
                        self.select(top_len - 1);
                    }
                    return;
                }
                let next = self.branch_selected as isize + delta;
                if next < 0 && top_len > 0 {
                    self.select(top_len - 1);
                } else {
                    self.select_branch(next.clamp(0, self.branch_changes.len().saturating_sub(1) as isize) as usize);
                }
            }
        }
    }

    fn group_state(&self, group: Group) -> StageState {
        let (mut staged, mut total, mut partial) = (0, 0, false);
        for change in self.changes.iter().filter(|change| change.group == group) {
            total += 1;
            match change.stage {
                StageState::Staged => staged += 1,
                StageState::PartiallyStaged => partial = true,
                StageState::Unstaged => {}
            }
        }
        if total > 0 && staged == total {
            StageState::Staged
        } else if staged > 0 || partial {
            StageState::PartiallyStaged
        } else {
            StageState::Unstaged
        }
    }

    fn staged_count(&self) -> usize {
        self.changes.iter().filter(|change| change.stage != StageState::Unstaged).count()
    }

    /// The paths to stage or unstage for a file: both ends of a rename.
    fn entry_paths(&self, change: &Change) -> Vec<PathBuf> {
        match self.status.as_ref().and_then(|status| status.entry(&change.path)) {
            Some(entry) => entry.paths().into_iter().map(Path::to_path_buf).collect(),
            None => vec![change.path.clone()],
        }
    }

    // ---- staging ---------------------------------------------------------------------

    fn stage(&self, paths: Vec<PathBuf>, stage: bool, cx: &mut Cx<'_>) -> bool {
        if paths.is_empty() {
            return false;
        }
        let label = match (&paths[..], stage) {
            ([one], true) => format!("Stage {}", one.display()),
            ([one], false) => format!("Unstage {}", one.display()),
            (many, true) => format!("Stage {} files", many.len()),
            (many, false) => format!("Unstage {} files", many.len()),
        };
        let _ = cx.project.git_job(label, move |repo| {
            if stage {
                repo.stage(&paths).map(|()| String::new())
            } else {
                repo.unstage(&paths).map(|()| String::new())
            }
        });
        true
    }

    fn toggle_group(&self, group: Group, cx: &mut Cx<'_>) {
        let stage = self.group_state(group) != StageState::Staged;
        let paths: Vec<PathBuf> = self.changes.iter().filter(|change| change.group == group).flat_map(|change| self.entry_paths(change)).collect();
        self.stage(paths, stage, cx);
    }

    // ---- discarding ------------------------------------------------------------------

    /// `git::RestoreFile`: the file in the action's `path` argument, else the selected one.
    fn restore_file(&mut self, action: &Action, cx: &mut Cx<'_>) -> bool {
        let skip_prompt = action.arg_bool("skip_prompt").unwrap_or(false);
        let rel = match action.arg_str("path") {
            Some(path) => PathBuf::from(path),
            None => match self.selected_change() {
                Some(change) => change.path.clone(),
                None => return false,
            },
        };
        match request_discard(cx, &rel, skip_prompt, "git::RestoreFile") {
            Discarding::Refused => false,
            Discarding::Asked => true,
            Discarding::Started { task, paths } => {
                self.pending.push(Pending { task, after: After::Changed(paths) });
                true
            }
        }
    }

    /// `git::RestoreTrackedFiles`: every tracked file back to the last commit. Always asks.
    fn restore_tracked(&mut self, action: &Action, cx: &mut Cx<'_>) -> bool {
        let Some(status) = self.status.clone() else { return false };
        let entries: Vec<&StatusEntry> = status.entries.iter().filter(|entry| !entry.untracked && !entry.conflicted).collect();
        if entries.is_empty() {
            cx.status("No tracked changes to discard");
            return false;
        }
        if action.arg_bool("confirmed") != Some(true) {
            let count = entries.len();
            cx.push(Effect::Ask(Question {
                message: format!("Discard the changes to {count} tracked {}?", if count == 1 { "file" } else { "files" }),
                detail: Some("Every tracked file goes back to how the last commit has it, and new files move to the Trash. This cannot be undone.".to_string()),
                answers: vec![
                    ("Discard all".to_string(), vec![Effect::Dispatch(Action::with_args("git::RestoreTrackedFiles", json!({ "confirmed": true })))]),
                    ("Cancel".to_string(), Vec::new()),
                ],
            }));
            return true;
        }
        let (mut restore, mut trash) = (Vec::new(), Vec::new());
        for entry in entries {
            if let Some(discard) = discard_of(entry) {
                restore.extend(discard.restore);
                trash.extend(discard.trash);
            }
        }
        let touched: Vec<PathBuf> = restore.iter().chain(&trash).filter_map(|path| cx.project.abs_path(path)).collect();
        let discard = Discard { restore, trash };
        let trash = trash_dir();
        let task = cx.project.git_job("Discard tracked changes", move |repo| run_discard(repo, &discard, trash.as_deref()).map(|()| String::new()));
        self.pending.push(Pending { task, after: After::Changed(touched) });
        true
    }

    /// `git::TrashUntrackedFiles`: every untracked file to the Trash. Always asks.
    fn trash_untracked(&mut self, action: &Action, cx: &mut Cx<'_>) -> bool {
        let Some(status) = self.status.clone() else { return false };
        let paths: Vec<PathBuf> = status.entries.iter().filter(|entry| entry.untracked).map(|entry| entry.path.clone()).collect();
        if paths.is_empty() {
            cx.status("No untracked files to move to the Trash");
            return false;
        }
        if action.arg_bool("confirmed") != Some(true) {
            let count = paths.len();
            cx.push(Effect::Ask(Question {
                message: format!("Move {count} untracked {} to the Trash?", if count == 1 { "file" } else { "files" }),
                detail: Some("Git has no copy of them.".to_string()),
                answers: vec![
                    ("Move to Trash".to_string(), vec![Effect::Dispatch(Action::with_args("git::TrashUntrackedFiles", json!({ "confirmed": true })))]),
                    ("Cancel".to_string(), Vec::new()),
                ],
            }));
            return true;
        }
        let touched: Vec<PathBuf> = paths.iter().filter_map(|path| cx.project.abs_path(path)).collect();
        let trash = trash_dir();
        let task = cx.project.git_job("Trash untracked files", move |repo| {
            for path in &paths {
                move_to_trash(trash.as_deref(), &repo.root().join(path)).map_err(trash_failure)?;
            }
            Ok(String::new())
        });
        self.pending.push(Pending { task, after: After::Changed(touched) });
        true
    }

    // ---- committing ------------------------------------------------------------------

    fn committing(&self) -> bool {
        self.pending.iter().any(|pending| matches!(pending.after, After::Commit))
    }

    fn message_text(&self) -> String {
        self.message.buffer().to_string()
    }

    fn set_message(&mut self, text: &str) {
        self.message = message_editor(text);
        self.message_top = 0;
        self.reveal_message = true;
    }

    fn focus_editor(&mut self, cx: &mut Cx<'_>) {
        self.focus = PanelFocus::Editor;
        self.reveal_message = true;
        cx.push(Effect::Focus(Focus::Panel(PanelId::Git)));
    }

    fn focus_changes(&mut self, cx: &mut Cx<'_>) {
        self.focus = PanelFocus::Changes;
        cx.push(Effect::Focus(Focus::Panel(PanelId::Git)));
    }

    /// `git::Commit`: commit what is staged with the message in the box (or amend, when
    /// the box is in amend mode).
    fn commit(&mut self, cx: &mut Cx<'_>) -> bool {
        if cx.project.repo().is_none() {
            cx.error("Not a git repository: there is nothing to commit to");
            return false;
        }
        if self.committing() {
            cx.status("A commit is already running");
            return true;
        }
        let message = self.message_text();
        if message.trim().is_empty() {
            cx.error("Write a commit message first");
            self.focus_editor(cx);
            return true;
        }
        let amend = self.amending.is_some();
        let merging = self.status.as_ref().is_some_and(|status| status.state == Some(RepoState::Merging));
        if !amend && !merging && self.staged_count() == 0 {
            cx.error("Nothing is staged: stage files with space, then commit");
            return true;
        }
        let task = cx.project.git_job(if amend { "Amend" } else { "Commit" }, move |repo| {
            let sha = repo.commit(&message, CommitOptions { amend, sign_off: false })?;
            let short: String = sha.chars().take(7).collect();
            Ok(format!("{} {short}", if amend { "Amended" } else { "Committed" }))
        });
        self.pending.push(Pending { task, after: After::Commit });
        true
    }

    /// `git::Amend`: the first press puts the last commit's message in the box and switches
    /// to amend mode; pressing it again (or `git::Commit`) amends.
    fn amend(&mut self, cx: &mut Cx<'_>) -> bool {
        if self.amending.is_some() {
            return self.commit(cx);
        }
        let Some(repo) = cx.project.repo().cloned() else {
            cx.error("Not a git repository: there is nothing to amend");
            return false;
        };
        if self.amend_load.is_none() {
            self.amend_load = Some(cx.project.spawn(move || {
                let message = repo.head_commit_message().map_err(|error| error.to_string())?;
                let sha = repo.head_sha().ok().flatten().unwrap_or_default();
                Ok(message.map(|text| (sha.chars().take(7).collect(), text)))
            }));
        }
        true
    }

    fn amend_loaded(&mut self, loaded: Result<LastCommit, String>, cx: &mut Cx<'_>) {
        match loaded {
            Ok(Some((sha, text))) => {
                let draft = self.message_text();
                self.amending = Some(Amending { draft, sha });
                self.set_message(text.trim_end());
                self.focus_editor(cx);
                cx.status("Amending the last commit: edit the message, then press the amend key again");
            }
            Ok(None) => cx.error("Nothing to amend: there are no commits yet"),
            Err(error) => cx.error(format!("Could not read the last commit: {error}")),
        }
    }

    /// `git::Cancel`: leave amend mode, putting back what was in the box; otherwise go back
    /// to the list.
    fn cancel(&mut self, cx: &mut Cx<'_>) -> bool {
        if let Some(amending) = self.amending.take() {
            self.set_message(&amending.draft);
            cx.status("Amend cancelled");
        } else {
            self.focus_changes(cx);
        }
        true
    }

    // ---- remotes ---------------------------------------------------------------------

    fn branch_name(&self) -> String {
        self.status.as_ref().and_then(|status| status.branch.clone()).unwrap_or_else(|| "HEAD".to_string())
    }

    fn remote(&mut self, action: &Action, cx: &mut Cx<'_>) -> bool {
        if cx.project.repo().is_none() {
            cx.error("Not a git repository: there is no remote to talk to");
            return false;
        }
        let branch = self.branch_name();
        match action.name.as_str() {
            "git::Fetch" => {
                let _ = cx.project.git_job("Fetch", |repo| repo.fetch().map(|output| summarize(&output, "Fetched".to_string())));
            }
            "git::Pull" | "git::PullRebase" => {
                let rebase = action.name == "git::PullRebase";
                let label = if rebase { format!("Pull --rebase {branch}") } else { format!("Pull {branch}") };
                let _ = cx.project.git_job(label, move |repo| repo.pull(rebase).map(|output| summarize(&output, format!("Pulled {branch}"))));
            }
            "git::Push" => {
                let _ = cx.project.git_job(format!("Push {branch}"), move |repo| repo.push(false).map(|output| summarize(&output, format!("Pushed {branch}"))));
            }
            _ => {
                if action.arg_bool("confirmed") != Some(true) {
                    cx.push(Effect::Ask(Question {
                        message: format!("Force push {branch}?"),
                        detail: Some("This replaces the remote branch with yours, unless someone else pushed to it since you last fetched (--force-with-lease).".to_string()),
                        answers: vec![
                            ("Force push".to_string(), vec![Effect::Dispatch(Action::with_args("git::ForcePush", json!({ "confirmed": true })))]),
                            ("Cancel".to_string(), Vec::new()),
                        ],
                    }));
                    return true;
                }
                let label = format!("Force push {branch}");
                let _ = cx.project.git_job(label, move |repo| repo.push(true).map(|output| summarize(&output, format!("Force pushed {branch}"))));
            }
        }
        true
    }

    // ---- counts of added and removed lines -------------------------------------------

    fn kick_counts(&mut self, project: &Project) {
        self.kick_branch_diff(project);
        self.kick_commits(project);
        self.kick_commit_diff(project);
        if self.counts_task.is_some() || !self.counts_stale {
            return;
        }
        let Some(repo) = project.repo().cloned() else {
            self.counts_stale = false;
            return;
        };
        if let Some(started) = self.counts_started {
            let wait = COUNTS_INTERVAL.saturating_sub(started.elapsed());
            if !wait.is_zero() {
                project.waker().wake_after(wait);
                return;
            }
        }
        self.counts_stale = false;
        self.counts_started = Some(Instant::now());
        self.counts_task = Some(project.spawn(move || {
            let mut counts = HashMap::new();
            if let Ok(files) = repo.diff_files(DiffTarget::HeadToWorktree) {
                for file in files {
                    counts.insert(file.path.clone(), (file.additions(), file.deletions()));
                }
            }
            counts
        }));
    }

    fn kick_branch_diff(&mut self, project: &Project) {
        if self.branch_task.is_some() || !self.branch_stale {
            return;
        }
        let Some(repo) = project.repo().cloned() else {
            self.branch_stale = false;
            return;
        };
        let base_ref = self.branch_base.clone();
        let status_snapshot = self.status.clone();
        self.branch_stale = false;
        self.branch_task = Some(project.spawn(move || {
            let files = repo.diff_against_ref(&base_ref).map_err(|error| error.to_string())?;
            let mut counts = HashMap::new();
            let mut changes = Vec::with_capacity(files.len());
            for file in files {
                counts.insert(file.path.clone(), (file.additions(), file.deletions()));
                let (status, group) = match file.status {
                    DiffStatus::Added => (DisplayStatus::Added, Group::Tracked),
                    DiffStatus::Deleted => (DisplayStatus::Deleted, Group::Tracked),
                    DiffStatus::Modified => (DisplayStatus::Modified, Group::Tracked),
                    DiffStatus::Renamed | DiffStatus::Copied => (DisplayStatus::Renamed, Group::Tracked),
                    DiffStatus::Untracked => (DisplayStatus::Untracked, Group::Untracked),
                    DiffStatus::Conflicted => (DisplayStatus::Conflict, Group::Conflicts),
                };
                let stage = status_snapshot
                    .as_deref()
                    .and_then(|st| st.entry(&file.path))
                    .map(StatusEntry::stage_state)
                    .unwrap_or(StageState::Unstaged);
                changes.push(Change { path: file.path, status, stage, group });
            }
            changes.sort_by_key(|change| match change.group {
                Group::Conflicts => 0,
                Group::Tracked => 1,
                Group::Untracked => 2,
            });
            Ok((base_ref, changes, counts))
        }));
    }

    fn kick_commits(&mut self, project: &Project) {
        if self.commits_task.is_some() || !self.commits_stale {
            return;
        }
        let Some(repo) = project.repo().cloned() else {
            self.commits_stale = false;
            return;
        };
        self.commits_stale = false;
        self.commits_task = Some(project.spawn(move || repo.log(MAX_HISTORY_COMMITS, None).unwrap_or_default()));
    }

    fn kick_commit_diff(&mut self, project: &Project) {
        let Some(commit) = self.viewing_commit() else { return };
        let sha = commit.sha.clone();
        if self.commit_loaded_sha.as_deref() == Some(sha.as_str()) || self.commit_task.is_some() {
            return;
        }
        let Some(repo) = project.repo().cloned() else { return };
        self.commit_task = Some(project.spawn(move || {
            let files = repo.diff_commit(&sha).map_err(|error| error.to_string())?;
            let mut counts = HashMap::new();
            let mut changes = Vec::with_capacity(files.len());
            for file in files {
                counts.insert(file.path.clone(), (file.additions(), file.deletions()));
                let status = match file.status {
                    DiffStatus::Added | DiffStatus::Untracked => DisplayStatus::Added,
                    DiffStatus::Deleted => DisplayStatus::Deleted,
                    DiffStatus::Modified => DisplayStatus::Modified,
                    DiffStatus::Renamed | DiffStatus::Copied => DisplayStatus::Renamed,
                    DiffStatus::Conflicted => DisplayStatus::Conflict,
                };
                changes.push(Change { path: file.path, status, stage: StageState::Unstaged, group: Group::Tracked });
            }
            Ok((sha, changes, counts))
        }));
    }

    // ---- the message editor ----------------------------------------------------------

    fn run_message(&mut self, command: Command, cx: &mut Cx<'_>) -> bool {
        if self.committing() {
            return true;
        }
        let rows = self.message_text.height.max(1) as usize;
        let outcome = self.message.apply(command.with_page_rows(rows));
        if let Some(clip) = outcome.clipboard {
            cx.clipboard.set(&clip.text, clip.linewise);
        }
        self.reveal_message = true;
        true
    }

    /// An `editor::` action for the message box. `None` means it is not one.
    fn message_action(&mut self, action: &Action, cx: &mut Cx<'_>) -> Option<bool> {
        if !action.name.starts_with("editor::") {
            return None;
        }
        match action.name.as_str() {
            "editor::Paste" => {
                let Some(clip) = cx.clipboard.get() else { return Some(true) };
                let piece = ClipboardPiece { text: clip.text, linewise: false };
                return Some(self.run_message(Command::Paste(TextClipboard::from_pieces(vec![piece])), cx));
            }
            // Escape collapses a selection if there is one; otherwise `git::Cancel` has it.
            "editor::Cancel" => {
                let outcome = self.message.apply(Command::Cancel);
                return Some(outcome.selections_changed || !outcome.is_noop());
            }
            _ => {}
        }
        let Some(command) = Command::from_action(&action.name, action.args.as_ref()) else { return Some(false) };
        Some(self.run_message(command, cx))
    }

    fn visual_rows(&self) -> Vec<VisualRow> {
        let mut rows = Vec::new();
        for line in 0..self.message.buffer().len_lines() {
            for range in self.message.line_rows(line) {
                rows.push(VisualRow { line, range });
            }
        }
        rows
    }

    /// The visual row and display column of the message's cursor.
    fn message_cursor(&self) -> (usize, usize) {
        let position = self.message.display_position(self.message.selections().primary().head);
        let before: usize = (0..position.line).map(|line| self.message.line_rows(line).len()).sum();
        (before + position.row, position.col)
    }

    fn message_position_at(&self, col: u16, row: u16) -> usize {
        let index = (self.message_top + row.saturating_sub(self.message_text.y) as usize).min(self.visual.len().saturating_sub(1));
        let Some(visual) = self.visual.get(index) else { return 0 };
        let first = self.message.line_rows(visual.line).iter().position(|range| *range == visual.range).unwrap_or(0);
        let x = col.saturating_sub(self.message_text.x) as usize;
        self.message.position_at(DisplayPosition { line: visual.line, row: first, col: x })
    }
}

// ---- drawing -----------------------------------------------------------------------------

/// Where the parts of the panel go.
struct Layout {
    header: Rect,
    list: Rect,
    branch_header: Rect,
    branch_list: Rect,
    message: Rect,
    buttons: Rect,
}

/// The header on top, the button row at the bottom, the message box above it with a gap,
/// and the split changes lists in between (top: uncommitted changes, bottom: changes since
/// the base branch). A short panel gives up the message box and bottom split before the top list.
fn layout(area: Rect, message_rows: usize) -> Layout {
    let header = Rect::new(area.x, area.y, area.width, 1.min(area.height));
    let below = area.height - header.height;
    let buttons_h = 1.min(below);
    let room = below.saturating_sub(buttons_h + MIN_LIST_ROWS);
    let message_h = if room >= 2 { (message_rows as u16).clamp(1, room - 1) } else { 0 };
    let gap = u16::from(message_h > 0);
    let lists_h = below - buttons_h - message_h - gap;
    let (list, branch_header, branch_list) = if lists_h >= 8 {
        let top_h = (lists_h - 1) / 2;
        let bottom_h = lists_h - top_h - 1;
        let top = Rect::new(area.x, area.y + header.height, area.width, top_h);
        let b_header = Rect::new(area.x, top.bottom(), area.width, 1);
        let bottom = Rect::new(area.x, b_header.bottom(), area.width, bottom_h);
        (top, b_header, bottom)
    } else {
        (Rect::new(area.x, area.y + header.height, area.width, lists_h), Rect::default(), Rect::default())
    };
    let message = Rect::new(area.x, area.y + header.height + lists_h + gap, area.width, message_h);
    let buttons = Rect::new(area.x, message.bottom(), area.width, buttons_h);
    Layout { header, list, branch_header, branch_list, message, buttons }
}

/// A button whose right edge is at `right` (exclusive). Returns where it was drawn.
fn button(canvas: &mut Canvas<'_>, right: u16, y: u16, label: &str, enabled: bool, theme: &Theme) -> Rect {
    let width = ui::width(label) as u16 + 2;
    let x = right.saturating_sub(width);
    let fg = if enabled { theme.text } else { theme.text_muted };
    canvas.fill(Rect::new(x, y, width, 1), theme.element_hover);
    canvas.text(x + 1, y, label, Style::colors(fg, theme.element_hover), width - 1);
    Rect::new(x, y, width, 1)
}

impl GitPanel {
    /// Draw a checkbox at `x`; returns the columns it and the gap after it take.
    fn checkbox(&self, canvas: &mut Canvas<'_>, x: u16, y: u16, state: StageState, bg: Rgb, cx: &RenderCx<'_>) -> u16 {
        let theme = cx.theme;
        let (glyph, color) = match (cx.settings.icons, state) {
            (Icons::Nerd, StageState::Staged) => ("\u{f14a}", theme.text_accent),
            (Icons::Nerd, StageState::PartiallyStaged) => ("\u{f147}", theme.text_accent),
            (Icons::Nerd, StageState::Unstaged) => ("\u{f096}", theme.text_muted),
            (Icons::Ascii, StageState::Staged) => ("[x]", theme.text_accent),
            (Icons::Ascii, StageState::PartiallyStaged) => ("[-]", theme.text_accent),
            (Icons::Ascii, StageState::Unstaged) => ("[ ]", theme.text_muted),
        };
        let width = checkbox_width(cx.settings.icons);
        canvas.text(x, y, glyph, Style::colors(color, bg), width);
        width + 1
    }

    fn draw_header(&mut self, canvas: &mut Canvas<'_>, area: Rect, cx: &RenderCx<'_>) {
        let theme = cx.theme;
        let bg = theme.panel_bg;
        self.prev_commit_button = Rect::default();
        self.next_commit_button = Rect::default();
        if area.height == 0 {
            return;
        }
        let inner = area.width.saturating_sub(2);
        if let Some(commit) = self.viewing_commit().cloned() {
            let mut x = area.x + 1;
            let has_older = self.commit_offset < self.commits.len();
            let left_col = if has_older { theme.text_accent } else { theme.text_muted };
            canvas.text(x, area.y, "←", Style::colors(left_col, bg).bold(), 1);
            if has_older {
                self.prev_commit_button = Rect::new(x, area.y, 2, 1);
            }
            x += 2;
            let right_arrow = "→";
            let count_str = if self.commit_task.is_some() {
                "…".to_string()
            } else {
                match self.commit_changes.len() {
                    0 => String::new(),
                    1 => "1 file".to_string(),
                    n => format!("{n} files"),
                }
            };
            let right_w = ui::width(&count_str) as u16 + if count_str.is_empty() { 1 } else { 3 };
            let end = area.right().saturating_sub(1 + right_w + 1);
            if x < end {
                canvas.text_truncated(x, area.y, &commit.short, Style::colors(theme.text_accent, bg).bold(), end - x);
            }
            let arrow_x = area.right().saturating_sub(2);
            canvas.text(arrow_x, area.y, right_arrow, Style::colors(theme.text_accent, bg).bold(), 1);
            self.next_commit_button = Rect::new(arrow_x.saturating_sub(1), area.y, 2, 1);
            if !count_str.is_empty() && arrow_x > area.x + 2 {
                canvas.text_right(arrow_x.saturating_sub(2), area.y, &count_str, Style::colors(theme.text_muted, bg));
            }
            return;
        }
        let repo_label = cx.project.active_repo_label().map(str::to_string);
        let multi_repo = cx.project.repos().len() > 1;
        let Some(status) = &self.status else {
            if let Some(label) = repo_label {
                let text = if multi_repo { format!("{label} ▾") } else { label };
                let used = canvas.text_truncated(area.x + 1, area.y, &text, Style::colors(theme.text_accent, bg).bold(), inner);
                if multi_repo {
                    self.repo_button = Rect::new(area.x + 1, area.y, used, 1);
                }
            } else {
                canvas.text_truncated(area.x + 1, area.y, "Git", Style::colors(theme.text, bg).bold(), inner);
            }
            return;
        };
        // The right side: the job that is running, else a ← history cue + uncommitted count.
        let history_hint = !self.commits.is_empty() && cx.project.busy().is_none();
        let right = match cx.project.busy() {
            Some(job) => format!("{job}…"),
            None => match (history_hint, self.changes.len()) {
                (true, 0) => "←".to_string(),
                (false, 0) => String::new(),
                (true, 1) => "← 1 change".to_string(),
                (false, 1) => "1 change".to_string(),
                (true, n) => format!("← {n} changes"),
                (false, n) => format!("{n} changes"),
            },
        };
        let right_accent = false;
        let right_width = ui::width(&right) as u16;
        let end = area.x + 1 + if right.is_empty() { inner } else { inner.saturating_sub(right_width + 2) };
        let mut x = area.x + 1;
        let put = |canvas: &mut Canvas<'_>, x: &mut u16, text: &str, style: Style| -> u16 {
            if *x < end {
                let used = canvas.text_truncated(*x, area.y, text, style, end - *x);
                *x += used;
                used
            } else {
                0
            }
        };
        if let Some(label) = repo_label {
            let start = x;
            let text = if multi_repo { format!("{label} ▾") } else { label };
            let used = put(canvas, &mut x, &text, Style::colors(theme.text_accent, bg).bold());
            if used > 0 && multi_repo {
                self.repo_button = Rect::new(start, area.y, used, 1);
            }
            put(canvas, &mut x, " / ", Style::colors(theme.text_muted, bg));
        }
        let (name, detached) = match (&status.branch, &status.head) {
            (Some(branch), _) => (branch.clone(), false),
            (None, Some(head)) => (format!("detached at {head}"), true),
            (None, None) => ("no branch".to_string(), true),
        };
        let branch_start = x;
        if cx.settings.icons == Icons::Nerd {
            put(canvas, &mut x, "\u{e0a0} ", Style::colors(theme.text_muted, bg));
        }
        put(canvas, &mut x, &name, Style::colors(if detached { theme.warning } else { theme.text }, bg).bold());
        if x > branch_start {
            self.branch_button = Rect::new(branch_start, area.y, x - branch_start, 1);
        }
        if status.ahead > 0 {
            put(canvas, &mut x, &format!("  ↑{}", status.ahead), Style::colors(theme.created, bg));
        }
        if status.behind > 0 {
            put(canvas, &mut x, &format!("  ↓{}", status.behind), Style::colors(theme.modified, bg));
        }
        if let Some(state) = status.state {
            let label = match state {
                RepoState::Merging => "merging",
                RepoState::Rebasing => "rebasing",
                RepoState::CherryPicking => "cherry-picking",
                RepoState::Reverting => "reverting",
                RepoState::Bisecting => "bisecting",
            };
            put(canvas, &mut x, &format!("  {label}"), Style::colors(theme.warning, bg).bold());
        }
        if status.upstream_gone {
            put(canvas, &mut x, "  upstream gone", Style::colors(theme.warning, bg));
        }
        if !right.is_empty() {
            let color = if right_accent { theme.text_accent } else { theme.text_muted };
            let style = if right_accent { Style::colors(color, bg).bold() } else { Style::colors(color, bg) };
            canvas.text_right(area.right() - 1, area.y, &right, style);
            if cx.project.busy().is_none() {
                let x = (area.right() - 1).saturating_sub(right_width);
                if history_hint {
                    self.prev_commit_button = Rect::new(x, area.y, 2, 1);
                    if right_width > 2 {
                        self.mode_button = Rect::new(x + 2, area.y, right_width - 2, 1);
                    }
                } else {
                    self.mode_button = Rect::new(x, area.y, right_width, 1);
                }
            }
        }
    }

    fn draw_empty(&self, canvas: &mut Canvas<'_>, area: Rect, cx: &RenderCx<'_>) {
        let theme = cx.theme;
        let bg = theme.panel_bg;
        let text = Style::colors(theme.text, bg);
        let muted = Style::colors(theme.text_muted, bg);
        let mut lines: Vec<(String, Style)> = Vec::new();
        if cx.project.repo().is_none() {
            lines.push(("Not a git repository".to_string(), text));
            lines.push(("Run git init in the terminal to track changes".to_string(), muted));
        } else if let Some(status) = &self.status {
            lines.push(("No changes".to_string(), text));
            lines.push(("Everything is committed. Edit a file and it appears here".to_string(), muted));
            if status.ahead > 0 {
                let (word, it) = if status.ahead == 1 { ("commit", "it") } else { ("commits", "them") };
                lines.push((format!("{} {word} not pushed yet: push to publish {it}", status.ahead), muted));
            }
        } else {
            lines.push(("Reading the status…".to_string(), muted));
        }
        for (offset, (line, style)) in lines.iter().enumerate() {
            if (offset as u16) < area.height {
                canvas.text_truncated(area.x + 1, area.y + offset as u16, line, *style, area.width.saturating_sub(2));
            }
        }
    }

    fn draw_list(&mut self, canvas: &mut Canvas<'_>, area: Rect, cx: &RenderCx<'_>) {
        let theme = cx.theme;
        let bg = theme.panel_bg;
        canvas.fill(area, bg);
        if area.height == 0 || area.width < 6 {
            return;
        }
        if self.commit_offset > 0 {
            self.draw_commit_list(canvas, area, cx);
            return;
        }
        if self.changes.is_empty() {
            self.draw_empty(canvas, area, cx);
            return;
        }
        let rows = self.list_rows();
        let visible = area.height as usize;
        let selected_row = rows.iter().position(|row| *row == ListRow::Change(self.selected));
        if self.reveal {
            if let Some(selected_row) = selected_row {
                // The first file of a group brings its header with it.
                let top = if selected_row > 0 && matches!(rows[selected_row - 1], ListRow::Header(_)) { selected_row - 1 } else { selected_row };
                if top < self.scroll {
                    self.scroll = top;
                } else if selected_row >= self.scroll + visible {
                    self.scroll = selected_row + 1 - visible;
                }
            }
            self.reveal = false;
        }
        self.scroll = self.scroll.min(rows.len().saturating_sub(visible));
        let scrollbar = rows.len() > visible && area.width > 8;
        let width = area.width - u16::from(scrollbar);
        if scrollbar {
            let marks = ScrollMarks { marks: Vec::new(), cursor_line: selected_row };
            paint::scrollbar(canvas, area.right() - 1, area.y, area.height, rows.len(), self.scroll, visible, &marks, theme);
        }
        // The columns of the counts are as wide as the widest in view, so the numbers line up.
        let (mut add_w, mut del_w) = (0u16, 0u16);
        for row in rows.iter().skip(self.scroll).take(visible) {
            if let ListRow::Change(index) = row {
                if let Some(&(added, removed)) = self.counts.get(&self.changes[*index].path) {
                    if added > 0 {
                        add_w = add_w.max(digits(added) + 1);
                    }
                    if removed > 0 {
                        del_w = del_w.max(digits(removed) + 1);
                    }
                }
            }
        }
        let counts_w = add_w + del_w + u16::from(add_w > 0 && del_w > 0);
        // Too narrow for the numbers: the name matters more.
        let show_counts = counts_w > 0 && width > counts_w + 16;
        for (offset, row) in rows.iter().skip(self.scroll).take(visible).enumerate() {
            let y = area.y + offset as u16;
            let line = Rect::new(area.x, y, width, 1);
            let ListRow::Change(index) = row else {
                if let ListRow::Header(group) = row {
                    self.draw_group_header(canvas, line, *group, cx);
                }
                continue;
            };
            let change = &self.changes[*index];
            let is_selected = self.active_section == ListSection::Uncommitted && *index == self.selected;
            let row_bg = if is_selected { theme.element_selected } else { bg };
            canvas.fill(line, row_bg);
            let mut x = area.x + 1;
            x += self.checkbox(canvas, x, y, change.stage, row_bg, cx);
            canvas.text(x, y, status_letter(change.status), Style::colors(status_color(change.status, theme), row_bg).bold(), 1);
            x += 2;
            let mut right = line.right() - 1;
            if show_counts {
                let (added, removed) = self.counts.get(&change.path).copied().unwrap_or((0, 0));
                if del_w > 0 {
                    if removed > 0 {
                        canvas.text_right(right, y, &format!("−{removed}"), Style::colors(theme.deleted, row_bg));
                    }
                    right = right.saturating_sub(del_w + 1);
                }
                if add_w > 0 {
                    if added > 0 {
                        canvas.text_right(right, y, &format!("+{added}"), Style::colors(theme.created, row_bg));
                    }
                    right = right.saturating_sub(add_w + 1);
                }
            }
            let room = right.saturating_sub(x);
            let name = change.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let name_color = if change.status == DisplayStatus::Deleted { theme.text_muted } else { theme.text };
            let used = canvas.text_truncated(x, y, &name, Style::colors(name_color, row_bg), room);
            let folder = change.path.parent().map(Path::to_string_lossy).unwrap_or_default().into_owned();
            let left = room.saturating_sub(used + 2);
            if !folder.is_empty() && left >= 4 {
                canvas.text(x + used + 2, y, &ui::truncate_start(&folder, left as usize), Style::colors(theme.text_muted, row_bg), left);
            }
        }
    }

    fn draw_commit_list(&mut self, canvas: &mut Canvas<'_>, area: Rect, cx: &RenderCx<'_>) {
        let theme = cx.theme;
        let bg = theme.panel_bg;
        self.commit_header_rows = 0;
        let list_area = if area.height >= 2 {
            let mut used_rows: u16 = 1;
            if let Some(commit) = self.viewing_commit() {
                let max_w = area.width.saturating_sub(2);
                let mut x = area.x + 1;
                let used = canvas.text_truncated(x, area.y, &commit.author, Style::colors(theme.text_accent, bg), max_w);
                x += used;
                let rem = (area.x + 1 + max_w).saturating_sub(x);
                if rem > 3 && !commit.relative_time.is_empty() {
                    canvas.text_truncated(x, area.y, &format!(" · {}", commit.relative_time), Style::colors(theme.text_muted, bg), rem);
                }

                // Wrap the commit subject across as many rows as needed so it is fully readable
                // without overflowing, while always keeping room for the file list below.
                let min_list_rows: u16 = if area.height >= 4 { 2 } else { 1 };
                let max_subject_rows = area.height.saturating_sub(used_rows + min_list_rows) as usize;
                if max_subject_rows > 0 && max_w > 0 && !commit.subject.is_empty() {
                    let wrapped = wrap_words(&commit.subject, max_w as usize);
                    let show = wrapped.len().min(max_subject_rows);
                    for (i, line) in wrapped.iter().take(show).enumerate() {
                        let y = area.y + used_rows + i as u16;
                        if i + 1 == show && wrapped.len() > show {
                            let rest = wrapped[i..].join(" ");
                            canvas.text_truncated(area.x + 1, y, &rest, Style::colors(theme.text, bg), max_w);
                        } else {
                            canvas.text_truncated(area.x + 1, y, line, Style::colors(theme.text, bg), max_w);
                        }
                    }
                    used_rows += show as u16;
                }
            }
            self.commit_header_rows = used_rows;
            Rect::new(area.x, area.y + used_rows, area.width, area.height.saturating_sub(used_rows))
        } else {
            area
        };
        if let Some(error) = &self.commit_error {
            canvas.text_truncated(list_area.x + 1, list_area.y, error, Style::colors(theme.error, bg), list_area.width.saturating_sub(2));
            return;
        }
        if self.commit_changes.is_empty() {
            let msg = if self.commit_task.is_some() { "Loading commit…" } else { "No file changes in this commit" };
            canvas.text_truncated(list_area.x + 1, list_area.y, msg, Style::colors(theme.text_muted, bg), list_area.width.saturating_sub(2));
            return;
        }
        let visible = list_area.height as usize;
        if self.commit_reveal {
            if self.commit_selected < self.commit_scroll {
                self.commit_scroll = self.commit_selected;
            } else if self.commit_selected >= self.commit_scroll + visible {
                self.commit_scroll = self.commit_selected + 1 - visible;
            }
            self.commit_reveal = false;
        }
        self.commit_scroll = self.commit_scroll.min(self.commit_changes.len().saturating_sub(visible));
        let scrollbar = self.commit_changes.len() > visible && list_area.width > 8;
        let width = list_area.width - u16::from(scrollbar);
        if scrollbar {
            let marks = ScrollMarks { marks: Vec::new(), cursor_line: Some(self.commit_selected) };
            paint::scrollbar(canvas, list_area.right() - 1, list_area.y, list_area.height, self.commit_changes.len(), self.commit_scroll, visible, &marks, theme);
        }
        let (mut add_w, mut del_w) = (0u16, 0u16);
        for change in self.commit_changes.iter().skip(self.commit_scroll).take(visible) {
            if let Some(&(added, removed)) = self.commit_counts.get(&change.path) {
                if added > 0 {
                    add_w = add_w.max(digits(added) + 1);
                }
                if removed > 0 {
                    del_w = del_w.max(digits(removed) + 1);
                }
            }
        }
        let counts_w = add_w + del_w + u16::from(add_w > 0 && del_w > 0);
        let show_counts = counts_w > 0 && width > counts_w + 14;
        for (offset, change) in self.commit_changes.iter().skip(self.commit_scroll).take(visible).enumerate() {
            let index = self.commit_scroll + offset;
            let y = list_area.y + offset as u16;
            let line = Rect::new(list_area.x, y, width, 1);
            let is_selected = self.active_section == ListSection::Uncommitted && index == self.commit_selected;
            let row_bg = if is_selected { theme.element_selected } else { bg };
            canvas.fill(line, row_bg);
            let mut x = area.x + 1;
            canvas.text(x, y, status_letter(change.status), Style::colors(status_color(change.status, theme), row_bg).bold(), 1);
            x += 2;
            let mut right = line.right() - 1;
            if show_counts {
                let (added, removed) = self.commit_counts.get(&change.path).copied().unwrap_or((0, 0));
                if del_w > 0 {
                    if removed > 0 {
                        canvas.text_right(right, y, &format!("−{removed}"), Style::colors(theme.deleted, row_bg));
                    }
                    right = right.saturating_sub(del_w + 1);
                }
                if add_w > 0 {
                    if added > 0 {
                        canvas.text_right(right, y, &format!("+{added}"), Style::colors(theme.created, row_bg));
                    }
                    right = right.saturating_sub(add_w + 1);
                }
            }
            let room = right.saturating_sub(x);
            let name = change.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let name_color = if change.status == DisplayStatus::Deleted { theme.text_muted } else { theme.text };
            let used = canvas.text_truncated(x, y, &name, Style::colors(name_color, row_bg), room);
            let folder = change.path.parent().map(Path::to_string_lossy).unwrap_or_default().into_owned();
            let left = room.saturating_sub(used + 2);
            if !folder.is_empty() && left >= 4 {
                canvas.text(x + used + 2, y, &ui::truncate_start(&folder, left as usize), Style::colors(theme.text_muted, row_bg), left);
            }
        }
    }

    fn draw_group_header(&self, canvas: &mut Canvas<'_>, line: Rect, group: Group, cx: &RenderCx<'_>) {
        let theme = cx.theme;
        let bg = theme.panel_bg;
        let mut x = line.x + 1;
        x += self.checkbox(canvas, x, line.y, self.group_state(group), bg, cx);
        let count = self.changes.iter().filter(|change| change.group == group).count();
        let color = if group == Group::Conflicts { theme.conflict } else { theme.text_muted };
        let used = canvas.text_truncated(x, line.y, group.title(), Style::colors(color, bg).bold(), line.right().saturating_sub(x + 1));
        let count_x = x + used + 2;
        if count_x < line.right() {
            canvas.text_truncated(count_x, line.y, &count.to_string(), Style::colors(theme.text_muted, bg), line.right() - count_x);
        }
    }

    fn draw_branch_header(&self, canvas: &mut Canvas<'_>, area: Rect, cx: &RenderCx<'_>) {
        if area.height == 0 || area.width < 6 {
            return;
        }
        let theme = cx.theme;
        let bg = theme.elevated_surface;
        canvas.fill(area, bg);
        let right = if self.branch_task.is_some() {
            "…".to_string()
        } else {
            format!("{} ▾", self.branch_changes.len())
        };
        let right_w = ui::width(&right) as u16;
        let title_color = if self.active_section == ListSection::Branch { theme.text_accent } else { theme.text };
        let title = format!("Since {}", self.branch_base);
        let room = area.width.saturating_sub(right_w + 3);
        canvas.text_truncated(area.x + 1, area.y, &title, Style::colors(title_color, bg).bold(), room);
        canvas.text_right(area.right() - 1, area.y, &right, Style::colors(theme.text_muted, bg));
    }

    fn draw_branch_list(&mut self, canvas: &mut Canvas<'_>, area: Rect, cx: &RenderCx<'_>) {
        let theme = cx.theme;
        let bg = theme.panel_bg;
        canvas.fill(area, bg);
        if area.height == 0 || area.width < 6 {
            return;
        }
        if self.branch_changes.is_empty() {
            let msg = if self.branch_task.is_some() || self.branch_stale {
                format!("Comparing against {}…", self.branch_base)
            } else if let Some(err) = &self.branch_error {
                err.clone()
            } else {
                format!("No changes since {}", self.branch_base)
            };
            canvas.text_truncated(area.x + 1, area.y, &msg, Style::colors(theme.text_muted, bg), area.width.saturating_sub(2));
            return;
        }
        let visible = area.height as usize;
        if self.branch_reveal {
            if self.branch_selected < self.branch_scroll {
                self.branch_scroll = self.branch_selected;
            } else if self.branch_selected >= self.branch_scroll + visible {
                self.branch_scroll = self.branch_selected + 1 - visible;
            }
            self.branch_reveal = false;
        }
        self.branch_scroll = self.branch_scroll.min(self.branch_changes.len().saturating_sub(visible));
        let scrollbar = self.branch_changes.len() > visible && area.width > 8;
        let width = area.width - u16::from(scrollbar);
        if scrollbar {
            let cursor_line = (self.active_section == ListSection::Branch).then_some(self.branch_selected);
            let marks = ScrollMarks { marks: Vec::new(), cursor_line };
            paint::scrollbar(canvas, area.right() - 1, area.y, area.height, self.branch_changes.len(), self.branch_scroll, visible, &marks, theme);
        }
        let (mut add_w, mut del_w) = (0u16, 0u16);
        for change in self.branch_changes.iter().skip(self.branch_scroll).take(visible) {
            if let Some(&(added, removed)) = self.branch_counts.get(&change.path) {
                if added > 0 {
                    add_w = add_w.max(digits(added) + 1);
                }
                if removed > 0 {
                    del_w = del_w.max(digits(removed) + 1);
                }
            }
        }
        let counts_w = add_w + del_w + u16::from(add_w > 0 && del_w > 0);
        let show_counts = counts_w > 0 && width > counts_w + 14;
        for (offset, change) in self.branch_changes.iter().skip(self.branch_scroll).take(visible).enumerate() {
            let index = self.branch_scroll + offset;
            let y = area.y + offset as u16;
            let line = Rect::new(area.x, y, width, 1);
            let is_selected = self.active_section == ListSection::Branch && index == self.branch_selected;
            let row_bg = if is_selected { theme.element_selected } else { bg };
            canvas.fill(line, row_bg);
            let mut x = area.x + 1;
            canvas.text(x, y, status_letter(change.status), Style::colors(status_color(change.status, theme), row_bg).bold(), 1);
            x += 2;
            let mut right = line.right() - 1;
            if show_counts {
                let (added, removed) = self.branch_counts.get(&change.path).copied().unwrap_or((0, 0));
                if del_w > 0 {
                    if removed > 0 {
                        canvas.text_right(right, y, &format!("−{removed}"), Style::colors(theme.deleted, row_bg));
                    }
                    right = right.saturating_sub(del_w + 1);
                }
                if add_w > 0 {
                    if added > 0 {
                        canvas.text_right(right, y, &format!("+{added}"), Style::colors(theme.created, row_bg));
                    }
                    right = right.saturating_sub(add_w + 1);
                }
            }
            let room = right.saturating_sub(x);
            let name = change.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let name_color = if change.status == DisplayStatus::Deleted { theme.text_muted } else { theme.text };
            let used = canvas.text_truncated(x, y, &name, Style::colors(name_color, row_bg), room);
            let folder = change.path.parent().map(Path::to_string_lossy).unwrap_or_default().into_owned();
            let left = room.saturating_sub(used + 2);
            if !folder.is_empty() && left >= 4 {
                canvas.text(x + used + 2, y, &ui::truncate_start(&folder, left as usize), Style::colors(theme.text_muted, row_bg), left);
            }
        }
    }

    fn draw_message(&mut self, canvas: &mut Canvas<'_>, area: Rect, cx: &RenderCx<'_>) -> Option<Cursor> {
        let theme = cx.theme;
        // The box is a band of its own colour: no border, a row of padding above and below.
        let bg = theme.elevated_surface;
        canvas.fill(area, bg);
        if area.height == 0 || area.width < 4 {
            return None;
        }
        let pad = u16::from(area.height >= 4);
        let text_area = Rect::new(area.x + 1, area.y + pad, area.width - 2, area.height - 2 * pad);
        self.message_text = text_area;
        let rows = text_area.height as usize;
        let (cursor_row, cursor_col) = self.message_cursor();
        if self.reveal_message {
            if cursor_row < self.message_top {
                self.message_top = cursor_row;
            } else if cursor_row >= self.message_top + rows {
                self.message_top = cursor_row + 1 - rows;
            }
            self.reveal_message = false;
        }
        self.message_top = self.message_top.min(self.visual.len().saturating_sub(rows));
        let buffer = self.message.buffer();
        let selections: Vec<Range<usize>> = self.message.selections().iter().filter(|s| !s.is_empty()).map(|s| s.range()).collect();
        let options = PaintOptions { tab_size: 4, left: 0, show_indent_guides: false, colorize_brackets: false, show_whitespace_in_selection: false };
        let editor_bg = ui::color(theme.editor_bg);
        for (offset, visual) in self.visual.iter().skip(self.message_top).take(rows).enumerate() {
            let y = text_area.y + offset as u16;
            let text = buffer.line_text(visual.line);
            let line_start = buffer.line_to_char(visual.line);
            let line_len = buffer.line_len(visual.line);
            let local: Vec<Range<usize>> = selections
                .iter()
                .filter(|range| range.end > line_start && range.start <= line_start + line_len)
                .map(|range| range.start.max(line_start) - line_start..range.end.min(line_start + line_len + 1) - line_start)
                .collect();
            let mut row = Row::plain(&text);
            row.segment = visual.range.clone();
            row.selections = &local;
            paint::row(canvas, text_area.x, y, text_area.width, &row, &options, theme);
            // The row painter fills with the editor's colour: put the box's own back.
            for x in text_area.x..text_area.right() {
                if let Some(cell) = canvas.cell_mut(x, y) {
                    if cell.bg == editor_bg {
                        cell.bg = ui::color(bg);
                    }
                }
            }
        }
        // The placeholder goes over the (empty) first row, once the rows are painted.
        if buffer.is_empty() {
            let hint = if self.amending.is_some() { "Amended commit message" } else { "Commit message" };
            canvas.text_truncated(text_area.x, text_area.y, hint, Style::colors(theme.text_placeholder, bg), text_area.width);
        }
        let shown = cursor_row >= self.message_top && cursor_row < self.message_top + rows;
        if !(cx.focused && self.focus == PanelFocus::Editor && shown) {
            return None;
        }
        let col = text_area.x + (cursor_col as u16).min(text_area.width.saturating_sub(1));
        Some(Cursor { col, row: text_area.y + (cursor_row - self.message_top) as u16, shape: CursorShape::Bar })
    }

    fn draw_buttons(&mut self, canvas: &mut Canvas<'_>, area: Rect, cx: &RenderCx<'_>) {
        let theme = cx.theme;
        let bg = theme.panel_bg;
        canvas.fill(area, bg);
        if area.height == 0 || area.width < 12 {
            return;
        }
        let committing = self.committing();
        let (first, second) = if self.amending.is_some() {
            ("Cancel", if committing { "Amending…" } else { "Amend" })
        } else {
            ("Amend", if committing { "Committing…" } else { "Commit" })
        };
        self.commit_button = button(canvas, area.right() - 1, area.y, second, !committing, theme);
        if area.width >= 26 {
            self.amend_button = button(canvas, self.commit_button.x.saturating_sub(1), area.y, first, true, theme);
        }
        // What the commit would hold, on the left.
        let left_end = if self.amend_button.width > 0 { self.amend_button.x } else { self.commit_button.x };
        let room = left_end.saturating_sub(area.x + 2);
        if room >= 6 {
            let (text, color) = match &self.amending {
                Some(amending) => (format!("Amending {}", amending.sha), theme.warning),
                None => match self.staged_count() {
                    0 => ("Nothing staged".to_string(), theme.text_muted),
                    n => (format!("{n} staged"), theme.text_muted),
                },
            };
            canvas.text_truncated(area.x + 1, area.y, &text, Style::colors(color, bg), room);
        }
    }
}

impl View for GitPanel {
    fn render(&mut self, canvas: &mut Canvas<'_>, area: Rect, cx: &RenderCx<'_>) -> Option<Cursor> {
        let theme = cx.theme;
        canvas.fill(area, theme.panel_bg);
        self.repo_button = Rect::default();
        self.branch_button = Rect::default();
        self.mode_button = Rect::default();
        self.list_area = Rect::default();
        self.branch_header = Rect::default();
        self.branch_list_area = Rect::default();
        self.message_area = Rect::default();
        self.message_text = Rect::default();
        self.commit_button = Rect::default();
        self.amend_button = Rect::default();
        if area.width < 4 || area.height == 0 {
            return None;
        }
        self.refresh(cx.project);
        if cx.project.repo().is_none() {
            self.draw_empty(canvas, area, cx);
            return None;
        }
        // The box is as tall as its wrapped text (and its padding), so wrap first.
        self.message.settings_mut().wrap_width = Some(area.width.saturating_sub(2) as usize);
        self.visual = self.visual_rows();
        let padding = 2;
        let layout = layout(area, self.visual.len().clamp(MESSAGE_MIN_ROWS, MESSAGE_MAX_ROWS) + padding);
        self.draw_header(canvas, layout.header, cx);
        self.list_area = layout.list;
        self.draw_list(canvas, layout.list, cx);
        self.branch_header = layout.branch_header;
        self.branch_list_area = layout.branch_list;
        if layout.branch_header.height > 0 {
            self.draw_branch_header(canvas, layout.branch_header, cx);
            self.draw_branch_list(canvas, layout.branch_list, cx);
        }
        self.message_area = layout.message;
        let cursor = self.draw_message(canvas, layout.message, cx);
        self.draw_buttons(canvas, layout.buttons, cx);
        cursor
    }

    fn contexts(&self, out: &mut Vec<Context>) {
        let editor = self.focus == PanelFocus::Editor;
        out.push(Context::new("GitPanel").flag_if(!editor, "ChangesList").flag_if(editor, "CommitEditor"));
        if editor {
            out.push(Context::new("CommitEditor"));
            out.push(Context::new("Editor").value("mode", "auto_height"));
        }
    }

    fn handle_action(&mut self, action: &Action, cx: &mut Cx<'_>) -> bool {
        let in_list = self.focus == PanelFocus::Changes;
        if !in_list {
            if let Some(handled) = self.message_action(action, cx) {
                return handled;
            }
        }
        match action.name.as_str() {
            "git_panel::SelectFile" => {
                let Some(path) = action.arg_str("path").map(PathBuf::from) else { return false };
                let Some(relative) = cx.project.git_path(&path) else { return false };
                if let Some(index) = self.changes.iter().position(|c| c.path == relative) {
                    self.active_section = ListSection::Uncommitted;
                    self.select(index);
                    return true;
                }
                if let Some(index) = self.branch_changes.iter().position(|c| c.path == relative) {
                    self.active_section = ListSection::Branch;
                    self.select_branch(index);
                    return true;
                }
                return false;
            }
            "menu::SelectNext" | "git_panel::NextEntry" if in_list => self.move_selection(1),
            "menu::SelectPrevious" | "git_panel::PreviousEntry" if in_list => self.move_selection(-1),
            "git_panel::PreviousCommit" if in_list => {
                return self.step_commit(1, cx);
            }
            "git_panel::NextCommit" if in_list => {
                return self.step_commit(-1, cx);
            }
            "menu::SelectFirst" | "git_panel::FirstEntry" if in_list => self.select(0),
            "menu::SelectLast" | "git_panel::LastEntry" if in_list => {
                if !self.branch_changes.is_empty() && self.branch_list_area.height > 0 {
                    self.select_branch(usize::MAX);
                } else {
                    self.select(usize::MAX);
                }
            }
            "menu::Confirm" if in_list => {
                let Some(path) = self.selected_change().and_then(|change| cx.project.abs_path(&change.path)) else { return false };
                match self.active_section {
                    ListSection::Uncommitted => {
                        if let Some(commit) = self.viewing_commit().cloned() {
                            let want = DiffBase::Commit { sha: commit.sha.clone(), label: commit.short.clone() };
                            if *cx.project.diff_base() != want {
                                cx.push(Effect::SetCommitDiff { sha: commit.sha, label: commit.short });
                            }
                        } else if *cx.project.diff_base() != DiffBase::Uncommitted {
                            cx.push(Effect::SetDiffBase(None));
                        }
                    }
                    ListSection::Branch => {
                        let want = DiffBase::Branch(self.branch_base.clone());
                        if *cx.project.diff_base() != want {
                            cx.push(Effect::SetDiffBase(Some(self.branch_base.clone())));
                        }
                    }
                }
                cx.push(Effect::ShowProjectDiff { path: Some(path), focus: false });
            }
            "menu::Cancel" if in_list => cx.push(Effect::Focus(Focus::Center)),
            // The arrow keys' sequence ends by putting the focus back on the list.
            "git_panel::FocusChanges" => self.focus_changes(cx),
            "git_panel::FocusEditor" => self.focus_editor(cx),
            "git::Cancel" => return self.cancel(cx),
            "git::Commit" => return self.commit(cx),
            "git::Amend" => return self.amend(cx),
            "git::ToggleStaged" if in_list => {
                if self.active_section == ListSection::Uncommitted && self.commit_offset > 0 {
                    cx.status("Viewing a past commit — press → to return to uncommitted changes");
                    return true;
                }
                let Some(change) = self.selected_change() else { return false };
                return self.stage(self.entry_paths(change), change.stage != StageState::Staged, cx);
            }
            "git::StageFile" | "git::UnstageFile" if in_list => {
                if self.active_section == ListSection::Uncommitted && self.commit_offset > 0 {
                    cx.status("Viewing a past commit — press → to return to uncommitted changes");
                    return true;
                }
                let Some(change) = self.selected_change() else { return false };
                return self.stage(self.entry_paths(change), action.name == "git::StageFile", cx);
            }
            "git::StageAll" => {
                let _ = cx.project.git_job("Stage all", |repo| repo.stage_all().map(|()| String::new()));
            }
            "git::UnstageAll" => {
                let _ = cx.project.git_job("Unstage all", |repo| repo.unstage_all().map(|()| String::new()));
            }
            "git::RestoreFile" if in_list => {
                if self.active_section == ListSection::Uncommitted && self.commit_offset > 0 {
                    cx.status("Viewing a past commit — press → to return to uncommitted changes");
                    return true;
                }
                return self.restore_file(action, cx);
            }
            "git::RestoreTrackedFiles" => return self.restore_tracked(action, cx),
            "git::TrashUntrackedFiles" => return self.trash_untracked(action, cx),
            "git::Fetch" | "git::Pull" | "git::PullRebase" | "git::Push" | "git::ForcePush" => return self.remote(action, cx),
            "workspace::CopyPath" | "workspace::CopyRelativePath" if in_list => {
                let Some(abs) = self.selected_change().and_then(|change| cx.project.abs_path(&change.path)) else { return false };
                let shown = if action.name == "workspace::CopyRelativePath" { abs.strip_prefix(cx.root).unwrap_or(&abs).to_path_buf() } else { abs };
                let text = shown.to_string_lossy().into_owned();
                cx.clipboard.set(&text, false);
                cx.status(format!("Copied {text}"));
            }
            _ => return false,
        }
        true
    }

    fn handle_key(&mut self, key: &KeyEvent, cx: &mut Cx<'_>) -> bool {
        if self.focus != PanelFocus::Editor {
            return false;
        }
        match key.text.as_deref() {
            Some(text) if key.is_down() => self.run_message(Command::Insert(text.to_string()), cx),
            _ => false,
        }
    }

    fn handle_paste(&mut self, text: &str, cx: &mut Cx<'_>) -> bool {
        if self.focus != PanelFocus::Editor {
            return false;
        }
        // Terminals send line breaks as carriage returns; the editor wants line feeds.
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        self.run_message(Command::Insert(text), cx)
    }

    fn handle_mouse(&mut self, event: &MouseEvent, _area: Rect, cx: &mut Cx<'_>) -> bool {
        let inside = |rect: Rect| rect.width > 0 && event.col >= rect.x && event.col < rect.right() && event.row >= rect.y && event.row < rect.bottom();
        match event.kind {
            MouseKind::ScrollDown | MouseKind::ScrollUp => {
                let down = event.kind == MouseKind::ScrollDown;
                if inside(self.message_area) {
                    let max = self.visual.len().saturating_sub(self.message_text.height as usize);
                    self.message_top = if down { (self.message_top + 1).min(max) } else { self.message_top.saturating_sub(1) };
                } else if inside(self.branch_list_area) {
                    let max = self.branch_changes.len().saturating_sub(self.branch_list_area.height as usize);
                    self.branch_scroll = if down { (self.branch_scroll + WHEEL_ROWS).min(max) } else { self.branch_scroll.saturating_sub(WHEEL_ROWS) };
                } else {
                    let max = self.list_rows().len().saturating_sub(self.list_area.height as usize);
                    self.scroll = if down { (self.scroll + WHEEL_ROWS).min(max) } else { self.scroll.saturating_sub(WHEEL_ROWS) };
                }
                true
            }
            MouseKind::Press(MouseButton::Left) => {
                if inside(self.repo_button) {
                    cx.push(Effect::Dispatch(Action::new("git::SelectRepository")));
                    return true;
                }
                if inside(self.branch_button) {
                    cx.push(Effect::Dispatch(Action::new("branches::OpenRecent")));
                    return true;
                }
                if inside(self.prev_commit_button) {
                    return self.step_commit(1, cx);
                }
                if inside(self.next_commit_button) {
                    return self.step_commit(-1, cx);
                }
                if inside(self.mode_button) || inside(self.branch_header) {
                    cx.push(Effect::Dispatch(Action::new("git::SelectDiffBase")));
                    return true;
                }
                if inside(self.commit_button) {
                    return self.handle_action(&Action::new(if self.amending.is_some() { "git::Amend" } else { "git::Commit" }), cx);
                }
                if inside(self.amend_button) {
                    return self.handle_action(&Action::new(if self.amending.is_some() { "git::Cancel" } else { "git::Amend" }), cx);
                }
                if inside(self.message_area) {
                    self.focus = PanelFocus::Editor;
                    let position = self.message_position_at(event.col, event.row);
                    if event.mods.contains(Mods::SHIFT) {
                        self.message.extend_primary_to(position);
                    } else {
                        self.message.set_cursor(position);
                    }
                    self.reveal_message = true;
                    return true;
                }
                if inside(self.branch_list_area) {
                    self.focus = PanelFocus::Changes;
                    let index = self.branch_scroll + (event.row - self.branch_list_area.y) as usize;
                    if let Some(change) = self.branch_changes.get(index).cloned() {
                        self.select_branch(index);
                        self.branch_reveal = false;
                        if let Some(path) = cx.project.abs_path(&change.path) {
                            let want = DiffBase::Branch(self.branch_base.clone());
                            if *cx.project.diff_base() != want {
                                cx.push(Effect::SetDiffBase(Some(self.branch_base.clone())));
                            }
                            cx.push(Effect::ShowProjectDiff { path: Some(path), focus: false });
                        }
                    }
                    return true;
                }
                if !inside(self.list_area) {
                    return true;
                }
                self.focus = PanelFocus::Changes;
                if self.commit_offset > 0 {
                    let header_rows = self.commit_header_rows;
                    if event.row < self.list_area.y + header_rows {
                        return true;
                    }
                    let index = self.commit_scroll + (event.row - self.list_area.y - header_rows) as usize;
                    if let Some(change) = self.commit_changes.get(index).cloned() {
                        self.select(index);
                        self.commit_reveal = false;
                        if let (Some(commit), Some(path)) = (self.viewing_commit().cloned(), cx.project.abs_path(&change.path)) {
                            let want = DiffBase::Commit { sha: commit.sha.clone(), label: commit.short.clone() };
                            if *cx.project.diff_base() != want {
                                cx.push(Effect::SetCommitDiff { sha: commit.sha, label: commit.short });
                            }
                            cx.push(Effect::ShowProjectDiff { path: Some(path), focus: false });
                        }
                    }
                    return true;
                }
                let rows = self.list_rows();
                let Some(row) = rows.get(self.scroll + (event.row - self.list_area.y) as usize).copied() else { return true };
                match row {
                    ListRow::Header(group) => self.toggle_group(group, cx),
                    ListRow::Change(index) => {
                        let first = self.list_area.x + 1;
                        let on_checkbox = event.col >= first && event.col < first + checkbox_width(cx.settings.icons);
                        self.select(index);
                        self.reveal = false;
                        let change = self.changes[index].clone();
                        if on_checkbox {
                            self.stage(self.entry_paths(&change), change.stage != StageState::Staged, cx);
                        } else if let Some(path) = cx.project.abs_path(&change.path) {
                            if *cx.project.diff_base() != DiffBase::Uncommitted {
                                cx.push(Effect::SetDiffBase(None));
                            }
                            cx.push(Effect::ShowProjectDiff { path: Some(path), focus: false });
                        }
                    }
                }
                true
            }
            MouseKind::Drag(MouseButton::Left) if self.focus == PanelFocus::Editor => {
                let position = self.message_position_at(event.col, event.row);
                self.message.extend_primary_to(position);
                self.reveal_message = true;
                true
            }
            _ => false,
        }
    }

    fn tick(&mut self, cx: &mut Cx<'_>) -> bool {
        let mut dirty = self.refresh(cx.project);
        if let Some(counts) = self.counts_task.as_mut().and_then(Task::poll) {
            self.counts = counts;
            dirty = true;
        }
        if self.counts_task.as_ref().is_some_and(Task::is_finished) {
            self.counts_task = None;
        }
        if let Some(loaded) = self.branch_task.as_mut().and_then(Task::poll) {
            self.branch_task = None;
            match loaded {
                Ok((base_ref, changes, counts)) if base_ref == self.branch_base => {
                    self.branch_error = None;
                    self.branch_counts = counts;
                    self.apply_branch_changes(changes);
                }
                Ok(_) => {}
                Err(error) => {
                    self.branch_error = Some(error);
                    self.branch_changes.clear();
                    self.branch_counts.clear();
                }
            }
            dirty = true;
        }
        if self.branch_task.as_ref().is_some_and(Task::is_finished) {
            self.branch_task = None;
        }
        if let Some(commits) = self.commits_task.as_mut().and_then(Task::poll) {
            self.commits_task = None;
            let prev_sha = self.viewing_commit().map(|c| c.sha.clone());
            self.commits = commits;
            if let Some(sha) = prev_sha {
                self.commit_offset = self.commits.iter().position(|c| c.sha == sha).map_or(0, |i| i + 1);
            } else {
                self.commit_offset = self.commit_offset.min(self.commits.len());
            }
            dirty = true;
        }
        if self.commits_task.as_ref().is_some_and(Task::is_finished) {
            self.commits_task = None;
        }
        if let Some(loaded) = self.commit_task.as_mut().and_then(Task::poll) {
            self.commit_task = None;
            let want_sha = self.viewing_commit().map(|c| c.sha.clone());
            match loaded {
                Ok((sha, changes, counts)) if want_sha.as_deref() == Some(sha.as_str()) => {
                    self.commit_error = None;
                    self.commit_loaded_sha = Some(sha);
                    self.commit_counts = counts;
                    self.commit_changes = changes;
                    self.commit_selected = self.commit_selected.min(self.commit_changes.len().saturating_sub(1));
                }
                Ok(_) => {}
                Err(error) => {
                    self.commit_error = Some(error);
                    self.commit_changes.clear();
                    self.commit_counts.clear();
                }
            }
            dirty = true;
        }
        if self.commit_task.as_ref().is_some_and(Task::is_finished) {
            self.commit_task = None;
        }
        self.kick_counts(cx.project);
        if let Some(loaded) = self.amend_load.as_mut().and_then(Task::poll) {
            self.amend_load = None;
            self.amend_loaded(loaded, cx);
            dirty = true;
        }
        let mut index = 0;
        while index < self.pending.len() {
            let result = self.pending[index].task.poll();
            if result.is_none() && !self.pending[index].task.is_finished() {
                index += 1;
                continue;
            }
            let pending = self.pending.remove(index);
            dirty = true;
            match pending.after {
                After::Commit if result.is_some_and(|result| result.is_ok()) => {
                    self.amending = None;
                    self.set_message("");
                    self.focus = PanelFocus::Changes;
                }
                After::Changed(paths) => cx.push(Effect::PathsChanged(paths)),
                After::Commit => {}
            }
        }
        dirty
    }

    fn paths_changed(&mut self, change: &FsChange, cx: &mut Cx<'_>) {
        // A file that was already modified and is edited again changes no status.
        if change.rescan || change.paths.iter().any(|path| cx.project.git_path(path).is_some()) {
            self.counts_stale = true;
            self.branch_stale = true;
            self.kick_counts(cx.project);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::test_support::{git, repo};
    use crate::view::test_support::Harness;

    fn settled(dir: &Path) -> Harness {
        let mut project = Project::open(dir, crate::background::Waker::detached());
        project.settle(Duration::from_secs(20));
        Harness::with_project(project)
    }

    /// Tick the panel (and the project) until `done` is true.
    fn until(harness: &mut Harness, panel: &mut GitPanel, what: &str, done: impl Fn(&GitPanel) -> bool) {
        let started = Instant::now();
        loop {
            harness.project.poll();
            panel.tick(&mut harness.cx());
            if done(panel) {
                return;
            }
            assert!(started.elapsed() < Duration::from_secs(20), "timed out waiting for: {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn act(harness: &mut Harness, panel: &mut GitPanel, name: &str) -> bool {
        panel.handle_action(&Action::new(name), &mut harness.cx())
    }

    fn type_text(harness: &mut Harness, panel: &mut GitPanel, text: &str) {
        for c in text.chars() {
            panel.handle_key(&KeyEvent::typed(c), &mut harness.cx());
        }
    }

    fn with_identity(dir: &Path) {
        git(dir, &["config", "user.name", "Test"]);
        git(dir, &["config", "user.email", "test@example.invalid"]);
        git(dir, &["config", "commit.gpgsign", "false"]);
    }

    fn panel_for(dir: &Path) -> (Harness, GitPanel) {
        let mut harness = settled(dir);
        let mut panel = GitPanel::new(&mut harness.cx());
        until(&mut harness, &mut panel, "the counts", |p| p.counts_task.is_none() && !p.counts_stale);
        (harness, panel)
    }

    fn git_log(dir: &Path) -> String {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(["log", "--format=%s"]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[test]
    fn lists_the_changes_grouped_with_their_state_and_counts() {
        let dir = repo();
        std::fs::write(dir.path().join("a.txt"), "one\nchanged\nmore\n").unwrap();
        std::fs::write(dir.path().join("src/new.rs"), "x\n").unwrap();
        git(dir.path(), &["add", "src/new.rs"]);
        std::fs::write(dir.path().join("notes.md"), "").unwrap();
        let (harness, mut panel) = panel_for(dir.path());
        let (text, _, _) = harness.draw(&mut panel, 40, 20);
        let rows: Vec<&str> = text.lines().collect();
        assert!(rows[0].contains("main") && rows[0].contains("3 changes"), "{text}");
        assert!(rows[1].contains("Tracked") && rows[1].contains('2'), "{text}");
        assert!(rows[2].contains("M a.txt") && rows[2].trim_end().ends_with("+2 −1"), "{:?}", rows[2]);
        assert!(rows[3].contains("A new.rs") && rows[3].contains("src") && rows[3].trim_end().ends_with("+1"), "{:?}", rows[3]);
        assert!(rows[4].contains("Untracked"), "{text}");
        assert!(rows[5].contains("U notes.md"), "{text}");
        // The empty message box says what it is for.
        assert!(text.contains(" Commit message"), "{text}");
    }

    #[test]
    fn enter_shows_the_diff_and_space_stages_keeping_the_selection() {
        let dir = repo();
        std::fs::write(dir.path().join("a.txt"), "one\nchanged\n").unwrap();
        std::fs::write(dir.path().join("z.txt"), "z\n").unwrap();
        let (mut harness, mut panel) = panel_for(dir.path());
        assert!(act(&mut harness, &mut panel, "menu::Confirm"));
        assert_eq!(harness.take_effects(), vec![Effect::ShowProjectDiff { path: Some(dir.path().join("a.txt")), focus: false }]);
        act(&mut harness, &mut panel, "git_panel::NextEntry");
        assert!(act(&mut harness, &mut panel, "git::ToggleStaged"));
        until(&mut harness, &mut panel, "z.txt staged", |p| p.changes.iter().any(|c| c.path == Path::new("z.txt") && c.stage == StageState::Staged));
        assert_eq!(panel.changes[panel.selected].path, Path::new("z.txt"), "the selection follows the file into Tracked");
        assert!(harness.draw(&mut panel, 40, 12).0.contains("A z.txt"));
    }

    #[test]
    fn the_arrow_key_sequence_leaves_the_focus_in_the_list() {
        let dir = repo();
        std::fs::write(dir.path().join("a.txt"), "one\nchanged\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "b\n").unwrap();
        let (mut harness, mut panel) = panel_for(dir.path());
        for name in ["menu::SelectNext", "menu::Confirm", "git_panel::FocusChanges"] {
            act(&mut harness, &mut panel, name);
        }
        let mut contexts = Vec::new();
        panel.contexts(&mut contexts);
        assert_eq!(contexts, vec![Context::new("GitPanel").flag("ChangesList")]);
        let effects = harness.take_effects();
        assert!(effects.contains(&Effect::ShowProjectDiff { path: Some(dir.path().join("b.txt")), focus: false }), "{effects:?}");
        assert!(effects.contains(&Effect::Focus(Focus::Panel(PanelId::Git))));
    }

    #[test]
    fn the_editor_has_its_own_context_and_takes_text() {
        let dir = repo();
        let (mut harness, mut panel) = panel_for(dir.path());
        act(&mut harness, &mut panel, "git_panel::FocusEditor");
        let mut contexts = Vec::new();
        panel.contexts(&mut contexts);
        assert_eq!(contexts[0], Context::new("GitPanel").flag("CommitEditor"));
        assert_eq!(contexts[1], Context::new("CommitEditor"));
        assert_eq!(contexts[2], Context::new("Editor").value("mode", "auto_height"));
        type_text(&mut harness, &mut panel, "fix: one");
        assert!(act(&mut harness, &mut panel, "editor::Newline"), "enter is a newline");
        type_text(&mut harness, &mut panel, "body");
        assert_eq!(panel.message_text(), "fix: one\nbody");
        let (text, cursor, _) = harness.draw(&mut panel, 40, 20);
        assert!(text.contains("fix: one") && text.contains("body"), "{text}");
        assert!(cursor.is_some(), "the text cursor is shown while typing");
        assert!(act(&mut harness, &mut panel, "editor::Undo"));
        assert!(!act(&mut harness, &mut panel, "editor::Cancel"), "escape with nothing selected is git::Cancel's");
        act(&mut harness, &mut panel, "git::Cancel");
        assert_eq!(panel.focus, PanelFocus::Changes);
    }

    #[test]
    fn commit_needs_a_message_and_staged_files_then_clears_the_box() {
        let dir = repo();
        with_identity(dir.path());
        std::fs::write(dir.path().join("a.txt"), "one\nchanged\n").unwrap();
        let (mut harness, mut panel) = panel_for(dir.path());
        act(&mut harness, &mut panel, "git::Commit");
        let effects = harness.take_effects();
        assert!(effects.iter().any(|e| matches!(e, Effect::Status { text, error: true } if text.contains("commit message"))), "{effects:?}");
        assert_eq!(panel.focus, PanelFocus::Editor, "the box gets the focus so the message can be written");
        type_text(&mut harness, &mut panel, "second");
        act(&mut harness, &mut panel, "git::Commit");
        let effects = harness.take_effects();
        assert!(effects.iter().any(|e| matches!(e, Effect::Status { text, error: true } if text.contains("Nothing is staged"))), "{effects:?}");
        act(&mut harness, &mut panel, "git::StageAll");
        until(&mut harness, &mut panel, "staged", |p| p.staged_count() == 1);
        assert!(act(&mut harness, &mut panel, "git::Commit"));
        until(&mut harness, &mut panel, "the commit", |p| p.changes.is_empty() && p.pending.is_empty());
        assert_eq!(panel.message_text(), "", "a commit that worked empties the box");
        assert_eq!(panel.focus, PanelFocus::Changes);
        assert_eq!(git_log(dir.path()), "second\nfirst\n");
        let text = harness.draw(&mut panel, 44, 12).0;
        assert!(text.contains("No changes"), "{text}");
    }

    #[test]
    fn a_failed_commit_keeps_the_message() {
        let dir = repo();
        with_identity(dir.path());
        std::fs::write(dir.path().join("a.txt"), "changed\n").unwrap();
        std::fs::create_dir_all(dir.path().join(".git/hooks")).unwrap();
        let hook = dir.path().join(".git/hooks/pre-commit");
        std::fs::write(&hook, "#!/bin/sh\necho refused >&2\nexit 1\n").unwrap();
        std::process::Command::new("chmod").arg("+x").arg(&hook).status().unwrap();
        git(dir.path(), &["add", "a.txt"]);
        let (mut harness, mut panel) = panel_for(dir.path());
        act(&mut harness, &mut panel, "git_panel::FocusEditor");
        type_text(&mut harness, &mut panel, "will fail");
        act(&mut harness, &mut panel, "git::Commit");
        until(&mut harness, &mut panel, "the failed commit", |p| p.pending.is_empty());
        assert_eq!(panel.message_text(), "will fail");
        assert_eq!(panel.focus, PanelFocus::Editor);
    }

    #[test]
    fn amend_prefills_the_last_message_and_cancel_puts_the_draft_back() {
        let dir = repo();
        with_identity(dir.path());
        let (mut harness, mut panel) = panel_for(dir.path());
        act(&mut harness, &mut panel, "git_panel::FocusEditor");
        type_text(&mut harness, &mut panel, "draft");
        act(&mut harness, &mut panel, "git::Amend");
        until(&mut harness, &mut panel, "the message", |p| p.amending.is_some());
        assert_eq!(panel.message_text(), "first");
        assert!(harness.draw(&mut panel, 44, 14).0.contains("Amending"));
        type_text(&mut harness, &mut panel, " edited");
        act(&mut harness, &mut panel, "git::Amend");
        until(&mut harness, &mut panel, "the amend", |p| p.amending.is_none() && p.pending.is_empty());
        assert_eq!(git_log(dir.path()), "first edited\n");
        act(&mut harness, &mut panel, "git_panel::FocusEditor");
        type_text(&mut harness, &mut panel, "keep me");
        act(&mut harness, &mut panel, "git::Amend");
        until(&mut harness, &mut panel, "amend mode again", |p| p.amending.is_some());
        act(&mut harness, &mut panel, "git::Cancel");
        assert_eq!(panel.message_text(), "keep me");
        assert!(panel.amending.is_none());
    }

    #[test]
    fn discarding_asks_first_and_untracked_files_go_to_the_trash() {
        let dir = repo();
        let trash = tempfile::tempdir().unwrap();
        set_test_trash(trash.path());
        std::fs::write(dir.path().join("a.txt"), "one\nchanged\n").unwrap();
        std::fs::write(dir.path().join("scratch.txt"), "tmp\n").unwrap();
        let (mut harness, mut panel) = panel_for(dir.path());
        assert!(act(&mut harness, &mut panel, "git::RestoreFile"));
        let effects = harness.take_effects();
        let Effect::Ask(question) = &effects[0] else { panic!("asked: {effects:?}") };
        assert!(question.message.contains("a.txt"));
        assert_eq!(question.answers.last().unwrap().0, "Cancel");
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "one\nchanged\n", "nothing happened yet");
        let Effect::Dispatch(confirm) = &question.answers[0].1[0] else { panic!("dispatches") };
        panel.handle_action(confirm, &mut harness.cx());
        until(&mut harness, &mut panel, "a.txt restored", |p| p.changes.iter().all(|c| c.path != Path::new("a.txt")) && p.pending.is_empty());
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "one\ntwo\n");
        // Skipping the prompt acts at once; an untracked file is moved, not deleted.
        assert!(panel.handle_action(&Action::with_args("git::RestoreFile", json!({ "skip_prompt": true })), &mut harness.cx()));
        until(&mut harness, &mut panel, "scratch trashed", |p| p.changes.is_empty() && p.pending.is_empty());
        assert!(!dir.path().join("scratch.txt").exists());
        assert_eq!(std::fs::read_to_string(trash.path().join("scratch.txt")).unwrap(), "tmp\n");
        assert!(harness.take_effects().iter().any(|e| matches!(e, Effect::PathsChanged(_))));
    }

    #[test]
    fn restoring_all_tracked_and_trashing_all_untracked_always_ask() {
        let dir = repo();
        let trash = tempfile::tempdir().unwrap();
        set_test_trash(trash.path());
        std::fs::write(dir.path().join("a.txt"), "changed\n").unwrap();
        std::fs::write(dir.path().join("fresh.txt"), "x\n").unwrap();
        git(dir.path(), &["add", "fresh.txt"]);
        std::fs::write(dir.path().join("loose.txt"), "y\n").unwrap();
        let (mut harness, mut panel) = panel_for(dir.path());
        for name in ["git::RestoreTrackedFiles", "git::TrashUntrackedFiles"] {
            assert!(act(&mut harness, &mut panel, name));
            assert!(matches!(harness.take_effects()[0], Effect::Ask(_)), "{name} asks");
        }
        panel.handle_action(&Action::with_args("git::RestoreTrackedFiles", json!({ "confirmed": true })), &mut harness.cx());
        until(&mut harness, &mut panel, "tracked discarded", |p| p.changes.iter().all(|c| c.group == Group::Untracked) && p.pending.is_empty());
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "one\ntwo\n");
        assert!(!dir.path().join("fresh.txt").exists(), "the new file went to the Trash");
        assert!(dir.path().join("loose.txt").exists(), "untracked files are not tracked changes");
        panel.handle_action(&Action::with_args("git::TrashUntrackedFiles", json!({ "confirmed": true })), &mut harness.cx());
        until(&mut harness, &mut panel, "untracked trashed", |p| p.changes.is_empty() && p.pending.is_empty());
        assert!(!dir.path().join("loose.txt").exists());
    }

    #[test]
    fn push_pull_and_fetch_run_as_jobs_and_force_push_asks() {
        let origin = tempfile::tempdir().unwrap();
        git(origin.path(), &["init", "--bare", "--quiet"]);
        let dir = repo();
        git(dir.path(), &["remote", "add", "origin", &origin.path().to_string_lossy()]);
        let (mut harness, mut panel) = panel_for(dir.path());
        assert!(act(&mut harness, &mut panel, "git::Push"));
        assert_eq!(harness.project.busy().as_deref(), Some("Push main"));
        let started = Instant::now();
        while harness.project.busy().is_some() {
            harness.project.poll();
            assert!(started.elapsed() < Duration::from_secs(20));
            std::thread::sleep(Duration::from_millis(10));
        }
        let pushed = std::process::Command::new("git").arg("-C").arg(origin.path()).args(["rev-parse", "main"]).output().unwrap();
        assert!(pushed.status.success(), "main was pushed");
        assert!(act(&mut harness, &mut panel, "git::ForcePush"));
        let effects = harness.take_effects();
        let Effect::Ask(question) = &effects[0] else { panic!("asked: {effects:?}") };
        assert!(question.message.contains("Force push main"), "{}", question.message);
        assert!(question.detail.as_deref().unwrap().contains("--force-with-lease"));
        assert!(act(&mut harness, &mut panel, "git::Fetch"));
        assert!(act(&mut harness, &mut panel, "git::Pull"));
    }

    #[test]
    fn empty_states_say_what_is_going_on() {
        let dir = repo();
        let (harness, mut panel) = panel_for(dir.path());
        let text = harness.draw(&mut panel, 60, 12).0;
        assert!(text.contains("No changes") && text.contains("Everything is committed"), "{text}");
        let plain = tempfile::tempdir().unwrap();
        let mut harness = settled(plain.path());
        let mut panel = GitPanel::new(&mut harness.cx());
        let text = harness.draw(&mut panel, 50, 6).0;
        assert!(text.contains("Not a git repository") && text.contains("git init"), "{text}");
        assert!(!act(&mut harness, &mut panel, "git::ToggleStaged"));
        assert!(!act(&mut harness, &mut panel, "git::Commit"));
    }

    #[test]
    fn mouse_toggles_the_checkbox_and_selects_rows() {
        let dir = repo();
        std::fs::write(dir.path().join("a.txt"), "one\nchanged\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "b\n").unwrap();
        let (mut harness, mut panel) = panel_for(dir.path());
        harness.draw(&mut panel, 40, 14);
        let area = Rect::new(0, 0, 40, 14);
        let click = |col, row| MouseEvent::new(MouseKind::Press(MouseButton::Left), col, row);
        // Rows: header, Tracked, a.txt, Untracked, b.txt.
        panel.handle_mouse(&click(10, 4), area, &mut harness.cx());
        assert_eq!(panel.changes[panel.selected].path, Path::new("b.txt"));
        assert!(harness.take_effects().iter().any(|e| matches!(e, Effect::ShowProjectDiff { .. })));
        panel.handle_mouse(&click(1, 2), area, &mut harness.cx());
        until(&mut harness, &mut panel, "a.txt staged", |p| p.changes.iter().any(|c| c.path == Path::new("a.txt") && c.stage == StageState::Staged));
        // A click in the message box moves the focus there.
        let box_row = panel.message_text.y;
        panel.handle_mouse(&click(5, box_row), area, &mut harness.cx());
        assert_eq!(panel.focus, PanelFocus::Editor);
    }

    #[test]
    fn narrow_and_short_panels_do_not_panic() {
        let dir = repo();
        std::fs::write(dir.path().join("a-very-long-file-name-indeed.txt"), "b\n").unwrap();
        let (harness, mut panel) = panel_for(dir.path());
        for (w, h) in [(4, 1), (6, 3), (12, 5), (20, 9), (80, 3), (30, 30)] {
            harness.draw(&mut panel, w, h);
        }
    }

    #[test]
    fn trash_picks_a_free_name() {
        let dir = tempfile::tempdir().unwrap();
        let trash = tempfile::tempdir().unwrap();
        for _ in 0..2 {
            std::fs::write(dir.path().join("x.txt"), "x").unwrap();
            trash_into(trash.path(), &dir.path().join("x.txt")).unwrap();
        }
        assert!(trash.path().join("x.txt").exists() && trash.path().join("x 2.txt").exists());
        assert!(trash_into(trash.path(), &dir.path().join("gone")).is_err());
    }

    #[test]
    fn wrap_words_wraps_long_commit_subjects_without_overflow() {
        let msg = "feat(epsilon): add Infrastructure Lens workload tree, Git diff panel, Image Builds, and NFS backups";
        let lines = wrap_words(msg, 30);
        assert!(lines.len() >= 3);
        for line in &lines {
            assert!(ui::width(line) <= 30, "line too wide: {line:?}");
        }
        assert_eq!(lines.join(" "), msg);
        assert_eq!(wrap_words("supercalifragilistic", 6), vec!["superc", "alifra", "gilist", "ic"]);
        assert!(wrap_words("", 20).is_empty());
        assert!(wrap_words("hi", 0).is_empty());
    }
}

/// Word-wrap `text` into lines of at most `max_width` terminal columns.
/// Words wider than `max_width` are split on character boundaries.
fn wrap_words(text: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return Vec::new();
    }
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_w = 0usize;
    for word in text.split_whitespace() {
        let word_w = ui::width(word);
        if current_w == 0 {
            if word_w <= max_width {
                current.push_str(word);
                current_w = word_w;
            } else {
                for ch in word.chars() {
                    let mut buf = [0u8; 4];
                    let ch_w = ui::width(ch.encode_utf8(&mut buf)).max(1);
                    if current_w > 0 && current_w + ch_w > max_width {
                        lines.push(std::mem::take(&mut current));
                        current_w = 0;
                    }
                    current.push(ch);
                    current_w += ch_w;
                }
            }
        } else if current_w + 1 + word_w <= max_width {
            current.push(' ');
            current.push_str(word);
            current_w += 1 + word_w;
        } else {
            lines.push(std::mem::take(&mut current));
            current_w = 0;
            if word_w <= max_width {
                current.push_str(word);
                current_w = word_w;
            } else {
                for ch in word.chars() {
                    let mut buf = [0u8; 4];
                    let ch_w = ui::width(ch.encode_utf8(&mut buf)).max(1);
                    if current_w > 0 && current_w + ch_w > max_width {
                        lines.push(std::mem::take(&mut current));
                        current_w = 0;
                    }
                    current.push(ch);
                    current_w += ch_w;
                }
            }
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}
