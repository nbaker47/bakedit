//! The editor driven the way the app drives it: through its public API, against real files.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use bakedit_text::{
    Buffer, Clipboard, Command, DiskState, EditSettings, Editor, LineEnding, Motion, SaveOptions,
    SearchQuery, Selection, Selections,
};
use serde_json::json;
use tempfile::TempDir;

fn write(dir: &TempDir, name: &str, text: &str) -> PathBuf {
    let path = dir.path().join(name);
    fs::write(&path, text).unwrap();
    path
}

fn action(name: &str) -> Command {
    Command::from_action(name, None).unwrap_or_else(|| panic!("{name} is not an editor action"))
}

fn text(editor: &Editor) -> String {
    editor.buffer().to_string()
}

fn cursors(editor: &Editor) -> Vec<usize> {
    editor
        .selections()
        .iter()
        .map(|selection| selection.head)
        .collect()
}

#[test]
fn a_whole_session() {
    let dir = TempDir::new().unwrap();
    let path = write(&dir, "main.rs", "fn main() {\n}\n");
    let mut editor = Editor::open(&path).unwrap();
    assert_eq!(editor.buffer().path(), Some(path.as_path()));
    assert!(!editor.buffer().is_dirty());

    // Go to the end of the first line, open a line and type a call.
    editor.apply(action("editor::MoveToEndOfLine"));
    editor.apply(action("editor::Newline"));
    for c in "println!(\"hi\");".chars() {
        editor.apply(Command::Insert(c.to_string()));
    }
    assert_eq!(text(&editor), "fn main() {\n    println!(\"hi\");\n}\n");
    assert!(editor.buffer().is_dirty());

    let outcome = editor
        .save(SaveOptions {
            ensure_final_newline: true,
        })
        .unwrap();
    assert!(
        outcome.is_noop(),
        "the file already ended in one line break"
    );
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "fn main() {\n    println!(\"hi\");\n}\n"
    );
    assert!(!editor.buffer().is_dirty());
    assert_eq!(editor.buffer().disk_state(), DiskState::Unchanged);

    // Undo goes back through the edits and makes the buffer dirty again.
    editor.apply(Command::Undo);
    assert!(editor.buffer().is_dirty());
    editor.apply(Command::Redo);
    assert!(!editor.buffer().is_dirty());
}

#[test]
fn crlf_files_are_edited_as_lf_and_saved_as_crlf() {
    let dir = TempDir::new().unwrap();
    let path = write(&dir, "win.txt", "one\r\ntwo\r\nthree\r\n");
    let mut editor = Editor::open(&path).unwrap();
    assert_eq!(editor.line_ending(), LineEnding::CrLf);
    assert_eq!(text(&editor), "one\ntwo\nthree\n");

    // Line-based commands see plain lines.
    editor.set_cursor(5);
    editor.apply(action("editor::DuplicateLineDown"));
    editor.apply(action("editor::MoveLineUp"));
    editor.apply(Command::Paste(Clipboard::plain("pasted\r\nlines ")));
    assert_eq!(text(&editor), "one\ntpasted\nlines wo\ntwo\nthree\n");
    assert_eq!(editor.buffer().len_lines(), 6);

    editor.save(SaveOptions::default()).unwrap();
    assert_eq!(
        fs::read(&path).unwrap(),
        b"one\r\ntpasted\r\nlines wo\r\ntwo\r\nthree\r\n"
    );

    editor.set_line_ending(LineEnding::Lf);
    assert!(editor.buffer().is_dirty());
    editor.save(SaveOptions::default()).unwrap();
    assert_eq!(
        fs::read(&path).unwrap(),
        b"one\ntpasted\nlines wo\ntwo\nthree\n"
    );
}

#[test]
fn reload_keeps_cursors_with_their_text() {
    let dir = TempDir::new().unwrap();
    let original = "use std::fs;\n\nfn load() {\n    read();\n}\n\nfn save() {\n    write();\n}\n";
    let path = write(&dir, "lib.rs", original);
    let mut editor = Editor::open(&path).unwrap();

    // One cursor inside `read`, a selection over `write`, a cursor at the very end.
    let read_at = original.find("read").unwrap() + 2;
    let write_at = original.find("write").unwrap();
    editor.set_selections(Selections::new(
        vec![
            Selection::cursor(read_at),
            Selection::new(write_at, write_at + 5),
            Selection::cursor(original.len()),
        ],
        1,
    ));

    // An agent rewrites the file: a new import, a renamed function, a new line in `load`.
    let rewritten = original
        .replace("use std::fs;\n", "use std::fs;\nuse std::io;\n")
        .replace("fn load()", "fn load_all()")
        .replace("    read();\n", "    open();\n    read();\n");
    fs::write(&path, &rewritten).unwrap();
    assert_eq!(editor.buffer().disk_state(), DiskState::Modified);

    let outcome = editor.reload().unwrap();
    assert!(outcome.text_changed() && outcome.selections_changed);
    assert_eq!(text(&editor), rewritten);
    assert!(!editor.buffer().is_dirty());
    assert_eq!(editor.buffer().disk_state(), DiskState::Unchanged);

    let new_read = rewritten.find("read").unwrap() + 2;
    let new_write = rewritten.find("write").unwrap();
    assert_eq!(
        editor.selections().as_slice(),
        &[
            Selection::cursor(new_read),
            Selection::new(new_write, new_write + 5),
            Selection::cursor(rewritten.len())
        ]
    );
    assert_eq!(
        editor.selections().primary(),
        Selection::new(new_write, new_write + 5)
    );

    // The reload is one undo step that also brings the old selections back.
    editor.apply(Command::Undo);
    assert_eq!(text(&editor), original);
    assert_eq!(
        cursors(&editor),
        vec![read_at, write_at + 5, original.len()]
    );
    assert!(
        editor.buffer().is_dirty(),
        "the text no longer matches the file"
    );
    editor.apply(Command::Redo);
    assert_eq!(text(&editor), rewritten);
    assert!(!editor.buffer().is_dirty());
}

