//! Differences between `HEAD`, the index and the work tree, file by file and hunk by hunk.
//!
//! The text of `git diff` is turned into data by [`parse_diff`], a pure function. The
//! [`Repo`] methods run git with options that pin the output format down, and add the
//! untracked files git leaves out.

use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::paths::{literal_pathspec, path_from_bytes, unquote_c_style};
use crate::repo::Repo;

/// Untracked files larger than this are listed without their content.
pub const MAX_UNTRACKED_FILE_SIZE: u64 = 1024 * 1024;

/// How much untracked content one call reads in total. Files beyond it are listed
/// without their content, so a forgotten build directory cannot exhaust memory.
pub const MAX_UNTRACKED_TOTAL_SIZE: u64 = 16 * 1024 * 1024;

/// How far into a file git looks for a NUL byte to decide that it is binary.
const BINARY_SNIFF_LEN: usize = 8000;

/// The options every diff is made with, so its text can be parsed whatever the user's
/// configuration says: no colour, no external diff or text conversion programs, renames
/// detected, three lines of context, the standard `a/` and `b/` prefixes, and submodules
/// as a one-line change. (Paths are always relative to the top of the work tree because
/// that is where every command runs.)
const DIFF_OPTIONS: [&str; 9] = [
    "diff",
    "--no-color",
    "--no-ext-diff",
    "--no-textconv",
    "-M",
    "-U3",
    "--src-prefix=a/",
    "--dst-prefix=b/",
    "--submodule=short",
];

/// Which two versions of the files to compare.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiffTarget {
    /// The last commit against the work tree: every change, staged or not, plus the
    /// untracked files.
    HeadToWorktree,
    /// The index against the work tree: what is not staged. For a conflicted file the
    /// index side is "our" version.
    IndexToWorktree,
    /// The last commit against the index: what is staged.
    HeadToIndex,
}

/// What a diff says happened to a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiffStatus {
    /// The file is new on the new side.
    Added,
    /// The file is gone on the new side.
    Deleted,
    /// The content or the mode changed.
    Modified,
    /// The file was moved from [`FileDiff::old_path`], possibly with changes.
    Renamed,
    /// The file is a copy of [`FileDiff::old_path`], possibly with changes.
    Copied,
    /// The file is not tracked; its whole content is shown as added.
    Untracked,
    /// The file is unmerged.
    Conflicted,
}

/// The changes to one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    /// The file, relative to the top of the work tree: its new name, or for a deleted
    /// file the name it had.
    pub path: PathBuf,
    /// For a rename or copy, where the file came from.
    pub old_path: Option<PathBuf>,
    /// What happened to the file.
    pub status: DiffStatus,
    /// The content is binary, so there are no hunks.
    pub binary: bool,
    /// The file is untracked and its content was left out because it is larger than
    /// [`MAX_UNTRACKED_FILE_SIZE`] or because [`MAX_UNTRACKED_TOTAL_SIZE`] was used up.
    pub too_large: bool,
    /// The file mode on the old side (`0o100644`, `0o100755`, `0o120000` for a symbolic
    /// link), when the diff states it.
    pub old_mode: Option<u32>,
    /// The file mode on the new side, when the diff states it.
    pub new_mode: Option<u32>,
    /// The changed regions, in file order. Empty for binary files, pure renames, mode
    /// changes and empty files.
    pub hunks: Vec<Hunk>,
}

impl FileDiff {
    /// How many lines the diff adds.
    pub fn additions(&self) -> usize {
        self.count_lines(DiffLineKind::Added)
    }

    /// How many lines the diff removes.
    pub fn deletions(&self) -> usize {
        self.count_lines(DiffLineKind::Removed)
    }

    fn count_lines(&self, kind: DiffLineKind) -> usize {
        self.hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.kind == kind)
            .count()
    }

    fn without_content(path: PathBuf, status: DiffStatus) -> FileDiff {
        FileDiff {
            path,
            old_path: None,
            status,
            binary: false,
            too_large: false,
            old_mode: None,
            new_mode: None,
            hunks: Vec::new(),
        }
    }
}

