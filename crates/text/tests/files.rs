//! Loading, saving and reloading buffers against real files in a temp directory.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bakedit_text::{Bias, Buffer, BufferError, DiskState, LineEnding, SaveOptions};
use tempfile::TempDir;

fn write(dir: &TempDir, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.path().join(name);
    fs::write(&path, bytes).unwrap();
    path
}

fn file_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

const KEEP: SaveOptions = SaveOptions {
    ensure_final_newline: false,
    remove_trailing_whitespace: false,
};
const FINAL_NEWLINE: SaveOptions = SaveOptions {
    ensure_final_newline: true,
    remove_trailing_whitespace: false,
};

#[test]
fn loads_a_plain_file() {
    let dir = TempDir::new().unwrap();
    let path = write(&dir, "a.txt", b"hello\nworld\n");
    let buffer = Buffer::from_file(&path).unwrap();
    assert_eq!(buffer.to_string(), "hello\nworld\n");
    assert_eq!(buffer.len_lines(), 3);
    assert_eq!(buffer.path(), Some(path.as_path()));
    assert_eq!(buffer.line_ending(), LineEnding::Lf);
    assert!(!buffer.is_dirty());
    assert!(!buffer.has_bom());
    assert!(!buffer.had_invalid_utf8());
    assert_eq!(buffer.disk_state(), DiskState::Unchanged);
}

#[test]
fn loads_an_empty_file_and_a_file_without_final_newline() {
    let dir = TempDir::new().unwrap();
    let empty = Buffer::from_file(write(&dir, "empty", b"")).unwrap();
    assert!(empty.is_empty());
    assert_eq!(empty.len_lines(), 1);

    let mut buffer = Buffer::from_file(write(&dir, "tail", b"a\nb")).unwrap();
    assert_eq!(buffer.len_lines(), 2);
    assert_eq!(buffer.line(1), "b");
    buffer.save(KEEP).unwrap();
    assert_eq!(
        fs::read(dir.path().join("tail")).unwrap(),
        b"a\nb",
        "saving does not add a newline unasked"
    );
}

#[test]
fn crlf_is_normalised_in_memory_and_restored_on_save() {
    let dir = TempDir::new().unwrap();
    let path = write(&dir, "win.txt", b"one\r\ntwo\r\nthree");
    let mut buffer = Buffer::from_file(&path).unwrap();
    assert_eq!(buffer.to_string(), "one\ntwo\nthree");
    assert_eq!(buffer.line_ending(), LineEnding::CrLf);
    assert_eq!(buffer.len_lines(), 3);
    assert_eq!(buffer.line(1), "two");
    assert!(!buffer.is_dirty());

    buffer.edit(&[(3..3, "!\nextra")]);
    assert_eq!(buffer.to_string(), "one!\nextra\ntwo\nthree");
    buffer.save(FINAL_NEWLINE).unwrap();
    assert_eq!(
        fs::read(&path).unwrap(),
        b"one!\r\nextra\r\ntwo\r\nthree\r\n"
    );
    assert!(!buffer.is_dirty());

    // Converting to LF is a change worth saving.
    buffer.set_line_ending(LineEnding::Lf);
    assert!(buffer.is_dirty());
    buffer.save(KEEP).unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"one!\nextra\ntwo\nthree\n");
    assert!(!buffer.is_dirty());
}

#[test]
fn a_byte_order_mark_is_hidden_and_written_back() {
    let dir = TempDir::new().unwrap();
    let path = write(&dir, "bom.txt", b"\xEF\xBB\xBFhi\n");
    let mut buffer = Buffer::from_file(&path).unwrap();
    assert_eq!(buffer.to_string(), "hi\n");
    assert!(buffer.has_bom());
    buffer.edit(&[(0..0, "oh ")]);
    buffer.save(KEEP).unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"\xEF\xBB\xBFoh hi\n");
    buffer.set_bom(false);
    buffer.save(KEEP).unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"oh hi\n");
}

