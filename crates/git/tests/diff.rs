//! `diff_files` and `diff_file` against real repositories.

mod common;

use std::path::Path;

use bakedit_git::{
    DiffLineKind, DiffStatus, DiffTarget, FileDiff, Hunk, MAX_UNTRACKED_FILE_SIZE,
    MAX_UNTRACKED_TOTAL_SIZE,
};
use common::TestRepo;

fn file<'a>(files: &'a [FileDiff], path: &str) -> &'a FileDiff {
    files
        .iter()
        .find(|file| file.path == Path::new(path))
        .unwrap_or_else(|| panic!("no diff for {path:?} in {:#?}", summary(files)))
}

/// `path: status` for every file, for comparing and for failure messages.
fn summary(files: &[FileDiff]) -> Vec<String> {
    files
        .iter()
        .map(|file| format!("{}: {:?}", file.path.display(), file.status))
        .collect()
}

/// A hunk as the text a patch would show, one line per line.
fn text(hunk: &Hunk) -> String {
    let mut text = String::new();
    for line in &hunk.lines {
        text.push(match line.kind {
            DiffLineKind::Context => ' ',
            DiffLineKind::Added => '+',
            DiffLineKind::Removed => '-',
        });
        text.push_str(&line.text);
        text.push('\n');
    }
    text
}

fn numbered(count: usize) -> String {
    (1..=count).map(|line| format!("line {line}\n")).collect()
}

/// A repository with every kind of change at once.
fn busy() -> TestRepo {
    let test = TestRepo::new();
    test.write("staged.txt", "one\ntwo\nthree\n");
    test.write("unstaged.txt", "one\ntwo\nthree\n");
    test.write("both.txt", "one\ntwo\nthree\n");
    test.write("deleted.txt", "going\n");
    test.write("moved.txt", "line 1\nline 2\nline 3\nline 4\nline 5\n");
    test.write("untouched.txt", "same\n");
    test.commit_all("base");

    test.write("staged.txt", "one\nTWO\nthree\n");
    test.git(&["add", "staged.txt"]);
    test.write("unstaged.txt", "one\ntwo\nthree\nfour\n");
    test.write("both.txt", "ONE\ntwo\nthree\n");
    test.git(&["add", "both.txt"]);
    test.write("both.txt", "ONE\ntwo\nTHREE\n");
    test.remove("deleted.txt");
    test.git(&["mv", "moved.txt", "moved away.txt"]);
    test.write(
        "moved away.txt",
        "line 1\nline 2\nline 3 changed\nline 4\nline 5\n",
    );
    test.git(&["add", "moved away.txt"]);
    test.write("added.txt", "brand new\n");
    test.git(&["add", "added.txt"]);
    test.write("untracked.txt", "not known yet\nsecond line\n");
    test
}

#[test]
fn staged_changes_are_head_to_index() {
    let test = busy();
    let files = test.repo.diff_files(DiffTarget::HeadToIndex).unwrap();
    assert_eq!(
        summary(&files),
        [
            "added.txt: Added",
            "both.txt: Modified",
            "moved away.txt: Renamed",
            "staged.txt: Modified",
        ]
    );
    assert_eq!(
        text(&file(&files, "both.txt").hunks[0]),
        "-one\n+ONE\n two\n three\n"
    );
    assert_eq!(
        text(&file(&files, "staged.txt").hunks[0]),
        " one\n-two\n+TWO\n three\n"
    );
    assert_eq!(text(&file(&files, "added.txt").hunks[0]), "+brand new\n");

    let moved = file(&files, "moved away.txt");
    assert_eq!(moved.old_path.as_deref(), Some(Path::new("moved.txt")));
    assert_eq!(
        text(&moved.hunks[0]),
        " line 1\n line 2\n-line 3\n+line 3 changed\n line 4\n line 5\n"
    );
    assert_eq!((moved.additions(), moved.deletions()), (1, 1));
}

