use std::path::PathBuf;
use std::time::{Duration, Instant};

use bakedit_core::{Action, KeyEvent, Mods, MouseButton, MouseEvent, MouseKind};
use bakedit_git::{LineHunk, LineHunkKind};
use serde_json::json;

use super::marks::GitMarks;
use super::*;
use crate::paint::{self, GitMark};
use crate::view::test_support::Harness;
use crate::view::{CursorShape, Effect};

const SAMPLE: &str = "fn main() {\n    let answer = 42;\n    println!(\"{answer}\");\n}\n\nstruct Point {\n    x: i32,\n}\n";

fn file(dir: &tempfile::TempDir, name: &str, text: &str) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, text).unwrap();
    path
}

fn act(item: &mut EditorItem, harness: &mut Harness, name: &str) -> bool {
    item.handle_action(&Action::new(name), &mut harness.cx())
}

fn act_with(item: &mut EditorItem, harness: &mut Harness, name: &str, args: serde_json::Value) -> bool {
    item.handle_action(&Action::with_args(name, args), &mut harness.cx())
}

fn type_text(item: &mut EditorItem, harness: &mut Harness, text: &str) {
    for c in text.chars() {
        item.handle_key(&KeyEvent::typed(c), &mut harness.cx());
    }
}

fn text_of(item: &EditorItem) -> String {
    item.editor.buffer().to_string()
}

/// An editor over `text` with the modes off: keys type.
fn plain(harness: &mut Harness, text: &str) -> EditorItem {
    let mut item = EditorItem::untitled(&mut harness.cx());
    item.editor.edit(&[(0..0, text)]);
    item.editor.set_cursor(0);
    item
}

fn modal(harness: &mut Harness, text: &str) -> EditorItem {
    harness.settings.vim_mode = true;
    plain(harness, text)
}

fn mouse(item: &mut EditorItem, harness: &mut Harness, kind: MouseKind, col: u16, row: u16, area: Rect) -> bool {
    item.handle_mouse(&MouseEvent::new(kind, col, row), area, &mut harness.cx())
}

fn settle_highlights(item: &mut EditorItem, harness: &mut Harness) {
    let started = Instant::now();
    loop {
        harness.project.poll();
        if item.tick(&mut harness.cx()) {
            return;
        }
        assert!(started.elapsed() < Duration::from_secs(20), "no highlights arrived");
        std::thread::sleep(Duration::from_millis(5));
    }
}

// ---- opening and drawing ----------------------------------------------------------------

#[test]
fn opens_a_file_and_draws_it_with_a_gutter_and_the_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir, "main.rs", SAMPLE);
    let mut harness = Harness::new(dir.path());
    harness.settings.breadcrumbs = false;
    let mut item = EditorItem::open(&path, &mut harness.cx()).unwrap();
    assert_eq!(item.path(), Some(path.as_path()));
    assert_eq!(item.kind(), "editor");
    let (text, cursor, _) = harness.draw(&mut item, 60, 12);
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].starts_with("   1 fn main() {"), "{:?}", lines[0]);
    assert!(lines[1].contains("let answer = 42;"));
    assert!(!lines[8].contains("9 "), "no number past the end: {:?}", lines[8]);
    let gutter = paint::gutter_width(9, true);
    assert_eq!(cursor, Some(Cursor { col: gutter, row: 0, shape: CursorShape::Bar }));
    let status = item.status();
    assert_eq!(status.position, Some((1, 1, 0)));
    assert_eq!(status.language.as_deref(), Some("Rust"));
    assert_eq!(status.line_ending, Some("LF"));
    assert_eq!(item.tab(&harness.render_cx(false)).title, "main.rs");
}

#[test]
fn breadcrumbs_show_the_path_below_the_root() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    let path = file(&dir, "src/main.rs", SAMPLE);
    let mut harness = Harness::new(dir.path());
    let mut item = EditorItem::open(&path, &mut harness.cx()).unwrap();
    let (text, cursor, _) = harness.draw(&mut item, 60, 12);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0].trim(), "src › main.rs");
    assert!(lines[1].starts_with("   1 fn main"), "{:?}", lines[1]);
    assert_eq!(cursor.map(|c| c.row), Some(1));
    harness.settings.breadcrumbs = false;
    let (text, _, _) = harness.draw(&mut item, 60, 12);
    assert!(text.lines().next().unwrap().contains("fn main"));
}

#[test]
fn pages_and_untitled_buffers() {
    let mut harness = Harness::new("/project");
    let mut page = EditorItem::page("Key Bindings", "{\n  \"cmd-p\": \"file_finder::Toggle\"\n}\n", Some("JSON"), &mut harness.cx());
    assert_eq!(page.kind(), "page");
    assert_eq!(page.path(), None);
    assert_eq!(page.tab(&harness.render_cx(false)).title, "Key Bindings");
    assert_eq!(page.status().language.as_deref(), Some("JSON"));
    let (text, _, _) = harness.draw(&mut page, 60, 6);
    assert!(text.contains("file_finder::Toggle"));
    assert!(!page.is_dirty());
    // Nobody types into a page, and it says why.
    type_text(&mut page, &mut harness, "x");
    act(&mut page, &mut harness, "editor::Backspace");
    assert!(!text_of(&page).starts_with('x'));
    assert!(harness.take_effects().iter().any(|e| matches!(e, Effect::Status { text, error: true } if text.contains("read-only"))));
    let mut untitled = EditorItem::untitled(&mut harness.cx());
    assert_eq!(untitled.tab(&harness.render_cx(false)).title, "untitled");
    let (text, cursor, _) = harness.draw(&mut untitled, 30, 4);
    assert!(text.contains("   1"), "{text:?}");
    assert!(cursor.is_some());
}

#[test]
fn a_tab_follows_its_file_when_bakedit_moves_it() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    let old = dir.path().join("src/notes.txt");
    std::fs::write(&old, "# Notes\n").unwrap();
    let mut harness = Harness::new(dir.path());
    let mut item = EditorItem::open(&old, &mut harness.cx()).unwrap();
    // Renamed in the tree: the tab shows the new name and the new language, and the file
    // is not taken for deleted.
    let renamed = dir.path().join("src/notes.md");
    std::fs::rename(&old, &renamed).unwrap();
    item.moved(&old, &renamed, &mut harness.cx());
    item.paths_changed(&FsChange::of(vec![old.clone(), renamed.clone()]), &mut harness.cx());
    assert_eq!(item.path(), Some(renamed.as_path()));
    assert_eq!(item.tab(&harness.render_cx(false)).title, "notes.md");
    assert_eq!(item.status().language.as_deref(), Some("Markdown"));
    assert!(harness.take_effects().is_empty(), "no word of a deleted file");
    // Its folder moved: the path below it follows.
    let moved = dir.path().join("docs");
    std::fs::rename(dir.path().join("src"), &moved).unwrap();
    item.moved(&dir.path().join("src"), &moved, &mut harness.cx());
    assert_eq!(item.path(), Some(moved.join("notes.md").as_path()));
    // Another file's move leaves it alone.
    item.moved(&dir.path().join("elsewhere.txt"), &dir.path().join("x.txt"), &mut harness.cx());
    assert_eq!(item.path(), Some(moved.join("notes.md").as_path()));
}

#[test]
fn missing_files_and_folders_say_so_and_binaries_open_as_a_note() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    let mut harness = Harness::new(dir.path());
    let error = EditorItem::open(&dir.path().join("nope.rs"), &mut harness.cx()).err().unwrap();
    assert_eq!(error, "nope.rs does not exist");
    let error = EditorItem::open(&dir.path().join("src"), &mut harness.cx()).err().unwrap();
    assert_eq!(error, "src is a folder");
    let binary = dir.path().join("data.bin");
    std::fs::write(&binary, [0u8, 159, 146, 150, 0, 0, 1, 2, 3, 0, 0, 0]).unwrap();
    let mut item = EditorItem::open(&binary, &mut harness.cx()).unwrap();
    assert_eq!(item.path(), Some(binary.as_path()));
    let (text, _, _) = harness.draw(&mut item, 70, 6);
    assert!(text.contains("data.bin is a binary file"), "{text}");
    type_text(&mut item, &mut harness, "x");
    assert!(!item.is_dirty());
    assert!(item.save(true, &mut harness.cx()).is_err());
}

// ---- editing ------------------------------------------------------------------------------

#[test]
fn typing_enter_indent_and_undo() {
    let mut harness = Harness::new("/project");
    let mut item = plain(&mut harness, "");
    type_text(&mut item, &mut harness, "fn a() {");
    assert_eq!(text_of(&item), "fn a() {}", "the closing brace is added");
    act(&mut item, &mut harness, "editor::Newline");
    type_text(&mut item, &mut harness, "x");
    assert_eq!(text_of(&item), "fn a() {\n    x\n}", "auto indent in the block");
    assert!(item.is_dirty());
    act(&mut item, &mut harness, "editor::Undo");
    act(&mut item, &mut harness, "editor::Undo");
    assert_eq!(text_of(&item), "fn a() {}");
    act(&mut item, &mut harness, "editor::Redo");
    assert!(text_of(&item).contains("\n    "));
    // Backspace and tab.
    let mut item = plain(&mut harness, "ab");
    act(&mut item, &mut harness, "editor::MoveToEnd");
    act(&mut item, &mut harness, "editor::Backspace");
    assert_eq!(text_of(&item), "a");
    act(&mut item, &mut harness, "editor::Tab");
    assert_eq!(text_of(&item), "a   ", "tab goes to the next tab stop");
}

#[test]
fn cut_copy_and_paste_use_the_clipboard_and_keep_lines_whole() {
    let mut harness = Harness::new("/project");
    let mut item = plain(&mut harness, "one\ntwo\nthree\n");
    act(&mut item, &mut harness, "editor::MoveDown");
    act(&mut item, &mut harness, "editor::Cut");
    assert_eq!(text_of(&item), "one\nthree\n");
    assert_eq!(harness.clipboard.get().map(|clip| (clip.text, clip.linewise)), Some(("two\n".to_string(), true)));
    act(&mut item, &mut harness, "editor::MoveToEnd");
    act(&mut item, &mut harness, "editor::Paste");
    assert!(text_of(&item).contains("two"), "{:?}", text_of(&item));
    // Bracketed paste inserts as typed.
    let mut other = plain(&mut harness, "");
    assert!(other.handle_paste("a\nb", &mut harness.cx()));
    assert_eq!(text_of(&other), "a\nb");
    // Comments follow the language.
    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir, "a.rs", "let a = 1;\n");
    let mut rust = EditorItem::open(&path, &mut harness.cx()).unwrap();
    rust.insert = true;
    act_with(&mut rust, &mut harness, "editor::ToggleComments", json!({ "advance_downwards": false, "comment_empty_lines": true }));
    assert_eq!(text_of(&rust), "// let a = 1;\n");
}

