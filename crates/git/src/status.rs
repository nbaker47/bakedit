//! The state of the index and the work tree, read from `git status --porcelain=v2`.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::paths::path_from_bytes;
use crate::repo::Repo;

/// How many characters of a commit hash [`Status::head`] shows.
const SHORT_HASH_LEN: usize = 7;

/// What happened to a file on one side: in the index compared with `HEAD`, or in the work
/// tree compared with the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileState {
    /// No change on this side.
    Unmodified,
    /// The file is new on this side. In the work tree this only happens for a file that
    /// was added with `git add --intent-to-add`.
    Added,
    /// The content or the executable bit changed.
    Modified,
    /// The file is gone.
    Deleted,
    /// The file was moved; [`StatusEntry::orig_path`] is where it came from.
    Renamed,
    /// The file is a copy of [`StatusEntry::orig_path`].
    Copied,
    /// The kind of file changed, for example from a regular file to a symbolic link.
    TypeChanged,
}

/// How much of a file's change is staged: the three states of the checkbox next to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StageState {
    /// Everything that changed is in the index.
    Staged,
    /// Nothing is in the index. Untracked and conflicted files are always in this state.
    Unstaged,
    /// The index holds some changes and the work tree holds more.
    PartiallyStaged,
}

/// The one-word summary of an entry, for its label and colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisplayStatus {
    /// New: staged as a new file, a copy, or marked intent-to-add.
    Added,
    /// Changed content, mode or file type.
    Modified,
    /// Deleted in the index or in the work tree.
    Deleted,
    /// Moved, with or without further changes.
    Renamed,
    /// Not known to git and not ignored.
    Untracked,
    /// Left unmerged by a merge, rebase, cherry-pick or revert.
    Conflict,
}

/// How the two sides of a merge disagree about a file. "Us" is the branch that was
/// checked out, "them" the one being merged in (during a rebase the roles are swapped, as
/// they are in git itself).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConflictKind {
    /// Both sides changed the file (`UU`).
    BothModified,
    /// Both sides added a file of this name (`AA`).
    BothAdded,
    /// Both sides deleted the file (`DD`).
    BothDeleted,
    /// We added it and they did something incompatible (`AU`).
    AddedByUs,
    /// They added it and we did something incompatible (`UA`).
    AddedByThem,
    /// We deleted it and they changed it (`DU`).
    DeletedByUs,
    /// They deleted it and we changed it (`UD`).
    DeletedByThem,
}

/// An operation git has started and not finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RepoState {
    /// A merge is waiting for conflicts to be resolved and committed.
    Merging,
    /// A rebase is stopped part way.
    Rebasing,
    /// A cherry-pick is stopped part way.
    CherryPicking,
    /// A revert is stopped part way.
    Reverting,
    /// A bisection is under way.
    Bisecting,
}

/// One changed, untracked or conflicted file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEntry {
    /// The file, relative to the top of the work tree.
    pub path: PathBuf,
    /// For a rename or copy, the path the file came from.
    pub orig_path: Option<PathBuf>,
    /// What is staged: the index compared with `HEAD`. Always `Unmodified` for untracked
    /// and conflicted files.
    pub index: FileState,
    /// What is not staged: the work tree compared with the index. Always `Unmodified`
    /// for untracked and conflicted files.
    pub worktree: FileState,
    /// The file is not known to git (and not ignored).
    pub untracked: bool,
    /// The file is unmerged.
    pub conflicted: bool,
    /// For a conflicted file, how the two sides disagree.
    pub conflict: Option<ConflictKind>,
    /// The entry is a submodule, not a file.
    pub submodule: bool,
}

impl StatusEntry {
    /// Whether the index holds a change to this file.
    pub fn has_staged(&self) -> bool {
        !self.untracked && !self.conflicted && self.index != FileState::Unmodified
    }

    /// Whether there is anything left to stage: a change in the work tree, an untracked
    /// file, or a conflict to mark as resolved.
    pub fn has_unstaged(&self) -> bool {
        self.untracked || self.conflicted || self.worktree != FileState::Unmodified
    }