#[test]
fn unstaged_changes_are_index_to_worktree() {
    let test = busy();
    let files = test.repo.diff_files(DiffTarget::IndexToWorktree).unwrap();
    assert_eq!(
        summary(&files),
        [
            "both.txt: Modified",
            "deleted.txt: Deleted",
            "unstaged.txt: Modified",
        ]
    );
    // Only the part that is not staged yet.
    assert_eq!(
        text(&file(&files, "both.txt").hunks[0]),
        " ONE\n two\n-three\n+THREE\n"
    );
    assert_eq!(text(&file(&files, "deleted.txt").hunks[0]), "-going\n");
    assert_eq!(
        text(&file(&files, "unstaged.txt").hunks[0]),
        " one\n two\n three\n+four\n"
    );
}

#[test]
fn everything_since_the_last_commit_is_head_to_worktree_with_untracked_files_last() {
    let test = busy();
    let files = test.repo.diff_files(DiffTarget::HeadToWorktree).unwrap();
    assert_eq!(
        summary(&files),
        [
            "added.txt: Added",
            "both.txt: Modified",
            "deleted.txt: Deleted",
            "moved away.txt: Renamed",
            "staged.txt: Modified",
            "unstaged.txt: Modified",
            "untracked.txt: Untracked",
        ]
    );
    // Staged and unstaged parts together.
    assert_eq!(
        text(&file(&files, "both.txt").hunks[0]),
        "-one\n+ONE\n two\n-three\n+THREE\n"
    );

    let untracked = file(&files, "untracked.txt");
    assert!(!untracked.binary && !untracked.too_large);
    assert_eq!(untracked.new_mode, Some(0o100644));
    assert_eq!(untracked.hunks.len(), 1);
    let hunk = &untracked.hunks[0];
    assert_eq!(
        (
            hunk.old_start,
            hunk.old_lines,
            hunk.new_start,
            hunk.new_lines
        ),
        (0, 0, 1, 2)
    );
    assert_eq!(text(hunk), "+not known yet\n+second line\n");
    assert_eq!(hunk.lines[1].new_no, Some(2));
    assert_eq!(hunk.lines[1].old_no, None);
    assert!(!hunk.lines[1].no_newline_at_end);
}

#[test]
fn a_clean_repository_has_no_diffs() {
    let test = TestRepo::with_commit();
    for target in [
        DiffTarget::HeadToWorktree,
        DiffTarget::IndexToWorktree,
        DiffTarget::HeadToIndex,
    ] {
        assert!(test.repo.diff_files(target).unwrap().is_empty());
        assert_eq!(test.repo.diff_file("README.md", target).unwrap(), None);
    }
}