/// One changed region of a file with the unchanged lines around it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// The 1-based line where the region starts on the old side. When `old_lines` is 0
    /// it is the line the insertion comes after (0 for the top of the file).
    pub old_start: u32,
    /// How many lines of the old side the region covers.
    pub old_lines: u32,
    /// The 1-based line where the region starts on the new side. When `new_lines` is 0
    /// it is the line the deletion comes after.
    pub new_start: u32,
    /// How many lines of the new side the region covers.
    pub new_lines: u32,
    /// The text after the `@@ ... @@` marker: usually the function the hunk is in.
    pub header: String,
    /// The lines of the region, in order.
    pub lines: Vec<DiffLine>,
}

/// What a line of a hunk is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiffLineKind {
    /// The line is the same on both sides.
    Context,
    /// The line only exists on the new side.
    Added,
    /// The line only exists on the old side.
    Removed,
}

/// One line of a hunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    /// Whether the line is unchanged, added or removed.
    pub kind: DiffLineKind,
    /// The line's 1-based number on the old side; `None` for an added line.
    pub old_no: Option<u32>,
    /// The line's 1-based number on the new side; `None` for a removed line.
    pub new_no: Option<u32>,
    /// The line without its marker and without its line feed. A carriage return before
    /// the line feed is part of the text. Bytes that are not valid UTF-8 are replaced by
    /// U+FFFD here; [`DiffLine::bytes`] still has them.
    pub text: String,
    /// The line is the last of its file and that file does not end with a line feed.
    pub no_newline_at_end: bool,
    /// The original bytes, kept only when `text` could not represent them.
    raw: Option<Box<[u8]>>,
}

impl DiffLine {
    /// A line of valid UTF-8 text.
    pub fn new(
        kind: DiffLineKind,
        old_no: Option<u32>,
        new_no: Option<u32>,
        text: impl Into<String>,
    ) -> DiffLine {
        DiffLine {
            kind,
            old_no,
            new_no,
            text: text.into(),
            no_newline_at_end: false,
            raw: None,
        }
    }

    /// The line exactly as it is in the file, without marker and line feed.
    pub fn bytes(&self) -> &[u8] {
        self.raw.as_deref().unwrap_or(self.text.as_bytes())
    }

    fn from_bytes(
        kind: DiffLineKind,
        old_no: Option<u32>,
        new_no: Option<u32>,
        bytes: &[u8],
    ) -> DiffLine {
        match std::str::from_utf8(bytes) {
            Ok(text) => DiffLine::new(kind, old_no, new_no, text),
            Err(_) => DiffLine {
                raw: Some(bytes.into()),
                ..DiffLine::new(kind, old_no, new_no, String::from_utf8_lossy(bytes))
            },
        }
    }
}

/// Reads the text of `git diff` (or `git diff --cached`, `git diff HEAD`, with or without
/// `--ours`) into one [`FileDiff`] per file, in the order of the text.
///
/// The text is expected to use the `a/` and `b/` prefixes. Anything that is not
/// understood is skipped; malformed input never panics, it gives fewer files or hunks.
///
/// Three things differ from what one might expect:
/// - A file whose type changed (a regular file that became a symbolic link) appears
///   twice, as git prints it: once deleted, once added.
/// - A conflicted file is reported with [`DiffStatus::Conflicted`]. It has hunks when the
///   text holds an ordinary diff for it (`git diff --ours` does) and none when it only
///   holds git's `* Unmerged path` note or a combined (`diff --cc`) diff.
/// - Paths are whatever bytes git printed, unquoted; they need not be UTF-8.
pub fn parse_diff(output: &[u8]) -> Vec<FileDiff> {
    let mut lines: Vec<&[u8]> = output.split(|&byte| byte == b'\n').collect();
    if lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    let mut parser = DiffParser {
        lines,
        at: 0,
        files: Vec::new(),
        current: None,
        unmerged: None,
    };
    parser.run();
    parser.files
}

struct DiffParser<'a> {
    lines: Vec<&'a [u8]>,
    /// Index of the next line to read.
    at: usize,
    files: Vec<FileDiff>,
    /// The file whose section is being read.
    current: Option<PendingFile>,
    /// The path of the last `* Unmerged path` note that no file has claimed yet.
    unmerged: Option<PathBuf>,
}