#[test]
fn undo_breaks_at_cursor_jumps() {
    let mut harness = Harness::new("/project");
    let mut item = plain(&mut harness, "\n\n");
    type_text(&mut item, &mut harness, "ab");
    act(&mut item, &mut harness, "editor::MoveDown");
    type_text(&mut item, &mut harness, "cd");
    act(&mut item, &mut harness, "editor::Undo");
    assert_eq!(text_of(&item), "ab\n\n", "only the second run is undone");
}

#[test]
fn multiple_cursors_type_together() {
    let mut harness = Harness::new("/project");
    let mut item = plain(&mut harness, "a\na\n");
    act_with(&mut item, &mut harness, "editor::AddSelectionBelow", json!({ "skip_soft_wrap": false }));
    type_text(&mut item, &mut harness, "x");
    assert_eq!(text_of(&item), "xa\nxa\n");
    assert_eq!(item.status().cursors, 2);
}

// ---- nav and edit mode --------------------------------------------------------------------

#[test]
fn nav_mode_types_nothing_and_the_toggle_goes_to_edit_mode() {
    let mut harness = Harness::new("/project");
    let mut item = modal(&mut harness, "hello\nworld\n");
    assert_eq!(item.status().mode, Some(ModeIndicator::Nav));
    type_text(&mut item, &mut harness, "zq hjkl:/");
    assert_eq!(text_of(&item), "hello\nworld\n");
    assert_eq!(item.position(), Some((1, 1)), "and no letter moves the cursor");
    let (_, cursor, _) = harness.draw(&mut item, 40, 5);
    assert_eq!(cursor.map(|c| c.shape), Some(CursorShape::Block));
    act(&mut item, &mut harness, "editor::MoveRight");
    act(&mut item, &mut harness, "bakedit::ToggleMode");
    assert_eq!(item.status().mode, Some(ModeIndicator::Edit));
    type_text(&mut item, &mut harness, "X");
    assert_eq!(text_of(&item), "hXello\nworld\n");
    let (_, cursor, _) = harness.draw(&mut item, 40, 5);
    assert_eq!(cursor.map(|c| c.shape), Some(CursorShape::Bar));
    // Escape goes back, the cursor left where it is (no vim step to the left).
    act(&mut item, &mut harness, "vim::NormalBefore");
    assert_eq!(item.status().mode, Some(ModeIndicator::Nav));
    assert_eq!(item.position(), Some((1, 3)));
}

#[test]
fn a_terminal_paste_pastes_in_nav_mode_as_cmd_v_does() {
    let mut harness = Harness::new("/project");
    let mut item = modal(&mut harness, "hello\n");
    act(&mut item, &mut harness, "editor::MoveToEndOfLine");
    assert!(item.handle_paste(" world", &mut harness.cx()));
    assert_eq!(item.status().mode, Some(ModeIndicator::Nav));
    assert_eq!(text_of(&item), "hello world\n");
    assert_eq!(item.position(), Some((1, 12)));
}

#[test]
fn the_vim_actions_are_gone() {
    let mut harness = Harness::new("/project");
    let mut item = modal(&mut harness, "hello\nworld\n");
    for name in ["vim::InsertBefore", "vim::InsertLineBelow", "vim::ToggleVisual", "vim::ScrollDown"] {
        assert!(!act(&mut item, &mut harness, name), "{name}");
    }
    assert_eq!(item.status().mode, Some(ModeIndicator::Nav));
    assert_eq!(text_of(&item), "hello\nworld\n");
}

#[test]
fn word_motions_in_nav_mode_go_where_edit_modes_do() {
    let mut harness = Harness::new("/project");
    let mut item = modal(&mut harness, "let words = vec;\nnext\n");
    act(&mut item, &mut harness, "editor::MoveToNextWordEnd");
    assert_eq!(item.position(), Some((1, 4)), "after let, as in edit mode");
    act(&mut item, &mut harness, "editor::MoveToNextWordEnd");
    assert_eq!(item.position(), Some((1, 10)));
}

#[test]
fn one_undo_takes_back_a_whole_edit_mode_session() {
    let mut harness = Harness::new("/project");
    let mut item = modal(&mut harness, "one\ntwo\n");
    let steps = item.editor.undo_steps();
    // Into edit mode, a new line, typing slowly enough for several undo steps, a paste, escape.
    act(&mut item, &mut harness, "bakedit::ToggleMode");
    act(&mut item, &mut harness, "editor::NewlineBelow");
    type_text(&mut item, &mut harness, "new");
    item.editor.break_undo_group();
    item.handle_paste(" line", &mut harness.cx());
    item.editor.break_undo_group();
    type_text(&mut item, &mut harness, "!");
    act(&mut item, &mut harness, "vim::NormalBefore");
    assert_eq!(text_of(&item), "one\nnew line!\ntwo\n");
    assert_eq!(item.editor.undo_steps(), steps + 1, "the session is one step");
    act(&mut item, &mut harness, "editor::Undo");
    assert_eq!(text_of(&item), "one\ntwo\n", "the new line goes too");
    assert_eq!(item.editor.undo_steps(), steps);
    act(&mut item, &mut harness, "editor::Redo");
    assert_eq!(text_of(&item), "one\nnew line!\ntwo\n");
    // A session of one keystroke is one step as well.
    act(&mut item, &mut harness, "bakedit::ToggleMode");
    type_text(&mut item, &mut harness, "x");
    act(&mut item, &mut harness, "vim::NormalBefore");
    act(&mut item, &mut harness, "editor::Undo");
    assert_eq!(text_of(&item), "one\nnew line!\ntwo\n");
}

#[test]
fn undo_in_nav_mode_leaves_what_edit_mode_leaves() {
    let run = |vim_mode: bool| {
        let mut harness = Harness::new("/project");
        let mut item = if vim_mode { modal(&mut harness, "one\ntwo\nthree\n") } else { plain(&mut harness, "one\ntwo\nthree\n") };
        act(&mut item, &mut harness, "editor::MoveDown");
        act(&mut item, &mut harness, "editor::SelectLine");
        act(&mut item, &mut harness, "editor::Cut");
        assert_eq!(text_of(&item), "one\nthree\n");
        act(&mut item, &mut harness, "editor::Undo");
        assert_eq!(text_of(&item), "one\ntwo\nthree\n");
        let after_undo = item.editor.selections().clone();
        act(&mut item, &mut harness, "editor::Redo");
        assert_eq!(text_of(&item), "one\nthree\n");
        (item.status().mode, after_undo, item.editor.selections().clone())
    };
    let (mode, nav_undo, nav_redo) = run(true);
    assert_eq!(mode, Some(ModeIndicator::Nav));
    let (_, edit_undo, edit_redo) = run(false);
    assert_eq!((nav_undo, nav_redo), (edit_undo, edit_redo));
}

#[test]
fn the_nav_cursor_moves_as_edit_modes_does() {
    let mut harness = Harness::new("/project");
    let mut item = modal(&mut harness, "a long line\nab\nanother long\n");
    act_with(&mut item, &mut harness, "editor::MoveToEndOfLine", json!({ "stop_at_soft_wraps": true }));
    assert_eq!(item.position(), Some((1, 12)), "past the last character, as in edit mode");
    act(&mut item, &mut harness, "editor::MoveRight");
    assert_eq!(item.position(), Some((2, 1)), "right at the end of a line goes on to the next");
    act(&mut item, &mut harness, "editor::MoveLeft");
    assert_eq!(item.position(), Some((1, 12)));
    act(&mut item, &mut harness, "editor::MoveDown");
    assert_eq!(item.position(), Some((2, 3)));
    act(&mut item, &mut harness, "editor::MoveDown");
    assert_eq!(item.position(), Some((3, 12)), "back to the column it was heading for");
    assert_eq!(item.status().mode, Some(ModeIndicator::Nav));
}

#[test]
fn shift_arrows_select_in_nav_mode_as_in_edit_mode() {
    let mut harness = Harness::new("/project");
    let mut item = modal(&mut harness, "one two three\nsecond\n");
    for _ in 0..3 {
        act(&mut item, &mut harness, "editor::SelectRight");
    }
    assert_eq!(item.status().mode, Some(ModeIndicator::Nav));
    assert_eq!(item.status().position.map(|p| p.2), Some(3), "`one` is selected");
    act(&mut item, &mut harness, "editor::Copy");
    assert_eq!(harness.clipboard.get().map(|clip| clip.text), Some("one".to_string()));
    // Escape lets go.
    act(&mut item, &mut harness, "editor::Cancel");
    assert_eq!(item.status().position.map(|p| p.2), Some(0));
    assert_eq!(text_of(&item), "one two three\nsecond\n");
}

#[test]
fn the_setting_turns_modes_off_and_on() {
    let mut harness = Harness::new("/project");
    let mut item = modal(&mut harness, "line 1\nline 2\n");
    harness.settings.vim_mode = false;
    assert!(item.tick(&mut harness.cx()));
    assert_eq!(item.status().mode, None);
    type_text(&mut item, &mut harness, "x");
    assert!(text_of(&item).starts_with("xline 1"));
    let mut contexts = Vec::new();
    item.contexts(&mut contexts);
    assert_eq!(contexts[0].values, vec![("mode", "full".to_string())]);
    harness.settings.vim_mode = true;
    assert!(item.tick(&mut harness.cx()));
    assert_eq!(item.status().mode, Some(ModeIndicator::Nav));
}

#[test]
fn the_nav_mode_setting_reaches_the_key_context() {
    let mut harness = Harness::new("/project");
    harness.settings.vim_mode = true;
    let mut item = EditorItem::untitled(&mut harness.cx());
    let mut contexts = Vec::new();
    item.contexts(&mut contexts);
    assert_eq!(contexts[0].values, vec![("mode", "full".to_string()), ("vim_mode", "normal".to_string())]);
    act(&mut item, &mut harness, "bakedit::ToggleMode");
    contexts.clear();
    item.contexts(&mut contexts);
    assert_eq!(contexts[0].values[1], ("vim_mode", "insert".to_string()));
}