#[test]
fn reload_when_the_cursor_line_itself_changed() {
    let dir = TempDir::new().unwrap();
    let path = write(&dir, "a.txt", "alpha beta gamma\nsecond line\n");
    let mut editor = Editor::open(&path).unwrap();
    editor.set_cursor(11); // before "gamma"

    fs::write(&path, "alpha BETA gamma delta\nsecond line\n").unwrap();
    editor.reload().unwrap();
    assert_eq!(
        cursors(&editor),
        vec![11],
        "the unchanged word keeps the cursor"
    );

    // The line the cursor is on disappears: the cursor falls to where it was.
    fs::write(&path, "second line\n").unwrap();
    editor.reload().unwrap();
    assert_eq!(text(&editor), "second line\n");
    assert_eq!(cursors(&editor), vec![0]);

    // An unchanged file is a no-op.
    assert!(editor.reload().unwrap().is_noop());
}

#[test]
fn reload_discards_local_edits_but_undo_brings_them_back() {
    let dir = TempDir::new().unwrap();
    let path = write(&dir, "notes.txt", "keep\nthis\n");
    let mut editor = Editor::open(&path).unwrap();
    editor.apply(Command::Move(Motion::End));
    editor.apply(Command::Insert("local".to_string()));
    fs::write(&path, "keep\nthat\n").unwrap();

    editor.reload().unwrap();
    assert_eq!(text(&editor), "keep\nthat\n");
    assert!(!editor.buffer().is_dirty());
    editor.apply(Command::Undo);
    assert_eq!(text(&editor), "keep\nthis\nlocal");
    editor.apply(Command::Undo);
    assert_eq!(text(&editor), "keep\nthis\n");
}

#[test]
fn saving_trims_the_tail_and_keeps_the_cursor_inside_the_text() {
    let dir = TempDir::new().unwrap();
    let path = write(&dir, "tail.txt", "text\n\n\n   ");
    let mut editor = Editor::open(&path).unwrap();
    editor.apply(Command::Move(Motion::End));
    assert_eq!(cursors(&editor), vec![10]);

    let outcome = editor
        .save(SaveOptions {
            ensure_final_newline: true,
        })
        .unwrap();
    assert!(outcome.text_changed() && outcome.selections_changed);
    assert_eq!(text(&editor), "text\n");
    assert_eq!(cursors(&editor), vec![5]);
    assert_eq!(fs::read_to_string(&path).unwrap(), "text\n");

    // Undo restores the tail and the cursor in it.
    editor.apply(Command::Undo);
    assert_eq!(text(&editor), "text\n\n\n   ");
    assert_eq!(cursors(&editor), vec![10]);
    assert!(editor.buffer().is_dirty());
}

#[test]
fn save_as_and_errors() {
    let dir = TempDir::new().unwrap();
    let mut editor = Editor::from_str("draft");
    assert!(editor.save(SaveOptions::default()).is_err(), "no path yet");
    assert!(editor.reload().is_err());

    let path = dir.path().join("draft.txt");
    editor
        .save_as(
            &path,
            SaveOptions {
                ensure_final_newline: true,
            },
        )
        .unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "draft\n");
    assert_eq!(editor.buffer().path(), Some(path.as_path()));
    assert!(!editor.buffer().is_dirty());

    assert!(editor
        .save_as(dir.path().join("missing").join("x"), SaveOptions::default())
        .is_err());
    assert_eq!(editor.buffer().path(), Some(path.as_path()));

    editor.set_path(None);
    assert_eq!(editor.buffer().path(), None);
    assert!(Editor::open(dir.path().join("nope")).is_err());
}