impl<'a> DiffParser<'a> {
    fn run(&mut self) {
        while let Some(line) = self.next_line() {
            if let Some(names) = line.strip_prefix(b"diff --git ") {
                self.finish_file();
                self.current = Some(PendingFile::from_git_header(names));
            } else if let Some(name) = line
                .strip_prefix(b"diff --cc ")
                .or_else(|| line.strip_prefix(b"diff --combined "))
            {
                self.finish_file();
                self.current = Some(PendingFile::combined(name));
            } else if let Some(path) = line.strip_prefix(b"* Unmerged path ") {
                self.finish_file();
                self.flush_unmerged();
                self.unmerged = Some(path_from_bytes(path));
            } else if line.starts_with(b"@@ ") {
                self.read_hunk(line);
            } else if let Some(file) = &mut self.current {
                file.read_header_line(line);
            }
        }
        self.finish_file();
        self.flush_unmerged();
    }

    fn next_line(&mut self) -> Option<&'a [u8]> {
        let line = self.lines.get(self.at).copied();
        self.at += usize::from(line.is_some());
        line
    }

    fn peek_line(&self) -> Option<&'a [u8]> {
        self.lines.get(self.at).copied()
    }

    /// Closes the section being read and stores its file.
    fn finish_file(&mut self) {
        let Some(mut file) = self.current.take().and_then(PendingFile::finish) else {
            return;
        };
        match self.unmerged.take() {
            Some(path) if path == file.path => file.status = DiffStatus::Conflicted,
            // The note was about another file, one that has no diff of its own.
            Some(path) => self
                .files
                .push(FileDiff::without_content(path, DiffStatus::Conflicted)),
            None => {}
        }
        self.files.push(file);
    }

    /// Stores a file for an `* Unmerged path` note that no diff followed.
    fn flush_unmerged(&mut self) {
        if let Some(path) = self.unmerged.take() {
            self.files
                .push(FileDiff::without_content(path, DiffStatus::Conflicted));
        }
    }

    /// Reads the hunk that starts with `header` into the current file. The hunk's size
    /// comes from its header, so content that looks like a header cannot end it early.
    fn read_hunk(&mut self, header: &[u8]) {
        let wanted = match &self.current {
            Some(file) if !file.combined => parse_hunk_header(header),
            _ => None,
        };
        let Some(mut hunk) = wanted else {
            return;
        };
        let (mut old_no, mut new_no) = (hunk.old_start, hunk.new_start);
        let (mut old_left, mut new_left) = (hunk.old_lines, hunk.new_lines);
        while old_left > 0 || new_left > 0 {
            let Some(line) = self.peek_line() else {
                break;
            };
            let kind = match line.first() {
                // `diff.suppressBlankEmpty` prints an empty context line with nothing
                // at all.
                Some(b' ') | None => DiffLineKind::Context,
                Some(b'-') => DiffLineKind::Removed,
                Some(b'+') => DiffLineKind::Added,
                Some(b'\\') => {
                    mark_no_newline(&mut hunk);
                    self.at += 1;
                    continue;
                }
                Some(_) => break,
            };
            let takes_old = kind != DiffLineKind::Added;
            let takes_new = kind != DiffLineKind::Removed;
            if (takes_old && old_left == 0) || (takes_new && new_left == 0) {
                break;
            }
            self.at += 1;
            hunk.lines.push(DiffLine::from_bytes(
                kind,
                takes_old.then_some(old_no),
                takes_new.then_some(new_no),
                line.get(1..).unwrap_or_default(),
            ));
            if takes_old {
                old_no += 1;
                old_left -= 1;
            }
            if takes_new {
                new_no += 1;
                new_left -= 1;
            }
        }
        // The note about a missing final line feed comes after the line it is about,
        // which may be the hunk's last.
        if self.peek_line().is_some_and(|line| line.starts_with(b"\\")) {
            mark_no_newline(&mut hunk);
            self.at += 1;
        }
        if let Some(file) = &mut self.current {
            file.hunks.push(hunk);
        }
    }
}

/// Applies a `\ No newline at end of file` note to the line before it.
fn mark_no_newline(hunk: &mut Hunk) {
    if let Some(line) = hunk.lines.last_mut() {
        line.no_newline_at_end = true;
    }
}

