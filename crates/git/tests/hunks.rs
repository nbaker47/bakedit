//! Staging, unstaging and discarding single hunks, end to end.

mod common;

use bakedit_git::{DiffStatus, DiffTarget, FileDiff, GitError, HunkAction, StageState};
use common::TestRepo;

/// Six identical blocks: a hunk taken out of its place would fit in five others.
fn repeated_blocks() -> String {
    "block\nsame\nsame\nsame\nvalue\nsame\nsame\nsame\nend\n".repeat(6)
}

/// `text` with 1-based line `number` replaced by `replacement` (which may be several
/// lines, or empty to delete the line).
fn replace_line(text: &str, number: usize, replacement: &str) -> String {
    text.split_inclusive('\n')
        .enumerate()
        .map(|(index, line)| {
            if index + 1 == number {
                replacement
            } else {
                line
            }
        })
        .collect()
}

/// Three changes far apart in a file of repeated blocks.
fn three_changes() -> (TestRepo, String, String) {
    let test = TestRepo::new();
    let base = repeated_blocks();
    test.write("rep.txt", &base);
    test.commit_all("base");
    // Line 4 goes, line 14 changes, line 41 changes and grows.
    let changed = replace_line(&base, 41, "value CHANGED B\nextra\n");
    let changed = replace_line(&changed, 14, "value CHANGED A\n");
    let changed = replace_line(&changed, 4, "");
    test.write("rep.txt", &changed);
    (test, base, changed)
}

fn unstaged(test: &TestRepo, path: &str) -> FileDiff {
    test.repo
        .diff_file(path, DiffTarget::IndexToWorktree)
        .unwrap()
        .unwrap_or_else(|| panic!("{path} has unstaged changes"))
}

fn staged(test: &TestRepo, path: &str) -> FileDiff {
    test.repo
        .diff_file(path, DiffTarget::HeadToIndex)
        .unwrap()
        .unwrap_or_else(|| panic!("{path} has staged changes"))
}

/// The staged content of a file, exactly as stored.
fn index_content(test: &TestRepo, path: &str) -> String {
    test.git(&["show", &format!(":0:{path}")])
}

#[test]
fn staging_one_hunk_of_three_stages_exactly_that_change() {
    let (test, base, changed) = three_changes();
    let diff = unstaged(&test, "rep.txt");
    assert_eq!(diff.hunks.len(), 3);

    test.repo.apply_hunk(&diff, 2, HunkAction::Stage).unwrap();

    // In the index: the base with only the third change, in the fifth block and nowhere
    // else.
    let expected = replace_line(&base, 41, "value CHANGED B\nextra\n");
    assert_eq!(index_content(&test, "rep.txt"), expected);
    assert_eq!(
        test.read("rep.txt"),
        changed,
        "the work tree is not touched"
    );
    assert_eq!(staged(&test, "rep.txt").hunks.len(), 1);
    assert_eq!(unstaged(&test, "rep.txt").hunks.len(), 2);
    let status = test.repo.status().unwrap();
    assert_eq!(
        status.entry("rep.txt").unwrap().stage_state(),
        StageState::PartiallyStaged
    );
}

#[test]
fn each_hunk_can_be_staged_on_its_own() {
    for index in 0..3 {
        let (test, base, _) = three_changes();
        let diff = unstaged(&test, "rep.txt");
        test.repo
            .apply_hunk(&diff, index, HunkAction::Stage)
            .unwrap();
        let expected = match index {
            0 => replace_line(&base, 4, ""),
            1 => replace_line(&base, 14, "value CHANGED A\n"),
            _ => replace_line(&base, 41, "value CHANGED B\nextra\n"),
        };
        assert_eq!(index_content(&test, "rep.txt"), expected, "hunk {index}");
    }
}

#[test]
fn staging_hunk_after_hunk_with_a_fresh_diff_stages_everything() {
    let (test, _, changed) = three_changes();
    for remaining in (1..=3).rev() {
        let diff = unstaged(&test, "rep.txt");
        assert_eq!(diff.hunks.len(), remaining);
        // Always the last one, so the earlier ones keep their place.
        test.repo
            .apply_hunk(&diff, remaining - 1, HunkAction::Stage)
            .unwrap();
    }
    assert_eq!(index_content(&test, "rep.txt"), changed);
    assert_eq!(
        test.repo
            .diff_file("rep.txt", DiffTarget::IndexToWorktree)
            .unwrap(),
        None
    );
    assert!(test
        .repo
        .status()
        .unwrap()
        .entry("rep.txt")
        .unwrap()
        .is_fully_staged());
}

