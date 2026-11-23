//! What bakedit remembers about a project between runs: open tabs and where the cursor was
//! in each, the dock layout, recently opened files and search history. One JSON file per
//! project directory under `~/.local/state/bakedit/sessions/`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::workspace::WorkspaceSnapshot;

/// Bumped when the file format changes in a way old code cannot read.
const VERSION: u32 = 1;
/// How many recent files and search queries are kept.
const HISTORY_LIMIT: usize = 100;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let root = Path::new("/Users/someone/Code/my project");
        assert_eq!(load(dir.path(), root), None);

        let mut session = Session::default();
        session.tabs.push(TabState { path: "/a/b.rs".into(), line: 10, column: 4, scroll_top: 2 });
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
}
