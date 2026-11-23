//! Branches: listing, switching and creating them.

use std::path::PathBuf;

use crate::error::{GitError, Result};
use crate::paths::path_from_bytes;
use crate::repo::Repo;

/// The fields asked of `git for-each-ref`, each followed by a NUL so that no value (a
/// work tree path may hold a line feed) can be mistaken for a separator.
const BRANCH_FORMAT: &str = "--format=%(refname)%00%(HEAD)%00%(objectname)%00\
%(committerdate:unix)%00%(upstream:short)%00%(symref)%00%(worktreepath)%00%(contents:subject)%00";

/// How many fields [`BRANCH_FORMAT`] asks for.
const BRANCH_FIELDS: usize = 8;

/// A local branch or a remote-tracking branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branch {
    /// The name as it is typed: `main`, or `origin/main` for a remote-tracking branch.
    pub name: String,
    /// Whether this is a remote-tracking branch (`refs/remotes/...`).
    pub remote: bool,
    /// Whether this is the branch checked out in this work tree.
    pub current: bool,
    /// The branch a local branch tracks, for example `origin/main`.
    pub upstream: Option<String>,
    /// The full hash of the branch's newest commit.
    pub sha: String,
    /// When that commit was made (its committer date), in seconds since the Unix epoch.
    pub last_commit_time: i64,
    /// The first line of that commit's message.
    pub subject: String,
    /// The work tree that has this branch checked out, when there is one: this work
    /// tree for the current branch, or another one, in which case git will refuse to
    /// check the branch out here.
    pub worktree: Option<PathBuf>,
}

/// Reads the output of `git for-each-ref` with [`BRANCH_FORMAT`]. Symbolic references
/// (`origin/HEAD`) and anything that is not a branch are left out.
fn parse_branches(output: &[u8]) -> Vec<Branch> {
    let fields: Vec<&[u8]> = output.split(|&byte| byte == 0).collect();
    fields
        .chunks_exact(BRANCH_FIELDS)
        .filter_map(|record| {
            let text = |index: usize| String::from_utf8_lossy(record[index]).into_owned();
            // Every record but the first starts with the line feed that ended the one
            // before it.
            let refname = text(0);
            let refname = refname.trim_start_matches('\n');
            let (name, remote) = match refname.strip_prefix("refs/heads/") {
                Some(name) => (name, false),
                None => (refname.strip_prefix("refs/remotes/")?, true),
            };
            let is_symbolic = !record[5].is_empty();
            if is_symbolic || name.is_empty() {
                return None;
            }
            Some(Branch {
                name: name.to_string(),
                remote,
                current: record[1] == b"*",
                upstream: Some(text(4)).filter(|upstream| !upstream.is_empty()),
                sha: text(2),
                last_commit_time: text(3).parse().unwrap_or(0),
                subject: text(7),
                worktree: Some(record[6])
                    .filter(|path| !path.is_empty())
                    .map(path_from_bytes),
            })
        })
        .collect()
}

/// Rejects names git would read as an option, and the empty name.
fn check_branch_name(name: &str) -> Result<()> {
    if name.is_empty() || name.starts_with('-') {
        return Err(GitError::InvalidBranchName(name.to_string()));
    }
    Ok(())
}

impl Repo {
    /// All local branches followed by all remote-tracking branches, each group with the
    /// most recently committed-to branch first. `origin/HEAD` style aliases are left out.
    ///
    /// Before the first commit there are no branches yet and the list is empty, even
    /// though [`Repo::current_branch`] already has a name. With a detached `HEAD` no
    /// branch is flagged as current.
    pub fn branches(&self) -> Result<Vec<Branch>> {
        let output = self
            .git()
            .args([
                "for-each-ref",
                "--sort=-committerdate",
                BRANCH_FORMAT,
                "refs/heads",
                "refs/remotes",
            ])
            .run()?;
        let mut branches = parse_branches(&output.stdout);
        // Stable: keeps the newest-first order inside both groups.
        branches.sort_by_key(|branch| branch.remote);
        Ok(branches)
    }

    /// Switches to `branch`.
    ///
    /// A local branch is checked out. A remote-tracking branch such as `origin/topic` is
    /// checked out as the local branch `topic`: the existing one if there is one, else a
    /// new one that tracks the remote branch. Any other name is handed to
    /// `git checkout` as it is, which detaches `HEAD` for a tag or a commit hash.
    ///
    /// Git refuses, and this returns [`GitError::Failed`] with its explanation, when
    /// local changes would be overwritten or the branch is checked out in another work
    /// tree. Names starting with `-` are rejected as [`GitError::InvalidBranchName`].
    pub fn checkout(&self, branch: &str) -> Result<()> {
        check_branch_name(branch)?;
        if !self.ref_exists(&format!("refs/heads/{branch}"))?
            && self.ref_exists(&format!("refs/remotes/{branch}"))?
        {
            if let Some(local) = self.local_name_of(branch)? {
                return if self.ref_exists(&format!("refs/heads/{local}"))? {
                    self.checkout_existing(&local)
                } else {
                    self.git()
                        .args(["checkout", "-b", &local, "--track", branch])
                        .run()
                        .map(drop)
                };
            }
        }
        self.checkout_existing(branch)
    }

    /// Creates the branch `name` at the current commit and, with `checkout`, switches to
    /// it (uncommitted changes come along).
    ///
    /// A name git does not accept, or one that is taken, is [`GitError::Failed`] with
    /// git's explanation; a name starting with `-` is [`GitError::InvalidBranchName`].
    /// Before the first commit only `checkout: true` works: it renames the branch that
    /// the first commit will be made on.
    pub fn create_branch(&self, name: &str, checkout: bool) -> Result<()> {
        check_branch_name(name)?;
        let command = if checkout {
            self.git().args(["checkout", "-b", name])
        } else {
            self.git().args(["branch", "--", name])
        };
        command.run().map(drop)
    }