#[test]
fn invalid_utf8_is_replaced_and_flagged() {
    let dir = TempDir::new().unwrap();
    let path = write(&dir, "latin1.txt", b"caf\xE9\nok\n");
    let mut buffer = Buffer::from_file(&path).unwrap();
    assert_eq!(buffer.to_string(), "caf\u{fffd}\nok\n");
    assert!(buffer.had_invalid_utf8());
    assert!(!buffer.is_dirty());
    buffer.save(KEEP).unwrap();
    assert!(
        !buffer.had_invalid_utf8(),
        "after a save the file is what the buffer holds"
    );
    assert_eq!(fs::read(&path).unwrap(), "caf\u{fffd}\nok\n".as_bytes());
}

#[test]
fn binary_files_are_refused() {
    let dir = TempDir::new().unwrap();
    let path = write(&dir, "image.bin", b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR");
    match Buffer::from_file(&path) {
        Err(BufferError::Binary { path: refused }) => assert_eq!(refused, path),
        other => panic!("expected a binary error, got {other:?}"),
    }

    // Only the first 8 KiB are sniffed.
    let mut late_nul = vec![b'a'; 8 * 1024];
    late_nul.push(0);
    let buffer = Buffer::from_file(write(&dir, "late.txt", &late_nul)).unwrap();
    assert_eq!(buffer.len_chars(), 8 * 1024 + 1);

    let mut early_nul = vec![b'a'; 8 * 1024 - 1];
    early_nul.push(0);
    assert!(matches!(
        Buffer::from_file(write(&dir, "early.txt", &early_nul)),
        Err(BufferError::Binary { .. })
    ));
}

#[test]
fn a_missing_file_is_an_io_error() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("nope.txt");
    match Buffer::from_file(&path) {
        Err(BufferError::Io {
            path: failed,
            source,
        }) => {
            assert_eq!(failed, path);
            assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
        }
        other => panic!("expected an io error, got {other:?}"),
    }
    assert!(
        matches!(Buffer::from_file(dir.path()), Err(BufferError::Io { .. })),
        "a directory is not a file"
    );
}

#[test]
fn save_replaces_the_file_and_leaves_no_temp_files() {
    let dir = TempDir::new().unwrap();
    let path = write(&dir, "note.txt", b"old\n");
    let mut buffer = Buffer::from_file(&path).unwrap();
    buffer.edit(&[(0..3, "new")]);
    assert!(buffer.is_dirty());
    let summary = buffer.save(KEEP).unwrap();
    assert!(summary.is_empty());
    assert_eq!(fs::read(&path).unwrap(), b"new\n");
    assert!(!buffer.is_dirty());
    assert_eq!(buffer.disk_state(), DiskState::Unchanged);
    assert_eq!(file_names(dir.path()), vec!["note.txt"]);

    // Undo after a save makes the buffer dirty; redo makes it clean again.
    buffer.undo().unwrap();
    assert!(buffer.is_dirty());
    buffer.redo().unwrap();
    assert!(!buffer.is_dirty());
}

#[test]
fn save_creates_a_new_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("fresh.txt");
    let mut buffer = Buffer::from_str("brand new");
    buffer.set_path(Some(path.clone()));
    assert_eq!(
        buffer.disk_state(),
        DiskState::Unchanged,
        "nothing on disk yet"
    );
    buffer.save(FINAL_NEWLINE).unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"brand new\n");
    assert_eq!(file_names(dir.path()), vec!["fresh.txt"]);
    assert!(!buffer.is_dirty());
}

#[test]
fn save_into_a_missing_directory_fails_and_keeps_the_buffer_dirty() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("no-such-dir").join("file.txt");
    let mut buffer = Buffer::from_str("text");
    buffer.edit(&[(0..0, "some ")]);
    buffer.set_path(Some(path.clone()));
    match buffer.save(KEEP) {
        Err(BufferError::Io { path: failed, .. }) => assert_eq!(failed, path),
        other => panic!("expected an io error, got {other:?}"),
    }
    assert!(buffer.is_dirty());
    assert_eq!(buffer.to_string(), "some text");
}