/// Reads `@@ -12,5 +12,6 @@ heading` into a hunk without lines.
fn parse_hunk_header(line: &[u8]) -> Option<Hunk> {
    let rest = line.strip_prefix(b"@@ -")?;
    let end = find(rest, b" @@")?;
    let ranges = &rest[..end];
    let plus = find(ranges, b" +")?;
    let (old_start, old_lines) = parse_range(&ranges[..plus])?;
    let (new_start, new_lines) = parse_range(&ranges[plus + 2..])?;
    let heading = &rest[end + 3..];
    let heading = heading.strip_prefix(b" ").unwrap_or(heading);
    Some(Hunk {
        old_start,
        old_lines,
        new_start,
        new_lines,
        header: String::from_utf8_lossy(heading).into_owned(),
        lines: Vec::new(),
    })
}

/// Reads `12,5` or `12` (which means one line) into start and length.
fn parse_range(text: &[u8]) -> Option<(u32, u32)> {
    let text = std::str::from_utf8(text).ok()?;
    match text.split_once(',') {
        Some((start, lines)) => Some((start.parse().ok()?, lines.parse().ok()?)),
        None => Some((text.parse().ok()?, 1)),
    }
}

/// The position of the first occurrence of `needle` in `text`.
fn find(text: &[u8], needle: &[u8]) -> Option<usize> {
    text.windows(needle.len())
        .position(|window| window == needle)
}

/// One side of a `---` / `+++` pair.
enum Side {
    /// `/dev/null`: the file does not exist on this side.
    Absent,
    Named(Vec<u8>),
}

/// A file section while it is being read.
#[derive(Default)]
struct PendingFile {
    /// Names from the `diff --git` line: ambiguous when they contain spaces, so only
    /// used when nothing better follows.
    header_old: Option<Vec<u8>>,
    header_new: Option<Vec<u8>>,
    /// Names from the `---` and `+++` lines.
    minus_side: Option<Side>,
    plus_side: Option<Side>,
    /// Names from `rename from` / `copy from` and `rename to` / `copy to`.
    moved_from: Option<Vec<u8>>,
    moved_to: Option<Vec<u8>>,
    status: Option<DiffStatus>,
    /// A `diff --cc` section: its hunks have two columns of markers and are skipped.
    combined: bool,
    binary: bool,
    old_mode: Option<u32>,
    new_mode: Option<u32>,
    hunks: Vec<Hunk>,
}

impl PendingFile {
    fn from_git_header(names: &[u8]) -> PendingFile {
        let (header_old, header_new) = split_git_header(names);
        PendingFile {
            header_old,
            header_new,
            ..PendingFile::default()
        }
    }

    fn combined(name: &[u8]) -> PendingFile {
        PendingFile {
            header_new: Some(parse_name(name)),
            status: Some(DiffStatus::Conflicted),
            combined: true,
            ..PendingFile::default()
        }
    }

    /// Reads one line of the section's header. Lines that are not header lines (binary
    /// patch data, a combined diff's hunks) are ignored.
    fn read_header_line(&mut self, line: &[u8]) {
        // Once the hunks have started nothing is a header line any more.
        if self.combined || !self.hunks.is_empty() {
            return;
        }
        let value = |prefix: &[u8]| line.strip_prefix(prefix);
        if let Some(mode) = value(b"old mode ") {
            self.old_mode = parse_mode(mode);
        } else if let Some(mode) = value(b"new mode ") {
            self.new_mode = parse_mode(mode);
        } else if let Some(mode) = value(b"deleted file mode ") {
            self.status = Some(DiffStatus::Deleted);
            self.old_mode = parse_mode(mode);
        } else if let Some(mode) = value(b"new file mode ") {
            self.status = Some(DiffStatus::Added);
            self.new_mode = parse_mode(mode);
        } else if let Some(name) = value(b"rename from ") {
            self.status = Some(DiffStatus::Renamed);
            self.moved_from = Some(parse_name(name));
        } else if let Some(name) = value(b"rename to ") {
            self.status = Some(DiffStatus::Renamed);
            self.moved_to = Some(parse_name(name));
        } else if let Some(name) = value(b"copy from ") {
            self.status = Some(DiffStatus::Copied);
            self.moved_from = Some(parse_name(name));
        } else if let Some(name) = value(b"copy to ") {
            self.status = Some(DiffStatus::Copied);
            self.moved_to = Some(parse_name(name));
        } else if let Some(index) = value(b"index ") {
            // index <old>..<new> <mode>: the mode is only there when it did not change.
            if let Some(mode) = index.split(|&byte| byte == b' ').nth(1) {
                let mode = parse_mode(mode);
                self.old_mode = self.old_mode.or(mode);
                self.new_mode = self.new_mode.or(mode);
            }
        } else if let Some(name) = value(b"--- ") {
            self.minus_side = Some(parse_side(name, b"a/"));
        } else if let Some(name) = value(b"+++ ") {
            self.plus_side = Some(parse_side(name, b"b/"));
        } else if line.starts_with(b"Binary files ") || line == b"GIT binary patch" {
            self.binary = true;
        }
    }

