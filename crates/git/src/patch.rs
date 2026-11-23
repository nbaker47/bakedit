//! Staging, unstaging and discarding one hunk at a time.
//!
//! A hunk taken from a [`FileDiff`] is written back out as a patch that holds nothing
//! else and handed to `git apply`.

use std::path::Path;

use crate::diff::{DiffLineKind, DiffStatus, FileDiff, Hunk};
use crate::error::{GitError, Result};
use crate::paths::{needs_quoting, path_bytes, quote_c_style};
use crate::repo::Repo;

/// What to do with a hunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HunkAction {
    /// Put the hunk's change into the index. For a hunk of an unstaged diff
    /// ([`crate::DiffTarget::IndexToWorktree`]).
    Stage,
    /// Take the hunk's change out of the index again, leaving the work tree alone. For a
    /// hunk of a staged diff ([`crate::DiffTarget::HeadToIndex`]).
    Unstage,
    /// Undo the hunk's change in the work tree file. For a hunk of an unstaged diff
    /// ([`crate::DiffTarget::IndexToWorktree`]).
    Discard,
}

impl Repo {
    /// Stages, unstages or discards the hunk `file_diff.hunks[hunk_index]` and nothing
    /// else of the file.
    ///
    /// The diff must be fresh: it describes the file as it was when the diff was made,
    /// and git refuses the patch ([`GitError::Failed`]) when the file has changed under
    /// it. Fetch the diff again after every call, because applying one hunk moves the
    /// line numbers of the ones after it.
    ///
    /// Which diff goes with which action is described on [`HunkAction`]. A hunk of a
    /// [`crate::DiffTarget::HeadToWorktree`] diff can be staged or discarded as long as
    /// nothing of that file is staged yet.
    ///
    /// Special cases:
    /// - Staging the hunk of an untracked file adds the file (its one hunk is all of it).
    /// - Discarding never deletes a file: for an untracked or newly added file it is
    ///   refused with [`GitError::InvalidHunk`]; move the file to the trash instead.
    /// - Unstaging the hunk of a newly added file takes the file out of the index; it
    ///   stays in the work tree, untracked.
    /// - Staging or discarding the hunk of a deleted file stages the deletion or brings
    ///   the file back.
    ///
    /// [`GitError::InvalidHunk`] is also returned when there is no such hunk (binary
    /// files, pure renames and mode changes have none) and for conflicted files.
    pub fn apply_hunk(
        &self,
        file_diff: &FileDiff,
        hunk_index: usize,
        action: HunkAction,
    ) -> Result<()> {
        let name = file_diff.path.display();
        if hunk_index >= file_diff.hunks.len() {
            return Err(GitError::InvalidHunk(format!(
                "{name} has no hunk {hunk_index}"
            )));
        }
        if file_diff.status == DiffStatus::Untracked {
            return match action {
                // The content shown for an untracked file is the raw file; `git add`
                // stores it the way the repository wants it.
                HunkAction::Stage => self.stage(&[&file_diff.path]),
                HunkAction::Unstage => Err(GitError::InvalidHunk(format!(
                    "{name} is not tracked, so nothing of it is staged"
                ))),
                HunkAction::Discard => Err(would_delete(&file_diff.path)),
            };
        }
        if action == HunkAction::Discard && file_diff.status == DiffStatus::Added {
            return Err(would_delete(&file_diff.path));
        }
        let reverse = action != HunkAction::Stage;
        let patch = hunk_patch(file_diff, hunk_index, reverse)?;
        let options: &[&str] = match action {
            HunkAction::Stage => &["--cached"],
            HunkAction::Unstage => &["--cached", "--reverse"],
            HunkAction::Discard => &["--reverse"],
        };
        self.git()
            .args(["apply", "--whitespace=nowarn"])
            .args(options)
            .arg("-")
            .stdin(patch)
            .run()?;
        Ok(())
    }
}

fn would_delete(path: &Path) -> GitError {
    GitError::InvalidHunk(format!(
        "discarding the changes to {} would delete the file; move it to the trash instead",
        path.display()
    ))
}