#[test]
fn hunks_carry_line_numbers_and_headings() {
    let test = TestRepo::new();
    let body: String = (1..=20).map(|line| format!("    line {line};\n")).collect();
    let source = format!("fn first() {{\n{body}}}\n\nfn second() {{\n{body}}}\n");
    test.write("code.rs", &source);
    test.commit_all("code");

    // Change line 3 of the first function, insert after line 1 of the second and
    // remove its line 18.
    let second = source.find("fn second").unwrap();
    let (first_half, second_half) = source.split_at(second);
    let changed = format!(
        "{}{}",
        first_half.replace("    line 3;\n", "    line three;\n"),
        second_half
            .replace("    line 1;\n", "    line 1;\n    inserted;\n")
            .replace("    line 18;\n", "")
    );
    test.write("code.rs", &changed);

    let diff = test
        .repo
        .diff_file("code.rs", DiffTarget::IndexToWorktree)
        .unwrap()
        .unwrap();
    assert_eq!(diff.hunks.len(), 3);
    let ranges: Vec<(u32, u32, u32, u32)> = diff
        .hunks
        .iter()
        .map(|hunk| {
            (
                hunk.old_start,
                hunk.old_lines,
                hunk.new_start,
                hunk.new_lines,
            )
        })
        .collect();
    assert_eq!(ranges, [(1, 7, 1, 7), (23, 6, 23, 7), (39, 7, 40, 6)]);
    assert_eq!(diff.hunks[0].header, "");
    assert_eq!(diff.hunks[1].header, "fn first() {");
    assert_eq!(diff.hunks[2].header, "fn second() {");

    let first = &diff.hunks[0];
    assert_eq!(
        text(first),
        concat!(
            " fn first() {\n",
            "     line 1;\n",
            "     line 2;\n",
            "-    line 3;\n",
            "+    line three;\n",
            "     line 4;\n",
            "     line 5;\n",
            "     line 6;\n",
        )
    );
    let removed = &first.lines[3];
    assert_eq!((removed.old_no, removed.new_no), (Some(4), None));
    let added = &first.lines[4];
    assert_eq!((added.old_no, added.new_no), (None, Some(4)));

    let last = &diff.hunks[2];
    let removed = last
        .lines
        .iter()
        .find(|line| line.kind == DiffLineKind::Removed)
        .unwrap();
    assert_eq!(removed.text, "    line 18;");
    assert_eq!((removed.old_no, removed.new_no), (Some(42), None));
    // Before the removal the new side is one line ahead (the insertion above); after
    // it both sides agree again.
    let before = last.lines.first().unwrap();
    assert_eq!((before.old_no, before.new_no), (Some(39), Some(40)));
    let after = last.lines.last().unwrap();
    assert_eq!((after.old_no, after.new_no), (Some(45), Some(45)));
}

#[test]
fn unusual_file_names_come_through_git_intact() {
    let test = TestRepo::new();
    let names = [
        "dir with space/\u{fc} \u{f1}.txt",
        "ta\tb.txt",
        "new\nline.txt",
        "quo\"te.txt",
        "back\\slash.txt",
        "\u{65e5}\u{672c}\u{8a9e}.txt",
        "a b/c d.txt",
        "x b/y",
    ];
    // Every file gets content of its own, so that git cannot mistake one for another.
    for (number, name) in names.iter().enumerate() {
        test.write(name, format!("old {number}\n"));
    }
    test.commit_all("odd names");
    for (number, name) in names.iter().enumerate() {
        test.write(name, format!("new {number}\n"));
    }

    let files = test.repo.diff_files(DiffTarget::IndexToWorktree).unwrap();
    assert_eq!(files.len(), names.len());
    for (number, name) in names.iter().enumerate() {
        let diff = file(&files, name);
        assert_eq!(diff.status, DiffStatus::Modified, "{name:?}");
        assert_eq!(
            text(&diff.hunks[0]),
            format!("-old {number}\n+new {number}\n"),
            "{name:?}"
        );
        let single = test
            .repo
            .diff_file(name, DiffTarget::IndexToWorktree)
            .unwrap()
            .unwrap();
        assert_eq!(&single, diff, "{name:?}");
    }

    // The same names untracked, renamed and deleted.
    test.commit_all("new content");
    test.git(&["mv", "ta\tb.txt", "moved \"here\".txt"]);
    test.git(&["rm", "--quiet", "--force", "new\nline.txt"]);
    test.write("un tracked/\u{e9}\t.txt", "fresh\n");
    let files = test.repo.diff_files(DiffTarget::HeadToWorktree).unwrap();
    let moved = file(&files, "moved \"here\".txt");
    assert_eq!(moved.status, DiffStatus::Renamed);
    assert_eq!(moved.old_path.as_deref(), Some(Path::new("ta\tb.txt")));
    assert!(moved.hunks.is_empty());
    assert_eq!(file(&files, "new\nline.txt").status, DiffStatus::Deleted);
    let untracked = file(&files, "un tracked/\u{e9}\t.txt");
    assert_eq!(untracked.status, DiffStatus::Untracked);
    assert_eq!(text(&untracked.hunks[0]), "+fresh\n");
    assert_eq!(files.len(), 3);
}