    /// The state of the file's checkbox.
    pub fn stage_state(&self) -> StageState {
        match (self.has_staged(), self.has_unstaged()) {
            (true, false) => StageState::Staged,
            (true, true) => StageState::PartiallyStaged,
            (false, _) => StageState::Unstaged,
        }
    }

    /// Whether every change to this file is staged.
    pub fn is_fully_staged(&self) -> bool {
        self.stage_state() == StageState::Staged
    }

    /// The one-word summary of the entry. A conflict wins over everything; otherwise a
    /// deletion on either side wins over an addition, which wins over a rename, and
    /// anything else is a modification.
    pub fn display_status(&self) -> DisplayStatus {
        let either = |state: FileState| self.index == state || self.worktree == state;
        if self.conflicted {
            DisplayStatus::Conflict
        } else if self.untracked {
            DisplayStatus::Untracked
        } else if either(FileState::Deleted) {
            DisplayStatus::Deleted
        } else if either(FileState::Added) || either(FileState::Copied) {
            DisplayStatus::Added
        } else if either(FileState::Renamed) {
            DisplayStatus::Renamed
        } else {
            DisplayStatus::Modified
        }
    }

    /// Every path the entry touches: [`StatusEntry::path`], and for a rename or copy the
    /// path it came from. Pass these to `unstage` to undo a staged rename as a whole.
    pub fn paths(&self) -> Vec<&Path> {
        let mut paths = vec![self.path.as_path()];
        if let Some(orig_path) = &self.orig_path {
            paths.push(orig_path.as_path());
        }
        paths
    }
}

/// How many entries of a [`Status`] fall into each group. An entry that is partly staged
/// counts as both staged and unstaged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StatusCounts {
    /// Entries with a change in the index.
    pub staged: usize,
    /// Tracked, unconflicted entries with a change in the work tree.
    pub unstaged: usize,
    /// Untracked files.
    pub untracked: usize,
    /// Conflicted files.
    pub conflicted: usize,
    /// All entries.
    pub total: usize,
}

/// A snapshot of the repository: where `HEAD` is and which files differ.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Status {
    /// The branch that is checked out, also before its first commit. `None` when `HEAD`
    /// is detached.
    pub branch: Option<String>,
    /// The first seven characters of the current commit's hash. `None` before the first
    /// commit.
    pub head: Option<String>,
    /// The full hash of the current commit. `None` before the first commit.
    pub head_sha: Option<String>,
    /// The branch this one tracks, for example `origin/main`.
    pub upstream: Option<String>,
    /// An upstream is configured but does not exist (any more), so `ahead` and `behind`
    /// are unknown and reported as 0.
    pub upstream_gone: bool,
    /// Commits on this branch that the upstream does not have.
    pub ahead: usize,
    /// Commits on the upstream that this branch does not have.
    pub behind: usize,
    /// The operation git is in the middle of, if any. [`parse_status`] cannot know this
    /// and leaves it `None`; [`Repo::status`] fills it in.
    pub state: Option<RepoState>,
    /// Changed, untracked and conflicted files in the order git lists them: tracked files
    /// by path, then conflicted files, then untracked files. Ignored files are left out.
    pub entries: Vec<StatusEntry>,
}

impl Status {
    /// Entries with something in the index, fully or partly staged.
    pub fn staged(&self) -> Vec<&StatusEntry> {
        self.matching(StatusEntry::has_staged)
    }

    /// Entries with something left to stage: work tree changes, untracked files and
    /// conflicts.
    pub fn unstaged(&self) -> Vec<&StatusEntry> {
        self.matching(StatusEntry::has_unstaged)
    }

    /// The untracked files.
    pub fn untracked(&self) -> Vec<&StatusEntry> {
        self.matching(|entry| entry.untracked)
    }

    /// The conflicted files.
    pub fn conflicts(&self) -> Vec<&StatusEntry> {
        self.matching(|entry| entry.conflicted)
    }

