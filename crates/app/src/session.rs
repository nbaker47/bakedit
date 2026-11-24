//! What bakedit remembers about a project between runs: open tabs and where the cursor was
//! in each, the dock layout, recently opened files and search history. One JSON file per
//! project directory under `~/.local/state/bakedit/sessions/`, and one list of the projects
//! opened lately (`recent-projects.json` in the same state directory).
//!
//! [`capture`] and [`restore`] turn a [`Shell`] into a [`Session`] and back.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::project::SearchKind;
use crate::shell::Shell;
use crate::workspace::{Focus, WorkspaceSnapshot};

/// Bumped when the file format changes in a way old code cannot read.
const VERSION: u32 = 1;
/// How many recent files and search queries are kept.
const HISTORY_LIMIT: usize = 100;
/// How many recent projects are kept.
const PROJECTS_LIMIT: usize = 30;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabState {
    pub path: PathBuf,
    /// Cursor position, 0-based.
    #[serde(default)]
    pub line: u32,
    #[serde(default)]
    pub column: u32,
    /// First visible line.
    #[serde(default)]
    pub scroll_top: u32,
    #[serde(default)]
    pub pinned: bool,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub tabs: Vec<TabState>,
    #[serde(default)]
    pub active_tab: Option<usize>,
    #[serde(default)]
    pub workspace: WorkspaceSnapshot,
    /// Most recent first.
    #[serde(default)]
    pub recent_files: Vec<PathBuf>,
    /// Most recent first.
    #[serde(default)]
    pub search_history: Vec<String>,
    #[serde(default)]
    pub project_search_history: Vec<String>,
    /// What each panel asked to keep ([`crate::view::View::session_state`]), by panel title.
    #[serde(default)]
    pub panels: BTreeMap<String, serde_json::Value>,
}

impl Session {
    /// Move `path` to the front of the recent files.
    pub fn touch_recent(&mut self, path: &Path) {
        push_front(&mut self.recent_files, path.to_path_buf());
    }

    pub fn remember_search(&mut self, query: &str) {
        if !query.is_empty() {
            push_front(&mut self.search_history, query.to_string());
        }
    }

    pub fn remember_project_search(&mut self, query: &str) {
        if !query.is_empty() {
            push_front(&mut self.project_search_history, query.to_string());
        }
    }
}

fn push_front<T: PartialEq>(list: &mut Vec<T>, item: T) {
    list.retain(|existing| existing != &item);
    list.insert(0, item);
    list.truncate(HISTORY_LIMIT);
}

/// 64-bit FNV-1a: enough to give each project directory its own file name.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The session file for a project: `<folder name>-<hash of the full path>.json`, so the
/// files are recognisable and two projects with the same folder name do not collide.
pub fn session_path(sessions_dir: &Path, root: &Path) -> PathBuf {
    let name: String = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "root".to_string())
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' })
        .take(40)
        .collect();
    let hash = fnv1a(root.to_string_lossy().as_bytes());
    sessions_dir.join(format!("{name}-{hash:016x}.json"))
}

/// Load the session saved for `root`. A missing, unreadable or newer-format file is `None`:
/// a session is a convenience and must never stop the editor from starting.
pub fn load(sessions_dir: &Path, root: &Path) -> Option<Session> {
    let bytes = fs::read(session_path(sessions_dir, root)).ok()?;
    let session: Session = serde_json::from_slice(&bytes).ok()?;
    if session.version > VERSION {
        return None;
    }
    Some(session)
}

/// Save the session for `root`, replacing the previous one atomically.
pub fn save(sessions_dir: &Path, root: &Path, session: &Session) -> io::Result<()> {
    fs::create_dir_all(sessions_dir)?;
    let path = session_path(sessions_dir, root);
    let mut session = session.clone();
    session.version = VERSION;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(&session)?)?;
    fs::rename(&tmp, &path)
}