#[test]
fn the_mode_toggle_goes_to_edit_mode_and_back_leaving_the_cursor() {
    let mut harness = Harness::new("/project");
    let mut item = modal(&mut harness, "hello\nworld\n");
    act(&mut item, &mut harness, "editor::MoveRight");
    act(&mut item, &mut harness, "editor::MoveRight");
    // Nav mode to edit mode with the cursor where it is: the bar before the block's character.
    assert!(act(&mut item, &mut harness, "bakedit::ToggleMode"));
    assert_eq!(item.status().mode, Some(ModeIndicator::Edit));
    assert_eq!(item.position(), Some((1, 3)));
    let (_, cursor, _) = harness.draw(&mut item, 40, 5);
    assert_eq!(cursor.map(|c| c.shape), Some(CursorShape::Bar));
    let mut contexts = Vec::new();
    item.contexts(&mut contexts);
    assert_eq!(contexts[0].values[1], ("vim_mode", "insert".to_string()));
    type_text(&mut item, &mut harness, "XY");
    assert_eq!(text_of(&item), "heXYllo\nworld\n");
    // And back, the cursor left where it is.
    assert!(act(&mut item, &mut harness, "bakedit::ToggleMode"));
    assert_eq!(item.status().mode, Some(ModeIndicator::Nav));
    assert_eq!(item.position(), Some((1, 5)), "still after what was typed");
    let (_, cursor, _) = harness.draw(&mut item, 40, 5);
    assert_eq!(cursor.map(|c| c.shape), Some(CursorShape::Block));
    type_text(&mut item, &mut harness, "z");
    assert_eq!(text_of(&item), "heXYllo\nworld\n", "nav mode types nothing");
    // What was typed is one undo step, as after escape.
    act(&mut item, &mut harness, "editor::Undo");
    assert_eq!(text_of(&item), "hello\nworld\n");
    // At the end of a line too: nav mode's cursor goes past the last character, as edit mode's.
    act(&mut item, &mut harness, "bakedit::ToggleMode");
    act_with(&mut item, &mut harness, "editor::MoveToEndOfLine", json!({ "stop_at_soft_wraps": true }));
    assert_eq!(item.position(), Some((1, 6)));
    act(&mut item, &mut harness, "bakedit::ToggleMode");
    assert_eq!(item.position(), Some((1, 6)));
    assert!(harness.take_effects().is_empty(), "the modes were on: nothing for the shell to do");
    // From the find bar it switches the text's mode; the bar keeps the keys.
    act(&mut item, &mut harness, "buffer_search::Deploy");
    assert!(act(&mut item, &mut harness, "bakedit::ToggleMode"));
    assert_eq!(item.status().mode, Some(ModeIndicator::Edit));
    assert!(item.find_focused());
}

#[test]
fn the_mode_toggle_keeps_a_selection_for_edit_mode_to_replace() {
    let mut harness = Harness::new("/project");
    let mut item = modal(&mut harness, "one two three\nsecond\n");
    act(&mut item, &mut harness, "editor::SelectToNextWordEnd");
    assert_eq!(item.status().mode, Some(ModeIndicator::Nav));
    let selected = item.editor.selections().primary().range();
    act(&mut item, &mut harness, "bakedit::ToggleMode");
    assert_eq!(item.status().mode, Some(ModeIndicator::Edit));
    assert_eq!(item.editor.selections().primary().range(), selected, "still selected");
    type_text(&mut item, &mut harness, "1");
    assert_eq!(text_of(&item), "1 two three\nsecond\n", "typing replaces it");
    // Whole lines too.
    act(&mut item, &mut harness, "bakedit::ToggleMode");
    act(&mut item, &mut harness, "editor::MoveDown");
    act(&mut item, &mut harness, "editor::SelectLine");
    act(&mut item, &mut harness, "bakedit::ToggleMode");
    assert_eq!(item.status().mode, Some(ModeIndicator::Edit));
    assert_eq!(item.editor.selections().primary().range(), 12..19);
}

#[test]
fn the_mode_toggle_turns_the_modes_on_when_they_are_off() {
    let mut harness = Harness::new("/project");
    let mut item = plain(&mut harness, "abc\n");
    for _ in 0..3 {
        act(&mut item, &mut harness, "editor::MoveRight");
    }
    assert_eq!(item.status().mode, None);
    assert!(act(&mut item, &mut harness, "bakedit::ToggleMode"));
    assert_eq!(item.status().mode, Some(ModeIndicator::Nav), "lands in nav mode");
    assert_eq!(item.position(), Some((1, 4)), "the cursor stays past the last character");
    let effects = harness.take_effects();
    assert_eq!(effects, vec![Effect::Dispatch(Action::new("bakedit::ToggleEditMode"))], "the setting goes on too");
    // Once the shell has done that, the next tick leaves it on.
    harness.settings.vim_mode = true;
    item.tick(&mut harness.cx());
    assert_eq!(item.status().mode, Some(ModeIndicator::Nav));
    // The setting just switched on but not yet seen by this editor: nothing more to ask.
    harness.settings.vim_mode = false;
    let mut late = plain(&mut harness, "x\n");
    harness.settings.vim_mode = true;
    act(&mut late, &mut harness, "bakedit::ToggleMode");
    assert_eq!(late.status().mode, Some(ModeIndicator::Nav));
    assert!(harness.take_effects().is_empty());
}

// ---- find ---------------------------------------------------------------------------------

#[test]
fn find_seeds_from_the_word_counts_steps_and_closes_keeping_the_selection() {
    let mut harness = Harness::new("/project");
    let mut item = plain(&mut harness, "foo bar foo\nbaz Foo foo\n");
    harness.settings.breadcrumbs = false;
    assert!(act(&mut item, &mut harness, "buffer_search::Deploy"));
    let mut contexts = Vec::new();
    item.contexts(&mut contexts);
    assert_eq!(contexts.iter().map(|c| c.name).collect::<Vec<_>>(), vec!["BufferSearchBar", "Editor"]);
    assert_eq!(contexts[1].values, vec![("mode", "single_line".to_string())]);
    let (text, cursor, _) = harness.draw(&mut item, 90, 6);
    let first = text.lines().next().unwrap();
    assert!(first.contains("Find") && first.contains("foo"), "{first:?}");
    assert!(first.contains("1 of 4"), "case-insensitive by default: {first:?}");
    assert_eq!(cursor.map(|c| c.row), Some(0), "the cursor is in the bar");
    assert_eq!(item.status().position.map(|p| p.2), Some(3), "the match under the cursor is selected");

    act(&mut item, &mut harness, "search::SelectNextMatch");
    assert_eq!(item.editor.selections().primary().range(), 8..11);
    act(&mut item, &mut harness, "search::SelectNextMatch");
    assert_eq!(item.editor.selections().primary().range(), 16..19, "Foo matches with case off");
    act(&mut item, &mut harness, "search::SelectNextMatch");
    act(&mut item, &mut harness, "search::SelectNextMatch");
    assert_eq!(item.editor.selections().primary().range(), 0..3, "wraps around");
    act(&mut item, &mut harness, "search::SelectPreviousMatch");
    assert_eq!(item.editor.selections().primary().range(), 20..23);
    let text = harness.draw(&mut item, 90, 6).0;
    assert!(text.lines().next().unwrap().contains("4 of 4"));

    // Case sensitivity on: Foo is no longer a match.
    act(&mut item, &mut harness, "search::ToggleCaseSensitive");
    let text = harness.draw(&mut item, 90, 6).0;
    assert!(text.lines().next().unwrap().contains("of 3"), "{text}");
    // Escape closes the bar and keeps the selection.
    let selected = item.editor.selections().primary().range();
    assert!(act(&mut item, &mut harness, "buffer_search::Dismiss"));
    assert_eq!(item.editor.selections().primary().range(), selected);
    assert!(!item.find_focused());
    assert!(!act(&mut item, &mut harness, "buffer_search::Dismiss"), "nothing to dismiss: escape falls through");
    let text = harness.draw(&mut item, 90, 6).0;
    assert!(!text.contains("Find"));
}

#[test]
fn typing_in_the_bar_searches_as_you_type_and_enter_steps() {
    let mut harness = Harness::new("/project");
    let mut item = plain(&mut harness, "alpha beta\ngamma beta\n");
    harness.settings.breadcrumbs = false;
    act(&mut item, &mut harness, "buffer_search::Deploy");
    type_text(&mut item, &mut harness, "beta");
    assert_eq!(item.editor.selections().primary().range(), 6..10);
    let text = harness.draw(&mut item, 90, 6).0;
    assert!(text.lines().next().unwrap().contains("1 of 2"));
    // Backspace edits the query, not the text.
    act(&mut item, &mut harness, "editor::Backspace");
    assert_eq!(text_of(&item), "alpha beta\ngamma beta\n");
    act(&mut item, &mut harness, "search::SelectNextMatch");
    assert_eq!(item.editor.selections().primary().range(), 17..20);
    // An unmatched query says so.
    type_text(&mut item, &mut harness, "zzz");
    let text = harness.draw(&mut item, 90, 6).0;
    assert!(text.lines().next().unwrap().contains("No matches"), "{text}");
    // A bad regex says so too.
    act(&mut item, &mut harness, "editor::SelectAll");
    act(&mut item, &mut harness, "editor::Backspace");
    act(&mut item, &mut harness, "search::ToggleRegex");
    type_text(&mut item, &mut harness, "(");
    let text = harness.draw(&mut item, 90, 6).0;
    assert!(text.lines().next().unwrap().contains("bad regex"), "{text}");
    // Tab moves the keys to the text with the bar still open.
    assert!(act(&mut item, &mut harness, "buffer_search::FocusEditor"));
    assert!(!item.find_focused());
    type_text(&mut item, &mut harness, "Q");
    assert!(text_of(&item).contains('Q'));
    // cmd-f again goes back to the bar.
    act(&mut item, &mut harness, "buffer_search::Deploy");
    assert!(item.find_focused());
}