    /// The entry for `path` (relative to the top of the work tree), if it has one.
    pub fn entry(&self, path: impl AsRef<Path>) -> Option<&StatusEntry> {
        let path = path.as_ref();
        self.entries.iter().find(|entry| entry.path == path)
    }

    /// How many entries fall into each group.
    pub fn counts(&self) -> StatusCounts {
        let mut counts = StatusCounts {
            total: self.entries.len(),
            ..StatusCounts::default()
        };
        for entry in &self.entries {
            if entry.untracked {
                counts.untracked += 1;
            } else if entry.conflicted {
                counts.conflicted += 1;
            } else {
                counts.staged += usize::from(entry.has_staged());
                counts.unstaged += usize::from(entry.has_unstaged());
            }
        }
        counts
    }

    /// Whether nothing differs: no changes, no untracked files, no conflicts.
    pub fn is_clean(&self) -> bool {
        self.entries.is_empty()
    }

    fn matching(&self, keep: impl Fn(&StatusEntry) -> bool) -> Vec<&StatusEntry> {
        self.entries.iter().filter(|entry| keep(entry)).collect()
    }
}

/// Reads the output of `git status --porcelain=v2 --branch -z`.
///
/// Records this function does not know (or cannot make sense of) are skipped, so a newer
/// git that adds record types still parses. [`Status::state`] is left `None`.
pub fn parse_status(output: &[u8]) -> Status {
    let mut status = Status::default();
    let mut records = output.split(|&byte| byte == 0);
    while let Some(record) = records.next() {
        let entry = match record.first() {
            Some(b'#') => {
                parse_header(record, &mut status);
                continue;
            }
            Some(b'1') => parse_changed(record, None),
            // The path a rename or copy came from follows as a record of its own.
            Some(b'2') => parse_changed(record, records.next()),
            Some(b'u') => parse_unmerged(record),
            Some(b'?') => record.strip_prefix(b"? ").map(untracked_entry),
            _ => None,
        };
        match entry {
            Some(entry) => status.entries.push(entry),
            None if record.is_empty() || record.starts_with(b"! ") => {}
            None => log::warn!(
                "skipping status record {:?}",
                String::from_utf8_lossy(record)
            ),
        }
    }
    status
}

/// Reads one `# branch.*` line into `status`.
fn parse_header(record: &[u8], status: &mut Status) {
    let text = String::from_utf8_lossy(record);
    let mut words = text.split(' ').skip(1);
    let (Some(key), Some(value)) = (words.next(), words.next()) else {
        return;
    };
    match key {
        "branch.oid" if value != "(initial)" => {
            status.head = Some(value.chars().take(SHORT_HASH_LEN).collect());
            status.head_sha = Some(value.to_string());
        }
        "branch.head" if value != "(detached)" => status.branch = Some(value.to_string()),
        "branch.upstream" => {
            status.upstream = Some(value.to_string());
            // Stays set unless a `branch.ab` line, which only an existing upstream
            // gets, follows.
            status.upstream_gone = true;
        }
        "branch.ab" => {
            let count = |word: Option<&str>| {
                word.and_then(|word| word.get(1..))
                    .and_then(|digits| digits.parse().ok())
                    .unwrap_or(0)
            };
            status.ahead = count(Some(value));
            status.behind = count(words.next());
            status.upstream_gone = false;
        }
        _ => {}
    }
}

/// Reads a `1` (changed) or `2` (renamed or copied) record. `orig_path` is the record
/// that followed a `2` record.
fn parse_changed(record: &[u8], orig_path: Option<&[u8]>) -> Option<StatusEntry> {
    let renamed = record.first() == Some(&b'2');
    // 1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>
    // 2 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <X><score> <path>
    let fields = split_fields(record, if renamed { 10 } else { 9 })?;
    let orig_path = match (renamed, orig_path) {
        (false, _) => None,
        (true, Some(orig_path)) if !orig_path.is_empty() => Some(path_from_bytes(orig_path)),
        (true, _) => return None,
    };
    let xy = fields[1];
    Some(StatusEntry {
        path: path_from_bytes(fields[fields.len() - 1]),
        orig_path,
        index: file_state(xy[0]),
        worktree: file_state(xy[1]),
        untracked: false,
        conflicted: false,
        conflict: None,
        submodule: fields[2].first() == Some(&b'S'),
    })
}

