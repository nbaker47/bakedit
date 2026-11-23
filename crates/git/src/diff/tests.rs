//! The diff parser on its own, fed the text git really prints.

use super::*;

fn parse(text: &str) -> Vec<FileDiff> {
    parse_diff(text.as_bytes())
}

/// Parses text that holds exactly one file.
fn parse_one(text: &str) -> FileDiff {
    let mut files = parse(text);
    assert_eq!(files.len(), 1, "{files:#?}");
    files.remove(0)
}

/// A hunk's lines as `(marker, old number, new number, text)`.
fn rows(hunk: &Hunk) -> Vec<(char, Option<u32>, Option<u32>, &str)> {
    hunk.lines
        .iter()
        .map(|line| {
            let marker = match line.kind {
                DiffLineKind::Context => ' ',
                DiffLineKind::Added => '+',
                DiffLineKind::Removed => '-',
            };
            (marker, line.old_no, line.new_no, line.text.as_str())
        })
        .collect()
}

fn ranges(hunk: &Hunk) -> (u32, u32, u32, u32) {
    (
        hunk.old_start,
        hunk.old_lines,
        hunk.new_start,
        hunk.new_lines,
    )
}

#[test]
fn nothing_gives_no_files() {
    assert!(parse("").is_empty());
    assert!(parse("\n").is_empty());
    assert!(parse("warning: something unrelated\n").is_empty());
}

#[test]
fn a_modified_file_with_two_hunks() {
    let file = parse_one(concat!(
        "diff --git a/f.txt b/f.txt\n",
        "index b03757e..96e9f48 100644\n",
        "--- a/f.txt\n",
        "+++ b/f.txt\n",
        "@@ -1,5 +1,5 @@\n",
        " a\n",
        "-b\n",
        "+B\n",
        " c\n",
        " d\n",
        " e\n",
        "@@ -12,5 +12,6 @@ fn heading(arg: u8) {\n",
        " l\n",
        " m\n",
        " n\n",
        "-o\n",
        "+O\n",
        "+extra\n",
        " p\n",
    ));
    assert_eq!(file.path, Path::new("f.txt"));
    assert_eq!(file.old_path, None);
    assert_eq!(file.status, DiffStatus::Modified);
    assert!(!file.binary && !file.too_large);
    assert_eq!(
        (file.old_mode, file.new_mode),
        (Some(0o100644), Some(0o100644))
    );
    assert_eq!(file.hunks.len(), 2);
    assert_eq!((file.additions(), file.deletions()), (3, 2));

    let first = &file.hunks[0];
    assert_eq!(ranges(first), (1, 5, 1, 5));
    assert_eq!(first.header, "");
    assert_eq!(
        rows(first),
        [
            (' ', Some(1), Some(1), "a"),
            ('-', Some(2), None, "b"),
            ('+', None, Some(2), "B"),
            (' ', Some(3), Some(3), "c"),
            (' ', Some(4), Some(4), "d"),
            (' ', Some(5), Some(5), "e"),
        ]
    );

    let second = &file.hunks[1];
    assert_eq!(ranges(second), (12, 5, 12, 6));
    assert_eq!(second.header, "fn heading(arg: u8) {");
    assert_eq!(
        rows(second),
        [
            (' ', Some(12), Some(12), "l"),
            (' ', Some(13), Some(13), "m"),
            (' ', Some(14), Some(14), "n"),
            ('-', Some(15), None, "o"),
            ('+', None, Some(15), "O"),
            ('+', None, Some(16), "extra"),
            (' ', Some(16), Some(17), "p"),
        ]
    );
    assert!(file
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .all(|line| !line.no_newline_at_end));
}

#[test]
fn a_new_file() {
    let file = parse_one(concat!(
        "diff --git a/added.txt b/added.txt\n",
        "new file mode 100755\n",
        "index 0000000..d5a09df\n",
        "--- /dev/null\n",
        "+++ b/added.txt\n",
        "@@ -0,0 +1,2 @@\n",
        "+brand new\n",
        "+second line\n",
    ));
    assert_eq!(file.path, Path::new("added.txt"));
    assert_eq!(file.status, DiffStatus::Added);
    assert_eq!((file.old_mode, file.new_mode), (None, Some(0o100755)));
    assert_eq!(ranges(&file.hunks[0]), (0, 0, 1, 2));
    assert_eq!(
        rows(&file.hunks[0]),
        [
            ('+', None, Some(1), "brand new"),
            ('+', None, Some(2), "second line"),
        ]
    );
}