#[test]
fn replace_and_select_all_matches() {
    let mut harness = Harness::new("/project");
    let mut item = plain(&mut harness, "cat dog cat\ncat\n");
    harness.settings.breadcrumbs = false;
    act_with(&mut item, &mut harness, "buffer_search::Deploy", json!({ "replace_enabled": true }));
    let mut contexts = Vec::new();
    item.contexts(&mut contexts);
    assert!(contexts[0].flags.contains(&"in_replace"));
    act(&mut item, &mut harness, "search::FocusSearch");
    type_text(&mut item, &mut harness, "cat");
    // Focus the replacement by clicking it, then type.
    let (text, _, _) = harness.draw(&mut item, 90, 6);
    assert!(text.lines().nth(1).unwrap().contains("Replace") && text.lines().nth(1).unwrap().contains("All"));
    let hit = item.find.as_ref().unwrap().hits.iter().find(|(_, hit)| *hit == find::FindHit::Replace).unwrap().0;
    mouse(&mut item, &mut harness, MouseKind::Press(MouseButton::Left), hit.x + 2, hit.y, Rect::new(0, 0, 90, 6));
    type_text(&mut item, &mut harness, "bird");
    act(&mut item, &mut harness, "search::ReplaceNext");
    assert_eq!(text_of(&item), "bird dog cat\ncat\n", "the selected match is replaced");
    assert_eq!(item.editor.selections().primary().range(), 9..12, "and the next one is selected");
    act(&mut item, &mut harness, "search::ReplaceAll");
    assert_eq!(text_of(&item), "bird dog bird\nbird\n");
    act(&mut item, &mut harness, "editor::Undo");
    assert!(item.find_focused(), "undo in the bar is the bar's own");
    // Select all matches puts a cursor on each and moves the keys to the text.
    let mut item = plain(&mut harness, "ab ab ab\n");
    act(&mut item, &mut harness, "buffer_search::Deploy");
    type_text(&mut item, &mut harness, "ab");
    act(&mut item, &mut harness, "search::SelectAllMatches");
    assert_eq!(item.status().cursors, 3);
    assert!(!item.find_focused());
}

#[test]
fn the_last_search_repeats_in_nav_mode_as_in_edit_mode() {
    let mut harness = Harness::new("/project");
    let mut item = modal(&mut harness, "x foo\nfoo\n");
    assert!(act(&mut item, &mut harness, "search::SelectNextMatch"));
    assert!(harness.take_effects().iter().any(|e| matches!(e, Effect::Status { text, .. } if text.contains("Nothing to search"))));
    act(&mut item, &mut harness, "buffer_search::Deploy");
    type_text(&mut item, &mut harness, "foo");
    // Closing the bar leaves the match it found selected, as in edit mode.
    act(&mut item, &mut harness, "buffer_search::Dismiss");
    assert_eq!(item.editor.selections().primary().range(), 2..5);
    // Next and previous match select the match, as in edit mode.
    act(&mut item, &mut harness, "search::SelectNextMatch");
    assert_eq!(item.editor.selections().primary().range(), 6..9);
    act(&mut item, &mut harness, "search::SelectNextMatch");
    assert_eq!(item.editor.selections().primary().range(), 2..5, "wraps around");
    act(&mut item, &mut harness, "search::SelectPreviousMatch");
    assert_eq!(item.editor.selections().primary().range(), 6..9);
    // While the bar is open, enter selects the match, as in Zed.
    act(&mut item, &mut harness, "buffer_search::Deploy");
    act(&mut item, &mut harness, "search::SelectNextMatch");
    assert!(!item.editor.selections().primary().is_empty());
}

#[test]
fn the_bar_survives_a_narrow_editor() {
    let mut harness = Harness::new("/project");
    let mut item = plain(&mut harness, "foo foo\n");
    harness.settings.breadcrumbs = false;
    act(&mut item, &mut harness, "buffer_search::Deploy");
    for width in [90, 50, 30, 20] {
        let (text, _, _) = harness.draw(&mut item, width, 6);
        let first = text.lines().next().unwrap();
        assert!(first.starts_with(" Find"), "{width}: {first:?}");
        if width == 90 {
            assert!(first.contains("Aa   ab   .*"), "the same marks as project search: {first:?}");
        }
        assert!(first.chars().count() <= width as usize);
    }
}

// ---- soft wrap, scrolling ----------------------------------------------------------------------

#[test]
fn long_lines_wrap_with_the_setting_and_the_toggle() {
    let mut harness = Harness::new("/project");
    harness.settings.breadcrumbs = false;
    harness.settings.minimap = crate::settings::MinimapShow::Never;
    let long = "word ".repeat(30);
    let mut item = plain(&mut harness, &format!("{long}\nshort\n"));
    item.language = bakedit_syntax::language_by_id("rust").unwrap();
    let (text, _, _) = harness.draw(&mut item, 40, 10);
    assert_eq!(text.lines().filter(|l| l.contains("word")).count(), 1, "no wrap by default: {text}");
    act(&mut item, &mut harness, "editor::ToggleSoftWrap");
    let (text, _, _) = harness.draw(&mut item, 40, 10);
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[1].starts_with("     "), "a continuation row has no number: {:?}", lines[1]);
    assert!(lines.iter().any(|l| l.starts_with("   2 short")), "{text}");
    assert!(lines[0].starts_with("   1 word"));
    // Vertical motion follows the wrapped rows, and the cursor is drawn on its row.
    act(&mut item, &mut harness, "editor::MoveDown");
    let (_, cursor, _) = harness.draw(&mut item, 40, 10);
    assert_eq!(cursor.map(|c| c.row), Some(1));
    act(&mut item, &mut harness, "editor::ToggleSoftWrap");
    let (text, _, _) = harness.draw(&mut item, 40, 10);
    assert_eq!(text.lines().filter(|l| l.contains("word")).count(), 1);
    // Markdown wraps on its own.
    let mut page = EditorItem::page("notes", &long, Some("Markdown"), &mut harness.cx());
    let (text, _, _) = harness.draw(&mut page, 40, 10);
    assert!(text.lines().filter(|l| l.contains("word")).count() > 1);
}

#[test]
fn scrolling_margin_relative_numbers_and_page_keys() {
    let mut harness = Harness::new("/project");
    harness.settings.breadcrumbs = false;
    harness.settings.vertical_scroll_margin = 3;
    let text: String = (1..=200).map(|n| format!("line {n}\n")).collect();
    let mut item = plain(&mut harness, &text);
    harness.draw(&mut item, 40, 20);
    for _ in 0..30 {
        act(&mut item, &mut harness, "editor::MoveDown");
    }
    let (_, cursor, _) = harness.draw(&mut item, 40, 20);
    assert_eq!(cursor.map(|c| c.row), Some(16), "three rows of margin below the cursor");
    act(&mut item, &mut harness, "editor::ScrollCursorCenter");
    let (_, cursor, _) = harness.draw(&mut item, 40, 20);
    assert_eq!(cursor.map(|c| c.row), Some(10));
    act(&mut item, &mut harness, "editor::LineDown");
    assert_eq!(item.scroll_top(), Some(21));
    act(&mut item, &mut harness, "editor::PageDown");
    assert_eq!(item.scroll_top(), Some(41));
    act(&mut item, &mut harness, "editor::PageUp");
    assert_eq!(item.scroll_top(), Some(21));
    // ctrl-v pages down and centres the cursor.
    act_with(&mut item, &mut harness, "editor::MovePageDown", json!({ "center_cursor": true }));
    let (_, cursor, _) = harness.draw(&mut item, 40, 20);
    assert_eq!(cursor.map(|c| c.row), Some(10));
    // Scrolling past the end is allowed down to the last line.
    for _ in 0..30 {
        act(&mut item, &mut harness, "editor::PageDown");
    }
    assert_eq!(item.scroll_top(), Some(200));
    // Relative numbers: the cursor line keeps its absolute number.
    harness.settings.relative_line_numbers = true;
    act(&mut item, &mut harness, "editor::MoveToBeginning");
    for _ in 0..4 {
        act(&mut item, &mut harness, "editor::MoveDown");
    }
    let (text, _, _) = harness.draw(&mut item, 40, 20);
    let lines: Vec<&str> = text.lines().collect();
    // The margin keeps three rows above the cursor.
    assert!(lines[3].trim_start().starts_with("5 "), "{:?}", lines[3]);
    assert!(lines[2].trim_start().starts_with("1 ") && lines[0].trim_start().starts_with("3 "), "{text}");
    act(&mut item, &mut harness, "editor::ToggleLineNumbers");
    let (text, _, _) = harness.draw(&mut item, 40, 20);
    assert!(text.lines().next().unwrap().starts_with("  line 2"), "{text}");
}

#[test]
fn going_to_a_line_scrolls_it_into_the_middle() {
    let dir = tempfile::tempdir().unwrap();
    let text: String = (1..=300).map(|n| format!("line {n}\n")).collect();
    let path = file(&dir, "long.txt", &text);
    let mut harness = Harness::new(dir.path());
    harness.settings.breadcrumbs = false;
    let mut item = EditorItem::open(&path, &mut harness.cx()).unwrap();
    item.go_to(200, Some(3));
    let (text, cursor, _) = harness.draw(&mut item, 40, 20);
    assert_eq!(item.position(), Some((200, 3)));
    assert_eq!(item.scroll_top(), Some(189), "line 200 sits in the middle of 20 rows");
    assert!(text.lines().nth(10).unwrap().contains("line 200"));
    assert_eq!(cursor.map(|c| c.row), Some(10));
    item.set_scroll_top(5);
    let (text, _, _) = harness.draw(&mut item, 40, 20);
    assert!(text.lines().next().unwrap().contains("line 6"));
    item.handle_mouse(&MouseEvent::new(MouseKind::ScrollDown, 10, 5), Rect::new(0, 0, 40, 20), &mut harness.cx());
    assert_eq!(item.scroll_top(), Some(8));
    assert_eq!(item.position(), Some((200, 3)));
}

// ---- mouse ----------------------------------------------------------------------------------------

#[test]
fn clicks_drags_double_and_triple_clicks_and_alt_click() {
    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir, "main.rs", SAMPLE);
    let mut harness = Harness::new(dir.path());
    harness.settings.breadcrumbs = false;
    let mut item = EditorItem::open(&path, &mut harness.cx()).unwrap();
    harness.draw(&mut item, 60, 12);
    let gutter = paint::gutter_width(9, true);
    let area = Rect::new(0, 0, 60, 12);
    let press = MouseKind::Press(MouseButton::Left);
    assert!(mouse(&mut item, &mut harness, press, gutter + 8, 1, area));
    assert_eq!(item.position(), Some((2, 9)));
    mouse(&mut item, &mut harness, MouseKind::Drag(MouseButton::Left), gutter + 14, 1, area);
    assert_eq!(item.status().position, Some((2, 15, 6)), "six characters selected");
    assert_eq!(item.query_suggestion().as_deref(), Some("answer"));
    mouse(&mut item, &mut harness, MouseKind::Release(MouseButton::Left), gutter + 14, 1, area);
    // Double click: the word. Triple: the line.
    harness.draw(&mut item, 60, 12);
    mouse(&mut item, &mut harness, press, gutter + 9, 1, area);
    mouse(&mut item, &mut harness, press, gutter + 9, 1, area);
    assert_eq!(item.query_suggestion().as_deref(), Some("answer"));
    mouse(&mut item, &mut harness, press, gutter + 9, 1, area);
    assert_eq!(item.editor.selections().primary().range(), 12..33, "the whole line with its break");
    mouse(&mut item, &mut harness, MouseKind::Release(MouseButton::Left), gutter + 9, 1, area);
    // Alt-click adds a cursor.
    mouse(&mut item, &mut harness, press, gutter + 2, 0, area);
    let alt = MouseEvent { kind: press, col: gutter + 6, row: 2, mods: Mods::ALT };
    item.handle_mouse(&alt, area, &mut harness.cx());
    assert_eq!(item.status().cursors, 2);
    // A click in the gutter selects the line; shift-click extends.
    mouse(&mut item, &mut harness, press, 1, 3, area);
    assert_eq!(item.editor.selections().primary().range(), 59..61);
    let shift = MouseEvent { kind: press, col: gutter + 3, row: 6, mods: Mods::SHIFT };
    item.handle_mouse(&shift, area, &mut harness.cx());
    assert_eq!(item.editor.selections().primary().start(), 59);
}