#[test]
fn untracked_files_of_every_kind() {
    let test = TestRepo::with_commit();
    test.write(".gitignore", "ignored/\n*.log\n");
    test.write("text.txt", "first\nsecond");
    test.write("empty.txt", "");
    test.write("binary.bin", b"\x00\x01\x02 binary \xff");
    test.write("nul-far-in.dat", [vec![b'a'; 9000], vec![0]].concat());
    test.write("latin1.txt", b"caf\xe9\n");
    test.write("dir/deep/nested.txt", "nested\n");
    test.write("script.sh", "#!/bin/sh\n");
    test.make_executable("script.sh");
    test.write("ignored/file.txt", "ignored\n");
    test.write("debug.log", "ignored\n");
    std::os::unix::fs::symlink("text.txt", test.path("link")).unwrap();
    // A repository inside the work tree is listed by git as a directory, not a file.
    common::init_repository(&test.path("nested-repo"));
    std::fs::write(test.path("nested-repo/inner.txt"), "inner\n").unwrap();

    let files = test.repo.diff_files(DiffTarget::HeadToWorktree).unwrap();
    assert_eq!(
        summary(&files),
        [
            ".gitignore: Untracked",
            "binary.bin: Untracked",
            "dir/deep/nested.txt: Untracked",
            "empty.txt: Untracked",
            "latin1.txt: Untracked",
            "link: Untracked",
            "nul-far-in.dat: Untracked",
            "script.sh: Untracked",
            "text.txt: Untracked",
        ]
    );

    let plain = file(&files, "text.txt");
    assert_eq!(text(&plain.hunks[0]), "+first\n+second\n");
    assert!(!plain.hunks[0].lines[0].no_newline_at_end);
    assert!(plain.hunks[0].lines[1].no_newline_at_end);

    let empty = file(&files, "empty.txt");
    assert!(empty.hunks.is_empty() && !empty.binary && !empty.too_large);

    let binary = file(&files, "binary.bin");
    assert!(binary.binary && binary.hunks.is_empty() && !binary.too_large);
    // Like git, only the start of a file is looked at.
    let late_nul = file(&files, "nul-far-in.dat");
    assert!(!late_nul.binary && late_nul.hunks.len() == 1);

    let latin1 = file(&files, "latin1.txt");
    assert_eq!(latin1.hunks[0].lines[0].text, "caf\u{fffd}");
    assert_eq!(latin1.hunks[0].lines[0].bytes(), b"caf\xe9");

    assert_eq!(file(&files, "script.sh").new_mode, Some(0o100755));
    assert_eq!(file(&files, "text.txt").new_mode, Some(0o100644));

    let link = file(&files, "link");
    assert_eq!(link.new_mode, Some(0o120000));
    assert_eq!(text(&link.hunks[0]), "+text.txt\n");
    assert!(link.hunks[0].lines[0].no_newline_at_end);

    // The other two targets never list untracked files.
    assert!(test
        .repo
        .diff_files(DiffTarget::IndexToWorktree)
        .unwrap()
        .is_empty());
    assert!(test
        .repo
        .diff_files(DiffTarget::HeadToIndex)
        .unwrap()
        .is_empty());
    assert_eq!(
        test.repo
            .diff_file("text.txt", DiffTarget::IndexToWorktree)
            .unwrap(),
        None
    );
}

#[test]
fn an_untracked_file_that_cannot_be_read_is_listed_without_content() {
    use std::os::unix::fs::PermissionsExt;
    let test = TestRepo::with_commit();
    test.write("locked.txt", "nobody may read this\n");
    test.write("open.txt", "anybody may\n");
    let locked = test.path("locked.txt");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    // An administrator reads everything; nothing to test then.
    if std::fs::read(&locked).is_ok() {
        return;
    }

    let files = test.repo.diff_files(DiffTarget::HeadToWorktree).unwrap();
    assert_eq!(
        summary(&files),
        ["locked.txt: Untracked", "open.txt: Untracked"]
    );
    let unreadable = file(&files, "locked.txt");
    assert!(unreadable.hunks.is_empty() && !unreadable.binary && !unreadable.too_large);
    assert_eq!(file(&files, "open.txt").hunks.len(), 1);
}