#[test]
fn save_as_switches_the_path_only_on_success() {
    let dir = TempDir::new().unwrap();
    let first = write(&dir, "first.txt", b"one\n");
    let mut buffer = Buffer::from_file(&first).unwrap();
    buffer.edit(&[(0..0, "# ")]);

    let bad = dir.path().join("missing").join("x.txt");
    assert!(buffer.save_as(&bad, KEEP).is_err());
    assert_eq!(buffer.path(), Some(first.as_path()));
    assert!(buffer.is_dirty());
    assert_eq!(buffer.disk_state(), DiskState::Unchanged);

    let second = dir.path().join("second.txt");
    buffer.save_as(&second, KEEP).unwrap();
    assert_eq!(buffer.path(), Some(second.as_path()));
    assert_eq!(fs::read(&second).unwrap(), b"# one\n");
    assert_eq!(
        fs::read(&first).unwrap(),
        b"one\n",
        "the old file is untouched"
    );
    assert!(!buffer.is_dirty());
}

#[cfg(unix)]
#[test]
fn save_keeps_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new().unwrap();
    let path = write(&dir, "script.sh", b"#!/bin/sh\n");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).unwrap();
    let mut buffer = Buffer::from_file(&path).unwrap();
    buffer.edit(&[(10..10, "echo hi\n")]);
    buffer.save(KEEP).unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
        0o750
    );
    assert_eq!(fs::read(&path).unwrap(), b"#!/bin/sh\necho hi\n");
}

#[cfg(unix)]
#[test]
fn save_refuses_a_read_only_file() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new().unwrap();
    let path = write(&dir, "locked.txt", b"keep me\n");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
    let mut buffer = Buffer::from_file(&path).unwrap();
    buffer.edit(&[(0..4, "lose")]);
    match buffer.save(KEEP) {
        Err(BufferError::Io { source, .. }) => {
            assert_eq!(source.kind(), std::io::ErrorKind::PermissionDenied)
        }
        other => panic!("expected a permission error, got {other:?}"),
    }
    assert_eq!(fs::read(&path).unwrap(), b"keep me\n");
    assert!(buffer.is_dirty());
    assert_eq!(file_names(dir.path()), vec!["locked.txt"]);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
}

#[cfg(unix)]
#[test]
fn save_follows_symlinks_instead_of_replacing_them() {
    let dir = TempDir::new().unwrap();
    let real_dir = dir.path().join("real");
    fs::create_dir(&real_dir).unwrap();
    let target = real_dir.join("target.txt");
    fs::write(&target, b"v1\n").unwrap();
    let link = dir.path().join("link.txt");
    std::os::unix::fs::symlink(Path::new("real").join("target.txt"), &link).unwrap();

    let mut buffer = Buffer::from_file(&link).unwrap();
    assert_eq!(buffer.to_string(), "v1\n");
    buffer.edit(&[(1..2, "2")]);
    buffer.save(KEEP).unwrap();

    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the link is still a link"
    );
    assert_eq!(fs::read(&target).unwrap(), b"v2\n");
    assert_eq!(file_names(&real_dir), vec!["target.txt"]);
    assert_eq!(file_names(dir.path()), vec!["link.txt", "real"]);
    assert_eq!(buffer.disk_state(), DiskState::Unchanged);

    // A link to a file that does not exist yet creates the file it points at.
    let dangling = dir.path().join("dangling.txt");
    std::os::unix::fs::symlink(real_dir.join("later.txt"), &dangling).unwrap();
    let mut buffer = Buffer::from_str("made through a link\n");
    buffer.set_path(Some(dangling.clone()));
    buffer.save(KEEP).unwrap();
    assert!(fs::symlink_metadata(&dangling)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        fs::read(real_dir.join("later.txt")).unwrap(),
        b"made through a link\n"
    );
}

#[cfg(unix)]
#[test]
fn save_keeps_hard_links_pointing_at_the_same_file() {
    let dir = TempDir::new().unwrap();
    let path = write(&dir, "a.txt", b"shared\n");
    let other = dir.path().join("b.txt");
    fs::hard_link(&path, &other).unwrap();
    let mut buffer = Buffer::from_file(&path).unwrap();
    buffer.edit(&[(0..0, "still ")]);
    buffer.save(KEEP).unwrap();
    assert_eq!(fs::read(&other).unwrap(), b"still shared\n");
    assert_eq!(file_names(dir.path()), vec!["a.txt", "b.txt"]);
}