#[test]
fn a_short_file_marks_the_scrollbar_where_its_lines_are() {
    let mut harness = Harness::new("/project");
    harness.settings.breadcrumbs = false;
    let mut item = plain(&mut harness, "one\ntwo\nthree\n");
    act(&mut item, &mut harness, "editor::MoveDown");
    let (frame, _, _) = harness.draw(&mut item, 60, 30);
    let bar = item.geo.scrollbar;
    let rows: Vec<&str> = frame.lines().collect();
    let marker = rows.iter().position(|row| row.chars().nth(bar.x as usize) == Some('━'));
    // Three lines in a 30-row view: the track stands for 3 + 29 lines, so line 2's mark is
    // on the first row, beside it, not stretched down the bar.
    assert_eq!(marker, Some(bar.y as usize), "{frame}");
}

#[test]
fn the_minimap_and_scrollbar_jump_on_a_click() {
    let mut harness = Harness::new("/project");
    harness.settings.breadcrumbs = false;
    harness.settings.minimap = crate::settings::MinimapShow::Always;
    let text: String = (1..=400).map(|n| format!("let value_{n} = {n};\n")).collect();
    let mut item = plain(&mut harness, &text);
    let (frame, _, buf) = harness.draw(&mut item, 100, 30);
    assert!(item.geo.minimap.width >= 8, "the minimap is on by default");
    assert!(frame.lines().next().unwrap().chars().any(|c| ('\u{2800}'..='\u{28ff}').contains(&c)), "braille dots: {frame}");
    let _ = buf;
    let area = Rect::new(0, 0, 100, 30);
    let map = item.geo.minimap;
    mouse(&mut item, &mut harness, MouseKind::Press(MouseButton::Left), map.x + 2, map.y + map.height - 1, area);
    harness.draw(&mut item, 100, 30);
    assert!(item.scroll.line > 100, "the click on the bottom of the map scrolled far down: {}", item.scroll.line);
    mouse(&mut item, &mut harness, MouseKind::Release(MouseButton::Left), map.x + 2, map.y, area);
    let bar = item.geo.scrollbar;
    mouse(&mut item, &mut harness, MouseKind::Press(MouseButton::Left), bar.x, bar.y, area);
    mouse(&mut item, &mut harness, MouseKind::Release(MouseButton::Left), bar.x, bar.y, area);
    harness.draw(&mut item, 100, 30);
    assert_eq!(item.scroll.line, 0);
    harness.settings.minimap = crate::settings::MinimapShow::Never;
    harness.draw(&mut item, 100, 30);
    assert_eq!(item.geo.minimap.width, 0);
    act(&mut item, &mut harness, "editor::ToggleMinimap");
    harness.draw(&mut item, 100, 30);
    assert!(item.geo.minimap.width > 0);
}

// ---- colours, git marks ---------------------------------------------------------------------------

#[test]
fn highlights_arrive_from_the_project_and_colour_the_text() {
    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir, "main.rs", SAMPLE);
    let mut harness = Harness::new(dir.path());
    harness.settings.breadcrumbs = false;
    let mut item = EditorItem::open(&path, &mut harness.cx()).unwrap();
    let plain_frame = harness.draw(&mut item, 60, 10).2;
    settle_highlights(&mut item, &mut harness);
    let coloured = harness.draw(&mut item, 60, 10).2;
    let gutter = paint::gutter_width(9, true);
    assert_ne!(coloured[(gutter, 0)].fg, plain_frame[(gutter, 0)].fg, "`fn` is a keyword");
    assert!(!item.tick(&mut harness.cx()), "nothing new after the draw");
}

#[test]
fn colours_stay_in_place_while_typing_until_new_ones_arrive() {
    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir, "main.rs", SAMPLE);
    let mut harness = Harness::new(dir.path());
    harness.settings.breadcrumbs = false;
    let mut item = EditorItem::open(&path, &mut harness.cx()).unwrap();
    harness.draw(&mut item, 60, 10);
    settle_highlights(&mut item, &mut harness);
    let before = harness.draw(&mut item, 60, 10).2;
    let gutter = paint::gutter_width(9, true);
    let keyword = before[(gutter, 0)].fg;
    assert_ne!(keyword, crate::ui::color(harness.theme.editor_fg));
    // A new first line pushes `fn main` down; before the new highlights arrive the keyword
    // colour must already be on the shifted line, and a typed character must not break it.
    item.insert = true;
    item.editor.set_cursor(0);
    act(&mut item, &mut harness, "editor::NewlineAbove");
    act(&mut item, &mut harness, "editor::MoveDown");
    act(&mut item, &mut harness, "editor::MoveToBeginning");
    act(&mut item, &mut harness, "editor::MoveDown");
    type_text(&mut item, &mut harness, " ");
    assert!(!item.shifts.is_empty());
    let during = harness.draw(&mut item, 60, 10).2;
    assert_eq!(during[(gutter + 1, 1)].fg, keyword, "`fn` is still a keyword one column over, before any new highlight");
    settle_highlights(&mut item, &mut harness);
    let after = harness.draw(&mut item, 60, 10).2;
    assert_eq!(after[(gutter + 1, 1)].fg, keyword);
    assert!(item.shifts.is_empty());
}

#[test]
fn git_marks_follow_the_hunks() {
    let hunk = |kind, new_start, new_lines| LineHunk { kind, new_start, new_lines, old_start: 0, old_lines: 0 };
    let marks = GitMarks::default();
    assert_eq!(marks.mark(3, 10), None);
    let mut marks = GitMarks::default();
    marks.hunks =
        vec![hunk(LineHunkKind::Added, 1, 2), hunk(LineHunkKind::Deleted, 5, 0), hunk(LineHunkKind::Modified, 7, 1), hunk(LineHunkKind::Deleted, 10, 0)];
    let at = |line| marks.mark(line, 10);
    assert_eq!(at(0), None);
    assert_eq!((at(1), at(2), at(3)), (Some(GitMark::Added), Some(GitMark::Added), None));
    assert_eq!(at(5), Some(GitMark::DeletedAbove));
    assert_eq!(at(7), Some(GitMark::Modified));
    assert_eq!(at(9), Some(GitMark::DeletedBelow), "lines deleted at the end show on the last line");
}

#[test]
fn the_gutter_marks_what_differs_from_head_and_follows_edits() {
    let dir = crate::project::test_support::repo();
    let path = dir.path().join("src/lib.rs");
    let mut harness = Harness::with_project(crate::project::Project::open(dir.path(), crate::background::Waker::detached()));
    harness.project.settle(Duration::from_secs(20));
    harness.settings.breadcrumbs = false;
    let mut item = EditorItem::open(&path, &mut harness.cx()).unwrap();
    item.insert = true;
    // The committed text arrives through the project.
    let started = Instant::now();
    while item.git.base.is_none() {
        harness.draw(&mut item, 50, 8);
        harness.project.poll();
        assert!(started.elapsed() < Duration::from_secs(20), "no committed text");
        std::thread::sleep(Duration::from_millis(10));
    }
    let (_, _, buf) = harness.draw(&mut item, 50, 8);
    assert_eq!(buf[(0, 1)].symbol(), " ", "unchanged");
    item.go_to(2, None);
    act(&mut item, &mut harness, "editor::MoveToEndOfLine");
    type_text(&mut item, &mut harness, "0");
    let (_, _, buf) = harness.draw(&mut item, 50, 8);
    assert_eq!(buf[(0, 1)].symbol(), "▎", "the edited line is marked modified");
    assert_eq!(buf[(0, 1)].fg, crate::ui::color(harness.theme.modified));
    // A line added above moves the mark down with it.
    item.go_to(1, None);
    act(&mut item, &mut harness, "editor::NewlineAbove");
    let (text, _, buf) = harness.draw(&mut item, 50, 8);
    assert!(text.contains("42"), "{text}");
    assert_eq!(buf[(0, 2)].symbol(), "▎", "the mark moved with its line");
}

// ---- files ------------------------------------------------------------------------------------------

#[test]
fn saving_cleans_up_formats_and_reports() {
    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir, "notes.txt", "a  \nb");
    let mut harness = Harness::new(dir.path());
    let mut item = EditorItem::open(&path, &mut harness.cx()).unwrap();
    item.insert = true;
    type_text(&mut item, &mut harness, "x");
    assert!(item.is_dirty());
    assert!(item.tab(&harness.render_cx(false)).dirty);
    item.save(true, &mut harness.cx()).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "xa\nb\n", "trailing space gone, final newline added");
    assert!(!item.is_dirty());
    // Settings turn the clean-ups off, per language too.
    harness.settings.remove_trailing_whitespace_on_save = false;
    harness.settings.ensure_final_newline_on_save = false;
    item.editor.edit(&[(0..0, "  ")]);
    item.editor.edit(&[(5..5, "  ")]);
    item.save(true, &mut harness.cx()).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "  xa\n  b\n");
}