    /// The finished file, or `None` when the section never named one.
    fn finish(self) -> Option<FileDiff> {
        let named = |side: Option<Side>| match side {
            Some(Side::Named(name)) => Some(name),
            Some(Side::Absent) | None => None,
        };
        let old = self
            .moved_from
            .or(named(self.minus_side))
            .or(self.header_old);
        let new = self.moved_to.or(named(self.plus_side)).or(self.header_new);
        let status = self.status.unwrap_or(DiffStatus::Modified);
        let (path, old_path) = match status {
            DiffStatus::Deleted => (old.or(new)?, None),
            DiffStatus::Renamed | DiffStatus::Copied => {
                let path = new.or_else(|| old.clone())?;
                let old_path = old.filter(|old| *old != path);
                (path, old_path)
            }
            _ => (new.or(old)?, None),
        };
        Some(FileDiff {
            path: path_from_bytes(&path),
            old_path: old_path.map(|old_path| path_from_bytes(&old_path)),
            status,
            binary: self.binary,
            too_large: false,
            old_mode: self.old_mode,
            new_mode: self.new_mode,
            hunks: self.hunks,
        })
    }
}

/// Splits what follows `diff --git ` into the old and the new name, without their `a/`
/// and `b/` prefixes. Names with unusual characters are quoted and split exactly.
/// Unquoted names may contain spaces, which makes the line ambiguous: the split is then
/// exact when both names are the same (every case but a rename, and a rename states its
/// names again on lines of their own) and a guess otherwise.
fn split_git_header(names: &[u8]) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    let strip = |name: Vec<u8>, prefix: &[u8]| match name.strip_prefix(prefix) {
        Some(stripped) => stripped.to_vec(),
        None => name,
    };
    let (old, new) = if names.first() == Some(&b'"') {
        // "a/old" b/new   or   "a/old" "b/new"
        let Some((old, used)) = unquote_c_style(names) else {
            return (None, None);
        };
        let new = names
            .get(used..)
            .and_then(|rest| rest.strip_prefix(b" "))
            .map(parse_name);
        (Some(old), new)
    } else if let Some(at) = find(names, b" \"").filter(|_| names.ends_with(b"\"")) {
        // a/old "b/new"
        (
            Some(names[..at].to_vec()),
            Some(parse_name(&names[at + 1..])),
        )
    } else if let Some((old, new)) = same_name_twice(names) {
        (Some(old.to_vec()), Some(new.to_vec()))
    } else if let Some(at) = find(names, b" b/") {
        (Some(names[..at].to_vec()), Some(names[at + 1..].to_vec()))
    } else {
        (None, None)
    };
    (
        old.map(|old| strip(old, b"a/")),
        new.map(|new| strip(new, b"b/")),
    )
}

/// Splits `a/<name> b/<name>` in the middle when the name is the same on both sides.
fn same_name_twice(names: &[u8]) -> Option<(&[u8], &[u8])> {
    // a/ + name + space + b/ + name
    let name_len = names.len().checked_sub(5)? / 2;
    let (old, rest) = names.split_at(2 + name_len);
    let new = rest.strip_prefix(b" ")?;
    let same = old.strip_prefix(b"a/")? == new.strip_prefix(b"b/")?;
    same.then_some((old, new))
}

/// A file name as git prints it in a header: quoted C-style when it has unusual
/// characters, plain otherwise.
fn parse_name(name: &[u8]) -> Vec<u8> {
    match unquote_c_style(name) {
        Some((unquoted, _)) => unquoted,
        None => name.to_vec(),
    }
}