#[test]
fn unstaging_a_hunk_takes_only_that_change_back_out() {
    let (test, base, changed) = three_changes();
    test.repo.stage(&["rep.txt"]).unwrap();
    let diff = staged(&test, "rep.txt");
    assert_eq!(diff.hunks.len(), 3);

    // The middle one: the hunk before it has moved it up by a line in the index.
    test.repo.apply_hunk(&diff, 1, HunkAction::Unstage).unwrap();
    let expected = replace_line(&base, 41, "value CHANGED B\nextra\n");
    let expected = replace_line(&expected, 4, "");
    assert_eq!(index_content(&test, "rep.txt"), expected);
    assert_eq!(
        test.read("rep.txt"),
        changed,
        "the work tree is not touched"
    );

    // Unstaging the rest one by one empties the index of changes.
    for _ in 0..2 {
        let diff = staged(&test, "rep.txt");
        test.repo.apply_hunk(&diff, 0, HunkAction::Unstage).unwrap();
    }
    assert_eq!(index_content(&test, "rep.txt"), base);
    assert_eq!(
        test.repo
            .diff_file("rep.txt", DiffTarget::HeadToIndex)
            .unwrap(),
        None
    );
    assert_eq!(unstaged(&test, "rep.txt").hunks.len(), 3);
}

#[test]
fn discarding_a_hunk_reverts_only_that_change_in_the_work_tree() {
    let (test, base, _) = three_changes();
    let diff = unstaged(&test, "rep.txt");

    test.repo.apply_hunk(&diff, 1, HunkAction::Discard).unwrap();
    let expected = replace_line(&base, 41, "value CHANGED B\nextra\n");
    let expected = replace_line(&expected, 4, "");
    assert_eq!(test.read("rep.txt"), expected);
    assert_eq!(
        index_content(&test, "rep.txt"),
        base,
        "the index is not touched"
    );

    // The last one, then the first: back to the committed file.
    let diff = unstaged(&test, "rep.txt");
    assert_eq!(diff.hunks.len(), 2);
    test.repo.apply_hunk(&diff, 1, HunkAction::Discard).unwrap();
    let diff = unstaged(&test, "rep.txt");
    test.repo.apply_hunk(&diff, 0, HunkAction::Discard).unwrap();
    assert_eq!(test.read("rep.txt"), base);
    assert!(test.repo.status().unwrap().is_clean());
}

#[test]
fn discarding_keeps_what_is_staged() {
    let (test, base, _) = three_changes();
    let diff = unstaged(&test, "rep.txt");
    test.repo.apply_hunk(&diff, 0, HunkAction::Stage).unwrap();
    let staged_content = replace_line(&base, 4, "");

    // Two unstaged hunks are left; drop them both.
    for _ in 0..2 {
        let diff = unstaged(&test, "rep.txt");
        test.repo.apply_hunk(&diff, 0, HunkAction::Discard).unwrap();
    }
    assert_eq!(test.read("rep.txt"), staged_content);
    assert_eq!(index_content(&test, "rep.txt"), staged_content);
    assert!(test
        .repo
        .status()
        .unwrap()
        .entry("rep.txt")
        .unwrap()
        .is_fully_staged());
}

#[test]
fn hunks_of_a_head_to_worktree_diff_can_be_staged_and_discarded() {
    let (test, base, _) = three_changes();
    let diff = test
        .repo
        .diff_file("rep.txt", DiffTarget::HeadToWorktree)
        .unwrap()
        .unwrap();
    test.repo.apply_hunk(&diff, 1, HunkAction::Stage).unwrap();
    assert_eq!(
        index_content(&test, "rep.txt"),
        replace_line(&base, 14, "value CHANGED A\n")
    );
    test.repo.apply_hunk(&diff, 2, HunkAction::Discard).unwrap();
    let expected = replace_line(&base, 14, "value CHANGED A\n");
    let expected = replace_line(&expected, 4, "");
    assert_eq!(test.read("rep.txt"), expected);
}