#[test]
fn large_untracked_files_are_listed_without_content() {
    let test = TestRepo::with_commit();
    let limit = usize::try_from(MAX_UNTRACKED_FILE_SIZE).unwrap();
    test.write("at-the-limit.txt", vec![b'x'; limit]);
    test.write("over-the-limit.txt", vec![b'x'; limit + 1]);

    let files = test.repo.diff_files(DiffTarget::HeadToWorktree).unwrap();
    let at_limit = file(&files, "at-the-limit.txt");
    assert!(!at_limit.too_large);
    assert_eq!(at_limit.hunks[0].lines[0].text.len(), limit);
    let over = file(&files, "over-the-limit.txt");
    assert!(over.too_large && over.hunks.is_empty() && !over.binary);
    assert_eq!(over.status, DiffStatus::Untracked);
    assert_eq!(over.new_mode, Some(0o100644));
}

#[test]
fn untracked_content_is_capped_in_total() {
    let test = TestRepo::with_commit();
    let size = 900_000;
    let fit = usize::try_from(MAX_UNTRACKED_TOTAL_SIZE).unwrap() / size;
    for number in 0..fit + 2 {
        // One long line each: plenty of bytes, hardly any lines.
        test.write(&format!("bulk/{number:02}.txt"), vec![b'x'; size]);
    }
    let files = test.repo.diff_files(DiffTarget::HeadToWorktree).unwrap();
    assert_eq!(files.len(), fit + 2);
    let with_content = files.iter().filter(|file| !file.too_large).count();
    assert_eq!(with_content, fit);
    assert!(files[..fit].iter().all(|file| file.hunks.len() == 1));
    assert!(files[fit..]
        .iter()
        .all(|file| file.too_large && file.hunks.is_empty()));
}

#[test]
fn before_the_first_commit_everything_is_added() {
    let test = TestRepo::new();
    test.write("staged.txt", "staged\n");
    test.repo.stage(&["staged.txt"]).unwrap();
    test.write("staged.txt", "staged\nand edited\n");
    test.write("untracked.txt", "untracked\n");

    let staged = test.repo.diff_files(DiffTarget::HeadToIndex).unwrap();
    assert_eq!(summary(&staged), ["staged.txt: Added"]);
    assert_eq!(text(&staged[0].hunks[0]), "+staged\n");

    let unstaged = test.repo.diff_files(DiffTarget::IndexToWorktree).unwrap();
    assert_eq!(summary(&unstaged), ["staged.txt: Modified"]);
    assert_eq!(text(&unstaged[0].hunks[0]), " staged\n+and edited\n");

    let all = test.repo.diff_files(DiffTarget::HeadToWorktree).unwrap();
    assert_eq!(
        summary(&all),
        ["staged.txt: Added", "untracked.txt: Untracked"]
    );
    assert_eq!(text(&all[0].hunks[0]), "+staged\n+and edited\n");

    let single = test
        .repo
        .diff_file("staged.txt", DiffTarget::HeadToWorktree)
        .unwrap()
        .unwrap();
    assert_eq!(single, all[0]);
}

