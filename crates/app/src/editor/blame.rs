//! Async per-line git blame for the editor (file last-editor header + inline/gutter blame).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use bakedit_git::{format_relative_time, BlameInfo, FileBlame, Repo};

use super::EditorItem;
use crate::background::Task;
use crate::project::{GitBase, Project};

/// A blame query running on a worker thread.
type RunningBlame = (PathBuf, Option<Arc<GitBase>>, u64, Task<Option<FileBlame>>);

/// Cached git blame state for an open editor buffer.
#[derive(Default)]
pub(super) struct GitBlame {
    /// Blame data for the current file.
    pub data: Option<Arc<FileBlame>>,
    /// The path, git base, and save generation `data` was loaded for.
    loaded_for: Option<(PathBuf, Option<Arc<GitBase>>, u64)>,
    /// In-flight worker task.
    running: Option<RunningBlame>,
    /// Full-file per-line blame column next to the line-number gutter.
    pub gutter_enabled: bool,
    /// Bumped whenever the buffer is saved so blame refreshes.
    pub generation: u64,
}

impl GitBlame {
    /// The newest committed change in this file, for the breadcrumb header.
    pub fn last_commit(&self) -> Option<&BlameInfo> {
        self.data.as_ref()?.last_commit.as_deref()
    }

    /// Blame info for 0-based `line`.
    pub fn line(&self, line: usize) -> Option<&BlameInfo> {
        self.data.as_ref()?.line(line)
    }

    /// Mark blame stale (e.g. after saving the file).
    pub fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }
}

fn same_base(a: &Option<Arc<GitBase>>, b: &Option<Arc<GitBase>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

/// Current Unix timestamp in seconds.
pub(super) fn unix_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Compact author name for narrow spaces (e.g. `"Nathan Baker"` -> `"Nathan B."`).
pub(super) fn short_author(name: &str, max_chars: usize) -> String {
    if name.chars().count() <= max_chars {
        return name.to_string();
    }
    let mut parts = name.split_whitespace();
    if let (Some(first), Some(last)) = (parts.next(), parts.next_back()) {
        if let Some(initial) = last.chars().next() {
            let compact = format!("{first} {initial}.");
            if compact.chars().count() <= max_chars {
                return compact;
            }
        }
    }
    let mut out: String = name.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Format a one-line blame summary for the file header or inline virtual text.
pub(super) fn format_blame_inline(info: &BlameInfo, now: i64) -> String {
    if info.uncommitted {
        return "You, Uncommitted".to_string();
    }
    let age = format_relative_time(info.timestamp, now);
    if info.summary.is_empty() {
        format!("{}, {age}", info.author)
    } else {
        format!("{}, {age} · {}", info.author, info.summary)
    }
}

/// Format a compact blame label for the breadcrumb header (right side).
pub(super) fn format_blame_header(info: &BlameInfo, now: i64) -> String {
    let age = format_relative_time(info.timestamp, now);
    if info.summary.is_empty() {
        format!("{} · {age}", info.author)
    } else {
        format!("{} · {age} · {}", info.author, info.summary)
    }
}

impl EditorItem {
    /// Poll a finished blame worker and kick off a new one when the file or git HEAD changes.
    pub(super) fn refresh_git_blame(&mut self, project: &Project) -> bool {
        let Some(path) = self.editor.buffer().path().map(std::path::Path::to_path_buf) else { return false };
        if self.large || self.kind == "page" {
            return false;
        }
        let base = project.git_base(&path);
        let gen = self.blame.generation;
        let mut changed = false;

        if let Some((started_path, started_base, started_gen, task)) = &mut self.blame.running {
            if let Some(result) = task.poll() {
                if *started_path == path && same_base(started_base, &base) && *started_gen == gen {
                    self.blame.data = result.map(Arc::new);
                    self.blame.loaded_for = Some((path.clone(), base.clone(), gen));
                    changed = true;
                }
                self.blame.running = None;
            } else if task.is_finished() {
                self.blame.running = None;
            }
        }

        let up_to_date = self.blame.loaded_for.as_ref().is_some_and(|(p, b, g)| *p == path && same_base(b, &base) && *g == gen);
        if self.blame.running.is_some() || up_to_date {
            return changed;
        }

        let target = path.clone();
        let task = project.spawn(move || {
            let repo = Repo::discover(&target)?;
            repo.blame(&target).ok()
        });
        self.blame.running = Some((path, base, gen, task));
        changed
    }
}