#[test]
fn a_deleted_file() {
    let file = parse_one(concat!(
        "diff --git a/deleted.txt b/deleted.txt\n",
        "deleted file mode 100644\n",
        "index 286c5f5..0000000\n",
        "--- a/deleted.txt\n",
        "+++ /dev/null\n",
        "@@ -1 +0,0 @@\n",
        "-gone\n",
    ));
    assert_eq!(file.path, Path::new("deleted.txt"));
    assert_eq!(file.status, DiffStatus::Deleted);
    assert_eq!((file.old_mode, file.new_mode), (Some(0o100644), None));
    // A range without a length is one line long.
    assert_eq!(ranges(&file.hunks[0]), (1, 1, 0, 0));
    assert_eq!(rows(&file.hunks[0]), [('-', Some(1), None, "gone")]);
}

#[test]
fn a_rename_with_changes() {
    let file = parse_one(concat!(
        "diff --git a/rename-me.txt b/renamed file.txt\n",
        "similarity index 72%\n",
        "rename from rename-me.txt\n",
        "rename to renamed file.txt\n",
        "index 3a31ef6..c8cd5d0 100644\n",
        "--- a/rename-me.txt\n",
        "+++ b/renamed file.txt\t\n",
        "@@ -1,3 +1,3 @@\n",
        " old content line 1\n",
        "-line 2\n",
        "+line 2 changed\n",
        " line 3\n",
    ));
    assert_eq!(file.path, Path::new("renamed file.txt"));
    assert_eq!(file.old_path.as_deref(), Some(Path::new("rename-me.txt")));
    assert_eq!(file.status, DiffStatus::Renamed);
    assert_eq!(file.hunks.len(), 1);
    assert_eq!((file.additions(), file.deletions()), (1, 1));
}

#[test]
fn a_pure_rename_has_no_hunks() {
    let file = parse_one(concat!(
        "diff --git a/old dir/a file.txt b/new dir/a file.txt\n",
        "similarity index 100%\n",
        "rename from old dir/a file.txt\n",
        "rename to new dir/a file.txt\n",
    ));
    assert_eq!(file.path, Path::new("new dir/a file.txt"));
    assert_eq!(
        file.old_path.as_deref(),
        Some(Path::new("old dir/a file.txt"))
    );
    assert_eq!(file.status, DiffStatus::Renamed);
    assert!(file.hunks.is_empty());
    assert_eq!((file.old_mode, file.new_mode), (None, None));
}

#[test]
fn a_copy() {
    let file = parse_one(concat!(
        "diff --git a/original.txt b/copy.txt\n",
        "similarity index 90%\n",
        "copy from original.txt\n",
        "copy to copy.txt\n",
        "index 3a31ef6..c8cd5d0 100644\n",
        "--- a/original.txt\n",
        "+++ b/copy.txt\n",
        "@@ -1 +1,2 @@\n",
        " shared\n",
        "+only in the copy\n",
    ));
    assert_eq!(file.status, DiffStatus::Copied);
    assert_eq!(file.path, Path::new("copy.txt"));
    assert_eq!(file.old_path.as_deref(), Some(Path::new("original.txt")));
}

#[test]
fn binary_files() {
    let files = parse(concat!(
        "diff --git a/bin.dat b/bin.dat\n",
        "index 0f49c4a..4c2de3c 100644\n",
        "Binary files a/bin.dat and b/bin.dat differ\n",
        "diff --git a/new image.png b/new image.png\n",
        "new file mode 100644\n",
        "index 0000000..4c2de3c\n",
        "Binary files /dev/null and b/new image.png differ\n",
        "diff --git a/gone.bin b/gone.bin\n",
        "deleted file mode 100644\n",
        "index 4c2de3c..0000000\n",
        "Binary files a/gone.bin and /dev/null differ\n",
    ));
    assert_eq!(files.len(), 3);
    assert!(files
        .iter()
        .all(|file| file.binary && file.hunks.is_empty()));
    assert_eq!(files[0].path, Path::new("bin.dat"));
    assert_eq!(files[0].status, DiffStatus::Modified);
    assert_eq!(files[1].path, Path::new("new image.png"));
    assert_eq!(files[1].status, DiffStatus::Added);
    assert_eq!(files[2].path, Path::new("gone.bin"));
    assert_eq!(files[2].status, DiffStatus::Deleted);
}