#[test]
fn format_on_save_runs_the_formatter_and_failure_is_said() {
    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir, "notes.txt", "hello\nworld\n");
    let mut harness = Harness::new(dir.path());
    harness.settings.format_on_save = true;
    harness.settings.formatters.insert("Plain Text".into(), vec!["tr".into(), "a-z".into(), "A-Z".into()]);
    let mut item = EditorItem::open(&path, &mut harness.cx()).unwrap();
    item.editor.set_cursor(8);
    item.insert = true;
    type_text(&mut item, &mut harness, "!");
    item.save(true, &mut harness.cx()).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "HELLO\nWO!RLD\n");
    assert_eq!(text_of(&item), "HELLO\nWO!RLD\n");
    assert_eq!(item.position(), Some((2, 4)), "the cursor stays with its text");
    // Without formatting the text is left alone.
    type_text(&mut item, &mut harness, "a");
    item.save(false, &mut harness.cx()).unwrap();
    assert!(std::fs::read_to_string(&path).unwrap().contains("WO!aRLD"));
    // A formatter that fails does not stop the save, and the failure is on the status line.
    harness.settings.formatters.insert("Plain Text".into(), vec!["sh".into(), "-c".into(), "echo broken >&2; exit 3".into()]);
    type_text(&mut item, &mut harness, "b");
    harness.take_effects();
    item.save(true, &mut harness.cx()).unwrap();
    assert!(std::fs::read_to_string(&path).unwrap().contains("WO!abRLD"), "saved anyway");
    let effects = harness.take_effects();
    assert!(effects.iter().any(|e| matches!(e, Effect::Status { text, error: true } if text.contains("without formatting") && text.contains("broken"))), "{effects:?}");
    // editor::Format formats in the background and says so.
    harness.settings.formatters.insert("Plain Text".into(), vec!["tr".into(), "a-z".into(), "A-Z".into()]);
    act(&mut item, &mut harness, "editor::Format");
    let started = Instant::now();
    while text_of(&item).contains("abR") {
        harness.project.poll();
        item.tick(&mut harness.cx());
        assert!(started.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(text_of(&item).contains("WO!ABRLD"));
}

#[test]
fn a_slow_formatter_does_not_hold_the_save() {
    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir, "notes.txt", "abc\n");
    let mut harness = Harness::new(dir.path());
    harness.settings.format_on_save = true;
    harness.settings.formatters.insert("Plain Text".into(), vec!["sh".into(), "-c".into(), "sleep 0.6; tr a-z A-Z".into()]);
    let mut item = EditorItem::open(&path, &mut harness.cx()).unwrap();
    item.insert = true;
    type_text(&mut item, &mut harness, "x");
    let started = Instant::now();
    item.save(true, &mut harness.cx()).unwrap();
    assert!(started.elapsed() < Duration::from_millis(500), "returned before the formatter was done");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "xabc\n", "the text as it was is on disk already");
    assert!(!item.is_dirty());
    let started = Instant::now();
    while text_of(&item) != "XABC\n" {
        harness.project.poll();
        item.tick(&mut harness.cx());
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}", text_of(&item));
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "XABC\n", "formatted and saved again");
    assert!(!item.is_dirty());
}

#[test]
fn untitled_buffers_ask_where_and_save_as_writes_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = Harness::new(dir.path());
    let mut item = plain(&mut harness, "");
    item.insert = true;
    type_text(&mut item, &mut harness, "fn main() {}");
    assert!(item.is_dirty());
    assert!(item.save(true, &mut harness.cx()).is_err(), "a close that saved first must not close");
    assert!(harness.take_effects().iter().any(|e| matches!(e, Effect::Dispatch(a) if a.name == "workspace::SaveAs")));
    let target = dir.path().join("sub/new.rs");
    assert!(act_with(&mut item, &mut harness, "workspace::SaveAsPath", json!({ "path": target.to_string_lossy() })));
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "fn main() {}\n");
    assert_eq!(item.path(), Some(target.as_path()));
    assert_eq!(item.tab(&harness.render_cx(false)).title, "new.rs");
    assert_eq!(item.status().language.as_deref(), Some("Rust"));
    assert!(!item.is_dirty());
    let effects = harness.take_effects();
    assert!(effects.iter().any(|e| matches!(e, Effect::PathsChanged(paths) if paths == &vec![target.clone()])));
}

#[test]
fn a_file_changed_on_disk_is_reloaded_or_asked_about() {
    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir, "notes.md", "one\n");
    let mut harness = Harness::new(dir.path());
    harness.settings.breadcrumbs = false;
    let mut item = EditorItem::open(&path, &mut harness.cx()).unwrap();
    std::fs::write(&path, "one\ntwo\n").unwrap();
    item.paths_changed(&FsChange::of(vec![dir.path().join("other.md")]), &mut harness.cx());
    assert!(!harness.draw(&mut item, 30, 4).0.contains("two"), "another file's change is not ours");
    item.paths_changed(&FsChange::of(vec![path.clone()]), &mut harness.cx());
    assert!(harness.draw(&mut item, 30, 4).0.contains("two"));
    // Our own save reports as a change too; it must not disturb anything.
    item.insert = true;
    type_text(&mut item, &mut harness, "z");
    item.save(true, &mut harness.cx()).unwrap();
    harness.take_effects();
    item.paths_changed(&FsChange::of(vec![path.clone()]), &mut harness.cx());
    assert!(text_of(&item).starts_with("zone"));
    assert!(harness.take_effects().is_empty());
    // Deleted: what was there stays on screen.
    std::fs::remove_file(&path).unwrap();
    item.paths_changed(&FsChange::of(vec![path.clone()]), &mut harness.cx());
    assert!(harness.draw(&mut item, 30, 4).0.contains("two"));
    // With unsaved changes the editor asks instead.
    std::fs::write(&path, "disk\n").unwrap();
    type_text(&mut item, &mut harness, "w");
    harness.take_effects();
    // Not the front tab: it asks when it is shown.
    item.paths_changed(&FsChange::of(vec![path.clone()]), &mut harness.cx());
    assert!(harness.take_effects().is_empty());
    item.focus_changed(true, &mut harness.cx());
    let effects = harness.take_effects();
    let Some(Effect::Ask(question)) = effects.first() else { panic!("{effects:?}") };
    assert!(question.message.contains("notes.md changed on disk"));
    assert_eq!(question.answers.last().unwrap().0, "Keep mine");
    assert!(act(&mut item, &mut harness, "editor::ReloadFromDisk"));
    assert_eq!(text_of(&item), "disk\n");
}

#[test]
fn autosave_on_focus_change_and_after_a_delay() {
    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir, "a.txt", "one\n");
    let mut harness = Harness::new(dir.path());
    harness.settings.autosave = crate::settings::Autosave::OnFocusChange;
    let mut item = EditorItem::open(&path, &mut harness.cx()).unwrap();
    item.insert = true;
    type_text(&mut item, &mut harness, "x");
    item.focus_changed(false, &mut harness.cx());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "xone\n");
    harness.settings.autosave = crate::settings::Autosave::AfterDelay { milliseconds: 50 };
    type_text(&mut item, &mut harness, "y");
    item.tick(&mut harness.cx());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "xone\n", "not yet");
    std::thread::sleep(Duration::from_millis(80));
    let mut cx = harness.cx();
    cx.now = Instant::now();
    item.tick(&mut cx);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "xyone\n");
}

#[test]
fn the_permalink_goes_to_the_clipboard() {
    let dir = crate::project::test_support::repo();
    crate::project::test_support::git(dir.path(), &["remote", "add", "origin", "https://github.com/acme/widgets.git"]);
    let path = dir.path().join("src/lib.rs");
    let mut harness = Harness::with_project(crate::project::Project::open(dir.path(), crate::background::Waker::detached()));
    harness.project.settle(Duration::from_secs(20));
    let mut item = EditorItem::open(&path, &mut harness.cx()).unwrap();
    item.go_to(2, None);
    assert!(act(&mut item, &mut harness, "editor::CopyPermalinkToLine"));
    let url = harness.clipboard.get().unwrap().text;
    assert!(url.starts_with("https://github.com/acme/widgets/blob/") && url.ends_with("/src/lib.rs#L2"), "{url}");
    act(&mut item, &mut harness, "editor::SelectToEnd");
    act(&mut item, &mut harness, "editor::CopyPermalinkToLine");
    assert!(harness.clipboard.get().unwrap().text.ends_with("#L2-L3"), "{}", harness.clipboard.get().unwrap().text);
}

// ---- pickers and the rest -----------------------------------------------------------------------------

#[test]
fn the_outline_lists_symbols_previews_and_jumps() {
    let dir = tempfile::tempdir().unwrap();
    let mut text = SAMPLE.to_string();
    text.push_str(&"// filler\n".repeat(80));
    text.push_str("fn far_away() {}\n");
    let path = file(&dir, "main.rs", &text);
    let mut harness = Harness::new(dir.path());
    let mut item = EditorItem::open(&path, &mut harness.cx()).unwrap();
    harness.draw(&mut item, 60, 10);
    let rows = item.picker_rows(PickerKind::Outline, &mut harness.cx()).expect("rust has an outline");
    let labels: Vec<&str> = rows.iter().map(|row| row.label.trim()).collect();
    assert!(labels.contains(&"main") && labels.contains(&"Point") && labels.contains(&"far_away"), "{labels:?}");
    let far = labels.iter().position(|l| *l == "far_away").unwrap();
    assert_eq!(rows[far].hint.as_deref(), Some("89"), "8 lines of sample, 80 of filler, then the function");
    item.picker_preview(PickerKind::Outline, Some(far), &mut harness.cx());
    assert!(item.scroll_top().unwrap() > 80);
    item.picker_preview(PickerKind::Outline, None, &mut harness.cx());
    assert_eq!(item.scroll_top(), Some(0));
    item.picker_confirm(PickerKind::Outline, far, false, &mut harness.cx());
    assert_eq!(item.position(), Some((89, 4)), "on the name of the function");
    let notes = file(&dir, "notes.txt", "hello\n");
    let mut plain_item = EditorItem::open(&notes, &mut harness.cx()).unwrap();
    assert!(plain_item.picker_rows(PickerKind::Outline, &mut harness.cx()).is_none());
    assert!(plain_item.picker_rows(PickerKind::Links, &mut harness.cx()).is_none());
}

#[test]
fn tab_sizes_follow_language_conventions_and_settings() {
    let mut settings = Settings::default();
    let json = bakedit_syntax::language_by_id("json").unwrap();
    let rust = bakedit_syntax::language_by_id("rust").unwrap();
    assert_eq!(effective_tab_size(&settings, json), 2);
    assert_eq!(effective_tab_size(&settings, rust), 4);
    settings.tab_size = 3;
    assert_eq!(effective_tab_size(&settings, rust), 3, "the global setting where the language has no convention");
    settings.languages.entry("JSON".into()).or_default().tab_size = Some(8);
    assert_eq!(effective_tab_size(&settings, json), 8, "the user's language setting wins");
}