/// Nine new lines near the top, and the same edit in the fourth and in the fifth block:
/// two hunks with identical text, both pushed down by exactly one block's height. A hunk
/// applied at the position the other hunks moved it to (or from) lands in the wrong block.
fn shifted_twins() -> (TestRepo, String, [String; 3]) {
    let test = TestRepo::new();
    let base = repeated_blocks();
    test.write("rep.txt", &base);
    test.commit_all("base");
    let inserted: String = (1..=9).map(|line| format!("new {line}\n")).collect();
    let with_insertion = replace_line(&base, 2, &format!("same\n{inserted}"));
    let with_fourth = replace_line(&base, 32, "value CHANGED\n");
    let with_fifth = replace_line(&base, 41, "value CHANGED\n");
    let everything = replace_line(&with_fourth, 41, "value CHANGED\n");
    let everything = replace_line(&everything, 2, &format!("same\n{inserted}"));
    test.write("rep.txt", &everything);
    (test, everything, [with_insertion, with_fourth, with_fifth])
}

#[test]
fn a_hunk_is_staged_where_it_belongs_even_when_other_hunks_moved_it() {
    for (index, block) in [(1, "fourth"), (2, "fifth")] {
        let (test, _, alone) = shifted_twins();
        let diff = unstaged(&test, "rep.txt");
        assert_eq!(diff.hunks.len(), 3);
        // The two edits look the same and sit one block further down than in the base.
        let look = |hunk: usize| -> Vec<_> {
            let lines = &diff.hunks[hunk].lines;
            lines
                .iter()
                .map(|line| (line.kind, line.text.as_str()))
                .collect()
        };
        assert_eq!(look(1), look(2));
        assert_eq!(diff.hunks[1].new_start, diff.hunks[1].old_start + 9);

        test.repo
            .apply_hunk(&diff, index, HunkAction::Stage)
            .unwrap();
        assert_eq!(
            index_content(&test, "rep.txt"),
            alone[index],
            "the {block} block"
        );
    }
}

#[test]
fn a_hunk_is_unstaged_and_discarded_where_it_belongs_even_when_other_hunks_moved_it() {
    let without =
        |everything: &str, changed_line: usize| replace_line(everything, changed_line, "value\n");
    // In the changed file the edits are at lines 41 and 50.
    for (index, line) in [(1, 41), (2, 50)] {
        let (test, everything, _) = shifted_twins();
        test.repo.stage(&["rep.txt"]).unwrap();
        let diff = staged(&test, "rep.txt");
        assert_eq!(diff.hunks.len(), 3);
        test.repo
            .apply_hunk(&diff, index, HunkAction::Unstage)
            .unwrap();
        assert_eq!(
            index_content(&test, "rep.txt"),
            without(&everything, line),
            "unstaging hunk {index}"
        );

        let (test, everything, _) = shifted_twins();
        let diff = unstaged(&test, "rep.txt");
        test.repo
            .apply_hunk(&diff, index, HunkAction::Discard)
            .unwrap();
        assert_eq!(
            test.read("rep.txt"),
            without(&everything, line),
            "discarding hunk {index}"
        );
    }
}

#[test]
fn a_stale_diff_is_refused_and_changes_nothing() {
    let (test, base, _) = three_changes();
    let diff = unstaged(&test, "rep.txt");
    // The file moves on after the diff was taken.
    test.write("rep.txt", "something else entirely\n");
    test.repo.stage(&["rep.txt"]).unwrap();

    for action in [HunkAction::Stage, HunkAction::Unstage, HunkAction::Discard] {
        match test.repo.apply_hunk(&diff, 2, action) {
            Err(GitError::Failed {
                command,
                code,
                stderr,
                ..
            }) => {
                assert_eq!(command, "apply");
                assert_eq!(code, Some(1));
                assert!(stderr.contains("rep.txt"), "{stderr}");
            }
            other => panic!("unexpected {other:?} for {action:?}"),
        }
    }
    assert_eq!(index_content(&test, "rep.txt"), "something else entirely\n");
    assert_eq!(test.read("rep.txt"), "something else entirely\n");
    assert_ne!(base, test.read("rep.txt"));
}

