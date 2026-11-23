//! Git backend over the git command line: status, diff, stage, commit, permalinks.

mod error;
mod exec;
mod index;
mod paths;
mod repo;
mod status;

pub use error::{GitError, Result};
pub use index::{CommitOptions, RestoreOutcome};
pub use repo::{Repo, DEFAULT_NETWORK_TIMEOUT};
pub use status::{
    parse_status, ConflictKind, DisplayStatus, FileState, RepoState, StageState, Status,
    StatusCounts, StatusEntry,
};