#[test]
fn a_big_file_stays_quick() {
    let dir = tempfile::tempdir().unwrap();
    let text: String = (0..50_000).map(|n| format!("    let value_{n} = compute({n}, \"text\");\n")).collect();
    let path = file(&dir, "big.rs", &text);
    let mut harness = Harness::new(dir.path());
    let started = Instant::now();
    let mut item = EditorItem::open(&path, &mut harness.cx()).unwrap();
    harness.draw(&mut item, 160, 50);
    assert!(started.elapsed() < Duration::from_secs(2), "open and first frame took {:?}", started.elapsed());
    // Hold the down key: every repeat moves and draws.
    let started = Instant::now();
    for _ in 0..300 {
        act(&mut item, &mut harness, "editor::MoveDown");
        harness.draw(&mut item, 160, 50);
    }
    assert!(started.elapsed() < Duration::from_secs(3), "300 moves and frames took {:?}", started.elapsed());
    // And typing, which also asks for new colours each time.
    item.insert = true;
    let started = Instant::now();
    for _ in 0..100 {
        type_text(&mut item, &mut harness, "a");
        harness.draw(&mut item, 160, 50);
    }
    assert!(started.elapsed() < Duration::from_secs(3), "100 keystrokes and frames took {:?}", started.elapsed());
    item.go_to(49_000, None);
    let (frame, _, _) = harness.draw(&mut item, 160, 50);
    assert!(frame.contains("value_48"));
}

// ---- through the real shell and keymap -------------------------------------------------------------

#[test]
fn the_keymap_drives_nav_mode_edit_mode_and_the_find_bar() {
    use crate::clipboard::{Clipboard, MemoryBackend};
    use crate::factory::{AppFactory, AppSource};
    use crate::keys::KeyRouter;
    use crate::shell::Shell;
    use bakedit_core::Key;

    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir, "a.txt", "hello world\nsecond foo line\nthird foo\n");
    let settings = Settings { vim_mode: true, breadcrumbs: false, ..Settings::default() };
    let mut shell = Shell::new(
        settings,
        bakedit_core::Theme::builtin(),
        crate::project::Project::detached(dir.path()),
        Clipboard::with_backend(Box::new(MemoryBackend::default())),
        Box::new(AppFactory),
        Box::new(AppSource::new(dir.path().to_path_buf())),
    );
    let (mut router, warnings) = KeyRouter::new(None);
    assert!(warnings.is_empty());
    shell.open_file(&path, None, None, false, true);
    let mut send = |shell: &mut Shell, event: KeyEvent| {
        router.key(shell, &event);
    };
    let frame = |shell: &mut Shell| {
        let mut buf = ratatui::buffer::Buffer::empty(Rect::new(0, 0, 80, 12));
        shell.render(&mut buf);
        crate::dump::to_text(&buf)
    };
    // Nav mode: arrows move, letters do nothing.
    for c in ['z', 'j', 'l', 'l'] {
        send(&mut shell, KeyEvent::typed(c));
    }
    assert!(frame(&mut shell).contains("hello world"));
    assert_eq!(shell.pane.active().unwrap().position(), Some((1, 1)));
    for key in [Key::Down, Key::Right, Key::Right] {
        send(&mut shell, KeyEvent::new(key, Mods::empty()));
    }
    assert_eq!(shell.pane.active().unwrap().position(), Some((2, 3)));
    // cmd-i then typing, escape.
    send(&mut shell, KeyEvent::new(Key::Char('i'), Mods::SUPER));
    for c in "XY".chars() {
        send(&mut shell, KeyEvent::typed(c));
    }
    send(&mut shell, KeyEvent::new(Key::Escape, Mods::empty()));
    assert!(frame(&mut shell).contains("seXYcond foo line"));
    assert_eq!(shell.pane.active().unwrap().status().mode, Some(ModeIndicator::Nav));
    // cmd-f opens the find bar; typing searches; enter steps; escape closes.
    send(&mut shell, KeyEvent::new(Key::Char('f'), Mods::SUPER));
    for c in "foo".chars() {
        send(&mut shell, KeyEvent::typed(c));
    }
    let text = frame(&mut shell);
    assert!(text.contains("Find  foo") && text.contains("1 of 2"), "{text}");
    send(&mut shell, KeyEvent::new(Key::Enter, Mods::empty()));
    assert!(frame(&mut shell).contains("2 of 2"));
    send(&mut shell, KeyEvent::new(Key::Escape, Mods::empty()));
    assert!(!frame(&mut shell).contains("Find"));
    assert_eq!(shell.pane.active().unwrap().position().map(|p| p.0), Some(3));
    // d and u do nothing; cmd-x cuts the line and cmd-z brings it back, as in edit mode.
    send(&mut shell, KeyEvent::typed('d'));
    send(&mut shell, KeyEvent::typed('u'));
    assert!(frame(&mut shell).contains("third foo"));
    send(&mut shell, KeyEvent::new(Key::Char('x'), Mods::SUPER));
    assert!(!frame(&mut shell).contains("third foo"));
    send(&mut shell, KeyEvent::new(Key::Char('z'), Mods::SUPER));
    assert!(frame(&mut shell).contains("third foo"));
    // cmd-s saves what was typed.
    send(&mut shell, KeyEvent::new(Key::Char('s'), Mods::SUPER));
    assert!(std::fs::read_to_string(&path).unwrap().contains("seXYcond"));
}

/// The app's own shell and items over `dir`, and the built-in keymap.
fn app_shell(dir: &tempfile::TempDir, settings: Settings) -> (crate::shell::Shell, crate::keys::KeyRouter) {
    use crate::clipboard::{Clipboard, MemoryBackend};
    use crate::factory::{AppFactory, AppSource};
    let shell = crate::shell::Shell::new(
        settings,
        bakedit_core::Theme::builtin(),
        crate::project::Project::detached(dir.path()),
        Clipboard::with_backend(Box::new(MemoryBackend::default())),
        Box::new(AppFactory),
        Box::new(AppSource::new(dir.path().to_path_buf())),
    );
    let (router, warnings) = crate::keys::KeyRouter::new(None);
    assert!(warnings.is_empty());
    (shell, router)
}

/// The whole window as text, and the cursor it asked for.
fn shell_frame(shell: &mut crate::shell::Shell, width: u16, height: u16) -> (String, Option<Cursor>) {
    let mut buf = ratatui::buffer::Buffer::empty(Rect::new(0, 0, width, height));
    let cursor = shell.render(&mut buf);
    (crate::dump::to_text(&buf), cursor)
}

#[test]
fn cmd_i_and_ctrl_bracket_i_switch_the_mode_through_the_keymap() {
    use crate::shell::Shell;
    use bakedit_core::Key;

    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir, "a.txt", "hello world\n");
    let (mut shell, mut router) = app_shell(&dir, Settings { vim_mode: true, breadcrumbs: false, ..Settings::default() });
    shell.open_file(&path, None, None, false, true);
    let mode = |shell: &Shell| shell.pane.active().unwrap().status().mode;
    let position = |shell: &Shell| shell.pane.active().unwrap().position();
    let text = |shell: &Shell| text_of(shell.pane.active().unwrap().as_any().downcast_ref::<EditorItem>().unwrap());
    let cmd_i = KeyEvent::new(Key::Char('i'), Mods::SUPER);
    let ctrl_bracket = KeyEvent::new(Key::Char(']'), Mods::CTRL);
    // cmd-i in nav mode, then in edit mode.
    for _ in 0..6 {
        router.key(&mut shell, &KeyEvent::new(Key::Right, Mods::empty()));
    }
    router.key(&mut shell, &cmd_i);
    assert_eq!(mode(&shell), Some(ModeIndicator::Edit));
    assert_eq!(position(&shell), Some((1, 7)));
    router.key(&mut shell, &KeyEvent::typed('x'));
    router.key(&mut shell, &cmd_i);
    assert_eq!(mode(&shell), Some(ModeIndicator::Nav));
    assert_eq!(position(&shell), Some((1, 8)), "the cursor stays after the x");
    assert_eq!(text(&shell), "hello xworld\n");
    // ctrl-] i, both ways; the i of the chord is not typed.
    router.key(&mut shell, &ctrl_bracket);
    router.key(&mut shell, &KeyEvent::typed('i'));
    assert_eq!(mode(&shell), Some(ModeIndicator::Edit));
    router.key(&mut shell, &ctrl_bracket);
    router.key(&mut shell, &KeyEvent::typed('i'));
    assert_eq!(mode(&shell), Some(ModeIndicator::Nav));
    assert_eq!(text(&shell), "hello xworld\n");
    // With a selection made with shift-right.
    router.key(&mut shell, &KeyEvent::new(Key::Right, Mods::SHIFT));
    assert_eq!(mode(&shell), Some(ModeIndicator::Nav));
    router.key(&mut shell, &cmd_i);
    assert_eq!(mode(&shell), Some(ModeIndicator::Edit));
    router.key(&mut shell, &cmd_i);
    // With the modes off, cmd-i turns them on for the session and lands in nav mode.
    shell.dispatch(&Action::new("bakedit::ToggleEditMode"));
    shell.tick();
    assert_eq!(mode(&shell), None);
    router.key(&mut shell, &cmd_i);
    assert_eq!(mode(&shell), Some(ModeIndicator::Nav));
    assert!(shell.settings.vim_mode);
    assert!(shell.status_message().is_some_and(|(text, _)| text.starts_with("Nav and edit modes on")));
    shell.tick();
    assert_eq!(mode(&shell), Some(ModeIndicator::Nav), "the next tick keeps it");
}

#[test]
fn cmd_i_from_a_selection_and_back_leaves_no_selection_in_nav_mode() {
    use bakedit_core::Key;

    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir, "a.txt", "hello world\nsecond\n");
    let (mut shell, mut router) = app_shell(&dir, Settings { vim_mode: true, breadcrumbs: false, ..Settings::default() });
    shell.open_file(&path, None, None, false, true);
    let cmd_i = KeyEvent::new(Key::Char('i'), Mods::SUPER);
    for _ in 0..5 {
        router.key(&mut shell, &KeyEvent::new(Key::Right, Mods::SHIFT));
    }
    router.key(&mut shell, &cmd_i);
    let editor = |shell: &crate::shell::Shell| shell.pane.active().unwrap().as_any().downcast_ref::<EditorItem>().unwrap().editor.selections().clone();
    assert!(!editor(&shell).is_single_cursor(), "edit mode keeps the selection to replace");
    router.key(&mut shell, &cmd_i);
    assert_eq!(shell.pane.active().unwrap().status().mode, Some(ModeIndicator::Nav));
    assert!(editor(&shell).is_single_cursor(), "nav mode starts with a cursor, not a selection");
    // And a letter there changes nothing.
    router.key(&mut shell, &KeyEvent::typed('d'));
    let text = text_of(shell.pane.active().unwrap().as_any().downcast_ref::<EditorItem>().unwrap());
    assert_eq!(text, "hello world\nsecond\n");
}