#[cfg(unix)]
#[test]
fn save_overwrites_in_place_when_the_directory_is_read_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new().unwrap();
    let locked = dir.path().join("locked");
    fs::create_dir(&locked).unwrap();
    let path = locked.join("file.txt");
    fs::write(&path, b"old\n").unwrap();
    let mut buffer = Buffer::from_file(&path).unwrap();
    buffer.edit(&[(0..3, "new")]);

    // No temporary file can be created next to the target, but the file is writable.
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
    let result = buffer.save(KEEP);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    result.unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"new\n");
    assert!(!buffer.is_dirty());
    assert_eq!(file_names(&locked), vec!["file.txt"]);
}

#[cfg(unix)]
#[test]
fn save_through_a_symlink_loop_is_an_error() {
    let dir = TempDir::new().unwrap();
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::os::unix::fs::symlink(&b, &a).unwrap();
    std::os::unix::fs::symlink(&a, &b).unwrap();
    let mut buffer = Buffer::from_str("text");
    buffer.set_path(Some(a.clone()));
    assert!(matches!(buffer.save(KEEP), Err(BufferError::Io { .. })));
    assert!(matches!(Buffer::from_file(&a), Err(BufferError::Io { .. })));
    assert_eq!(file_names(dir.path()), vec!["a", "b"]);
}

#[test]
fn ensure_final_newline_adds_trims_and_is_undoable() {
    let dir = TempDir::new().unwrap();

    let path = write(&dir, "none.txt", b"a\nb");
    let mut buffer = Buffer::from_file(&path).unwrap();
    let summary = buffer.save(FINAL_NEWLINE).unwrap();
    assert!(!summary.is_empty());
    assert_eq!(summary.map_offset(3, Bias::Left), 3);
    assert_eq!(buffer.to_string(), "a\nb\n");
    assert_eq!(fs::read(&path).unwrap(), b"a\nb\n");
    assert!(
        !buffer.is_dirty(),
        "the newline edit is part of the saved state"
    );
    buffer.undo().unwrap();
    assert_eq!(buffer.to_string(), "a\nb");
    assert!(buffer.is_dirty());

    let path = write(&dir, "many.txt", b"a\nb  \n\n \n\n");
    let mut buffer = Buffer::from_file(&path).unwrap();
    let summary = buffer.save(FINAL_NEWLINE).unwrap();
    assert_eq!(buffer.to_string(), "a\nb\n");
    assert_eq!(fs::read(&path).unwrap(), b"a\nb\n");
    assert_eq!(
        summary.map_offset(9, Bias::Left),
        4,
        "a position in the trimmed tail moves to the end"
    );

    let path = write(&dir, "fine.txt", b"a\nb\n");
    let mut buffer = Buffer::from_file(&path).unwrap();
    assert!(buffer.save(FINAL_NEWLINE).unwrap().is_empty());
    assert!(!buffer.can_undo(), "no edit was needed");

    let path = write(&dir, "empty.txt", b"");
    let mut buffer = Buffer::from_file(&path).unwrap();
    assert!(buffer.save(FINAL_NEWLINE).unwrap().is_empty());
    assert_eq!(fs::read(&path).unwrap(), b"", "an empty file stays empty");

    let path = write(&dir, "keep.txt", b"a\n\n\n");
    let mut buffer = Buffer::from_file(&path).unwrap();
    buffer.edit(&[(0..1, "b")]);
    buffer.save(KEEP).unwrap();
    assert_eq!(
        fs::read(&path).unwrap(),
        b"b\n\n\n",
        "without the option nothing is trimmed"
    );
}

