//! Git backend over the git command line: status, diff, stage, commit, permalinks.

mod error;
mod exec;
mod paths;
mod repo;

pub use error::{GitError, Result};
pub use repo::{Repo, DEFAULT_NETWORK_TIMEOUT};