/// Reads a `u` (unmerged) record.
fn parse_unmerged(record: &[u8]) -> Option<StatusEntry> {
    // u <XY> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>
    let fields = split_fields(record, 11)?;
    let conflict = match fields[1] {
        b"DD" => ConflictKind::BothDeleted,
        b"AU" => ConflictKind::AddedByUs,
        b"UD" => ConflictKind::DeletedByThem,
        b"UA" => ConflictKind::AddedByThem,
        b"DU" => ConflictKind::DeletedByUs,
        b"AA" => ConflictKind::BothAdded,
        _ => ConflictKind::BothModified,
    };
    Some(StatusEntry {
        path: path_from_bytes(fields[10]),
        orig_path: None,
        index: FileState::Unmodified,
        worktree: FileState::Unmodified,
        untracked: false,
        conflicted: true,
        conflict: Some(conflict),
        submodule: fields[2].first() == Some(&b'S'),
    })
}

/// Splits a record into exactly `count` space separated fields, the last of which is the
/// path and may itself contain spaces. `None` unless all fields are there, the second one
/// is a two-letter `XY` and the path is not empty.
fn split_fields(record: &[u8], count: usize) -> Option<Vec<&[u8]>> {
    let fields: Vec<&[u8]> = record.splitn(count, |&byte| byte == b' ').collect();
    let complete = fields.len() == count && fields[1].len() == 2 && !fields[count - 1].is_empty();
    complete.then_some(fields)
}

fn untracked_entry(path: &[u8]) -> StatusEntry {
    StatusEntry {
        path: path_from_bytes(path),
        orig_path: None,
        index: FileState::Unmodified,
        worktree: FileState::Unmodified,
        untracked: true,
        conflicted: false,
        conflict: None,
        submodule: false,
    }
}

/// One letter of the two-letter `XY` field.
fn file_state(letter: u8) -> FileState {
    match letter {
        b'M' => FileState::Modified,
        b'T' => FileState::TypeChanged,
        b'A' => FileState::Added,
        b'D' => FileState::Deleted,
        b'R' => FileState::Renamed,
        b'C' => FileState::Copied,
        _ => FileState::Unmodified,
    }
}

impl Repo {
    /// The current branch, its upstream, the operation in progress and every file that is
    /// changed, untracked or conflicted.
    ///
    /// Untracked directories are listed file by file and ignored files are left out. The
    /// call takes no locks, so it neither waits for nor disturbs a git command the user is
    /// running at the same time.
    pub fn status(&self) -> Result<Status> {
        let output = self
            .git()
            .args([
                "status",
                "--porcelain=v2",
                "--branch",
                "-z",
                "--untracked-files=all",
            ])
            .run()?;
        let mut status = parse_status(&output.stdout);
        status.state = self.state();
        Ok(status)
    }

    /// The operation git is in the middle of, read from the marker files it keeps in the
    /// git directory. When several apply, the outermost one is reported: a merge before a
    /// rebase, a rebase before the cherry-pick it is made of, and a bisection last.
    fn state(&self) -> Option<RepoState> {
        let exists = |name: &str| self.git_dir().join(name).exists();
        if exists("MERGE_HEAD") {
            Some(RepoState::Merging)
        } else if exists("rebase-merge")
            || (exists("rebase-apply") && !exists("rebase-apply/applying"))
        {
            Some(RepoState::Rebasing)
        } else if self.has_pseudo_ref("CHERRY_PICK_HEAD") || self.sequencer_runs("pick") {
            Some(RepoState::CherryPicking)
        } else if self.has_pseudo_ref("REVERT_HEAD") || self.sequencer_runs("revert") {
            Some(RepoState::Reverting)
        } else if exists("BISECT_LOG") {
            Some(RepoState::Bisecting)
        } else {
            None
        }
    }