#[test]
fn remove_trailing_whitespace_on_save() {
    let dir = TempDir::new().unwrap();
    let tidy = SaveOptions {
        ensure_final_newline: true,
        remove_trailing_whitespace: true,
    };
    let path = write(&dir, "messy.txt", b"one  \n\ttwo\t \n   \nthree   ");
    let mut buffer = Buffer::from_file(&path).unwrap();
    let summary = buffer.save(tidy).unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"one\n\ttwo\n\nthree\n");
    assert_eq!(buffer.to_string(), "one\n\ttwo\n\nthree\n");
    assert!(!buffer.is_dirty());
    assert_eq!(
        summary.map_offset(5, Bias::Left),
        3,
        "a position in removed whitespace"
    );
    assert_eq!(
        summary.map_offset(7, Bias::Left),
        5,
        "a position after it shifts"
    );

    // Both clean-ups are a single undo step.
    buffer.undo().unwrap();
    assert_eq!(buffer.to_string(), "one  \n\ttwo\t \n   \nthree   ");
    assert!(!buffer.can_undo());

    // CRLF files are trimmed the same way and keep their line ending.
    let path = write(&dir, "win.txt", b"a \r\nb\t\r\n");
    let mut buffer = Buffer::from_file(&path).unwrap();
    buffer.save(tidy).unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"a\r\nb\r\n");

    // Nothing to do is nothing recorded.
    let path = write(&dir, "clean.txt", b"a\nb\n");
    let mut buffer = Buffer::from_file(&path).unwrap();
    assert!(buffer.save(tidy).unwrap().is_empty());
    assert!(!buffer.can_undo());
}

/// Rewrite a file from outside and make sure the buffer can tell, whatever the clock's
/// resolution: the length changes too.
fn rewrite(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
}

#[test]
fn disk_state_follows_outside_changes() {
    let dir = TempDir::new().unwrap();
    let path = write(&dir, "watched.txt", b"one\n");
    let mut buffer = Buffer::from_file(&path).unwrap();
    assert_eq!(buffer.disk_state(), DiskState::Unchanged);

    rewrite(&path, "one\ntwo\n");
    assert_eq!(buffer.disk_state(), DiskState::Modified);
    buffer.reload().unwrap();
    assert_eq!(buffer.disk_state(), DiskState::Unchanged);

    fs::remove_file(&path).unwrap();
    assert_eq!(buffer.disk_state(), DiskState::Deleted);
    assert!(matches!(buffer.reload(), Err(BufferError::Io { .. })));
    assert_eq!(
        buffer.to_string(),
        "one\ntwo\n",
        "a failed reload leaves the buffer alone"
    );

    // Saving brings the file back.
    buffer.save(KEEP).unwrap();
    assert_eq!(buffer.disk_state(), DiskState::Unchanged);

    // A path that was never read: fine while missing, modified once something is there.
    let mut fresh = Buffer::new();
    let other = dir.path().join("other.txt");
    fresh.set_path(Some(other.clone()));
    assert_eq!(fresh.disk_state(), DiskState::Unchanged);
    fs::write(&other, b"appeared").unwrap();
    assert_eq!(fresh.disk_state(), DiskState::Modified);
}

#[test]
fn reload_applies_only_what_changed() {
    let dir = TempDir::new().unwrap();
    let original = "fn main() {\n    let x = 1;\n    let y = 2;\n    println!(\"{}\", x + y);\n}\n";
    let path = write(&dir, "main.rs", original.as_bytes());
    let mut buffer = Buffer::from_file(&path).unwrap();

    // A cursor in the last statement, before `x + y`.
    let cursor = original.find("x + y").unwrap();
    let on_changed_line = original.find("= 2").unwrap();

    let rewritten = original.replace("let y = 2;", "let y = 20;\n    let z = 3;");
    rewrite(&path, &rewritten);
    let summary = buffer.reload().unwrap();
    assert_eq!(buffer.to_string(), rewritten);
    assert!(!buffer.is_dirty());
    assert_eq!(buffer.disk_state(), DiskState::Unchanged);

    // Only the changed region was edited, so positions elsewhere just shift.
    let total_removed: usize = summary.changes().map(|change| change.old.len()).sum();
    assert!(
        total_removed <= 2,
        "expected a minimal edit, got {summary:?}"
    );
    let moved = summary.map_offset(cursor, Bias::Left);
    assert_eq!(moved, rewritten.find("x + y").unwrap());
    assert_eq!(summary.map_offset(0, Bias::Left), 0);
    assert_eq!(
        summary.map_offset(on_changed_line, Bias::Left),
        rewritten.find("= 20").unwrap()
    );

    // The reload is one undo step.
    buffer.undo().unwrap();
    assert_eq!(buffer.to_string(), original);
    assert!(buffer.is_dirty(), "the buffer no longer matches the file");
    assert!(!buffer.can_undo());
    buffer.redo().unwrap();
    assert_eq!(buffer.to_string(), rewritten);
    assert!(!buffer.is_dirty());
}