/// The name on a `---` or `+++` line, without its prefix.
fn parse_side(name: &[u8], prefix: &[u8]) -> Side {
    if name == b"/dev/null" {
        return Side::Absent;
    }
    let name = match unquote_c_style(name) {
        Some((unquoted, _)) => unquoted,
        // Git ends an unquoted name that contains a space with a tab, so that tools can
        // tell where it stops.
        None => name.strip_suffix(b"\t").unwrap_or(name).to_vec(),
    };
    Side::Named(match name.strip_prefix(prefix) {
        Some(stripped) => stripped.to_vec(),
        None => name,
    })
}

/// A file mode as git prints it: octal digits.
fn parse_mode(text: &[u8]) -> Option<u32> {
    u32::from_str_radix(std::str::from_utf8(text).ok()?, 8).ok()
}

impl Repo {
    /// The changes between two of `HEAD`, the index and the work tree, one [`FileDiff`]
    /// per changed file, sorted by path. (A file whose type changed, from a regular file
    /// to a symbolic link say, is the exception: git reports it twice, as a deletion and
    /// an addition.)
    ///
    /// [`DiffTarget::HeadToWorktree`] ends with the untracked files (ignored files left
    /// out), each shown as one hunk that adds every line. An untracked file that is
    /// binary, or too large (see [`MAX_UNTRACKED_FILE_SIZE`]), is listed without content
    /// and flagged; one that cannot be read is listed without content.
    ///
    /// Before the first commit `HEAD` counts as empty: everything staged is added.
    pub fn diff_files(&self, target: DiffTarget) -> Result<Vec<FileDiff>> {
        self.diff_paths(target, &[])
    }

    /// The changes to one file, or `None` when it does not differ between the two
    /// versions `target` names.
    ///
    /// A renamed file is found under its new path and compared with the file it was
    /// renamed from, so only the lines that really changed show up. For a file whose type
    /// changed, the half that adds the new kind of file is returned.
    pub fn diff_file(
        &self,
        path: impl AsRef<Path>,
        target: DiffTarget,
    ) -> Result<Option<FileDiff>> {
        let relative = self.relative_path(path)?;
        let pick = |files: Vec<FileDiff>| files.into_iter().rfind(|file| file.path == relative);
        let found = pick(self.diff_paths(target, std::slice::from_ref(&relative))?);
        // Looked at alone, a renamed file seems brand new: git needs to see the old path
        // too to pair them up.
        let seems_new = |file: &FileDiff| file.status == DiffStatus::Added;
        if found.as_ref().is_some_and(seems_new) {
            if let Some(source) = self.rename_source(target, &relative)? {
                let paired = pick(self.diff_paths(target, &[source, relative.clone()])?);
                if paired.is_some() {
                    return Ok(paired);
                }
            }
        }
        Ok(found)
    }

    /// The diff for `target`, limited to `paths` (all files when empty).
    fn diff_paths(&self, target: DiffTarget, paths: &[PathBuf]) -> Result<Vec<FileDiff>> {
        let mut command = self
            .git()
            .args(DIFF_OPTIONS)
            .args(self.diff_target_args(target)?)
            .arg("--");
        for path in paths {
            command = command.arg(literal_pathspec(path));
        }
        let mut files = parse_diff(&command.run()?.stdout);
        if target == DiffTarget::HeadToWorktree {
            files.extend(self.untracked_diffs(paths)?);
        }
        Ok(files)
    }

    /// The arguments that make `git diff` compare what `target` names.
    fn diff_target_args(&self, target: DiffTarget) -> Result<Vec<String>> {
        Ok(match target {
            // For an unmerged file compare with "our" side, which gives an ordinary
            // diff instead of a combined one. Other files are not affected.
            DiffTarget::IndexToWorktree => vec!["--ours".to_string()],
            DiffTarget::HeadToIndex => vec!["--cached".to_string()],
            DiffTarget::HeadToWorktree => vec![self.head_or_empty_tree()?],
        })
    }

    /// `HEAD`, or before the first commit the name of the empty tree, which is what the
    /// first commit will be compared with.
    fn head_or_empty_tree(&self) -> Result<String> {
        if self.head_sha()?.is_some() {
            return Ok("HEAD".to_string());
        }
        let output = self
            .git()
            .args(["hash-object", "-t", "tree", "/dev/null"])
            .run()?;
        Ok(output.line_text())
    }