#[test]
fn a_binary_patch_is_binary_and_its_data_is_skipped() {
    let files = parse(concat!(
        "diff --git a/bin.dat b/bin.dat\n",
        "index 0f49c4a..4c2de3c 100644\n",
        "GIT binary patch\n",
        "literal 10\n",
        "RcmZQzWMXDvWn^b%00961\n",
        "\n",
        "literal 9\n",
        "QcmZQzWMXDvWn^b%0096100\n",
        "\n",
        "diff --git a/after.txt b/after.txt\n",
        "index 0f49c4a..4c2de3c 100644\n",
        "--- a/after.txt\n",
        "+++ b/after.txt\n",
        "@@ -1 +1 @@\n",
        "-a\n",
        "+b\n",
    ));
    assert_eq!(files.len(), 2);
    assert!(files[0].binary && files[0].hunks.is_empty());
    assert_eq!(files[1].path, Path::new("after.txt"));
    assert_eq!(files[1].hunks.len(), 1);
}

#[test]
fn a_mode_change_alone() {
    let file = parse_one(concat!(
        "diff --git a/script.sh b/script.sh\n",
        "old mode 100644\n",
        "new mode 100755\n",
    ));
    assert_eq!(file.path, Path::new("script.sh"));
    assert_eq!(file.status, DiffStatus::Modified);
    assert_eq!(
        (file.old_mode, file.new_mode),
        (Some(0o100644), Some(0o100755))
    );
    assert!(file.hunks.is_empty() && !file.binary);
}

#[test]
fn a_mode_change_with_content_keeps_both_modes() {
    let file = parse_one(concat!(
        "diff --git a/script.sh b/script.sh\n",
        "old mode 100644\n",
        "new mode 100755\n",
        "index 3a31ef6..c8cd5d0\n",
        "--- a/script.sh\n",
        "+++ b/script.sh\n",
        "@@ -1 +1 @@\n",
        "-echo old\n",
        "+echo new\n",
    ));
    assert_eq!(
        (file.old_mode, file.new_mode),
        (Some(0o100644), Some(0o100755))
    );
    assert_eq!(file.hunks.len(), 1);
}

#[test]
fn missing_final_line_feeds_are_flagged_on_the_right_lines() {
    let flags = |file: &FileDiff| -> Vec<(char, bool)> {
        file.hunks[0]
            .lines
            .iter()
            .map(|line| {
                let marker = match line.kind {
                    DiffLineKind::Context => ' ',
                    DiffLineKind::Added => '+',
                    DiffLineKind::Removed => '-',
                };
                (marker, line.no_newline_at_end)
            })
            .collect()
    };

    // Neither side ends with a line feed.
    let both = parse_one(concat!(
        "diff --git a/nonl.txt b/nonl.txt\n",
        "index 20cbb4d..0ba6e66 100644\n",
        "--- a/nonl.txt\n",
        "+++ b/nonl.txt\n",
        "@@ -1 +1 @@\n",
        "-no newline\n",
        "\\ No newline at end of file\n",
        "+no newline changed\n",
        "\\ No newline at end of file\n",
    ));
    assert_eq!(flags(&both), [('-', true), ('+', true)]);
    assert_eq!(both.hunks[0].lines[1].text, "no newline changed");

    // Only the old side lacks it: the new side gained the line feed and a line.
    let gained = parse_one(concat!(
        "diff --git a/f b/f\n",
        "--- a/f\n",
        "+++ b/f\n",
        "@@ -1,2 +1,3 @@\n",
        " first\n",
        "-last\n",
        "\\ No newline at end of file\n",
        "+last\n",
        "+more\n",
    ));
    assert_eq!(
        flags(&gained),
        [(' ', false), ('-', true), ('+', false), ('+', false)]
    );

    // Only the new side lacks it.
    let lost = parse_one(concat!(
        "diff --git a/f b/f\n",
        "--- a/f\n",
        "+++ b/f\n",
        "@@ -1,2 +1,2 @@\n",
        " first\n",
        "-last\n",
        "+last\n",
        "\\ No newline at end of file\n",
    ));
    assert_eq!(flags(&lost), [(' ', false), ('-', false), ('+', true)]);

    // An unchanged last line without a line feed is context.
    let context = parse_one(concat!(
        "diff --git a/f b/f\n",
        "--- a/f\n",
        "+++ b/f\n",
        "@@ -1,2 +1,2 @@\n",
        "-first\n",
        "+FIRST\n",
        " last\n",
        "\\ No newline at end of file\n",
    ));
    assert_eq!(flags(&context), [('-', false), ('+', false), (' ', true)]);
}

