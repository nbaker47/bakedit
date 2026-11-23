//! Git change marks for the gutter and the scrollbar.
//!
//! Small files are compared on the spot (it takes microseconds); larger ones on a worker
//! thread, one comparison at a time, while the old marks are shifted along with the edits
//! so nothing jumps while typing.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bakedit_git::{LineHunk, LineHunkKind};

use super::EditorItem;
use crate::background::Task;
use crate::paint::GitMark;
use crate::project::{GitBase, Project};

/// Files with at most this many lines are compared on the UI thread.
const SYNC_LINES: usize = 2000;
/// How long a comparison may take before it settles for a coarser answer.
const DIFF_BUDGET: Duration = Duration::from_millis(250);

/// A comparison on a worker: the buffer version and committed text it was started for.
type RunningDiff = (u64, Option<Arc<GitBase>>, Task<Vec<LineHunk>>);

/// Git change marks for the text as it is, against the committed text.
#[derive(Default)]
pub(super) struct GitMarks {
    /// The committed text the marks were worked out against.
    pub base: Option<Arc<GitBase>>,
    /// The buffer version they were worked out for.
    pub version: Option<u64>,
    /// Sorted by `new_start`.
    pub hunks: Vec<LineHunk>,
    /// A comparison running on a worker: the version and base it was started for.
    running: Option<RunningDiff>,
}

impl GitMarks {
    /// The mark for `line` of a text of `line_count` lines.
    pub fn mark(&self, line: usize, line_count: usize) -> Option<GitMark> {
        // Hunks are sorted by `new_start`: only those starting at or before the line matter.
        let end = self.hunks.partition_point(|hunk| hunk.new_start <= line);
        for hunk in self.hunks[..end].iter().rev() {
            match hunk.kind {
                LineHunkKind::Deleted if hunk.new_start == line => return Some(GitMark::DeletedAbove),
                LineHunkKind::Deleted if hunk.new_start >= line_count && line + 1 == line_count => return Some(GitMark::DeletedBelow),
                LineHunkKind::Added if line < hunk.new_start + hunk.new_lines => return Some(GitMark::Added),
                LineHunkKind::Modified if line < hunk.new_start + hunk.new_lines => return Some(GitMark::Modified),
                _ => {}
            }
            if hunk.new_start + hunk.new_lines.max(1) <= line {
                break;
            }
        }
        // A deletion at the very end is drawn on the last line.
        if line + 1 == line_count {
            if let Some(last) = self.hunks.last() {
                if last.kind == LineHunkKind::Deleted && last.new_start >= line_count {
                    return Some(GitMark::DeletedBelow);
                }
            }
        }
        None
    }
}

fn same_base(a: &Option<Arc<GitBase>>, b: &Option<Arc<GitBase>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

impl EditorItem {
    /// Collect a finished comparison and start one if the marks are out of date. Returns
    /// whether the marks on screen changed.
    pub(super) fn refresh_git_marks(&mut self, project: &Project) -> bool {
        let Some(path) = self.editor.buffer().path().map(std::path::Path::to_path_buf) else { return false };
        if self.large || self.kind == "page" {
            return false;
        }
        let version = self.editor.buffer().version();
        let base = project.git_base(&path);
        let mut changed = false;

        if let Some((started_for, started_base, task)) = &mut self.git.running {
            if let Some(hunks) = task.poll() {
                let fits = *started_for == version && same_base(started_base, &base);
                if fits {
                    self.git.hunks = hunks;
                    self.git.version = Some(version);
                    self.git.base = base.clone();
                    changed = true;
                }
                self.git.running = None;
            } else if task.is_finished() {
                self.git.running = None;
            }
        }
        if self.git.running.is_some() || (same_base(&base, &self.git.base) && self.git.version == Some(version)) {
            return changed;
        }

        // An untracked file has nothing to compare with and gets no marks, as in Zed.
        let Some(head) = base.as_ref().and_then(|base| base.head.clone()) else {
            changed |= !self.git.hunks.is_empty() || !same_base(&base, &self.git.base);
            self.git = GitMarks { base, version: Some(version), hunks: Vec::new(), running: None };
            return changed;
        };
        let buffer = self.editor.buffer();
        if buffer.len_lines() <= SYNC_LINES {
            let hunks = bakedit_git::line_hunks_with_deadline(&head, &buffer.to_string(), Some(Instant::now() + DIFF_BUDGET));
            self.git = GitMarks { base, version: Some(version), hunks, running: None };
            return true;
        }
        let current = buffer.to_string();
        let task = project.spawn(move || bakedit_git::line_hunks_with_deadline(&head, &current, Some(Instant::now() + DIFF_BUDGET)));
        self.git.running = Some((version, base, task));
        changed
    }
}