#[test]
fn reload_keeps_earlier_undo_history_and_discards_local_edits() {
    let dir = TempDir::new().unwrap();
    let path = write(&dir, "doc.txt", b"alpha\nbeta\ngamma\n");
    let mut buffer = Buffer::from_file(&path).unwrap();
    buffer.edit(&[(0..0, "LOCAL ")]);
    assert!(buffer.is_dirty());

    rewrite(&path, "alpha\nbeta\ngamma\ndelta\n");
    buffer.reload().unwrap();
    assert_eq!(
        buffer.to_string(),
        "alpha\nbeta\ngamma\ndelta\n",
        "the buffer now holds the file's text"
    );
    assert!(!buffer.is_dirty());

    buffer.undo().unwrap();
    assert_eq!(
        buffer.to_string(),
        "LOCAL alpha\nbeta\ngamma\n",
        "undoing the reload restores the local edit"
    );
    buffer.undo().unwrap();
    assert_eq!(buffer.to_string(), "alpha\nbeta\ngamma\n");
}

#[test]
fn reload_of_an_unchanged_file_does_nothing() {
    let dir = TempDir::new().unwrap();
    let path = write(&dir, "same.txt", b"same\n");
    let mut buffer = Buffer::from_file(&path).unwrap();
    let version = buffer.version();
    let summary = buffer.reload().unwrap();
    assert!(summary.is_empty());
    assert_eq!(buffer.version(), version);
    assert!(!buffer.can_undo());
    assert!(!buffer.is_dirty());
}

#[test]
fn reload_picks_up_a_new_line_ending() {
    let dir = TempDir::new().unwrap();
    let path = write(&dir, "eol.txt", b"a\nb\n");
    let mut buffer = Buffer::from_file(&path).unwrap();
    fs::write(&path, b"a\r\nb\r\nc\r\n").unwrap();
    buffer.reload().unwrap();
    assert_eq!(buffer.to_string(), "a\nb\nc\n");
    assert_eq!(buffer.line_ending(), LineEnding::CrLf);
    assert!(!buffer.is_dirty());

    // A reload that turns the file binary is refused and changes nothing.
    fs::write(&path, b"\0\0\0").unwrap();
    assert!(matches!(buffer.reload(), Err(BufferError::Binary { .. })));
    assert_eq!(buffer.to_string(), "a\nb\nc\n");
}

#[test]
fn a_large_file_loads_edits_and_saves_quickly() {
    // 100 MB of ordinary lines. The bound is loose; a per-character loop over the file
    // would take far longer than this.
    let dir = TempDir::new().unwrap();
    let line = "the quick brown fox jumps over the lazy dog 0123456789 abcdefghij klmnopqrst\n";
    let lines = 100 * 1024 * 1024 / line.len();
    let path = write(&dir, "big.txt", line.repeat(lines).as_bytes());

    let start = Instant::now();
    let mut buffer = Buffer::from_file(&path).unwrap();
    let load_time = start.elapsed();
    assert_eq!(buffer.len_lines(), lines + 1);
    assert_eq!(buffer.len_bytes(), lines * line.len());
    assert!(
        load_time < Duration::from_secs(10),
        "loading took {load_time:?}"
    );

    let middle = buffer.line_to_char(lines / 2);
    let start = Instant::now();
    buffer.edit(&[(middle..middle, "inserted\n")]);
    assert_eq!(buffer.line(lines / 2), "inserted");
    assert_eq!(buffer.len_lines(), lines + 2);
    buffer.undo().unwrap();
    assert!(start.elapsed() < Duration::from_secs(1));

    let start = Instant::now();
    buffer.edit(&[(0..0, "# header\n")]);
    buffer.save(SaveOptions::default()).unwrap();
    let save_time = start.elapsed();
    assert!(
        save_time < Duration::from_secs(10),
        "saving took {save_time:?}"
    );
    assert_eq!(
        fs::metadata(&path).unwrap().len() as usize,
        lines * line.len() + 9
    );
    assert!(!buffer.is_dirty());
}
