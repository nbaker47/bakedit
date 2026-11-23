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
    /// user's configuration says (`pull.rebase`, `pull.ff`). With nothing configured git
    /// fast-forwards when it can, and when both sides have new commits it refuses and
    /// asks for a choice.
    ///
    /// That refusal, a pull that stops on a conflict, and a branch without an upstream
    /// are all errors from git: [`GitError::Failed`] with its output. After a conflict
    /// [`Repo::status`] shows the conflicted files and the merge or rebase in progress.
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
    /// name on the remote (`branch.<name>.pushRemote` or `remote.pushDefault` if set,
    /// else `origin`, else the only remote there is) with upstream tracking set up, so
    /// the next push and pull need no arguments. A branch with an upstream is pushed with
    /// a plain `git push`.
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
                args.push(self.publish_remote(&branch)?);
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

    /// The remote `branch`, which has no upstream, is published to: the one the user
    /// configured for pushing this branch or for pushing in general, else the fallback.
    fn publish_remote(&self, branch: &str) -> Result<String> {
        let configured = match self.config(&format!("branch.{branch}.pushRemote"))? {
            Some(remote) => Some(remote),
            None => self.config("remote.pushDefault")?,
        };
        let remote = match configured {
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
        let result = self
            .git()
            .args(args.iter().map(AsRef::as_ref))
            .merged()
            .timeout(self.network_timeout())
            .run();
        match result {
            Ok(output) => Ok(output.stdout_text()),
            // The one captured text holds what git wrote to standard error, which is
            // where a reader of the error looks for the explanation.
            Err(GitError::Failed {
                command,
                code,
                stdout,
                stderr,
            }) if stderr.is_empty() => Err(GitError::Failed {
                command,
                code,
                stdout: String::new(),
                stderr: stdout,
            }),
            Err(error) => Err(error),
        }
    }
}