/// A patch that holds only hunk `hunk_index` of `file`, for `git apply` to apply forward
/// (to the old side's file) or, with `reverse`, to take back out of the new side's file.
///
/// The hunk's position is rewritten for a file in which none of the diff's other hunks
/// are applied (forward) or all of them are (reverse): git starts looking for the hunk at
/// the position the patch states, and in a file with repeated blocks a wrong position
/// would let it settle on the wrong block.
pub(crate) fn hunk_patch(file: &FileDiff, hunk_index: usize, reverse: bool) -> Result<Vec<u8>> {
    let name = file.path.display();
    let hunk = file
        .hunks
        .get(hunk_index)
        .ok_or_else(|| GitError::InvalidHunk(format!("{name} has no hunk {hunk_index}")))?;
    if file.status == DiffStatus::Conflicted {
        return Err(GitError::InvalidHunk(format!(
            "{name} is conflicted; resolve the conflict and stage the whole file"
        )));
    }

    // The file the patch is applied to: the diff's new file when a change is taken back
    // out of it, else the file the change was made to.
    let target = match (&file.old_path, reverse) {
        (Some(old_path), false) => old_path,
        _ => &file.path,
    };
    let old_name = prefixed_name(b"a/", target);
    let new_name = prefixed_name(b"b/", target);

    let mut patch = Vec::new();
    patch.extend_from_slice(b"diff --git ");
    patch.extend_from_slice(&old_name);
    patch.push(b' ');
    patch.extend_from_slice(&new_name);
    patch.push(b'\n');
    match file.status {
        DiffStatus::Added => {
            let mode = file.new_mode.unwrap_or(0o100644);
            patch.extend_from_slice(format!("new file mode {mode:o}\n").as_bytes());
            patch.extend_from_slice(b"--- /dev/null\n");
            push_name_line(&mut patch, b"+++ ", &new_name);
        }
        DiffStatus::Deleted => {
            let mode = file.old_mode.unwrap_or(0o100644);
            patch.extend_from_slice(format!("deleted file mode {mode:o}\n").as_bytes());
            push_name_line(&mut patch, b"--- ", &old_name);
            patch.extend_from_slice(b"+++ /dev/null\n");
        }
        _ => {
            push_name_line(&mut patch, b"--- ", &old_name);
            push_name_line(&mut patch, b"+++ ", &new_name);
        }
    }

    let (old_start, new_start) = standalone_starts(hunk, reverse);
    patch.extend_from_slice(
        format!(
            "@@ -{old_start},{} +{new_start},{} @@\n",
            hunk.old_lines, hunk.new_lines
        )
        .as_bytes(),
    );
    for line in &hunk.lines {
        patch.push(match line.kind {
            DiffLineKind::Context => b' ',
            DiffLineKind::Added => b'+',
            DiffLineKind::Removed => b'-',
        });
        patch.extend_from_slice(line.bytes());
        patch.push(b'\n');
        if line.no_newline_at_end {
            patch.extend_from_slice(b"\\ No newline at end of file\n");
        }
    }
    Ok(patch)
}

/// Where `hunk` starts on both sides when it is the only hunk of its patch.
///
/// Applied forward, nothing before the hunk has changed, so its new side begins at the
/// same line as its old side. Applied in reverse the roles are swapped: the new side is
/// where the hunk really is in the file, and the old side follows it. A side of length 0
/// names the line before the change instead of its first line, which is what the two
/// conversions are about.
fn standalone_starts(hunk: &Hunk, reverse: bool) -> (u32, u32) {
    let lines_before = |start: u32, lines: u32| {
        if lines == 0 {
            start
        } else {
            start.saturating_sub(1)
        }
    };
    let start_after = |before: u32, lines: u32| if lines == 0 { before } else { before + 1 };
    if reverse {
        let before = lines_before(hunk.new_start, hunk.new_lines);
        (start_after(before, hunk.old_lines), hunk.new_start)
    } else {
        let before = lines_before(hunk.old_start, hunk.old_lines);
        (hunk.old_start, start_after(before, hunk.new_lines))
    }
}