#[test]
fn binary_mode_and_newline_details_of_tracked_files() {
    let test = TestRepo::new();
    test.write("image.bin", b"\x00\x01\x02 old");
    test.write("tool.sh", "#!/bin/sh\n");
    test.write("no-newline.txt", "first\nlast");
    test.write("gains-newline.txt", "first\nlast");
    test.write("crlf.txt", "one\r\ntwo\r\n");
    test.commit_all("base");
    test.write("image.bin", b"\x00\x01\x02 new");
    test.make_executable("tool.sh");
    test.write("no-newline.txt", "first\nlast changed");
    test.write("gains-newline.txt", "first\nlast\n");
    test.write("crlf.txt", "one\r\nTWO\r\n");

    let files = test.repo.diff_files(DiffTarget::IndexToWorktree).unwrap();
    assert_eq!(files.len(), 5);

    let image = file(&files, "image.bin");
    assert!(image.binary && image.hunks.is_empty());
    assert_eq!(image.status, DiffStatus::Modified);

    let tool = file(&files, "tool.sh");
    assert_eq!(
        (tool.old_mode, tool.new_mode),
        (Some(0o100644), Some(0o100755))
    );
    assert!(tool.hunks.is_empty() && !tool.binary);

    let flags = |path: &str| -> Vec<(DiffLineKind, bool)> {
        file(&files, path).hunks[0]
            .lines
            .iter()
            .map(|line| (line.kind, line.no_newline_at_end))
            .collect()
    };
    use DiffLineKind::{Added, Context, Removed};
    assert_eq!(
        flags("no-newline.txt"),
        [(Context, false), (Removed, true), (Added, true)]
    );
    assert_eq!(
        flags("gains-newline.txt"),
        [(Context, false), (Removed, true), (Added, false)]
    );

    // The carriage returns are part of the lines.
    assert_eq!(
        text(&file(&files, "crlf.txt").hunks[0]),
        " one\r\n-two\r\n+TWO\r\n"
    );
}

#[test]
fn a_renamed_file_is_compared_with_where_it_came_from() {
    let test = TestRepo::new();
    test.write("old/name.rs", numbered(30));
    test.commit_all("base");
    std::fs::create_dir(test.path("new dir")).unwrap();
    test.git(&["mv", "old/name.rs", "new dir/new name.rs"]);
    test.write(
        "new dir/new name.rs",
        numbered(30).replace("line 15\n", "line fifteen\n"),
    );
    test.git(&["add", "--all"]);

    for target in [DiffTarget::HeadToIndex, DiffTarget::HeadToWorktree] {
        let diff = test
            .repo
            .diff_file("new dir/new name.rs", target)
            .unwrap()
            .unwrap();
        assert_eq!(diff.status, DiffStatus::Renamed, "{target:?}");
        assert_eq!(diff.old_path.as_deref(), Some(Path::new("old/name.rs")));
        // One changed line, not thirty added ones.
        assert_eq!((diff.additions(), diff.deletions()), (1, 1), "{target:?}");

        // Asked about its old path alone, the file is gone.
        let old = test.repo.diff_file("old/name.rs", target).unwrap().unwrap();
        assert_eq!(old.status, DiffStatus::Deleted);
        assert_eq!(old.deletions(), 30);
    }
    // The rename is fully staged: nothing differs between index and work tree.
    assert_eq!(
        test.repo
            .diff_file("new dir/new name.rs", DiffTarget::IndexToWorktree)
            .unwrap(),
        None
    );

    // A file that really is new stays new.
    test.write("truly new.rs", "fresh\n");
    test.git(&["add", "--all"]);
    let fresh = test
        .repo
        .diff_file("truly new.rs", DiffTarget::HeadToIndex)
        .unwrap()
        .unwrap();
    assert_eq!(fresh.status, DiffStatus::Added);
    assert_eq!(fresh.old_path, None);
}

#[test]
fn diff_file_takes_absolute_paths_and_finds_untracked_and_deleted_files() {
    let test = busy();
    let by_absolute_path = test
        .repo
        .diff_file(test.path("unstaged.txt"), DiffTarget::IndexToWorktree)
        .unwrap()
        .unwrap();
    assert_eq!(by_absolute_path.path, Path::new("unstaged.txt"));
    assert_eq!(by_absolute_path.additions(), 1);

    let untracked = test
        .repo
        .diff_file("untracked.txt", DiffTarget::HeadToWorktree)
        .unwrap()
        .unwrap();
    assert_eq!(untracked.status, DiffStatus::Untracked);
    assert_eq!(untracked.additions(), 2);

    let deleted = test
        .repo
        .diff_file(test.path("deleted.txt"), DiffTarget::IndexToWorktree)
        .unwrap()
        .unwrap();
    assert_eq!(deleted.status, DiffStatus::Deleted);

    assert_eq!(
        test.repo
            .diff_file("untouched.txt", DiffTarget::HeadToWorktree)
            .unwrap(),
        None
    );
    assert_eq!(
        test.repo
            .diff_file("no-such-file.txt", DiffTarget::HeadToWorktree)
            .unwrap(),
        None
    );
    // The staged file has no unstaged changes.
    assert_eq!(
        test.repo
            .diff_file("staged.txt", DiffTarget::IndexToWorktree)
            .unwrap(),
        None
    );
}