    /// The path `path` was renamed from according to the diff for `target`, if it is the
    /// destination of a rename.
    fn rename_source(&self, target: DiffTarget, path: &Path) -> Result<Option<PathBuf>> {
        let output = self
            .git()
            .args(["diff", "--raw", "-z", "-M", "--diff-filter=R"])
            .args(self.diff_target_args(target)?)
            .arg("--")
            .run()?;
        // :<modes and hashes> R<score> NUL <from> NUL <to> NUL
        let mut fields = output.stdout.split(|&byte| byte == 0);
        while let Some(record) = fields.next() {
            if !record.starts_with(b":") {
                continue;
            }
            let (Some(from), Some(to)) = (fields.next(), fields.next()) else {
                break;
            };
            if to == path.as_os_str().as_bytes() {
                return Ok(Some(path_from_bytes(from)));
            }
        }
        Ok(None)
    }

    /// The untracked files below `paths` (everywhere when empty), each as a diff that
    /// adds the whole file.
    fn untracked_diffs(&self, paths: &[PathBuf]) -> Result<Vec<FileDiff>> {
        let mut command =
            self.git()
                .args(["ls-files", "-z", "--others", "--exclude-standard", "--"]);
        for path in paths {
            command = command.arg(literal_pathspec(path));
        }
        let output = command.run()?;
        let mut budget = MAX_UNTRACKED_TOTAL_SIZE;
        Ok(output
            .stdout
            .split(|&byte| byte == 0)
            .filter(|name| !name.is_empty())
            .filter_map(|name| untracked_diff(self.root(), path_from_bytes(name), &mut budget))
            .collect())
    }
}

/// The diff for the untracked file `path` in the work tree at `root`: everything added.
/// `budget` is how many bytes of content may still be read; it shrinks by what this file
/// takes. `None` for something that is not a file, like a nested repository, which git
/// lists as a directory.
fn untracked_diff(root: &Path, path: PathBuf, budget: &mut u64) -> Option<FileDiff> {
    let full_path = root.join(&path);
    let metadata = fs::symlink_metadata(&full_path).ok()?;
    let mut file = FileDiff::without_content(path, DiffStatus::Untracked);
    let content = if metadata.file_type().is_symlink() {
        // Git stores a link as the path it points to, without a line feed.
        file.new_mode = Some(0o120000);
        fs::read_link(&full_path)
            .map(|target| target.as_os_str().as_bytes().to_vec())
            .ok()
    } else if metadata.is_file() {
        let executable = metadata.permissions().mode() & 0o111 != 0;
        file.new_mode = Some(if executable { 0o100755 } else { 0o100644 });
        if metadata.len() > MAX_UNTRACKED_FILE_SIZE || metadata.len() > *budget {
            file.too_large = true;
            return Some(file);
        }
        match fs::read(&full_path) {
            Ok(content) => Some(content),
            Err(error) => {
                log::warn!("cannot read {}: {error}", full_path.display());
                None
            }
        }
    } else {
        return None;
    };
    let Some(content) = content else {
        return Some(file);
    };
    *budget = budget.saturating_sub(content.len() as u64);
    if content.iter().take(BINARY_SNIFF_LEN).any(|&byte| byte == 0) {
        file.binary = true;
    } else if let Some(hunk) = added_hunk(&content) {
        file.hunks.push(hunk);
    }
    Some(file)
}

/// A hunk that adds every line of `content`, or `None` for an empty file.
fn added_hunk(content: &[u8]) -> Option<Hunk> {
    if content.is_empty() {
        return None;
    }
    let ends_with_newline = content.ends_with(b"\n");
    let body = content.strip_suffix(b"\n").unwrap_or(content);
    let mut lines: Vec<DiffLine> = body
        .split(|&byte| byte == b'\n')
        .zip(1u32..)
        .map(|(line, number)| DiffLine::from_bytes(DiffLineKind::Added, None, Some(number), line))
        .collect();
    if !ends_with_newline {
        if let Some(last) = lines.last_mut() {
            last.no_newline_at_end = true;
        }
    }
    Some(Hunk {
        old_start: 0,
        old_lines: 0,
        new_start: 1,
        new_lines: u32::try_from(lines.len()).unwrap_or(u32::MAX),
        header: String::new(),
        lines,
    })
}

#[cfg(test)]
mod tests;
