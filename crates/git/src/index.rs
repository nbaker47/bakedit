//! Changing the index and the work tree: staging, unstaging, discarding and committing.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::error::{GitError, Result};
use crate::log::LOG_OPTIONS;
use crate::paths::pathspec_input;
use crate::repo::Repo;
use crate::status::{FileState, StatusEntry};

/// How [`Repo::commit`] makes its commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CommitOptions {
    /// Replace the last commit instead of adding a new one.
    pub amend: bool,
    /// Add a `Signed-off-by` line for the committer.
    pub sign_off: bool,
}

/// What [`Repo::restore`] did with the files it was pointed at.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RestoreOutcome {
    /// Tracked files whose work tree changes were discarded: they hold the index's
    /// content again (and exist again, if they had been deleted).
    pub restored: Vec<PathBuf>,
    /// Files that only exist in the work tree: untracked files, and files that were added
    /// with `git add --intent-to-add`. They were **not** touched. Discarding them means
    /// deleting them, which is for the caller to do, to the trash and only after asking.
    pub untracked: Vec<PathBuf>,
    /// Conflicted files. They were not touched either: there is no single index version
    /// to go back to.
    pub conflicted: Vec<PathBuf>,
}

impl Repo {
    /// Stages the work tree's version of each path: new content, new files and deletions.
    /// A directory stages everything below it, and staging a conflicted file marks the
    /// conflict as resolved. Names are taken literally; `*` and `[` are not patterns.
    ///
    /// An empty list does nothing. A path that matches no file is an error from git, as
    /// is an ignored file.
    pub fn stage<P: AsRef<Path>>(&self, paths: &[P]) -> Result<()> {
        let paths = self.relative_paths(paths)?;
        if paths.is_empty() {
            return Ok(());
        }
        self.git()
            .args([
                "add",
                "--all",
                "--pathspec-from-file=-",
                "--pathspec-file-nul",
            ])
            .stdin(pathspec_input(&paths))
            .run()?;
        Ok(())
    }

    /// Takes each path's changes out of the index again, leaving the work tree alone.
    /// Works before the first commit too, where it turns staged files back into
    /// untracked ones. A directory unstages everything below it.
    ///
    /// A staged rename is two paths: pass both ([`StatusEntry::paths`]) to undo it as a
    /// whole. Unstaging a conflicted file replaces its conflict entry by the `HEAD`
    /// version, as `git reset` does. An empty list does nothing.
    pub fn unstage<P: AsRef<Path>>(&self, paths: &[P]) -> Result<()> {
        let paths = self.relative_paths(paths)?;
        if paths.is_empty() {
            return Ok(());
        }
        self.git()
            .args([
                "reset",
                "--quiet",
                "--pathspec-from-file=-",
                "--pathspec-file-nul",
            ])
            .stdin(pathspec_input(&paths))
            .run()?;
        Ok(())
    }

    /// Stages everything: all changes, all untracked files, all deletions. Conflicted
    /// files are marked as resolved in whatever state they are in.
    pub fn stage_all(&self) -> Result<()> {
        self.git().args(["add", "--all"]).run()?;
        Ok(())
    }

    /// Unstages every staged change. Conflicted files and a merge in progress are left as
    /// they are.
    pub fn unstage_all(&self) -> Result<()> {
        let status = self.status()?;
        let staged: Vec<&Path> = status
            .staged()
            .into_iter()
            .flat_map(StatusEntry::paths)
            .collect();
        self.unstage(&staged)
    }

    /// Discards the work tree changes of tracked files among `paths`, taking each back to
    /// what the index holds: unstaged edits are dropped and deleted files come back.
    /// Staged changes stay staged. A directory covers everything below it.
    ///
    /// Nothing is ever deleted. Untracked files and conflicted files among the paths are
    /// left alone and reported in the [`RestoreOutcome`], so the caller can ask the user
    /// and move the former to the trash. An empty list does nothing.
    pub fn restore<P: AsRef<Path>>(&self, paths: &[P]) -> Result<RestoreOutcome> {
        let paths = self.relative_paths(paths)?;
        if paths.is_empty() {
            return Ok(RestoreOutcome::default());
        }
        // A file is covered when it, or a directory above it, is among the paths.
        let paths: HashSet<&Path> = paths.iter().map(PathBuf::as_path).collect();
        self.restore_entries(|entry| {
            entry
                .path
                .ancestors()
                .any(|ancestor| paths.contains(ancestor))
        })
    }

