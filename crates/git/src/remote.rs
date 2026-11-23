//! Talking to remotes: fetch, pull and push.

use crate::error::{GitError, Result};
use crate::repo::Repo;

impl Repo {
    /// Runs `git fetch` for the current branch's remote (`origin` when it has none) and
    /// returns what git printed, both output streams in the order they were written. A
    /// repository without remotes fetches nothing and returns an empty text.
    ///
    /// Stopped with [`GitError::TimedOut`] when it exceeds [`Repo::network_timeout`].
    pub fn fetch(&self) -> Result<String> {
        self.run_network_command(&["fetch"])
    }

    /// Runs `git pull` and returns what git printed. With `rebase` the local commits are
    /// rebased onto the upstream (`--rebase`); without it git integrates the way the
    /// user's configuration says (`pull.rebase`, `pull.ff`), which is a merge by default.
    ///
    /// When the pull stops on a conflict, git exits with an error: this returns
    /// [`GitError::Failed`] with the output, and [`Repo::status`] then shows the
    /// conflicted files and the merge or rebase in progress. A branch without an upstream
    /// is an error from git as well.
    pub fn pull(&self, rebase: bool) -> Result<String> {
        if rebase {
            self.run_network_command(&["pull", "--no-edit", "--rebase"])
        } else {
            self.run_network_command(&["pull", "--no-edit"])
        }
    }

    /// Pushes the current branch and returns what git printed.
    ///
    /// A branch that has no upstream yet is published: pushed to the branch of the same
    /// name on the remote (`remote.pushDefault` if set, else `origin`, else the only
    /// remote there is) with upstream tracking set up, so the next push and pull need no
    /// arguments. A branch with an upstream is pushed with a plain `git push`.
    ///
    /// `force_with_lease` overwrites the remote branch, but only if it still is where the
    /// last fetch saw it. A rejected push, a detached `HEAD` and a failing pre-push hook
    /// are [`GitError::Failed`] with git's explanation; [`GitError::NoRemote`] means
    /// there was no remote to publish to.
    pub fn push(&self, force_with_lease: bool) -> Result<String> {
        let mut args = vec!["push".to_string()];
        if force_with_lease {
            args.push("--force-with-lease".to_string());
        }
        if let Some(branch) = self.current_branch()? {
            if self.config(&format!("branch.{branch}.remote"))?.is_none() {
                args.push("--set-upstream".to_string());
                args.push(self.publish_remote()?);
                args.push(format!("refs/heads/{branch}:refs/heads/{branch}"));
            }
        }
        self.run_network_command(&args)
    }

    /// The names of the configured remotes.
    pub(crate) fn remotes(&self) -> Result<Vec<String>> {
        let output = self.git().arg("remote").run()?;
        Ok(output
            .stdout_text()
            .lines()
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// The remote a branch without an upstream is published to.
    fn publish_remote(&self) -> Result<String> {
        let remote = match self.config("remote.pushDefault")? {
            Some(remote) => remote,
            None => self.fallback_remote()?,
        };
        // A name git would read as an option cannot be a remote.
        if remote.is_empty() || remote.starts_with('-') {
            return Err(GitError::NoRemote);
        }
        Ok(remote)
    }

    /// `origin` when it exists, else the repository's only remote.
    pub(crate) fn fallback_remote(&self) -> Result<String> {
        let mut remotes = self.remotes()?;
        if remotes.iter().any(|name| name == "origin") {
            Ok("origin".to_string())
        } else if remotes.len() == 1 {
            Ok(remotes.remove(0))
        } else {
            Err(GitError::NoRemote)
        }
    }

    /// Runs a command that talks to a remote: time limited, with both output streams
    /// captured as one text.
    fn run_network_command<S: AsRef<str>>(&self, args: &[S]) -> Result<String> {
        let output = self
            .git()
            .args(args.iter().map(AsRef::as_ref))
            .merged()
            .timeout(self.network_timeout())
            .run()?;
        Ok(output.stdout_text())
    }
}