#[test]
fn the_note_after_one_hunk_does_not_leak_into_the_next_file() {
    let files = parse(concat!(
        "diff --git a/one b/one\n",
        "--- a/one\n",
        "+++ b/one\n",
        "@@ -1 +1 @@\n",
        "-a\n",
        "+b\n",
        "\\ No newline at end of file\n",
        "diff --git a/two b/two\n",
        "--- a/two\n",
        "+++ b/two\n",
        "@@ -1 +1 @@\n",
        "-c\n",
        "+d\n",
    ));
    assert_eq!(files.len(), 2);
    assert!(files[0].hunks[0].lines[1].no_newline_at_end);
    assert!(files[1].hunks[0]
        .lines
        .iter()
        .all(|line| !line.no_newline_at_end));
}

#[test]
fn names_with_spaces_and_letters_beyond_ascii() {
    let file = parse_one(concat!(
        "diff --git a/dir with space/\u{fc} \u{f1}.txt b/dir with space/\u{fc} \u{f1}.txt\n",
        "index b77b4eb..04ec35a 100644\n",
        "--- a/dir with space/\u{fc} \u{f1}.txt\t\n",
        "+++ b/dir with space/\u{fc} \u{f1}.txt\t\n",
        "@@ -1,2 +1,3 @@\n",
        " x\n",
        " y\n",
        "+z\n",
    ));
    assert_eq!(file.path, Path::new("dir with space/\u{fc} \u{f1}.txt"));
    assert_eq!(file.old_path, None);

    // Without ---/+++ lines the header alone has to give the name.
    let mode_only = parse_one(concat!(
        "diff --git a/my script \u{65e5}\u{672c}.sh b/my script \u{65e5}\u{672c}.sh\n",
        "old mode 100644\n",
        "new mode 100755\n",
    ));
    assert_eq!(mode_only.path, Path::new("my script \u{65e5}\u{672c}.sh"));

    // A name that contains the very text that separates the two names.
    let tricky = parse_one(concat!(
        "diff --git a/x b/y b/x b/y\n",
        "new file mode 100644\n",
        "index 0000000..e69de29\n",
    ));
    assert_eq!(tricky.path, Path::new("x b/y"));
    assert_eq!(tricky.status, DiffStatus::Added);
}

#[test]
fn quoted_names() {
    let files = parse(concat!(
        "diff --git \"a/ta\\tb.txt\" \"b/ta\\tb.txt\"\n",
        "index 8cc35a3..bac686b 100644\n",
        "--- \"a/ta\\tb.txt\"\n",
        "+++ \"b/ta\\tb.txt\"\n",
        "@@ -1 +1 @@\n",
        "-tab\n",
        "+tab2\n",
        "diff --git \"a/quo\\\"te.txt\" \"b/quo\\\"te.txt\"\n",
        "old mode 100644\n",
        "new mode 100755\n",
        "diff --git \"a/back\\\\slash.txt\" \"b/back\\\\slash.txt\"\n",
        "deleted file mode 100644\n",
        "index e69de29..0000000\n",
        "diff --git \"a/new\\nline.txt\" \"b/new\\nline.txt\"\n",
        "new file mode 100644\n",
        "index 0000000..e69de29\n",
        "diff --git \"a/octal \\303\\274 \\001.txt\" \"b/octal \\303\\274 \\001.txt\"\n",
        "new file mode 100644\n",
        "index 0000000..e69de29\n",
    ));
    let paths: Vec<&Path> = files.iter().map(|file| file.path.as_path()).collect();
    assert_eq!(
        paths,
        [
            Path::new("ta\tb.txt"),
            Path::new("quo\"te.txt"),
            Path::new("back\\slash.txt"),
            Path::new("new\nline.txt"),
            Path::new("octal \u{fc} \u{1}.txt"),
        ]
    );
    assert_eq!(files[0].hunks.len(), 1);
    assert_eq!(files[2].status, DiffStatus::Deleted);
}