#[test]
fn hunk_indexes_out_of_range_are_refused() {
    let (test, _, _) = three_changes();
    let diff = unstaged(&test, "rep.txt");
    for action in [HunkAction::Stage, HunkAction::Unstage, HunkAction::Discard] {
        match test.repo.apply_hunk(&diff, 3, action) {
            Err(GitError::InvalidHunk(message)) => {
                assert_eq!(message, "rep.txt has no hunk 3")
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(test.repo.status().unwrap().staged().is_empty());
}

#[test]
fn files_without_hunks_have_nothing_to_apply() {
    let test = TestRepo::new();
    test.write("image.bin", b"\x00\x01 old");
    test.write("tool.sh", "#!/bin/sh\n");
    test.commit_all("base");
    test.write("image.bin", b"\x00\x01 new");
    test.make_executable("tool.sh");

    for path in ["image.bin", "tool.sh"] {
        let diff = unstaged(&test, path);
        assert!(diff.hunks.is_empty());
        assert!(matches!(
            test.repo.apply_hunk(&diff, 0, HunkAction::Stage),
            Err(GitError::InvalidHunk(_))
        ));
    }
}

#[test]
fn the_hunk_of_an_untracked_file_is_the_whole_file() {
    let test = TestRepo::with_commit();
    test.write("new dir/new file.txt", "one\ntwo\n");
    let diff = test
        .repo
        .diff_file("new dir/new file.txt", DiffTarget::HeadToWorktree)
        .unwrap()
        .unwrap();
    assert_eq!(diff.status, DiffStatus::Untracked);

    // Unstaging makes no sense and discarding would delete the file: both refused.
    assert!(matches!(
        test.repo.apply_hunk(&diff, 0, HunkAction::Unstage),
        Err(GitError::InvalidHunk(_))
    ));
    match test.repo.apply_hunk(&diff, 0, HunkAction::Discard) {
        Err(GitError::InvalidHunk(message)) => assert!(message.contains("trash"), "{message}"),
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(test.read("new dir/new file.txt"), "one\ntwo\n");

    test.repo.apply_hunk(&diff, 0, HunkAction::Stage).unwrap();
    assert_eq!(index_content(&test, "new dir/new file.txt"), "one\ntwo\n");
    assert!(test
        .repo
        .status()
        .unwrap()
        .entry("new dir/new file.txt")
        .unwrap()
        .is_fully_staged());
}

#[test]
fn a_newly_added_file_can_be_unstaged_but_never_discarded() {
    let test = TestRepo::with_commit();
    test.write("added.sh", "#!/bin/sh\necho hi\n");
    test.make_executable("added.sh");
    test.repo.stage(&["added.sh"]).unwrap();

    let diff = staged(&test, "added.sh");
    assert_eq!(diff.status, DiffStatus::Added);
    for target in [DiffTarget::HeadToIndex, DiffTarget::HeadToWorktree] {
        let diff = test.repo.diff_file("added.sh", target).unwrap().unwrap();
        match test.repo.apply_hunk(&diff, 0, HunkAction::Discard) {
            Err(GitError::InvalidHunk(message)) => assert!(message.contains("trash")),
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(test.exists("added.sh"));

    test.repo.apply_hunk(&diff, 0, HunkAction::Unstage).unwrap();
    let status = test.repo.status().unwrap();
    assert!(status.entry("added.sh").unwrap().untracked);
    assert_eq!(test.read("added.sh"), "#!/bin/sh\necho hi\n");
}

#[test]
fn a_file_known_by_name_only_is_staged_with_its_mode() {
    let test = TestRepo::with_commit();
    test.write("tool.sh", "#!/bin/sh\necho hi\n");
    test.make_executable("tool.sh");
    test.git(&["add", "--intent-to-add", "tool.sh"]);

    let diff = unstaged(&test, "tool.sh");
    assert_eq!(diff.status, DiffStatus::Added);
    assert!(matches!(
        test.repo.apply_hunk(&diff, 0, HunkAction::Discard),
        Err(GitError::InvalidHunk(_))
    ));
    test.repo.apply_hunk(&diff, 0, HunkAction::Stage).unwrap();
    assert_eq!(index_content(&test, "tool.sh"), "#!/bin/sh\necho hi\n");
    let entry = test.git(&["ls-files", "--stage", "tool.sh"]);
    assert!(entry.starts_with("100755 "), "{entry}");
}

#[test]
fn the_hunk_of_a_deleted_file_stages_the_deletion_or_brings_the_file_back() {
    let test = TestRepo::new();
    test.write("doomed.txt", "one\ntwo\nthree\n");
    test.commit_all("base");

    test.remove("doomed.txt");
    let diff = unstaged(&test, "doomed.txt");
    assert_eq!(diff.status, DiffStatus::Deleted);
    test.repo.apply_hunk(&diff, 0, HunkAction::Discard).unwrap();
    assert_eq!(test.read("doomed.txt"), "one\ntwo\nthree\n");
    assert!(test.repo.status().unwrap().is_clean());

    test.remove("doomed.txt");
    let diff = unstaged(&test, "doomed.txt");
    test.repo.apply_hunk(&diff, 0, HunkAction::Stage).unwrap();
    let status = test.repo.status().unwrap();
    assert!(status.entry("doomed.txt").unwrap().is_fully_staged());

    // And a staged deletion can be unstaged again.
    let diff = staged(&test, "doomed.txt");
    test.repo.apply_hunk(&diff, 0, HunkAction::Unstage).unwrap();
    let status = test.repo.status().unwrap();
    assert_eq!(
        status.entry("doomed.txt").unwrap().stage_state(),
        StageState::Unstaged
    );
    assert_eq!(index_content(&test, "doomed.txt"), "one\ntwo\nthree\n");
}

#[test]
fn hunks_before_the_first_commit() {
    let test = TestRepo::new();
    let base: String = (1..=30).map(|line| format!("line {line}\n")).collect();
    test.write("early.txt", &base);
    test.repo.stage(&["early.txt"]).unwrap();
    let changed = base
        .replace("line 2\n", "line two\n")
        .replace("line 29\n", "line twenty-nine\n");
    test.write("early.txt", &changed);

    let diff = unstaged(&test, "early.txt");
    assert_eq!(diff.hunks.len(), 2);
    test.repo.apply_hunk(&diff, 0, HunkAction::Stage).unwrap();
    assert_eq!(
        index_content(&test, "early.txt"),
        base.replace("line 2\n", "line two\n")
    );
    test.repo.apply_hunk(&diff, 1, HunkAction::Discard).unwrap();
    assert_eq!(
        test.read("early.txt"),
        base.replace("line 2\n", "line two\n")
    );

    // What is staged is one new file; unstaging its hunk makes it untracked again.
    let diff = staged(&test, "early.txt");
    assert_eq!(diff.status, DiffStatus::Added);
    test.repo.apply_hunk(&diff, 0, HunkAction::Unstage).unwrap();
    assert!(
        test.repo
            .status()
            .unwrap()
            .entry("early.txt")
            .unwrap()
            .untracked
    );
}

#[test]
fn hunks_at_the_start_and_end_of_a_file() {
    let test = TestRepo::new();
    let base: String = (1..=30).map(|line| format!("line {line}\n")).collect();
    test.write("edges.txt", &base);
    test.commit_all("base");
    let changed = format!("new first line\n{base}new last line\n");
    test.write("edges.txt", &changed);

    let diff = unstaged(&test, "edges.txt");
    assert_eq!(diff.hunks.len(), 2);
    test.repo.apply_hunk(&diff, 1, HunkAction::Stage).unwrap();
    assert_eq!(
        index_content(&test, "edges.txt"),
        format!("{base}new last line\n")
    );

    let diff = unstaged(&test, "edges.txt");
    assert_eq!(diff.hunks.len(), 1);
    test.repo.apply_hunk(&diff, 0, HunkAction::Discard).unwrap();
    assert_eq!(test.read("edges.txt"), format!("{base}new last line\n"));
    assert!(test
        .repo
        .status()
        .unwrap()
        .entry("edges.txt")
        .unwrap()
        .is_fully_staged());
}

#[test]
fn files_without_a_final_line_feed() {
    let test = TestRepo::new();
    let base: String = (1..=20).map(|line| format!("line {line}\n")).collect();
    test.write("nonl.txt", format!("{base}last"));
    test.commit_all("base");
    let changed = format!("{}last changed", base.replace("line 2\n", "line two\n"));
    test.write("nonl.txt", &changed);

    let diff = unstaged(&test, "nonl.txt");
    assert_eq!(diff.hunks.len(), 2);
    // The hunk that touches the unterminated last line.
    test.repo.apply_hunk(&diff, 1, HunkAction::Stage).unwrap();
    assert_eq!(
        index_content(&test, "nonl.txt"),
        format!("{base}last changed")
    );
    let fresh = staged(&test, "nonl.txt");
    test.repo
        .apply_hunk(&fresh, 0, HunkAction::Unstage)
        .unwrap();
    assert_eq!(index_content(&test, "nonl.txt"), format!("{base}last"));

    test.repo.apply_hunk(&diff, 1, HunkAction::Discard).unwrap();
    assert_eq!(
        test.read("nonl.txt"),
        format!("{}last", base.replace("line 2\n", "line two\n"))
    );

    // Gaining the final line feed is a hunk like any other.
    test.write("nonl.txt", format!("{base}last\n"));
    let diff = unstaged(&test, "nonl.txt");
    test.repo.apply_hunk(&diff, 0, HunkAction::Stage).unwrap();
    assert_eq!(index_content(&test, "nonl.txt"), format!("{base}last\n"));
}

#[test]
fn carriage_returns_and_other_encodings_survive_the_round_trip() {
    let test = TestRepo::new();
    let mut base = Vec::new();
    for line in 1..=20 {
        base.extend_from_slice(format!("caf\u{e9} line {line}\r\n").as_bytes());
    }
    // A line in Latin-1 for good measure: not valid UTF-8.
    base.extend_from_slice(b"na\xefve\r\n");
    test.write("crlf.txt", &base);
    test.commit_all("base");

    let text = String::from_utf8_lossy(&base).into_owned();
    assert!(text.contains('\u{fffd}'));
    let mut changed = base.clone();
    let at = changed
        .windows(8)
        .position(|window| window == b" line 3\r")
        .unwrap();
    changed.splice(at..at + 8, b" line three\r".iter().copied());
    changed.extend_from_slice(b"na\xefve again\r\n");
    test.write("crlf.txt", &changed);

    let diff = unstaged(&test, "crlf.txt");
    assert_eq!(diff.hunks.len(), 2);
    test.repo.apply_hunk(&diff, 1, HunkAction::Stage).unwrap();
    let mut expected = base.clone();
    expected.extend_from_slice(b"na\xefve again\r\n");
    assert_eq!(
        test.repo.index_bytes("crlf.txt").unwrap().unwrap(),
        expected
    );

    test.repo.apply_hunk(&diff, 0, HunkAction::Discard).unwrap();
    assert_eq!(std::fs::read(test.path("crlf.txt")).unwrap(), expected);
    assert!(test
        .repo
        .status()
        .unwrap()
        .entry("crlf.txt")
        .unwrap()
        .is_fully_staged());
}

#[test]
fn unusual_file_names_can_be_patched() {
    for name in [
        "dir with space/\u{fc} \u{f1}.txt",
        "ta\tb.txt",
        "quo\"te.txt",
        "back\\slash.txt",
        "new\nline.txt",
        "\u{65e5}\u{672c}\u{8a9e}.txt",
        "x b/y",
        " leading space.txt",
        "trailing space ",
    ] {
        let test = TestRepo::new();
        let base: String = (1..=30).map(|line| format!("line {line}\n")).collect();
        test.write(name, &base);
        test.commit_all("base");
        let changed = base
            .replace("line 2\n", "line two\n")
            .replace("line 29\n", "line twenty-nine\n");
        test.write(name, &changed);

        let diff = unstaged(&test, name);
        assert_eq!(diff.hunks.len(), 2, "{name:?}");
        test.repo.apply_hunk(&diff, 1, HunkAction::Stage).unwrap();
        assert_eq!(
            test.repo.index_text(name).unwrap().unwrap(),
            base.replace("line 29\n", "line twenty-nine\n"),
            "{name:?}"
        );
        test.repo.apply_hunk(&diff, 0, HunkAction::Discard).unwrap();
        assert_eq!(
            test.read(name),
            base.replace("line 29\n", "line twenty-nine\n"),
            "{name:?}"
        );
        let diff = staged(&test, name);
        test.repo.apply_hunk(&diff, 0, HunkAction::Unstage).unwrap();
        assert_eq!(
            test.repo.index_text(name).unwrap().unwrap(),
            base,
            "{name:?}"
        );
    }
}

#[test]
fn unstaging_the_edit_of_a_renamed_file_keeps_the_rename() {
    let test = TestRepo::new();
    let base: String = (1..=30).map(|line| format!("line {line}\n")).collect();
    test.write("old name.txt", &base);
    test.commit_all("base");
    test.git(&["mv", "old name.txt", "new name.txt"]);
    test.write("new name.txt", base.replace("line 15\n", "line fifteen\n"));
    test.repo.stage_all().unwrap();

    let diff = staged(&test, "new name.txt");
    assert_eq!(diff.status, DiffStatus::Renamed);
    assert_eq!(diff.hunks.len(), 1);
    test.repo.apply_hunk(&diff, 0, HunkAction::Unstage).unwrap();

    // The index now holds the old content under the new name: a pure rename.
    assert_eq!(index_content(&test, "new name.txt"), base);
    let diff = staged(&test, "new name.txt");
    assert_eq!(diff.status, DiffStatus::Renamed);
    assert!(diff.hunks.is_empty());
    // The edit is still in the work tree, now unstaged.
    assert_eq!(unstaged(&test, "new name.txt").hunks.len(), 1);
}

#[test]
fn conflicted_files_are_refused() {
    let test = TestRepo::new();
    test.write("conflict.txt", "one\ntwo\nthree\n");
    test.commit_all("base");
    test.git(&["checkout", "--quiet", "-b", "other"]);
    test.write("conflict.txt", "one\nTWO other\nthree\n");
    test.commit_all("other");
    test.git(&["checkout", "--quiet", "main"]);
    test.write("conflict.txt", "one\nTWO main\nthree\n");
    test.commit_all("main");
    assert!(test.try_git(&["merge", "other"]).is_err());

    let diff = unstaged(&test, "conflict.txt");
    assert_eq!(diff.status, DiffStatus::Conflicted);
    assert_eq!(diff.hunks.len(), 1);
    for action in [HunkAction::Stage, HunkAction::Unstage, HunkAction::Discard] {
        match test.repo.apply_hunk(&diff, 0, action) {
            Err(GitError::InvalidHunk(message)) => {
                assert!(message.contains("conflicted"), "{message}")
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(
        test.repo
            .status()
            .unwrap()
            .entry("conflict.txt")
            .unwrap()
            .conflicted
    );
}

#[test]
fn whitespace_settings_do_not_alter_what_is_staged() {
    let test = TestRepo::new();
    let base: String = (1..=10).map(|line| format!("line {line}\n")).collect();
    test.write("ws.txt", &base);
    test.commit_all("base");
    // A setting that would make `git apply` strip the trailing blanks.
    test.git(&["config", "apply.whitespace", "fix"]);
    let changed = base.replace("line 5\n", "line 5 with trailing blanks   \n\t\n");
    test.write("ws.txt", &changed);

    let diff = unstaged(&test, "ws.txt");
    test.repo.apply_hunk(&diff, 0, HunkAction::Stage).unwrap();
    assert_eq!(index_content(&test, "ws.txt"), changed);
}

#[test]
fn files_checked_out_with_crlf_line_endings() {
    let test = TestRepo::new();
    test.write(".gitattributes", "*.txt text eol=crlf\n");
    let base: String = (1..=30).map(|line| format!("line {line}\r\n")).collect();
    test.write("win.txt", &base);
    test.commit_all("base");
    // Stored with line feeds only.
    assert!(!test.git(&["show", "HEAD:win.txt"]).contains('\r'));

    let changed = base
        .replace("line 2\r\n", "line two\r\n")
        .replace("line 29\r\n", "line twenty-nine\r\n");
    test.write("win.txt", &changed);
    let diff = unstaged(&test, "win.txt");
    assert_eq!(diff.hunks.len(), 2);

    test.repo.apply_hunk(&diff, 0, HunkAction::Stage).unwrap();
    assert_eq!(
        test.repo.index_text("win.txt").unwrap().unwrap(),
        base.replace("line 2\r\n", "line two\r\n")
    );
    test.repo.apply_hunk(&diff, 1, HunkAction::Discard).unwrap();
    assert_eq!(
        test.read("win.txt"),
        base.replace("line 2\r\n", "line two\r\n")
    );
}
