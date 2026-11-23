use std::path::PathBuf;
use std::time::Duration;

use bakedit_core::{Action, MouseButton, MouseEvent, MouseKind};
use serde_json::json;

use super::*;
use crate::view::test_support::Harness;

const SAMPLE: &str = "fn main() {\n    let answer = 42;\n    println!(\"{answer}\");\n}\n\nstruct Point {\n    x: i32,\n}\n";

fn file(dir: &tempfile::TempDir, name: &str, text: &str) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, text).unwrap();
    path
}

fn act(item: &mut EditorItem, harness: &mut Harness, name: &str) -> bool {
    item.handle_action(&Action::new(name), &mut harness.cx())
}

#[test]
fn opens_a_file_and_draws_it_with_a_gutter_and_the_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir, "main.rs", SAMPLE);
    let mut harness = Harness::new(dir.path());
    let mut item = EditorItem::open(&path, &mut harness.cx()).unwrap();
    assert_eq!(item.path(), Some(path.as_path()));
    assert_eq!(item.kind(), "editor");
    let (text, cursor, _) = harness.draw(&mut item, 60, 12);
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].starts_with("   1 fn main() {"), "{:?}", lines[0]);
    assert!(lines[1].contains("let answer = 42;"));
    assert!(lines[8].trim().is_empty() || !lines[8].contains("9 "), "no number past the end: {:?}", lines[8]);
    let gutter = paint::gutter_width(9, true);
    assert_eq!(cursor, Some(Cursor { col: gutter, row: 0, shape: CursorShape::Block }));
    let status = item.status();
    assert_eq!(status.position, Some((1, 1, 0)));
    assert_eq!(status.language.as_deref(), Some("Rust"));
    assert_eq!(status.line_ending, Some("LF"));
    assert_eq!(item.tab(&harness.render_cx(false)).title, "main.rs");
}

#[test]
fn motions_move_and_edits_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir, "main.rs", SAMPLE);
    let mut harness = Harness::new(dir.path());
    let mut item = EditorItem::open(&path, &mut harness.cx()).unwrap();
    assert!(act(&mut item, &mut harness, "editor::MoveDown"));
    assert_eq!(item.position(), Some((2, 1)));
    // The user's cmd-down: five lines.
    let five = Action::with_args("editor::MoveDownByLines", json!({ "lines": 5 }));
    assert!(item.handle_action(&five, &mut harness.cx()));
    assert_eq!(item.position(), Some((7, 1)));
    assert!(act(&mut item, &mut harness, "editor::MoveToEnd"));
    assert_eq!(item.position(), Some((9, 1)));
    // Read-only: nothing that changes the text is taken, so the next candidate can be.
    assert!(!act(&mut item, &mut harness, "editor::Backspace"));
    assert!(!act(&mut item, &mut harness, "editor::Newline"));
    assert!(!item.handle_key(&KeyEvent::typed('x'), &mut harness.cx()));
    assert!(!act(&mut item, &mut harness, "not::AnAction"));
    // Copying works: the whole line when nothing is selected.
    item.go_to(2, None);
    assert!(act(&mut item, &mut harness, "editor::Copy"));
    assert_eq!(harness.clipboard.get().map(|clip| clip.text), Some("    let answer = 42;\n".to_string()));
}

#[test]
fn going_to_a_line_scrolls_it_into_the_middle() {
    let dir = tempfile::tempdir().unwrap();
    let text: String = (1..=300).map(|n| format!("line {n}\n")).collect();
    let path = file(&dir, "long.txt", &text);
    let mut harness = Harness::new(dir.path());
    let mut item = EditorItem::open(&path, &mut harness.cx()).unwrap();
    item.go_to(200, Some(3));
    let (text, cursor, _) = harness.draw(&mut item, 40, 20);
    assert_eq!(item.position(), Some((200, 3)));
    assert_eq!(item.scroll_top(), Some(189), "line 200 sits in the middle of 20 rows");
    assert!(text.lines().nth(10).unwrap().contains("line 200"));
    assert_eq!(cursor.map(|c| c.row), Some(10));
    // The session restores the scroll as it was.
    item.set_scroll_top(5);
    let (text, _, _) = harness.draw(&mut item, 40, 20);
    assert!(text.lines().next().unwrap().contains("line 6"));
    // The wheel scrolls without moving the cursor.
    item.handle_mouse(&MouseEvent::new(MouseKind::ScrollDown, 10, 5), Rect::new(0, 0, 40, 20), &mut harness.cx());
    assert_eq!(item.scroll_top(), Some(8));
    assert_eq!(item.position(), Some((200, 3)));
}

#[test]
fn a_click_puts_the_cursor_where_it_landed() {
    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir, "main.rs", SAMPLE);
    let mut harness = Harness::new(dir.path());
    let mut item = EditorItem::open(&path, &mut harness.cx()).unwrap();
    harness.draw(&mut item, 60, 12);
    let gutter = paint::gutter_width(9, true);
    let area = Rect::new(0, 0, 60, 12);
    let click = MouseEvent::new(MouseKind::Press(MouseButton::Left), gutter + 8, 1);
    assert!(item.handle_mouse(&click, area, &mut harness.cx()));
    assert_eq!(item.position(), Some((2, 9)));
    let drag = MouseEvent::new(MouseKind::Drag(MouseButton::Left), gutter + 14, 1);
    item.handle_mouse(&drag, area, &mut harness.cx());
    assert_eq!(item.status().position, Some((2, 15, 6)), "six characters selected");
    assert_eq!(item.query_suggestion().as_deref(), Some("answer"));
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
    let mut untitled = EditorItem::untitled(&mut harness.cx());
    assert_eq!(untitled.tab(&harness.render_cx(false)).title, "untitled");
    assert_eq!(untitled.kind(), "editor");
    let (text, cursor, _) = harness.draw(&mut untitled, 30, 4);
    assert!(text.starts_with("   1"), "{text:?}");
    assert!(cursor.is_some());
}