    /// [`Repo::restore`] for the whole work tree: discards every unstaged change to a
    /// tracked file and reports the untracked and conflicted files it left alone.
    pub fn restore_all(&self) -> Result<RestoreOutcome> {
        self.restore_entries(|_| true)
    }

    fn restore_entries(&self, selected: impl Fn(&StatusEntry) -> bool) -> Result<RestoreOutcome> {
        let status = self.status()?;
        let mut outcome = RestoreOutcome::default();
        for entry in status.entries.iter().filter(|entry| selected(entry)) {
            if entry.conflicted {
                outcome.conflicted.push(entry.path.clone());
            } else if entry.untracked || only_in_work_tree(entry.worktree) {
                outcome.untracked.push(entry.path.clone());
            } else if entry.worktree != FileState::Unmodified && !entry.submodule {
                outcome.restored.push(entry.path.clone());
            }
        }
        if !outcome.restored.is_empty() {
            self.git()
                .args([
                    "restore",
                    "--worktree",
                    "--pathspec-from-file=-",
                    "--pathspec-file-nul",
                ])
                .stdin(pathspec_input(&outcome.restored))
                .run()?;
        }
        Ok(outcome)
    }

    /// Commits what is staged with `message` and returns the new commit's full hash.
    ///
    /// The message is handed to git on standard input and committed as typed: it can
    /// hold anything, and lines starting with `#` are kept (only blank lines at either
    /// end and trailing spaces are removed). The user's hooks run and their configuration
    /// applies, signing included.
    ///
    /// Errors: [`GitError::EmptyCommitMessage`] when the message is blank;
    /// [`GitError::NothingStaged`] when the index holds no change, unless this is an
    /// amend (which may change only the message) or concludes a merge;
    /// [`GitError::Failed`] with git's or the hook's output when git refuses, for example
    /// because a hook failed or files are still conflicted.
    pub fn commit(&self, message: &str, options: CommitOptions) -> Result<String> {
        if message.trim().is_empty() {
            return Err(GitError::EmptyCommitMessage);
        }
        let concludes_merge = self.git_dir().join("MERGE_HEAD").exists();
        if !options.amend && !concludes_merge && !self.has_staged_changes()? {
            return Err(GitError::NothingStaged);
        }
        // The message is taken as typed, whatever `commit.cleanup` and
        // `i18n.commitEncoding` say: it is not an editor template with comment lines,
        // and it is UTF-8.
        let mut command = self.git().config("i18n.commitEncoding=UTF-8").args([
            "commit",
            "--file=-",
            "--cleanup=whitespace",
        ]);
        if options.amend {
            command = command.arg("--amend");
        }
        if options.sign_off {
            command = command.arg("--signoff");
        }
        command.stdin(message).run()?;
        self.head_sha()?.ok_or(GitError::NoCommits)
    }

    /// The full message of the current commit, without trailing blank lines: what an
    /// amend starts from. `None` before the first commit.
    pub fn head_commit_message(&self) -> Result<Option<String>> {
        if self.head_sha()?.is_none() {
            return Ok(None);
        }
        let output = self
            .git()
            .args(LOG_OPTIONS)
            .args(["-1", "--format=%B"])
            .run()?;
        Ok(Some(output.stdout_text().trim_end().to_string()))
    }

    /// Whether the index differs from `HEAD` (from nothing, before the first commit).
    fn has_staged_changes(&self) -> Result<bool> {
        let output = self
            .git()
            .args(["diff", "--cached", "--quiet", "--no-ext-diff"])
            .probe()?;
        Ok(output.code == Some(1))
    }
}

/// Whether a work tree state means the file's content exists nowhere but in the work
/// tree. Git knows such a file only by name (`git add --intent-to-add`), and "restoring"
/// it would empty it.
fn only_in_work_tree(state: FileState) -> bool {
    matches!(
        state,
        FileState::Added | FileState::Renamed | FileState::Copied
    )
}