    /// Whether a reference like `CHERRY_PICK_HEAD` exists. It is a plain file in the git
    /// directory, except in repositories that use the reftable format, where only git
    /// itself can tell.
    fn has_pseudo_ref(&self, name: &str) -> bool {
        if self.git_dir().join(name).exists() {
            return true;
        }
        let uses_reftable =
            self.git_dir().join("reftable").is_dir() || self.common_dir().join("reftable").is_dir();
        uses_reftable
            && self
                .git()
                .args(["rev-parse", "--quiet", "--verify", name])
                .probe()
                .is_ok_and(|output| output.code == Some(0))
    }

    /// Whether a cherry-pick or revert of several commits is under way, between two of
    /// its commits: the list of what is left to do starts with `command`.
    fn sequencer_runs(&self, command: &str) -> bool {
        fs::read_to_string(self.git_dir().join("sequencer/todo"))
            .is_ok_and(|todo| todo.split_whitespace().next() == Some(command))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZEROS: &str = "0000000000000000000000000000000000000000";
    const HASH_A: &str = "64b315f2b629105534c93def2883d23442b3d1b8";
    const HASH_B: &str = "587be6b4c3f93f93c489c0111bba5596147a26cb";

    /// Joins records the way `-z` does.
    fn nul_joined(records: &[&[u8]]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for record in records {
            bytes.extend_from_slice(record);
            bytes.push(0);
        }
        bytes
    }

    fn changed(xy: &str, path: &str) -> Vec<u8> {
        format!("1 {xy} N... 100644 100644 100644 {HASH_A} {HASH_B} {path}").into_bytes()
    }

    fn entry_at<'a>(status: &'a Status, path: &str) -> &'a StatusEntry {
        status
            .entry(path)
            .unwrap_or_else(|| panic!("no entry for {path}"))
    }

    #[test]
    fn empty_output_is_a_clean_status() {
        let status = parse_status(b"");
        assert_eq!(status, Status::default());
        assert!(status.is_clean());
        assert_eq!(status.counts(), StatusCounts::default());
    }

    #[test]
    fn branch_headers_with_an_upstream() {
        let status = parse_status(&nul_joined(&[
            b"# branch.oid 8cbc8e3948a4d8437af274ed8924f9a77582e614",
            b"# branch.head feat/x",
            b"# branch.upstream origin/feat/x",
            b"# branch.ab +12 -3",
        ]));
        assert_eq!(status.branch.as_deref(), Some("feat/x"));
        assert_eq!(status.head.as_deref(), Some("8cbc8e3"));
        assert_eq!(
            status.head_sha.as_deref(),
            Some("8cbc8e3948a4d8437af274ed8924f9a77582e614")
        );
        assert_eq!(status.upstream.as_deref(), Some("origin/feat/x"));
        assert!(!status.upstream_gone);
        assert_eq!((status.ahead, status.behind), (12, 3));
        assert_eq!(status.state, None);
        assert!(status.is_clean());
    }

    #[test]
    fn a_repository_without_commits_has_a_branch_but_no_head() {
        let status = parse_status(&nul_joined(&[
            b"# branch.oid (initial)",
            b"# branch.head main",
        ]));
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert_eq!(status.head, None);
        assert_eq!(status.head_sha, None);
        assert_eq!(status.upstream, None);
        assert!(!status.upstream_gone);
    }

    #[test]
    fn a_detached_head_has_no_branch() {
        let status = parse_status(&nul_joined(&[
            b"# branch.oid 05c16969050e1b3ed1da06fa5e46f34bac38a6f9",
            b"# branch.head (detached)",
        ]));
        assert_eq!(status.branch, None);
        assert_eq!(status.head.as_deref(), Some("05c1696"));
    }

    #[test]
    fn an_upstream_without_counts_is_gone() {
        let status = parse_status(&nul_joined(&[
            b"# branch.oid 05c16969050e1b3ed1da06fa5e46f34bac38a6f9",
            b"# branch.head feat/x",
            b"# branch.upstream origin/feat/x",
        ]));
        assert_eq!(status.upstream.as_deref(), Some("origin/feat/x"));
        assert!(status.upstream_gone);
        assert_eq!((status.ahead, status.behind), (0, 0));
    }

    #[test]
    fn unknown_headers_and_records_are_skipped() {
        let status = parse_status(&nul_joined(&[
            b"# stash 3",
            b"# branch.head main",
            b"# branch.future something new",
            b"x some record type from the future",
            b"1 garbage",
            b"1 M",
            b"! ignored.log",
            b"? kept.txt",
        ]));
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert_eq!(status.entries.len(), 1);
        assert!(entry_at(&status, "kept.txt").untracked);
    }

    #[test]
    fn ordinary_changes_carry_index_and_work_tree_states() {
        let status = parse_status(&nul_joined(&[
            &changed("M.", "staged.txt"),
            &changed(".M", "unstaged.txt"),
            &changed("MM", "both.txt"),
            &changed("A.", "added.txt"),
            &changed("AM", "added-then-edited.txt"),
            &changed("D.", "staged-deletion.txt"),
            &changed(".D", "deleted.txt"),
            &changed(".A", "intent-to-add.txt"),
            &changed("T.", "became-a-link"),
            &changed(".T", "link-in-work-tree"),
            &changed("AD", "added-then-deleted.txt"),
        ]));
        let states = |path: &str| {
            let entry = entry_at(&status, path);
            assert!(!entry.untracked && !entry.conflicted && !entry.submodule);
            assert_eq!(entry.orig_path, None);
            assert_eq!(entry.conflict, None);
            (
                entry.index,
                entry.worktree,
                entry.stage_state(),
                entry.display_status(),
            )
        };
        use DisplayStatus as D;
        use FileState as F;
        use StageState as S;
        assert_eq!(
            states("staged.txt"),
            (F::Modified, F::Unmodified, S::Staged, D::Modified)
        );
        assert_eq!(
            states("unstaged.txt"),
            (F::Unmodified, F::Modified, S::Unstaged, D::Modified)
        );
        assert_eq!(
            states("both.txt"),
            (F::Modified, F::Modified, S::PartiallyStaged, D::Modified)
        );
        assert_eq!(
            states("added.txt"),
            (F::Added, F::Unmodified, S::Staged, D::Added)
        );
        assert_eq!(
            states("added-then-edited.txt"),
            (F::Added, F::Modified, S::PartiallyStaged, D::Added)
        );
        assert_eq!(
            states("staged-deletion.txt"),
            (F::Deleted, F::Unmodified, S::Staged, D::Deleted)
        );
        assert_eq!(
            states("deleted.txt"),
            (F::Unmodified, F::Deleted, S::Unstaged, D::Deleted)
        );
        assert_eq!(
            states("intent-to-add.txt"),
            (F::Unmodified, F::Added, S::Unstaged, D::Added)
        );
        assert_eq!(
            states("became-a-link"),
            (F::TypeChanged, F::Unmodified, S::Staged, D::Modified)
        );
        assert_eq!(
            states("link-in-work-tree"),
            (F::Unmodified, F::TypeChanged, S::Unstaged, D::Modified)
        );
        assert_eq!(
            states("added-then-deleted.txt"),
            (F::Added, F::Deleted, S::PartiallyStaged, D::Deleted)
        );
        assert!(entry_at(&status, "staged.txt").is_fully_staged());
        assert!(!entry_at(&status, "both.txt").is_fully_staged());
        assert!(!entry_at(&status, "unstaged.txt").is_fully_staged());
    }

    #[test]
    fn renames_and_copies_take_the_following_record_as_their_origin() {
        let rename = format!("2 R. N... 100644 100644 100644 {HASH_A} {HASH_A} R100 new name.txt");
        let copy = format!("2 C. N... 100644 100644 100644 {HASH_A} {HASH_B} C75 copy.txt");
        let renamed_and_edited =
            format!("2 RM N... 100644 100644 100644 {HASH_A} {HASH_B} R86 moved/edited.rs");
        let status = parse_status(&nul_joined(&[
            copy.as_bytes(),
            b"original.txt",
            renamed_and_edited.as_bytes(),
            b"edited.rs",
            rename.as_bytes(),
            b"old name.txt",
            &changed(".M", "after.txt"),
        ]));
        assert_eq!(status.entries.len(), 4);

        let renamed = entry_at(&status, "new name.txt");
        assert_eq!(
            renamed.orig_path.as_deref(),
            Some(Path::new("old name.txt"))
        );
        assert_eq!(renamed.index, FileState::Renamed);
        assert_eq!(renamed.worktree, FileState::Unmodified);
        assert_eq!(renamed.display_status(), DisplayStatus::Renamed);
        assert_eq!(renamed.stage_state(), StageState::Staged);
        assert_eq!(
            renamed.paths(),
            [Path::new("new name.txt"), Path::new("old name.txt")]
        );

        let copied = entry_at(&status, "copy.txt");
        assert_eq!(copied.orig_path.as_deref(), Some(Path::new("original.txt")));
        assert_eq!(copied.index, FileState::Copied);
        assert_eq!(copied.display_status(), DisplayStatus::Added);

        let edited = entry_at(&status, "moved/edited.rs");
        assert_eq!(edited.orig_path.as_deref(), Some(Path::new("edited.rs")));
        assert_eq!(edited.stage_state(), StageState::PartiallyStaged);
        assert_eq!(edited.display_status(), DisplayStatus::Renamed);

        // The record after the rename's origin is read as a record again.
        let after = entry_at(&status, "after.txt");
        assert_eq!(after.orig_path, None);
        assert_eq!(after.paths(), [Path::new("after.txt")]);
    }

    #[test]
    fn a_rename_without_its_origin_record_is_skipped() {
        let rename = format!("2 R. N... 100644 100644 100644 {HASH_A} {HASH_A} R100 lonely.txt");
        let mut bytes = rename.into_bytes();
        // The output was cut short: no terminator, no origin.
        assert!(parse_status(&bytes).entries.is_empty());
        // With the terminator the "origin" is the empty last record, which is no path.
        bytes.push(0);
        assert!(parse_status(&bytes).entries.is_empty());
    }

    #[test]
    fn truncated_records_are_skipped() {
        let full = changed(".M", "file.txt");
        for cut in 0..full.len() - "file.txt".len() {
            let status = parse_status(&full[..cut]);
            assert!(status.entries.is_empty(), "cut at {cut}");
        }
        let unmerged = format!("u UU N... 100644 100644 100644 100644 {HASH_A} {HASH_B} {ZEROS}");
        assert!(parse_status(unmerged.as_bytes()).entries.is_empty());
        assert!(parse_status(b"1 MMM N... 1 2 3 a b path")
            .entries
            .is_empty());
    }

    #[test]
    fn conflicts_report_how_the_sides_disagree() {
        let unmerged = |xy: &str, path: &str| {
            format!("u {xy} N... 100644 100644 100644 100644 {HASH_A} {HASH_B} {ZEROS} {path}")
                .into_bytes()
        };
        let cases = [
            ("UU", ConflictKind::BothModified),
            ("AA", ConflictKind::BothAdded),
            ("DD", ConflictKind::BothDeleted),
            ("AU", ConflictKind::AddedByUs),
            ("UA", ConflictKind::AddedByThem),
            ("DU", ConflictKind::DeletedByUs),
            ("UD", ConflictKind::DeletedByThem),
        ];
        let records: Vec<Vec<u8>> = cases
            .iter()
            .map(|(xy, _)| unmerged(xy, &format!("dir/{xy} file.txt")))
            .collect();
        let records: Vec<&[u8]> = records.iter().map(Vec::as_slice).collect();
        let status = parse_status(&nul_joined(&records));
        assert_eq!(status.entries.len(), cases.len());
        for (xy, kind) in cases {
            let entry = entry_at(&status, &format!("dir/{xy} file.txt"));
            assert!(entry.conflicted, "{xy}");
            assert_eq!(entry.conflict, Some(kind), "{xy}");
            assert_eq!(entry.index, FileState::Unmodified);
            assert_eq!(entry.worktree, FileState::Unmodified);
            assert_eq!(entry.display_status(), DisplayStatus::Conflict);
            assert_eq!(entry.stage_state(), StageState::Unstaged);
            assert!(!entry.has_staged() && entry.has_unstaged());
        }
        assert_eq!(status.conflicts().len(), cases.len());
        assert_eq!(status.counts().conflicted, cases.len());
        assert_eq!(status.counts().unstaged, 0);
    }

    #[test]
    fn unusual_names_arrive_byte_for_byte() {
        let status = parse_status(&nul_joined(&[
            &changed(".M", "dir with space/\u{fc} \u{f1}.txt"),
            &changed("M.", "ta\tb.txt"),
            &changed(".M", "new\nline.txt"),
            &changed(".M", "quo\"te.txt"),
            &changed(".M", "back\\slash.txt"),
            &changed(".M", " leading and trailing "),
            b"? untracked dir/\xff\xfe not utf8",
            b"? nested-repository/",
            "? \u{65e5}\u{672c}\u{8a9e}.txt".as_bytes(),
        ]));
        assert_eq!(status.entries.len(), 9);
        for path in [
            "dir with space/\u{fc} \u{f1}.txt",
            "ta\tb.txt",
            "new\nline.txt",
            "quo\"te.txt",
            "back\\slash.txt",
            " leading and trailing ",
            "nested-repository/",
            "\u{65e5}\u{672c}\u{8a9e}.txt",
        ] {
            assert!(status.entry(path).is_some(), "{path:?}");
        }
        let raw = path_from_bytes(b"untracked dir/\xff\xfe not utf8");
        let entry = status.entry(&raw).expect("the non UTF-8 name");
        assert!(entry.untracked);
        assert_eq!(entry.display_status(), DisplayStatus::Untracked);
        assert_eq!(entry.stage_state(), StageState::Unstaged);
    }

    #[test]
    fn submodules_are_flagged() {
        let status = parse_status(&nul_joined(&[
            format!("1 .M S.M. 160000 160000 160000 {HASH_A} {HASH_A} vendor/library").as_bytes(),
            format!("1 M. SC.. 160000 160000 160000 {HASH_A} {HASH_B} vendor/other").as_bytes(),
            &changed(".M", "file.txt"),
        ]));
        assert!(entry_at(&status, "vendor/library").submodule);
        assert!(entry_at(&status, "vendor/other").submodule);
        assert!(!entry_at(&status, "file.txt").submodule);
    }

    #[test]
    fn groups_and_counts() {
        let unmerged =
            format!("u UU N... 100644 100644 100644 100644 {HASH_A} {HASH_B} {ZEROS} conflict.txt");
        let status = parse_status(&nul_joined(&[
            &changed("M.", "staged.txt"),
            &changed(".M", "unstaged.txt"),
            &changed("MM", "both.txt"),
            unmerged.as_bytes(),
            b"? new-1.txt",
            b"? new-2.txt",
        ]));
        let names = |entries: Vec<&StatusEntry>| -> Vec<String> {
            entries
                .iter()
                .map(|entry| entry.path.display().to_string())
                .collect()
        };
        assert_eq!(names(status.staged()), ["staged.txt", "both.txt"]);
        assert_eq!(
            names(status.unstaged()),
            [
                "unstaged.txt",
                "both.txt",
                "conflict.txt",
                "new-1.txt",
                "new-2.txt"
            ]
        );
        assert_eq!(names(status.untracked()), ["new-1.txt", "new-2.txt"]);
        assert_eq!(names(status.conflicts()), ["conflict.txt"]);
        assert_eq!(
            status.counts(),
            StatusCounts {
                staged: 2,
                unstaged: 2,
                untracked: 2,
                conflicted: 1,
                total: 6,
            }
        );
        assert!(!status.is_clean());
    }
}