/// The session of what the shell shows now: the tabs that show files (pages, the diff and
/// the search are not kept), the docks, the history and what the panels keep.
pub fn capture(shell: &Shell) -> Session {
    let mut tabs = Vec::new();
    let mut active_tab = None;
    for (index, slot) in shell.pane.slots().iter().enumerate() {
        let item = &slot.item;
        let (Some(path), "editor") = (item.path(), item.kind()) else { continue };
        let (line, column) = item.position().unwrap_or((1, 1));
        if shell.pane.active_index() == Some(index) {
            active_tab = Some(tabs.len());
        }
        tabs.push(TabState {
            path: path.to_path_buf(),
            line: line.saturating_sub(1),
            column: column.saturating_sub(1),
            scroll_top: item.scroll_top().unwrap_or(0),
            pinned: slot.pinned,
        });
    }
    Session {
        version: VERSION,
        tabs,
        active_tab,
        workspace: shell.workspace.snapshot(),
        recent_files: shell.project.recent_files(),
        search_history: shell.project.search_history(SearchKind::Buffer),
        project_search_history: shell.project.search_history(SearchKind::Project),
        panels: shell.panel_states(),
    }
}

/// Put a saved session back: the docks as they were, every tab whose file still exists at
/// its position and scroll, the front tab in front, the history, the panels' own state.
/// The keyboard ends up in the centre.
pub fn restore(shell: &mut Shell, session: &Session) {
    shell.workspace.restore(&session.workspace);
    shell.project.set_search_history(SearchKind::Buffer, session.search_history.clone());
    shell.project.set_search_history(SearchKind::Project, session.project_search_history.clone());
    let mut opened = Vec::new();
    for (index, tab) in session.tabs.iter().enumerate() {
        if !tab.path.is_file() {
            continue;
        }
        shell.open_file(&tab.path, Some(tab.line + 1), Some(tab.column + 1), false, false);
        if let Some(item) = shell.pane.active_mut() {
            if item.path() == Some(tab.path.as_path()) {
                item.set_scroll_top(tab.scroll_top);
                if tab.pinned {
                    if let Some(slot) = shell.pane.active_index() {
                        shell.pane.pin(slot);
                    }
                }
                opened.push((index, shell.pane.active_index()));
            }
        }
    }
    if let Some(front) = session.active_tab.and_then(|active| opened.iter().find(|(index, _)| *index == active)) {
        if let Some(slot) = front.1 {
            shell.pane.activate(slot);
        }
    }
    shell.restore_panel_states(&session.panels);
    // Reopening the saved tabs is neither navigating nor opening files lately: the history
    // and the recent files stay as they were saved.
    shell.pane.clear_history();
    shell.project.set_recent_files(session.recent_files.clone());
    shell.set_focus(Focus::Center);
}

/// The file listing the projects opened lately.
pub fn recent_projects_path(state_dir: &Path) -> PathBuf {
    state_dir.join("recent-projects.json")
}

/// The projects opened lately, most recent first, leaving out folders that are gone.
pub fn recent_projects(state_dir: &Path) -> Vec<PathBuf> {
    let Ok(bytes) = fs::read(recent_projects_path(state_dir)) else { return Vec::new() };
    let list: Vec<PathBuf> = serde_json::from_slice(&bytes).unwrap_or_default();
    list.into_iter().filter(|path| path.is_dir()).collect()
}