#[test]
fn renames_between_quoted_and_plain_names() {
    let files = parse(concat!(
        "diff --git \"a/ta\\tb.txt\" b/plain name.txt\n",
        "similarity index 100%\n",
        "rename from \"ta\\tb.txt\"\n",
        "rename to plain name.txt\n",
        "diff --git a/plain name.txt \"b/quo\\\"te.txt\"\n",
        "similarity index 100%\n",
        "rename from plain name.txt\n",
        "rename to \"quo\\\"te.txt\"\n",
    ));
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].path, Path::new("plain name.txt"));
    assert_eq!(files[0].old_path.as_deref(), Some(Path::new("ta\tb.txt")));
    assert_eq!(files[1].path, Path::new("quo\"te.txt"));
    assert_eq!(
        files[1].old_path.as_deref(),
        Some(Path::new("plain name.txt"))
    );
}

#[test]
fn header_names_alone_are_enough() {
    // No rename lines and no ---/+++ lines: only the first line names the file.
    let quoted_then_plain = parse_one("diff --git \"a/ta\\tb\" b/plain\nold mode 100644\n");
    assert_eq!(quoted_then_plain.path, Path::new("plain"));
    let plain_then_quoted = parse_one("diff --git a/plain \"b/ta\\tb\"\nold mode 100644\n");
    assert_eq!(plain_then_quoted.path, Path::new("ta\tb"));
    let different = parse_one("diff --git a/one b/two\nold mode 100644\n");
    assert_eq!(different.path, Path::new("two"));
}

#[test]
fn file_names_that_are_not_utf8() {
    let mut text = b"diff --git a/caf\xe9.txt b/caf\xe9.txt\n".to_vec();
    text.extend_from_slice(b"--- a/caf\xe9.txt\n+++ b/caf\xe9.txt\n@@ -1 +1 @@\n-a\n+b\n");
    let files = parse_diff(&text);
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, path_from_bytes(b"caf\xe9.txt"));
}

#[test]
fn empty_files() {
    let files = parse(concat!(
        "diff --git a/new-empty.txt b/new-empty.txt\n",
        "new file mode 100644\n",
        "index 0000000..e69de29\n",
        "diff --git a/old empty.txt b/old empty.txt\n",
        "deleted file mode 100644\n",
        "index e69de29..0000000\n",
        "diff --git a/emptied.txt b/emptied.txt\n",
        "index 3a31ef6..e69de29 100644\n",
        "--- a/emptied.txt\n",
        "+++ b/emptied.txt\n",
        "@@ -1,2 +0,0 @@\n",
        "-one\n",
        "-two\n",
        "diff --git a/filled.txt b/filled.txt\n",
        "index e69de29..3a31ef6 100644\n",
        "--- a/filled.txt\n",
        "+++ b/filled.txt\n",
        "@@ -0,0 +1 @@\n",
        "+one\n",
    ));
    assert_eq!(files.len(), 4);
    assert_eq!(files[0].path, Path::new("new-empty.txt"));
    assert_eq!(files[0].status, DiffStatus::Added);
    assert!(files[0].hunks.is_empty() && !files[0].binary);
    assert_eq!(files[0].new_mode, Some(0o100644));

    assert_eq!(files[1].path, Path::new("old empty.txt"));
    assert_eq!(files[1].status, DiffStatus::Deleted);
    assert!(files[1].hunks.is_empty());

    assert_eq!(files[2].status, DiffStatus::Modified);
    assert_eq!(ranges(&files[2].hunks[0]), (1, 2, 0, 0));
    assert_eq!(files[3].status, DiffStatus::Modified);
    assert_eq!(ranges(&files[3].hunks[0]), (0, 0, 1, 1));
    assert_eq!(rows(&files[3].hunks[0]), [('+', None, Some(1), "one")]);
}

#[test]
fn content_that_looks_like_headers_stays_content() {
    let file = parse_one(concat!(
        "diff --git a/notes.md b/notes.md\n",
        "index 3a31ef6..c8cd5d0 100644\n",
        "--- a/notes.md\n",
        "+++ b/notes.md\n",
        "@@ -1,6 +1,6 @@\n",
        " diff --git a/fake b/fake\n",
        "--- a/removed line that starts with two dashes\n",
        "+++ b/added line that starts with two pluses\n",
        " @@ -1 +1 @@\n",
        "-* Unmerged path fake\n",
        "+new mode 100755\n",
        " rename from nowhere\n",
        " Binary files a and b differ\n",
        "@@ -20 +20 @@\n",
        "-@@ -5 +5 @@\n",
        "+diff --cc fake\n",
    ));
    assert_eq!(file.path, Path::new("notes.md"));
    assert_eq!(file.status, DiffStatus::Modified);
    assert!(!file.binary);
    assert_eq!(
        (file.old_mode, file.new_mode),
        (Some(0o100644), Some(0o100644))
    );
    assert_eq!(file.hunks.len(), 2);
    assert_eq!(
        rows(&file.hunks[0]),
        [
            (' ', Some(1), Some(1), "diff --git a/fake b/fake"),
            (
                '-',
                Some(2),
                None,
                "-- a/removed line that starts with two dashes"
            ),
            (
                '+',
                None,
                Some(2),
                "++ b/added line that starts with two pluses"
            ),
            (' ', Some(3), Some(3), "@@ -1 +1 @@"),
            ('-', Some(4), None, "* Unmerged path fake"),
            ('+', None, Some(4), "new mode 100755"),
            (' ', Some(5), Some(5), "rename from nowhere"),
            (' ', Some(6), Some(6), "Binary files a and b differ"),
        ]
    );
    assert_eq!(
        rows(&file.hunks[1]),
        [
            ('-', Some(20), None, "@@ -5 +5 @@"),
            ('+', None, Some(20), "diff --cc fake"),
        ]
    );
}

