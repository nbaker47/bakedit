//! Git backend over the git command line: status, diff, stage, commit, permalinks.

mod branch;
mod content;
mod error;
mod exec;
mod index;
mod log;
mod paths;
mod remote;
mod repo;
mod status;

pub use branch::Branch;
pub use error::{GitError, Result};
pub use index::{CommitOptions, RestoreOutcome};
pub use log::Commit;
pub use repo::{Repo, DEFAULT_NETWORK_TIMEOUT};
pub use status::{
    parse_status, ConflictKind, DisplayStatus, FileState, RepoState, StageState, Status,
    StatusCounts, StatusEntry,
};