#[test]
fn a_file_whose_type_changed_appears_as_deleted_and_added() {
    let test = TestRepo::with_commit();
    test.write("target.txt", "target\n");
    test.write("was-a-file", "regular content\n");
    test.commit_all("base");
    test.remove("was-a-file");
    std::os::unix::fs::symlink("target.txt", test.path("was-a-file")).unwrap();

    let files = test.repo.diff_files(DiffTarget::IndexToWorktree).unwrap();
    assert_eq!(
        summary(&files),
        ["was-a-file: Deleted", "was-a-file: Added"]
    );
    assert_eq!(files[0].old_mode, Some(0o100644));
    assert_eq!(files[1].new_mode, Some(0o120000));

    let single = test
        .repo
        .diff_file("was-a-file", DiffTarget::IndexToWorktree)
        .unwrap()
        .unwrap();
    assert_eq!(single.status, DiffStatus::Added);
    assert_eq!(text(&single.hunks[0]), "+target.txt\n");
}

#[test]
fn conflicted_files_in_each_target() {
    let test = TestRepo::new();
    test.write("conflict.txt", "one\ntwo\nthree\n");
    test.write("deleted-by-us.txt", "keep\n");
    test.commit_all("base");
    test.git(&["checkout", "--quiet", "-b", "other"]);
    test.write("conflict.txt", "one\nTWO other\nthree\n");
    test.write("deleted-by-us.txt", "changed by them\n");
    test.write("clean.txt", "merged cleanly\n");
    test.commit_all("other");
    test.git(&["checkout", "--quiet", "main"]);
    test.write("conflict.txt", "one\nTWO main\nthree\n");
    test.git(&["rm", "--quiet", "deleted-by-us.txt"]);
    test.commit_all("main");
    assert!(test.try_git(&["merge", "other"]).is_err());

    // Not staged: the conflicted file against our side, markers and all.
    let unstaged = test.repo.diff_files(DiffTarget::IndexToWorktree).unwrap();
    assert_eq!(
        summary(&unstaged),
        ["conflict.txt: Conflicted", "deleted-by-us.txt: Conflicted"]
    );
    assert_eq!(
        text(&file(&unstaged, "conflict.txt").hunks[0]),
        " one\n+<<<<<<< HEAD\n TWO main\n+=======\n+TWO other\n+>>>>>>> other\n three\n"
    );
    assert!(file(&unstaged, "deleted-by-us.txt").hunks.is_empty());

    // Staged: what merged cleanly, and the conflicts as such.
    let staged = test.repo.diff_files(DiffTarget::HeadToIndex).unwrap();
    assert_eq!(
        summary(&staged),
        [
            "clean.txt: Added",
            "conflict.txt: Conflicted",
            "deleted-by-us.txt: Conflicted",
        ]
    );
    assert!(file(&staged, "conflict.txt").hunks.is_empty());

    // Against the last commit a conflicted file is an ordinary changed file.
    let all = test.repo.diff_files(DiffTarget::HeadToWorktree).unwrap();
    assert_eq!(file(&all, "conflict.txt").status, DiffStatus::Modified);
    assert_eq!(file(&all, "conflict.txt").additions(), 4);

    let single = test
        .repo
        .diff_file("conflict.txt", DiffTarget::IndexToWorktree)
        .unwrap()
        .unwrap();
    assert_eq!(single.status, DiffStatus::Conflicted);
    assert_eq!(single.hunks.len(), 1);
}