#[test]
fn carriage_returns_and_odd_bytes_in_lines_are_kept() {
    let mut text = b"diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,3 +1,3 @@\n".to_vec();
    text.extend_from_slice(
        b" crlf line\r\n-caf\xe9 latin1\n+caf\xc3\xa9 utf8\n \ttab and trailing space \n",
    );
    let file = &parse_diff(&text)[0];
    let lines = &file.hunks[0].lines;
    assert_eq!(lines[0].text, "crlf line\r");
    assert_eq!(lines[0].bytes(), b"crlf line\r");
    // Not UTF-8: shown with a replacement character, kept byte for byte.
    assert_eq!(lines[1].text, "caf\u{fffd} latin1");
    assert_eq!(lines[1].bytes(), b"caf\xe9 latin1");
    assert_eq!(lines[2].text, "caf\u{e9} utf8");
    assert_eq!(lines[2].bytes(), "caf\u{e9} utf8".as_bytes());
    assert_eq!(lines[3].text, "\ttab and trailing space ");
}

#[test]
fn empty_context_lines_with_and_without_their_space() {
    // The second form is what `diff.suppressBlankEmpty` produces.
    for blank in [" \n", "\n"] {
        let text =
            format!("diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,3 +1,3 @@\n a\n{blank}-b\n+c\n");
        let file = parse_one(&text);
        assert_eq!(
            rows(&file.hunks[0]),
            [
                (' ', Some(1), Some(1), "a"),
                (' ', Some(2), Some(2), ""),
                ('-', Some(3), None, "b"),
                ('+', None, Some(3), "c"),
            ],
            "{blank:?}"
        );
    }
}

#[test]
fn unmerged_paths_without_a_diff() {
    let files = parse(concat!(
        "* Unmerged path both.txt\n",
        "* Unmerged path dir/c with space.txt\n",
        "diff --git a/clean.txt b/clean.txt\n",
        "index 9766475..866e03d 100644\n",
        "--- a/clean.txt\n",
        "+++ b/clean.txt\n",
        "@@ -1 +1,2 @@\n",
        " ok\n",
        "+more\n",
        "* Unmerged path del-by-them.txt\n",
    ));
    let summary: Vec<(&Path, DiffStatus, usize)> = files
        .iter()
        .map(|file| (file.path.as_path(), file.status, file.hunks.len()))
        .collect();
    assert_eq!(
        summary,
        [
            (Path::new("both.txt"), DiffStatus::Conflicted, 0),
            (Path::new("dir/c with space.txt"), DiffStatus::Conflicted, 0),
            (Path::new("clean.txt"), DiffStatus::Modified, 1),
            (Path::new("del-by-them.txt"), DiffStatus::Conflicted, 0),
        ]
    );
}