#[test]
fn missing_files_and_folders_say_so() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    let mut harness = Harness::new(dir.path());
    let error = EditorItem::open(&dir.path().join("nope.rs"), &mut harness.cx()).err().unwrap();
    assert_eq!(error, "nope.rs does not exist");
    let error = EditorItem::open(&dir.path().join("src"), &mut harness.cx()).err().unwrap();
    assert_eq!(error, "src is a folder");
}

#[test]
fn the_nav_mode_setting_reaches_the_key_context() {
    let mut harness = Harness::new("/project");
    harness.settings.vim_mode = true;
    let mut item = EditorItem::untitled(&mut harness.cx());
    let mut contexts = Vec::new();
    item.contexts(&mut contexts);
    assert_eq!(contexts[0].values, vec![("mode", "full".to_string()), ("vim_mode", "normal".to_string())]);
    assert_eq!(item.status().mode, Some(ModeIndicator::Nav));
    harness.settings.vim_mode = false;
    assert!(item.tick(&mut harness.cx()), "the change is drawn");
    contexts.clear();
    item.contexts(&mut contexts);
    assert_eq!(contexts[0].values, vec![("mode", "full".to_string())]);
}

#[test]
fn a_file_changed_on_disk_is_reloaded() {
    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir, "notes.md", "one\n");
    let mut harness = Harness::new(dir.path());
    let mut item = EditorItem::open(&path, &mut harness.cx()).unwrap();
    std::fs::write(&path, "one\ntwo\n").unwrap();
    item.paths_changed(&FsChange::of(vec![dir.path().join("other.md")]), &mut harness.cx());
    assert!(!harness.draw(&mut item, 30, 4).0.contains("two"), "another file's change is not ours");
    item.paths_changed(&FsChange::of(vec![path.clone()]), &mut harness.cx());
    assert!(harness.draw(&mut item, 30, 4).0.contains("two"));
    // Deleted: what was there stays on screen.
    std::fs::remove_file(&path).unwrap();
    item.paths_changed(&FsChange::of(vec![path]), &mut harness.cx());
    assert!(harness.draw(&mut item, 30, 4).0.contains("two"));
}

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
    // A preview scrolls there; dismissing the picker puts the view back.
    item.picker_preview(PickerKind::Outline, Some(far), &mut harness.cx());
    assert!(item.scroll_top().unwrap() > 80);
    item.picker_preview(PickerKind::Outline, None, &mut harness.cx());
    assert_eq!(item.scroll_top(), Some(0));
    item.picker_confirm(PickerKind::Outline, far, false, &mut harness.cx());
    assert_eq!(item.position(), Some((89, 4)), "on the name of the function");
    // Plain text has no outline at all.
    let notes = file(&dir, "notes.txt", "hello\n");
    let mut plain = EditorItem::open(&notes, &mut harness.cx()).unwrap();
    assert!(plain.picker_rows(PickerKind::Outline, &mut harness.cx()).is_none());
    assert!(plain.picker_rows(PickerKind::Links, &mut harness.cx()).is_none());
}

#[test]
fn highlights_arrive_from_the_project_and_colour_the_text() {
    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir, "main.rs", SAMPLE);
    let mut harness = Harness::new(dir.path());
    let mut item = EditorItem::open(&path, &mut harness.cx()).unwrap();
    let plain = harness.draw(&mut item, 60, 10).2;
    // The highlighting thread answers; the next tick says a redraw is due.
    let started = Instant::now();
    loop {
        harness.project.poll();
        if item.tick(&mut harness.cx()) {
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(20), "no highlights arrived");
        std::thread::sleep(Duration::from_millis(5));
    }
    let coloured = harness.draw(&mut item, 60, 10).2;
    let gutter = paint::gutter_width(9, true);
    assert_ne!(coloured[(gutter, 0)].fg, plain[(gutter, 0)].fg, "`fn` is a keyword");
    assert!(!item.tick(&mut harness.cx()), "nothing new after the draw");
}

#[test]
fn git_marks_follow_the_hunks() {
    let hunk = |kind, new_start, new_lines| LineHunk { kind, new_start, new_lines, old_start: 0, old_lines: 0 };
    let marks = GitMarks {
        base: None,
        version: None,
        hunks: vec![hunk(LineHunkKind::Added, 1, 2), hunk(LineHunkKind::Deleted, 5, 0), hunk(LineHunkKind::Modified, 7, 1), hunk(LineHunkKind::Deleted, 10, 0)],
    };
    let at = |line| marks.mark(line, 10);
    assert_eq!(at(0), None);
    assert_eq!((at(1), at(2), at(3)), (Some(GitMark::Added), Some(GitMark::Added), None));
    assert_eq!(at(5), Some(GitMark::DeletedAbove));
    assert_eq!(at(7), Some(GitMark::Modified));
    assert_eq!(at(9), Some(GitMark::DeletedBelow), "lines deleted at the end show on the last line");
    assert_eq!(GitMarks::default().mark(3, 10), None);
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
