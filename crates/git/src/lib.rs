//! Git for the IDE: the git panel, the editor gutter and the project diff view.
//!
//! Everything goes through the user's own `git` command line (no libgit2), so their
//! configuration, hooks, credential helpers and commit signing all apply. This crate has
//! no user interface and prints nothing; it returns data and typed errors.
//!
//! # The pieces
//!
//! - [`Repo::discover`] finds the work tree a path belongs to.
//! - [`Repo::status`] reads the branch, its upstream and every changed file.
//! - [`Repo::stage`], [`Repo::unstage`], [`Repo::restore`] and [`Repo::commit`] change the
//!   index and the work tree; [`Repo::fetch`], [`Repo::pull`] and [`Repo::push`] talk to
//!   remotes; [`Repo::branches`], [`Repo::checkout`], [`Repo::create_branch`] and
//!   [`Repo::log`] deal with history.
//! - [`Repo::head_text`] and [`Repo::index_text`] give the editor something to compare a
//!   buffer with, and [`line_hunks`] compares them, in process, so unsaved text works.
//! - [`Repo::diff_files`] and [`Repo::diff_file`] return diffs as files, hunks and lines;
//!   [`Repo::apply_hunk`] stages, unstages or discards a single hunk.
//! - [`Repo::permalink`] builds a link to a file's lines on the hosting site.
//!
//! The parsers behind these are pure functions that can be used on their own:
//! [`parse_status`], [`parse_diff`], [`parse_remote_url`] and [`build_permalink`].
//!
//! # Blocking and threads
//!
//! Every [`Repo`] method starts a git process and waits for it. Call them from worker
//! threads: a `Repo` is `Send + Sync` and cheap to clone, and so is everything it
//! returns. Reading (status, diffs, contents, log) can run in parallel with anything.
//! Calls that change the index (staging, committing, checking out, applying hunks) should
//! not overlap each other for one repository: git protects the index with a lock file and
//! the loser fails with [`GitError::Failed`].
//!
//! # Paths
//!
//! Path arguments are either absolute (inside the work tree) or relative to
//! [`Repo::root`]; never relative to the process's current directory. Paths in results
//! are relative to the root. File names are taken literally (`*` and `[` are not
//! patterns) and are bytes as far as this crate is concerned: they need not be UTF-8.
//!
//! On macOS git spells file names with accents in composed form whatever form the file
//! system holds them in. Paths passed in are converted for you; to compare a path of
//! your own with one in a result, run it through [`Repo::relative_path`] first.
//!
//! # How git is run
//!
//! Every command is `git -C <root>` with a fixed environment: messages untranslated and
//! uncoloured, no credential prompt, no editor, no optional locks (so a status never
//! waits for, or gets in the way of, a git command running in a terminal), standard
//! input closed and no controlling terminal. A hook, ssh or gpg that wants to ask
//! something on the terminal fails with a message instead of drawing over the IDE;
//! passwords and passphrases have to come from an agent, a credential helper or a
//! graphical prompt. [`Repo::fetch`], [`Repo::pull`] and [`Repo::push`] are stopped,
//! with everything they started, when they run longer than [`Repo::network_timeout`].
//!
//! # Requirements
//!
//! A Unix system with git 2.26 or newer on `PATH`.

#![warn(missing_docs)]

mod blame;
mod branch;
mod content;
mod diff;
mod error;
mod exec;
mod index;
mod line_diff;
mod log;
mod patch;
mod paths;
mod permalink;
#[cfg(target_os = "macos")]
mod precompose;
mod remote;
mod repo;
mod status;
#[cfg(test)]
mod testing;

pub use blame::{format_relative_time, parse_blame_porcelain, BlameInfo, FileBlame};
pub use branch::Branch;
pub use diff::{
    parse_diff, DiffLine, DiffLineKind, DiffStatus, DiffTarget, FileDiff, Hunk,
    MAX_UNTRACKED_FILE_SIZE, MAX_UNTRACKED_TOTAL_SIZE,
};
pub use error::{GitError, Result};
pub use index::{CommitOptions, RestoreOutcome};
pub use line_diff::{
    line_hunks, line_hunks_with_deadline, LineHunk, LineHunkKind, LINE_DIFF_TIMEOUT,
};
pub use log::Commit;
pub use patch::HunkAction;
pub use permalink::{build_permalink, parse_remote_url, HostKind, RemoteUrl};
pub use repo::{Repo, DEFAULT_NETWORK_TIMEOUT};
pub use status::{
    parse_status, ConflictKind, DisplayStatus, FileState, RepoState, StageState, Status,
    StatusCounts, StatusEntry,
};

#[cfg(test)]
mod tests {
    use super::*;

    /// The app runs every call on a worker thread and hands the result to the UI thread.
    #[test]
    fn everything_public_can_cross_threads() {
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<Repo>();
        assert_send_sync::<GitError>();
        assert_send_sync::<Result<Status>>();
        assert_send_sync::<Status>();
        assert_send_sync::<StatusEntry>();
        assert_send_sync::<StatusCounts>();
        assert_send_sync::<RestoreOutcome>();
        assert_send_sync::<CommitOptions>();
        assert_send_sync::<Branch>();
        assert_send_sync::<Commit>();
        assert_send_sync::<FileDiff>();
        assert_send_sync::<Hunk>();
        assert_send_sync::<DiffLine>();
        assert_send_sync::<DiffTarget>();
        assert_send_sync::<HunkAction>();
        assert_send_sync::<LineHunk>();
        assert_send_sync::<RemoteUrl>();
        assert_send_sync::<HostKind>();
    }
}