/// Put `root` at the front of the recent projects.
pub fn remember_project(state_dir: &Path, root: &Path) -> io::Result<()> {
    let mut list = recent_projects(state_dir);
    list.retain(|existing| existing != root);
    list.insert(0, root.to_path_buf());
    list.truncate(PROJECTS_LIMIT);
    fs::create_dir_all(state_dir)?;
    let path = recent_projects_path(state_dir);
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(&list)?)?;
    fs::rename(&tmp, &path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let root = Path::new("/Users/someone/Code/my project");
        assert_eq!(load(dir.path(), root), None);

        let mut session = Session::default();
        session.tabs.push(TabState { path: "/a/b.rs".into(), line: 10, column: 4, scroll_top: 2, pinned: true });
        session.active_tab = Some(0);
        session.touch_recent(Path::new("/a/b.rs"));
        session.remember_search("needle");
        save(dir.path(), root, &session).unwrap();

        let loaded = load(dir.path(), root).unwrap();
        assert_eq!(loaded.version, VERSION);
        assert_eq!(loaded.tabs, session.tabs);
        assert_eq!(loaded.recent_files, vec![PathBuf::from("/a/b.rs")]);
        assert_eq!(loaded.search_history, vec!["needle".to_string()]);
        // Another project has its own file.
        assert_eq!(load(dir.path(), Path::new("/Users/someone/Code/other")), None);
    }

    #[test]
    fn file_names_are_safe_and_distinct() {
        let dir = Path::new("/state");
        let a = session_path(dir, Path::new("/x/my project"));
        let b = session_path(dir, Path::new("/y/my project"));
        assert_ne!(a, b);
        let name = a.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("my_project-"), "{name}");
        assert!(name.ends_with(".json"));
        assert!(name.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)));
        // The root directory itself has no file name.
        assert!(session_path(dir, Path::new("/")).to_string_lossy().contains("root-"));
    }

    #[test]
    fn history_is_deduplicated_and_bounded() {
        let mut session = Session::default();
        session.remember_search("a");
        session.remember_search("b");
        session.remember_search("a");
        session.remember_search("");
        assert_eq!(session.search_history, vec!["a".to_string(), "b".to_string()]);
        for i in 0..300 {
            session.touch_recent(Path::new(&format!("/f{i}")));
        }
        assert_eq!(session.recent_files.len(), HISTORY_LIMIT);
        assert_eq!(session.recent_files[0], PathBuf::from("/f299"));
    }

    #[test]
    fn damaged_or_future_files_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let root = Path::new("/p");
        fs::write(session_path(dir.path(), root), b"{ not json").unwrap();
        assert_eq!(load(dir.path(), root), None);
        fs::write(session_path(dir.path(), root), br#"{"version": 999}"#).unwrap();
        assert_eq!(load(dir.path(), root), None);
        // Unknown fields and missing fields are fine.
        fs::write(session_path(dir.path(), root), br#"{"version": 1, "future_field": true}"#).unwrap();
        assert_eq!(load(dir.path(), root).unwrap().tabs, Vec::new());
    }

    #[test]
    fn recent_projects_are_most_recent_first_and_skip_missing_folders() {
        let state = tempfile::tempdir().unwrap();
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        assert_eq!(recent_projects(state.path()), Vec::<PathBuf>::new());
        remember_project(state.path(), a.path()).unwrap();
        remember_project(state.path(), b.path()).unwrap();
        remember_project(state.path(), a.path()).unwrap();
        assert_eq!(recent_projects(state.path()), vec![a.path().to_path_buf(), b.path().to_path_buf()]);
        let gone = b.path().to_path_buf();
        drop(b);
        assert_eq!(recent_projects(state.path()), vec![a.path().to_path_buf()], "{gone:?} is gone");
        fs::write(recent_projects_path(state.path()), b"not json").unwrap();
        assert_eq!(recent_projects(state.path()), Vec::<PathBuf>::new(), "a damaged list is an empty one");
    }

    #[test]
    fn a_shell_is_captured_and_restored() {
        let dir = tempfile::tempdir().unwrap();
        let files: Vec<PathBuf> = ["a.rs", "b.rs"].iter().map(|n| dir.path().join(n)).collect();
        for file in &files {
            fs::write(file, "x\n").unwrap();
        }
        let (mut shell, _) = crate::shell_tests::shell();
        shell.open_file(&files[0], Some(12), Some(3), false, true);
        shell.open_file(&files[1], None, None, false, true);
        shell.dispatch(&bakedit_core::Action::new("terminal_panel::Toggle"));
        shell.project.touch_recent(&files[0]);
        let session = capture(&shell);
        assert_eq!(session.tabs.len(), 2);
        // Kept 0-based; the stand-in tab knows only its line (its column is always 1).
        assert_eq!((session.tabs[0].line, session.tabs[0].column), (11, 0));
        assert_eq!(session.active_tab, Some(1));
        assert!(session.workspace.left.as_ref().is_some_and(|dock| dock.open));

        let (mut fresh, _) = crate::shell_tests::shell();
        restore(&mut fresh, &session);
        let names: Vec<_> = fresh.pane.slots().iter().map(|s| s.item.path().unwrap().to_path_buf()).collect();
        assert_eq!(names, files);
        assert_eq!(fresh.pane.active_index(), Some(1));
        assert_eq!(crate::shell_tests::fake(&fresh, 0).line, 12);
        assert_eq!(fresh.workspace.focus(), Focus::Center);
        assert_eq!(fresh.workspace.visible_panel(crate::workspace::DockPosition::Left), Some(crate::workspace::PanelId::Terminal));
        assert_eq!(fresh.project.recent_files().first(), Some(&files[0]));
        // A file that is gone by the next start is skipped.
        fs::remove_file(&files[0]).unwrap();
        let (mut later, _) = crate::shell_tests::shell();
        restore(&mut later, &session);
        assert_eq!(later.pane.len(), 1);
        assert_eq!(later.pane.active_index(), Some(0));
    }
}