#[test]
fn unmerged_paths_followed_by_their_diff_against_our_side() {
    let files = parse(concat!(
        "* Unmerged path both.txt\n",
        "diff --git a/both.txt b/both.txt\n",
        "index 4c6dd32..eaa2413 100644\n",
        "--- a/both.txt\n",
        "+++ b/both.txt\n",
        "@@ -1 +1,5 @@\n",
        "+<<<<<<< HEAD\n",
        " both added main\n",
        "+=======\n",
        "+both added other\n",
        "+>>>>>>> other\n",
        "* Unmerged path del-by-us.txt\n",
        "diff --git a/later.txt b/later.txt\n",
        "index 9766475..866e03d 100644\n",
        "--- a/later.txt\n",
        "+++ b/later.txt\n",
        "@@ -1 +1 @@\n",
        "-a\n",
        "+b\n",
    ));
    assert_eq!(files.len(), 3);
    assert_eq!(files[0].path, Path::new("both.txt"));
    assert_eq!(files[0].status, DiffStatus::Conflicted);
    assert_eq!(files[0].additions(), 4);
    // The note about del-by-us.txt is not about the file that happens to follow it.
    assert_eq!(files[1].path, Path::new("del-by-us.txt"));
    assert_eq!(files[1].status, DiffStatus::Conflicted);
    assert!(files[1].hunks.is_empty());
    assert_eq!(files[2].path, Path::new("later.txt"));
    assert_eq!(files[2].status, DiffStatus::Modified);
}

#[test]
fn combined_diffs_are_conflicts_without_hunks() {
    let files = parse(concat!(
        "diff --cc both.txt\n",
        "index 4c6dd32,2264291..0000000\n",
        "--- a/both.txt\n",
        "+++ b/both.txt\n",
        "@@@ -1,1 -1,1 +1,5 @@@\n",
        "++<<<<<<< HEAD\n",
        " +both added main\n",
        "++=======\n",
        "+ both added other\n",
        "++>>>>>>> other\n",
        "+++ a line whose content starts with a plus and a space\n",
        "--- and one with dashes\n",
        "diff --cc \"ta\\tb.txt\"\n",
        "index 4dc8328,5319304..0000000\n",
        "--- \"a/ta\\tb.txt\"\n",
        "+++ \"b/ta\\tb.txt\"\n",
        "@@@ -1,3 -1,3 +1,7 @@@\n",
        "  one\n",
        "diff --git a/after.txt b/after.txt\n",
        "--- a/after.txt\n",
        "+++ b/after.txt\n",
        "@@ -1 +1 @@\n",
        "-a\n",
        "+b\n",
    ));
    assert_eq!(files.len(), 3);
    assert_eq!(files[0].path, Path::new("both.txt"));
    assert_eq!(files[0].status, DiffStatus::Conflicted);
    assert!(files[0].hunks.is_empty());
    assert_eq!(files[1].path, Path::new("ta\tb.txt"));
    assert_eq!(files[1].status, DiffStatus::Conflicted);
    assert_eq!(files[2].path, Path::new("after.txt"));
    assert_eq!(files[2].hunks.len(), 1);
}

#[test]
fn a_type_change_is_a_deletion_and_an_addition() {
    let files = parse(concat!(
        "diff --git a/a.txt b/a.txt\n",
        "deleted file mode 100644\n",
        "index 7898192..0000000\n",
        "--- a/a.txt\n",
        "+++ /dev/null\n",
        "@@ -1 +0,0 @@\n",
        "-a\n",
        "diff --git a/a.txt b/a.txt\n",
        "new file mode 120000\n",
        "index 0000000..743fca0\n",
        "--- /dev/null\n",
        "+++ b/a.txt\n",
        "@@ -0,0 +1 @@\n",
        "+moved name.txt\n",
        "\\ No newline at end of file\n",
    ));
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].status, DiffStatus::Deleted);
    assert_eq!(files[0].old_mode, Some(0o100644));
    assert_eq!(files[1].status, DiffStatus::Added);
    assert_eq!(files[1].new_mode, Some(0o120000));
    assert!(files[1].hunks[0].lines[0].no_newline_at_end);
    assert!(files.iter().all(|file| file.path == Path::new("a.txt")));
}

#[test]
fn a_submodule_change_is_a_one_line_hunk() {
    let file = parse_one(concat!(
        "diff --git a/vendor/library b/vendor/library\n",
        "index 8cbc8e3..05c1696 160000\n",
        "--- a/vendor/library\n",
        "+++ b/vendor/library\n",
        "@@ -1 +1 @@\n",
        "-Subproject commit 8cbc8e3948a4d8437af274ed8924f9a77582e614\n",
        "+Subproject commit 05c16969050e1b3ed1da06fa5e46f34bac38a6f9\n",
    ));
    assert_eq!(file.path, Path::new("vendor/library"));
    assert_eq!(file.new_mode, Some(0o160000));
    assert_eq!((file.additions(), file.deletions()), (1, 1));
}