#[test]
fn cmd_i_from_the_find_bar_switches_the_texts_mode_through_the_keymap() {
    use bakedit_core::Key;

    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir, "a.txt", "hello world\n");
    let (mut shell, mut router) = app_shell(&dir, Settings { vim_mode: true, breadcrumbs: false, ..Settings::default() });
    shell.open_file(&path, None, None, false, true);
    let item = |shell: &crate::shell::Shell| shell.pane.active().unwrap().as_any().downcast_ref::<EditorItem>().unwrap().find_focused();
    let cmd_i = KeyEvent::new(Key::Char('i'), Mods::SUPER);
    router.key(&mut shell, &KeyEvent::new(Key::Char('f'), Mods::SUPER));
    assert!(item(&shell), "the find bar has the keys");
    router.key(&mut shell, &cmd_i);
    assert_eq!(shell.pane.active().unwrap().status().mode, Some(ModeIndicator::Edit));
    assert!(item(&shell), "the bar keeps the keys");
    router.key(&mut shell, &cmd_i);
    assert_eq!(shell.pane.active().unwrap().status().mode, Some(ModeIndicator::Nav));
}

#[test]
fn nav_mode_keys_change_nothing_but_the_rest_work_as_in_edit_mode() {
    use crate::shell::Shell;
    use bakedit_core::Key;

    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir, "a.txt", "one two\nthree\nfour\n");
    let (mut shell, mut router) = app_shell(&dir, Settings { vim_mode: true, breadcrumbs: false, ..Settings::default() });
    shell.open_file(&path, None, None, false, true);
    let mode = |shell: &Shell| shell.pane.active().unwrap().status().mode;
    let status = |shell: &Shell| shell.pane.active().unwrap().status().position.unwrap();
    let text = |shell: &Shell| text_of(shell.pane.active().unwrap().as_any().downcast_ref::<EditorItem>().unwrap());
    let key = |key: Key, mods: Mods| KeyEvent::new(key, mods);
    let with_text = |key: Key, text: &str| KeyEvent { text: Some(text.to_string()), ..KeyEvent::new(key, Mods::empty()) };
    let start = text(&shell);
    router.key(&mut shell, &key(Key::Right, Mods::empty()));
    assert_eq!(status(&shell), (1, 2, 0));

    // The old vim letters, digits, punctuation and the space leader: nothing moves or changes.
    for c in "hjklwbe0^$gGG{}%iaIAoOvVydcpxuUJ><nN*HL:/ fq".chars() {
        router.key(&mut shell, &KeyEvent::typed(c));
        assert_eq!((text(&shell), status(&shell), mode(&shell)), (start.clone(), (1, 2, 0), Some(ModeIndicator::Nav)), "{c:?}");
    }
    assert!(shell.modal.is_none(), "no picker or palette opened");
    // Enter, tab, backspace and delete edit nothing, with or without the text a terminal sends.
    let plain = [Key::Enter, Key::Tab, Key::Backspace, Key::Delete];
    for k in plain {
        for mods in [Mods::empty(), Mods::SHIFT] {
            router.key(&mut shell, &key(k, mods));
            assert_eq!((text(&shell), status(&shell)), (start.clone(), (1, 2, 0)), "{k:?} {mods:?}");
        }
    }
    router.key(&mut shell, &with_text(Key::Enter, "\r"));
    router.key(&mut shell, &with_text(Key::Tab, "\t"));
    assert_eq!((text(&shell), status(&shell)), (start.clone(), (1, 2, 0)));
    // Nor do their emacs ctrl aliases (ctrl-h is also what a legacy terminal sends for
    // backspace); ctrl-d no longer scrolls either.
    for c in "hdwkyt".chars() {
        router.key(&mut shell, &key(Key::Char(c), Mods::CTRL));
        assert_eq!((text(&shell), status(&shell)), (start.clone(), (1, 2, 0)), "ctrl-{c}");
    }
    assert!(!shell.pane.active().unwrap().is_dirty());

    // Arrows, shift-arrows and cmd-up/down work as in edit mode.
    router.key(&mut shell, &key(Key::Down, Mods::empty()));
    assert_eq!(status(&shell), (2, 2, 0));
    router.key(&mut shell, &key(Key::Right, Mods::SHIFT));
    router.key(&mut shell, &key(Key::Right, Mods::SHIFT));
    assert_eq!(status(&shell), (2, 4, 2), "two characters selected");
    router.key(&mut shell, &key(Key::Escape, Mods::empty()));
    assert_eq!(status(&shell).2, 0, "escape lets go");
    assert_eq!(mode(&shell), Some(ModeIndicator::Nav));
    router.key(&mut shell, &key(Key::Up, Mods::SUPER));
    assert_eq!(status(&shell), (1, 4, 0), "cmd-up jumps 5 lines, here to the first");
    router.key(&mut shell, &key(Key::Down, Mods::SUPER));
    assert_eq!(status(&shell).0, 4, "cmd-down jumps 5 lines, here to the last");

    // cmd-i into edit mode, where typing works; cmd-s saves from nav mode.
    router.key(&mut shell, &key(Key::Up, Mods::SUPER));
    router.key(&mut shell, &key(Key::Left, Mods::SUPER));
    assert_eq!(status(&shell), (1, 1, 0));
    router.key(&mut shell, &KeyEvent::new(Key::Char('i'), Mods::SUPER));
    assert_eq!(mode(&shell), Some(ModeIndicator::Edit));
    for c in "hi".chars() {
        router.key(&mut shell, &KeyEvent::typed(c));
    }
    router.key(&mut shell, &key(Key::Enter, Mods::empty()));
    assert_eq!(text(&shell), "hi\none two\nthree\nfour\n");
    router.key(&mut shell, &KeyEvent::new(Key::Char('i'), Mods::SUPER));
    assert_eq!(mode(&shell), Some(ModeIndicator::Nav));
    router.key(&mut shell, &KeyEvent::new(Key::Char('s'), Mods::SUPER));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "hi\none two\nthree\nfour\n");
}

#[test]
fn a_click_on_the_mode_in_the_status_bar_switches_it() {
    use crate::shell::Shell;
    use crate::workspace::{Focus, PanelId};

    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir, "a.txt", "hello world\n");
    let settings = Settings { vim_mode: true, breadcrumbs: false, usage_meter: false, ..Settings::default() };
    let (mut shell, _) = app_shell(&dir, settings);
    shell.open_file(&path, None, None, false, true);
    let mode = |shell: &Shell| shell.pane.active().unwrap().status().mode;
    let press = |shell: &mut Shell, col: u16| shell.handle_mouse(&MouseEvent::new(MouseKind::Press(MouseButton::Left), col, 11));
    let (text, cursor) = shell_frame(&mut shell, 80, 12);
    assert!(text.lines().last().unwrap().starts_with(" NAV "), "{text}");
    assert_eq!(cursor.map(|c| c.shape), Some(CursorShape::Block));
    // A click on it: edit mode, and the pill and the cursor follow at once.
    assert!(press(&mut shell, 2));
    assert_eq!(mode(&shell), Some(ModeIndicator::Edit));
    let (text, cursor) = shell_frame(&mut shell, 80, 12);
    assert!(text.lines().last().unwrap().starts_with(" EDIT "), "{text}");
    assert_eq!(cursor.map(|c| c.shape), Some(CursorShape::Bar));
    // Its last cell counts; the bar beside it does not.
    press(&mut shell, 12);
    assert_eq!(mode(&shell), Some(ModeIndicator::Edit));
    assert!(press(&mut shell, 5));
    assert_eq!(mode(&shell), Some(ModeIndicator::Nav));
    // With a picker open the click is the picker's.
    shell.dispatch(&Action::new("file_finder::Toggle"));
    shell_frame(&mut shell, 80, 12);
    press(&mut shell, 2);
    assert_eq!(mode(&shell), Some(ModeIndicator::Nav));
    shell.dispatch(&Action::new("menu::Cancel"));
    // With the keyboard in a panel there is no mode shown, and nothing to click.
    shell.set_focus(Focus::Panel(PanelId::Terminal));
    let (text, _) = shell_frame(&mut shell, 80, 12);
    assert!(!text.lines().last().unwrap().contains("NAV"), "{text}");
    press(&mut shell, 2);
    assert_eq!(mode(&shell), Some(ModeIndicator::Nav));
}

/// Writes frames as HTML for looking at, when `BAKEDIT_SNAPSHOT_DIR` names a folder.
#[test]
fn write_snapshots_for_looking_at() {
    let Ok(out) = std::env::var("BAKEDIT_SNAPSHOT_DIR") else { return };
    let dir = tempfile::tempdir().unwrap();
    let mut sample = String::new();
    for n in 0..60 {
        sample.push_str(&format!("fn function_{n}(value: i32) -> i32 {{\n    // a comment about {n}\n    let result = value * {n} + 42;\n    println!(\"{{result}}\");\n    result\n}}\n\n"));
    }
    std::fs::create_dir(dir.path().join("src")).unwrap();
    let path = file(&dir, "src/main.rs", &sample);
    let mut harness = Harness::new(dir.path());
    harness.settings.minimap = crate::settings::MinimapShow::Always;
    harness.settings.vim_mode = true;
    let mut item = EditorItem::open(&path, &mut harness.cx()).unwrap();
    harness.draw(&mut item, 130, 34);
    settle_highlights(&mut item, &mut harness);
    let write = |name: &str, harness: &Harness, item: &mut EditorItem, w: u16, h: u16| {
        let (_, cursor, buf) = harness.draw(item, w, h);
        let html = crate::dump::to_html(&buf, "#dce0e5", "#282c33", 14, cursor.map(|c| (c.col, c.row)));
        std::fs::write(std::path::Path::new(&out).join(name), html).unwrap();
    };
    item.go_to(10, Some(5));
    act(&mut item, &mut harness, "editor::SelectToNextWordEnd");
    write("editor-wide.html", &harness, &mut item, 130, 34);
    act(&mut item, &mut harness, "editor::Cancel");
    act_with(&mut item, &mut harness, "buffer_search::Deploy", json!({ "replace_enabled": true }));
    type_text(&mut item, &mut harness, "result");
    write("editor-find.html", &harness, &mut item, 130, 34);
    write("editor-narrow.html", &harness, &mut item, 50, 20);
    write("editor-tiny.html", &harness, &mut item, 28, 10);
    act(&mut item, &mut harness, "buffer_search::Dismiss");
    let mut md = EditorItem::page("notes.md", &"A long paragraph of text that has to wrap onto several rows when the window is narrow. ".repeat(6), Some("Markdown"), &mut harness.cx());
    write("editor-wrap.html", &harness, &mut md, 60, 12);
}
