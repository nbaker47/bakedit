//! The error type every fallible call in this crate returns.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

/// Shorthand for results of git operations.
pub type Result<T> = std::result::Result<T, GitError>;

/// Why a git operation did not produce a result.
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    /// The `git` program could not be started or supervised: it is not installed, not on
    /// `PATH`, or the pipes to it could not be set up.
    #[error("could not run git: {0}")]
    Spawn(#[source] io::Error),

    /// git ran and exited with a status the operation does not accept. `code` is `None`
    /// when a signal ended the process. The network commands (`fetch`, `pull`, `push`)
    /// capture both output streams as one text, in the order it was written; it arrives
    /// in `stdout` and `stderr` stays empty. [`GitError::git_output`] reads either.
    #[error("{}", failure_text(.command, *.code, .stderr, .stdout))]
    Failed {
        /// The git subcommand, for example `commit`.
        command: String,
        /// The exit code, or `None` when a signal ended the process.
        code: Option<i32>,
        /// Everything git wrote to standard output.
        stdout: String,
        /// Everything git wrote to standard error (hook output arrives here too).
        stderr: String,
    },

    /// A network command ran longer than the repository's network timeout and was
    /// stopped. `output` is whatever it had written until then.
    #[error("git {command} did not finish within {} seconds and was stopped", .limit.as_secs())]
    TimedOut {
        /// The git subcommand, for example `fetch`.
        command: String,
        /// The limit that was exceeded.
        limit: Duration,
        /// Output captured before the process was stopped.
        output: String,
    },

    /// `commit` was called with a message that is empty or only white space.
    #[error("the commit message is empty")]
    EmptyCommitMessage,

    /// `commit` was called with nothing staged (and it was not an amend or the conclusion
    /// of a merge).
    #[error("nothing is staged to commit")]
    NothingStaged,

    /// The repository has no commits yet, so there is nothing to refer to.
    #[error("the repository has no commits yet")]
    NoCommits,

    /// A path argument is absolute and lies outside the work tree, or climbs out of it
    /// with `..`.
    #[error("{} is not inside the repository", .0.display())]
    PathOutsideRepo(PathBuf),

    /// No remote could be chosen: the current branch has no upstream remote, there is no
    /// remote called `origin`, and there is not exactly one other remote to fall back on.
    #[error("the repository has no remote to use")]
    NoRemote,

    /// The remote's URL is not one a web link can be made from (a local path, for
    /// example).
    #[error("cannot make a web link from the remote URL {0}")]
    UnsupportedRemoteUrl(String),

    /// A branch name that git would read as an option, or an empty one.
    #[error("{0:?} is not a valid branch name")]
    InvalidBranchName(String),

    /// `apply_hunk` was asked for something a patch cannot do: the hunk index is out of
    /// range, the file is binary or conflicted, or the action would delete a file.
    #[error("{0}")]
    InvalidHunk(String),

    /// A file in the work tree could not be read.
    #[error("could not read {}: {source}", .path.display())]
    Io {
        /// The file that could not be read.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: io::Error,
    },

    /// git answered with something this crate does not understand.
    #[error("unexpected output from git {command}: {detail}")]
    UnexpectedOutput {
        /// The git subcommand that produced the output.
        command: String,
        /// What was wrong with it.
        detail: String,
    },
}

impl GitError {
    /// The exit code of the failed git process, when there was one.
    pub fn exit_code(&self) -> Option<i32> {
        match self {
            GitError::Failed { code, .. } => *code,
            _ => None,
        }
    }

    /// What git printed while failing: its standard error (or standard output when
    /// standard error was empty) for [`GitError::Failed`], the partial output for
    /// [`GitError::TimedOut`], and `None` for errors that did not come from a git run.
    pub fn git_output(&self) -> Option<&str> {
        match self {
            GitError::Failed { stdout, stderr, .. } => Some(preferred_output(stderr, stdout)),
            GitError::TimedOut { output, .. } => Some(output.trim()),
            _ => None,
        }
    }
}

/// The stream that explains a failure: standard error, or standard output when git put its
/// explanation there (`nothing to commit` is an example).
fn preferred_output<'a>(stderr: &'a str, stdout: &'a str) -> &'a str {
    let stderr = stderr.trim();
    if stderr.is_empty() {
        stdout.trim()
    } else {
        stderr
    }
}

fn failure_text(command: &str, code: Option<i32>, stderr: &str, stdout: &str) -> String {
    let how = match code {
        Some(code) => format!("exit code {code}"),
        None => "stopped by a signal".to_string(),
    };
    let output = preferred_output(stderr, stdout);
    if output.is_empty() {
        format!("git {command} failed ({how})")
    } else {
        format!("git {command} failed ({how}): {output}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed(code: Option<i32>, stdout: &str, stderr: &str) -> GitError {
        GitError::Failed {
            command: "commit".to_string(),
            code,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        }
    }

    #[test]
    fn failure_shows_the_exit_code_and_standard_error() {
        let error = failed(Some(1), "ignored\n", "hook said no\n");
        assert_eq!(
            error.to_string(),
            "git commit failed (exit code 1): hook said no"
        );
        assert_eq!(error.exit_code(), Some(1));
        assert_eq!(error.git_output(), Some("hook said no"));
    }

    #[test]
    fn failure_falls_back_to_standard_output() {
        let error = failed(Some(1), "nothing to commit\n", "  \n");
        assert_eq!(
            error.to_string(),
            "git commit failed (exit code 1): nothing to commit"
        );
        assert_eq!(error.git_output(), Some("nothing to commit"));
    }

    #[test]
    fn failure_without_output_or_exit_code() {
        let error = failed(None, "", "");
        assert_eq!(error.to_string(), "git commit failed (stopped by a signal)");
        assert_eq!(error.exit_code(), None);
    }

    #[test]
    fn other_errors_carry_no_git_output() {
        assert_eq!(GitError::NothingStaged.git_output(), None);
        assert_eq!(GitError::NothingStaged.exit_code(), None);
        let timed_out = GitError::TimedOut {
            command: "fetch".to_string(),
            limit: Duration::from_secs(300),
            output: "From somewhere\n".to_string(),
        };
        assert_eq!(timed_out.git_output(), Some("From somewhere"));
        assert_eq!(
            timed_out.to_string(),
            "git fetch did not finish within 300 seconds and was stopped"
        );
    }

    #[test]
    fn errors_cross_threads() {
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<GitError>();
    }
}