    fn checkout_existing(&self, name: &str) -> Result<()> {
        // The trailing `--` makes git read the name as a revision even when a file of
        // the same name exists.
        self.git().args(["checkout", name, "--"]).run().map(drop)
    }

    /// Whether the fully spelled-out reference `name` exists.
    fn ref_exists(&self, name: &str) -> Result<bool> {
        let output = self
            .git()
            .args(["show-ref", "--verify", "--quiet", name])
            .probe()?;
        Ok(output.code == Some(0))
    }

    /// The local name for a remote-tracking branch: `topic` for `origin/topic`. Remote
    /// names may contain slashes themselves, so the configured remotes decide where the
    /// remote's name ends; `None` when no remote matches.
    fn local_name_of(&self, remote_branch: &str) -> Result<Option<String>> {
        let local = self
            .remotes()?
            .iter()
            .filter_map(|remote| {
                remote_branch
                    .strip_prefix(remote.as_str())?
                    .strip_prefix('/')
            })
            .min_by_key(|local| local.len())
            .filter(|local| !local.is_empty())
            .map(str::to_string);
        Ok(local)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// One record the way `for-each-ref` writes it with [`BRANCH_FORMAT`].
    fn record(fields: [&str; BRANCH_FIELDS]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for field in fields {
            bytes.extend_from_slice(field.as_bytes());
            bytes.push(0);
        }
        bytes.push(b'\n');
        bytes
    }

    const SHA_A: &str = "8cbc8e3948a4d8437af274ed8924f9a77582e614";
    const SHA_B: &str = "05c16969050e1b3ed1da06fa5e46f34bac38a6f9";

    #[test]
    fn parses_local_and_remote_branches() {
        let mut output = record([
            "refs/heads/feat/x",
            "*",
            SHA_A,
            "1795406241",
            "origin/feat/x",
            "",
            "/work/project",
            "local change",
        ]);
        output.extend(record([
            "refs/remotes/origin/feat/x",
            " ",
            SHA_A,
            "1795406241",
            "",
            "",
            "",
            "local change",
        ]));
        output.extend(record([
            "refs/heads/main",
            " ",
            SHA_B,
            "1795406240",
            "",
            "",
            "/work/other tree",
            "",
        ]));
        let branches = parse_branches(&output);
        assert_eq!(
            branches,
            [
                Branch {
                    name: "feat/x".to_string(),
                    remote: false,
                    current: true,
                    upstream: Some("origin/feat/x".to_string()),
                    sha: SHA_A.to_string(),
                    last_commit_time: 1795406241,
                    subject: "local change".to_string(),
                    worktree: Some(PathBuf::from("/work/project")),
                },
                Branch {
                    name: "origin/feat/x".to_string(),
                    remote: true,
                    current: false,
                    upstream: None,
                    sha: SHA_A.to_string(),
                    last_commit_time: 1795406241,
                    subject: "local change".to_string(),
                    worktree: None,
                },
                Branch {
                    name: "main".to_string(),
                    remote: false,
                    current: false,
                    upstream: None,
                    sha: SHA_B.to_string(),
                    last_commit_time: 1795406240,
                    subject: String::new(),
                    worktree: Some(PathBuf::from("/work/other tree")),
                },
            ]
        );
    }

    #[test]
    fn symbolic_references_and_other_refs_are_left_out() {
        let mut output = record([
            "refs/remotes/origin/HEAD",
            " ",
            SHA_A,
            "1795406241",
            "",
            "refs/remotes/origin/main",
            "",
            "subject",
        ]);
        output.extend(record([
            "refs/tags/v1.0",
            " ",
            SHA_A,
            "1795406241",
            "",
            "",
            "",
            "subject",
        ]));
        output.extend(record([
            "refs/remotes/origin/main",
            " ",
            SHA_A,
            "not a number",
            "",
            "",
            "",
            "subject",
        ]));
        let branches = parse_branches(&output);
        assert_eq!(branches.len(), 1);
        assert_eq!(branches[0].name, "origin/main");
        assert_eq!(branches[0].last_commit_time, 0);
    }

    #[test]
    fn a_line_feed_in_a_work_tree_path_does_not_split_the_record() {
        let mut output = record([
            "refs/heads/odd",
            " ",
            SHA_A,
            "1",
            "",
            "",
            "/work/odd\npath",
            "subject",
        ]);
        output.extend(record([
            "refs/heads/next",
            " ",
            SHA_B,
            "2",
            "",
            "",
            "",
            "s",
        ]));
        let branches = parse_branches(&output);
        assert_eq!(branches.len(), 2);
        assert_eq!(
            branches[0].worktree.as_deref(),
            Some(Path::new("/work/odd\npath"))
        );
        assert_eq!(branches[1].name, "next");
    }

    #[test]
    fn empty_and_truncated_output_give_no_branches() {
        assert!(parse_branches(b"").is_empty());
        assert!(parse_branches(b"refs/heads/main\0*\0abc").is_empty());
    }

    #[test]
    fn names_that_look_like_options_are_rejected() {
        assert!(check_branch_name("feature/x").is_ok());
        for name in ["", "-f", "--orphan"] {
            match check_branch_name(name) {
                Err(GitError::InvalidBranchName(rejected)) => assert_eq!(rejected, name),
                other => panic!("unexpected {other:?}"),
            }
        }
    }
}