/// `prefix` and `path` as one name in a patch header, quoted the way git quotes it.
fn prefixed_name(prefix: &[u8], path: &Path) -> Vec<u8> {
    let mut name = prefix.to_vec();
    name.extend_from_slice(path_bytes(path));
    if needs_quoting(&name) {
        quote_c_style(&name)
    } else {
        name
    }
}

/// Writes a `--- name` or `+++ name` line. Git ends an unquoted name that contains a
/// space with a tab, and expects the same when it reads a patch.
fn push_name_line(patch: &mut Vec<u8>, marker: &[u8], name: &[u8]) {
    patch.extend_from_slice(marker);
    patch.extend_from_slice(name);
    if !name.starts_with(b"\"") && name.contains(&b' ') {
        patch.push(b'\t');
    }
    patch.push(b'\n');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::parse_diff;

    fn parse_one(text: &str) -> FileDiff {
        let mut files = parse_diff(text.as_bytes());
        assert_eq!(files.len(), 1);
        files.remove(0)
    }

    fn patch_text(file: &FileDiff, hunk_index: usize, reverse: bool) -> String {
        String::from_utf8(hunk_patch(file, hunk_index, reverse).unwrap()).unwrap()
    }

    const THREE_HUNKS: &str = concat!(
        "diff --git a/rep.txt b/rep.txt\n",
        "index 56395f9..7508dc1 100644\n",
        "--- a/rep.txt\n",
        "+++ b/rep.txt\n",
        "@@ -1,7 +1,6 @@\n",
        " block\n",
        " same\n",
        " same\n",
        "-same\n",
        " value\n",
        " same\n",
        " same\n",
        "@@ -11,7 +10,7 @@ block\n",
        " same\n",
        " same\n",
        " same\n",
        "-value\n",
        "+value CHANGED A\n",
        " same\n",
        " same\n",
        " same\n",
        "@@ -38,7 +37,8 @@ block\n",
        " same\n",
        " same\n",
        " same\n",
        "-value\n",
        "+value CHANGED B\n",
        "+extra\n",
        " same\n",
        " same\n",
        " same\n",
    );

    #[test]
    fn a_forward_patch_keeps_the_old_position_and_follows_it_on_the_new_side() {
        let file = parse_one(THREE_HUNKS);
        assert_eq!(
            patch_text(&file, 2, false),
            concat!(
                "diff --git a/rep.txt b/rep.txt\n",
                "--- a/rep.txt\n",
                "+++ b/rep.txt\n",
                "@@ -38,7 +38,8 @@\n",
                " same\n",
                " same\n",
                " same\n",
                "-value\n",
                "+value CHANGED B\n",
                "+extra\n",
                " same\n",
                " same\n",
                " same\n",
            )
        );
        assert!(patch_text(&file, 1, false).contains("@@ -11,7 +11,7 @@\n"));
        assert!(patch_text(&file, 0, false).contains("@@ -1,7 +1,6 @@\n"));
    }

    #[test]
    fn a_reverse_patch_keeps_the_new_position_and_follows_it_on_the_old_side() {
        let file = parse_one(THREE_HUNKS);
        assert!(patch_text(&file, 2, true).contains("@@ -37,7 +37,8 @@\n"));
        assert!(patch_text(&file, 1, true).contains("@@ -10,7 +10,7 @@\n"));
        assert!(patch_text(&file, 0, true).contains("@@ -1,7 +1,6 @@\n"));
    }

    #[test]
    fn sides_without_lines_name_the_line_before_the_change() {
        let hunk = |old_start, old_lines, new_start, new_lines| Hunk {
            old_start,
            old_lines,
            new_start,
            new_lines,
            header: String::new(),
            lines: Vec::new(),
        };
        // A new file, or lines added to an empty one: nothing before on either side.
        assert_eq!(standalone_starts(&hunk(0, 0, 1, 3), false), (0, 1));
        assert_eq!(standalone_starts(&hunk(0, 0, 1, 3), true), (0, 1));
        // A file that is emptied.
        assert_eq!(standalone_starts(&hunk(1, 3, 0, 0), false), (1, 0));
        assert_eq!(standalone_starts(&hunk(1, 3, 0, 0), true), (1, 0));
        // A pure insertion after old line 20, which earlier hunks moved to new line 26.
        assert_eq!(standalone_starts(&hunk(20, 0, 26, 2), false), (20, 21));
        assert_eq!(standalone_starts(&hunk(20, 0, 26, 2), true), (25, 26));
        // A pure deletion of old lines 20-21, after which new line 17 follows.
        assert_eq!(standalone_starts(&hunk(20, 2, 17, 0), false), (20, 19));
        assert_eq!(standalone_starts(&hunk(20, 2, 17, 0), true), (18, 17));
        // An ordinary change.
        assert_eq!(standalone_starts(&hunk(38, 7, 37, 8), false), (38, 38));
        assert_eq!(standalone_starts(&hunk(38, 7, 37, 8), true), (37, 37));
    }

    #[test]
    fn new_and_deleted_files_keep_their_mode_lines() {
        let added = parse_one(concat!(
            "diff --git a/tool.sh b/tool.sh\n",
            "new file mode 100755\n",
            "index 0000000..d5a09df\n",
            "--- /dev/null\n",
            "+++ b/tool.sh\n",
            "@@ -0,0 +1,2 @@\n",
            "+#!/bin/sh\n",
            "+echo hi\n",
        ));
        let expected = concat!(
            "diff --git a/tool.sh b/tool.sh\n",
            "new file mode 100755\n",
            "--- /dev/null\n",
            "+++ b/tool.sh\n",
            "@@ -0,0 +1,2 @@\n",
            "+#!/bin/sh\n",
            "+echo hi\n",
        );
        assert_eq!(patch_text(&added, 0, false), expected);
        assert_eq!(patch_text(&added, 0, true), expected);

        let deleted = parse_one(concat!(
            "diff --git a/old.txt b/old.txt\n",
            "deleted file mode 100644\n",
            "index 286c5f5..0000000\n",
            "--- a/old.txt\n",
            "+++ /dev/null\n",
            "@@ -1 +0,0 @@\n",
            "-gone\n",
        ));
        let expected = concat!(
            "diff --git a/old.txt b/old.txt\n",
            "deleted file mode 100644\n",
            "--- a/old.txt\n",
            "+++ /dev/null\n",
            "@@ -1,1 +0,0 @@\n",
            "-gone\n",
        );
        assert_eq!(patch_text(&deleted, 0, false), expected);
        assert_eq!(patch_text(&deleted, 0, true), expected);
    }

    #[test]
    fn missing_final_line_feeds_are_written_back() {
        let file = parse_one(concat!(
            "diff --git a/nonl.txt b/nonl.txt\n",
            "--- a/nonl.txt\n",
            "+++ b/nonl.txt\n",
            "@@ -1,2 +1,2 @@\n",
            " first\n",
            "-no newline\n",
            "\\ No newline at end of file\n",
            "+no newline changed\n",
            "\\ No newline at end of file\n",
        ));
        assert_eq!(
            patch_text(&file, 0, false),
            concat!(
                "diff --git a/nonl.txt b/nonl.txt\n",
                "--- a/nonl.txt\n",
                "+++ b/nonl.txt\n",
                "@@ -1,2 +1,2 @@\n",
                " first\n",
                "-no newline\n",
                "\\ No newline at end of file\n",
                "+no newline changed\n",
                "\\ No newline at end of file\n",
            )
        );
    }

    #[test]
    fn names_are_written_the_way_git_writes_them() {
        let spaced = parse_one(concat!(
            "diff --git a/dir with space/\u{fc}.txt b/dir with space/\u{fc}.txt\n",
            "--- a/dir with space/\u{fc}.txt\t\n",
            "+++ b/dir with space/\u{fc}.txt\t\n",
            "@@ -1 +1 @@\n",
            "-a\n",
            "+b\n",
        ));
        assert!(patch_text(&spaced, 0, false).starts_with(concat!(
            "diff --git a/dir with space/\u{fc}.txt b/dir with space/\u{fc}.txt\n",
            "--- a/dir with space/\u{fc}.txt\t\n",
            "+++ b/dir with space/\u{fc}.txt\t\n",
        )));

        let quoted = parse_one(concat!(
            "diff --git \"a/ta\\tb \\\"q\\\".txt\" \"b/ta\\tb \\\"q\\\".txt\"\n",
            "--- \"a/ta\\tb \\\"q\\\".txt\"\n",
            "+++ \"b/ta\\tb \\\"q\\\".txt\"\n",
            "@@ -1 +1 @@\n",
            "-a\n",
            "+b\n",
        ));
        assert_eq!(quoted.path, Path::new("ta\tb \"q\".txt"));
        assert!(patch_text(&quoted, 0, true).starts_with(concat!(
            "diff --git \"a/ta\\tb \\\"q\\\".txt\" \"b/ta\\tb \\\"q\\\".txt\"\n",
            "--- \"a/ta\\tb \\\"q\\\".txt\"\n",
            "+++ \"b/ta\\tb \\\"q\\\".txt\"\n",
        )));
    }

    #[test]
    fn a_renamed_file_is_patched_under_the_name_it_has_where_the_patch_goes() {
        let file = parse_one(concat!(
            "diff --git a/old.txt b/new.txt\n",
            "similarity index 72%\n",
            "rename from old.txt\n",
            "rename to new.txt\n",
            "index 3a31ef6..c8cd5d0 100644\n",
            "--- a/old.txt\n",
            "+++ b/new.txt\n",
            "@@ -1,2 +1,2 @@\n",
            " keep\n",
            "-old line\n",
            "+new line\n",
        ));
        // Taken back out of the side that has the new name.
        assert!(patch_text(&file, 0, true)
            .starts_with("diff --git a/new.txt b/new.txt\n--- a/new.txt\n+++ b/new.txt\n@@"));
        // Applied to the side that still has the old one.
        assert!(patch_text(&file, 0, false)
            .starts_with("diff --git a/old.txt b/old.txt\n--- a/old.txt\n+++ b/old.txt\n@@"));
    }

    #[test]
    fn bytes_that_are_not_utf8_are_written_back_exactly() {
        let mut text = b"diff --git a/latin1.txt b/latin1.txt\n".to_vec();
        text.extend_from_slice(b"--- a/latin1.txt\n+++ b/latin1.txt\n@@ -1,2 +1,2 @@\n");
        text.extend_from_slice(b" caf\xe9\r\n-na\xefve\n+naive\n");
        let file = &parse_diff(&text)[0];
        let patch = hunk_patch(file, 0, false).unwrap();
        assert!(patch.ends_with(b"@@ -1,2 +1,2 @@\n caf\xe9\r\n-na\xefve\n+naive\n"));
    }

    #[test]
    fn hunks_that_do_not_exist_and_conflicts_are_refused() {
        let file = parse_one(THREE_HUNKS);
        assert!(matches!(
            hunk_patch(&file, 3, false),
            Err(GitError::InvalidHunk(message)) if message == "rep.txt has no hunk 3"
        ));
        let binary = parse_one(concat!(
            "diff --git a/bin.dat b/bin.dat\n",
            "Binary files a/bin.dat and b/bin.dat differ\n",
        ));
        assert!(matches!(
            hunk_patch(&binary, 0, false),
            Err(GitError::InvalidHunk(_))
        ));
        let conflicted = parse_one(concat!(
            "* Unmerged path both.txt\n",
            "diff --git a/both.txt b/both.txt\n",
            "--- a/both.txt\n",
            "+++ b/both.txt\n",
            "@@ -1 +1 @@\n",
            "-a\n",
            "+b\n",
        ));
        assert!(matches!(
            hunk_patch(&conflicted, 0, false),
            Err(GitError::InvalidHunk(message)) if message.contains("conflicted")
        ));
    }
}