#[test]
fn keymap_actions_drive_the_editor() {
    let mut editor = Editor::from_str("let value = compute(first, second);\nlet other = 1;\n");
    editor.set_cursor(22); // inside "first"

    // cmd-k ( : select between parens, twice.
    let between =
        Command::from_action("editor::SelectBetween", Some(&json!({"kind": "paren"}))).unwrap();
    editor.apply(between.clone());
    assert_eq!(
        editor.buffer().slice(editor.selections().primary().range()),
        "first, second"
    );

    // cmd-d on a word, then type over every occurrence.
    editor.set_cursor(5);
    let next = Command::from_action(
        "editor::SelectNext",
        Some(&json!({"replace_newest": false})),
    )
    .unwrap();
    editor.apply(next.clone());
    assert_eq!(
        editor.buffer().slice(editor.selections().primary().range()),
        "value"
    );

    // Move by five lines the way the user's keymap binds cmd-down.
    let down = Command::from_action("editor::MoveDownByLines", Some(&json!({"lines": 5}))).unwrap();
    editor.apply(down);
    assert_eq!(
        editor
            .buffer()
            .char_to_point(editor.selections().primary().head)
            .line,
        2
    );

    // The nav layer turns any movement into an extension of the selection.
    editor.set_cursor(0);
    editor.apply(action("editor::MoveToNextWordEnd").extending());
    assert_eq!(editor.selections().primary(), Selection::new(0, 3));

    // Comments take their prefix from the language.
    let comment = action("editor::ToggleComments").with_line_comment("//");
    editor.apply(comment.clone());
    assert!(text(&editor).starts_with("// let value"));
    editor.apply(comment);
    assert!(text(&editor).starts_with("let value"));

    // Paging takes its size from the view.
    let page = action("editor::MovePageDown").with_page_rows(1);
    editor.set_cursor(0);
    editor.apply(page);
    assert_eq!(
        editor
            .buffer()
            .char_to_point(editor.selections().primary().head)
            .line,
        1
    );
}

#[test]
fn find_and_replace_through_the_editor() {
    let mut editor = Editor::from_str("alpha beta alpha\nALPHA beta\n");
    let mut query = SearchQuery::text("alpha");
    query.case_sensitive = false;
    let search = query.compile().unwrap();
    let rope = editor.buffer().rope();
    assert_eq!(
        search.find_all(rope, 0..rope.len_chars()),
        vec![0..5, 11..16, 17..22]
    );
    assert_eq!(search.find_next(rope, 1, true), Some(11..16));
    assert_eq!(search.find_prev(rope, 11, true), Some(0..5));

    let (replaced, _) = editor.replace_next(&search, 6, true, "omega");
    assert_eq!(replaced, Some(11..16));
    let outcome = editor.replace_all(&search, "A");
    assert_eq!(outcome.edits.changes().count(), 2);
    assert_eq!(text(&editor), "A beta omega\nA beta\n");
    editor.apply(Command::Undo);
    editor.apply(Command::Undo);
    assert_eq!(text(&editor), "alpha beta alpha\nALPHA beta\n");
    assert!(SearchQuery::regex("(").compile().is_err());
}

#[test]
fn typing_stays_fast_in_a_large_buffer() {
    // About 50 MB. Each keystroke must cost far less than a frame.
    let line = "the quick brown fox jumps over the lazy dog 0123456789 abcdefghij klmnopqrst\n";
    let lines = 50 * 1024 * 1024 / line.len();
    let mut editor = Editor::new(Buffer::from_str(&line.repeat(lines)));
    let middle = editor.buffer().line_to_char(lines / 2) + 10;
    editor.set_cursor(middle);

    let start = Instant::now();
    for c in "fn f(x) { return [x, \"y\"]; }".chars() {
        editor.apply(Command::Insert(c.to_string()));
    }
    for _ in 0..10 {
        editor.apply(Command::Backspace);
        editor.apply(Command::Move(Motion::Up));
        editor.apply(Command::Move(Motion::PreviousWordStart));
        editor.apply(Command::Newline);
        editor.matching_bracket(editor.selections().primary().head);
    }
    editor.apply(Command::Move(Motion::End));
    editor.apply(Command::Move(Motion::Beginning));
    editor.apply(Command::Move(Motion::GoToLine {
        line: lines - 5,
        column: 3,
    }));
    while editor.buffer().can_undo() {
        editor.apply(Command::Undo);
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "about 80 commands took {elapsed:?}"
    );
    assert_eq!(editor.buffer().len_lines(), lines + 1);
}

#[test]
fn many_cursors_stay_fast() {
    let line = "let value = other_value + 1;\n";
    let mut editor = Editor::with_settings(
        Buffer::from_str(&line.repeat(5_000)),
        EditSettings::default(),
    );
    editor.set_cursor(5);
    let start = Instant::now();
    editor.apply(Command::SelectAllMatches);
    assert_eq!(editor.selections().len(), 5_000, "whole-word matches only");
    for c in "renamed".chars() {
        editor.apply(Command::Insert(c.to_string()));
    }
    editor.apply(Command::Move(Motion::EndOfLine {
        stop_at_soft_wraps: false,
    }));
    editor.apply(Command::Backspace);
    editor.apply(Command::Newline);
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "editing at 5000 cursors took {elapsed:?}"
    );
    assert!(text(&editor).starts_with("let renamed = other_value + 1\n\nlet renamed"));
    editor.apply(Command::Cancel);
    assert_eq!(editor.selections().len(), 1);
}