#[test]
fn text_before_the_first_file_is_ignored() {
    let files = parse(concat!(
        "some tool printed a banner\n",
        "@@ -1 +1 @@\n",
        "-not in any file\n",
        "+still not\n",
        "--- a/nothing\n",
        "diff --git a/real.txt b/real.txt\n",
        "--- a/real.txt\n",
        "+++ b/real.txt\n",
        "@@ -1 +1 @@\n",
        "-a\n",
        "+b\n",
    ));
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, Path::new("real.txt"));
    assert_eq!(
        rows(&files[0].hunks[0]),
        [('-', Some(1), None, "a"), ('+', None, Some(1), "b")]
    );
}

#[test]
fn truncated_and_malformed_input_gives_what_was_readable() {
    let complete = concat!(
        "diff --git a/f.txt b/f.txt\n",
        "index b03757e..96e9f48 100644\n",
        "--- a/f.txt\n",
        "+++ b/f.txt\n",
        "@@ -1,5 +1,5 @@ heading\n",
        " a\n",
        "-b\n",
        "+B\n",
        " c\n",
        " d\n",
        " e\n",
        "\\ No newline at end of file\n",
    );
    // Cut anywhere: never a panic, never more than the one file.
    for cut in 0..=complete.len() {
        let files = parse_diff(&complete.as_bytes()[..cut]);
        assert!(files.len() <= 1, "cut at {cut}");
    }
    // The hunk promises five lines per side and delivers two.
    let short = parse_one("diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,5 +1,5 @@\n a\n-b\n");
    assert_eq!(short.hunks.len(), 1);
    assert_eq!(short.hunks[0].lines.len(), 2);

    // A hunk that claims less than it holds stops where it said it would.
    let long = parse_one("diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n-a\n+b\n+c\n-d\n");
    assert_eq!(
        rows(&long.hunks[0]),
        [('-', Some(1), None, "a"), ('+', None, Some(1), "b")]
    );

    // Headers that are not headers.
    for bad in [
        "@@ nonsense @@\n",
        "@@ -a,b +c,d @@\n",
        "@@ -1,2 @@\n",
        "@@ -1,2 +1,2\n",
        "@@ -99999999999 +1 @@\n",
    ] {
        let text = format!("diff --git a/f b/f\n--- a/f\n+++ b/f\n{bad}-a\n+b\n");
        let file = parse_one(&text);
        assert!(file.hunks.is_empty(), "{bad:?}");
        assert_eq!(file.path, Path::new("f"));
    }

    // A header line without names is no file.
    assert!(parse("diff --git \n").is_empty());
    assert!(parse("diff --git \"unterminated\n").is_empty());
    // Bad modes are simply unknown.
    let file = parse_one("diff --git a/f b/f\nold mode banana\nnew mode 99\n");
    assert_eq!((file.old_mode, file.new_mode), (None, None));
}

#[test]
fn a_line_only_file_is_constructible_and_exact() {
    let line = DiffLine::new(DiffLineKind::Added, None, Some(3), "text");
    assert_eq!(line.bytes(), b"text");
    assert!(!line.no_newline_at_end);
    assert_eq!(line.new_no, Some(3));
}

#[test]
fn added_hunks_for_untracked_content() {
    assert_eq!(added_hunk(b""), None);

    let hunk = added_hunk(b"one\ntwo\n").unwrap();
    assert_eq!(ranges(&hunk), (0, 0, 1, 2));
    assert_eq!(
        rows(&hunk),
        [('+', None, Some(1), "one"), ('+', None, Some(2), "two")]
    );
    assert!(hunk.lines.iter().all(|line| !line.no_newline_at_end));

    let hunk = added_hunk(b"one\ntwo").unwrap();
    assert_eq!(ranges(&hunk), (0, 0, 1, 2));
    assert!(!hunk.lines[0].no_newline_at_end);
    assert!(hunk.lines[1].no_newline_at_end);

    // A file that is one empty line, and blank lines in general, are lines.
    let hunk = added_hunk(b"\n").unwrap();
    assert_eq!(rows(&hunk), [('+', None, Some(1), "")]);
    let hunk = added_hunk(b"a\n\n\nb\r\n").unwrap();
    assert_eq!(
        rows(&hunk),
        [
            ('+', None, Some(1), "a"),
            ('+', None, Some(2), ""),
            ('+', None, Some(3), ""),
            ('+', None, Some(4), "b\r"),
        ]
    );

    let hunk = added_hunk(b"caf\xe9\n").unwrap();
    assert_eq!(hunk.lines[0].bytes(), b"caf\xe9");
}