#[test]
fn the_users_diff_configuration_does_not_break_parsing() {
    let test = busy();
    let expected: Vec<_> = [
        DiffTarget::HeadToWorktree,
        DiffTarget::IndexToWorktree,
        DiffTarget::HeadToIndex,
    ]
    .into_iter()
    .map(|target| test.repo.diff_files(target).unwrap())
    .collect();

    for (key, value) in [
        ("diff.noprefix", "true"),
        ("diff.mnemonicPrefix", "true"),
        ("diff.srcPrefix", "SRC/"),
        ("diff.dstPrefix", "DST/"),
        ("diff.relative", "true"),
        ("diff.external", "false"),
        ("diff.context", "0"),
        ("diff.interHunkContext", "50"),
        ("diff.suppressBlankEmpty", "true"),
        ("diff.submodule", "log"),
        ("diff.renames", "false"),
        ("diff.algorithm", "histogram"),
        ("diff.colorMoved", "zebra"),
        ("color.ui", "always"),
        ("color.diff", "always"),
        ("core.quotepath", "true"),
        ("core.pager", "false"),
    ] {
        test.git(&["config", key, value]);
    }
    let with_config: Vec<_> = [
        DiffTarget::HeadToWorktree,
        DiffTarget::IndexToWorktree,
        DiffTarget::HeadToIndex,
    ]
    .into_iter()
    .map(|target| test.repo.diff_files(target).unwrap())
    .collect();
    assert_eq!(with_config, expected);
    // Status still parses. It follows the user's rename setting, so with renames
    // switched off the moved file shows up as a deletion and an addition.
    let status = test.repo.status().unwrap();
    assert_eq!(status.entries.len(), 8);
    assert!(status.entries.iter().all(|entry| entry.orig_path.is_none()));
    test.git(&["config", "diff.renames", "true"]);
    assert_eq!(test.repo.status().unwrap().entries.len(), 7);
}

#[test]
fn text_conversion_filters_are_not_applied() {
    let test = TestRepo::new();
    test.write(".gitattributes", "*.data diff=upper\n");
    test.write("file.data", "content\n");
    test.commit_all("base");
    test.git(&["config", "diff.upper.textconv", "tr a-z A-Z <"]);
    test.write("file.data", "content changed\n");

    let diff = test
        .repo
        .diff_file("file.data", DiffTarget::IndexToWorktree)
        .unwrap()
        .unwrap();
    // The real bytes, so that the hunk can be applied.
    assert_eq!(text(&diff.hunks[0]), "-content\n+content changed\n");
}

#[test]
fn a_submodule_that_moved_is_a_one_line_change() {
    let test = TestRepo::with_commit();
    let library = test.sandbox().join("library");
    common::init_repository(&library);
    std::fs::write(library.join("lib.txt"), "one\n").unwrap();
    common::git_in(&library, &["add", "--all"]);
    common::git_in(&library, &["commit", "--quiet", "--message", "one"]);
    test.git(&[
        "-c",
        "protocol.file.allow=always",
        "submodule",
        "--quiet",
        "add",
        library.to_str().unwrap(),
        "vendor/library",
    ]);
    test.commit_all("add the library");
    let inside = test.path("vendor/library");
    std::fs::write(inside.join("lib.txt"), "two\n").unwrap();
    common::git_in(&inside, &["add", "--all"]);
    common::git_in(
        &inside,
        &[
            "-c",
            "user.name=T",
            "-c",
            "user.email=t@example.com",
            "commit",
            "--quiet",
            "--message",
            "two",
        ],
    );

    let files = test.repo.diff_files(DiffTarget::IndexToWorktree).unwrap();
    assert_eq!(summary(&files), ["vendor/library: Modified"]);
    let hunk = &files[0].hunks[0];
    assert_eq!(hunk.lines.len(), 2);
    assert!(hunk.lines[0].text.starts_with("Subproject commit "));
    assert_eq!(files[0].new_mode, Some(0o160000));
}
