use std::time::{Duration, Instant};

use bakedit_core::{Key, KeyEvent, Mods};
use serde_json::json;

use super::*;
use crate::view::test_support::Harness;

fn dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a.rs"), "fn needle() {}\n    let needle = 1;\n").unwrap();
    std::fs::write(dir.path().join("b.txt"), "no match here\nNeedle again\n").unwrap();
    dir
}

fn act(search: &mut ProjectSearch, harness: &mut Harness, name: &str) -> bool {
    search.handle_action(&Action::new(name), &mut harness.cx())
}

/// Tick until nothing is running, pending or being replaced.
fn settle(search: &mut ProjectSearch, harness: &mut Harness) {
    let started = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(5));
        search.tick(&mut harness.cx());
        if search.state != State::Running && search.job.is_none() && search.due.is_none() {
            return;
        }
        assert!(started.elapsed() < Duration::from_secs(20), "the search never finished");
    }
}

fn search_for(search: &mut ProjectSearch, harness: &mut Harness, query: &str) {
    let focus = Action::with_args("search::FocusSearch", json!({ "query": query }));
    search.handle_action(&focus, &mut harness.cx());
    settle(search, harness);
}

fn type_text(search: &mut ProjectSearch, harness: &mut Harness, text: &str) {
    let mut key = KeyEvent::new(Key::Char(text.chars().next().unwrap()), Mods::empty());
    key.text = Some(text.to_string());
    assert!(search.handle_key(&key, &mut harness.cx()));
}

fn lines(text: &str) -> Vec<&str> {
    text.lines().collect()
}

#[test]
fn finds_matches_grouped_by_file() {
    let dir = dir();
    let mut harness = Harness::new(dir.path());
    let mut search = ProjectSearch::new(false, &mut harness.cx());
    let (text, cursor, _) = harness.draw(&mut search, 70, 14);
    assert!(text.contains("Find in files") && text.contains("Nothing searched yet."), "{text}");
    assert!(text.contains("include") && text.contains("exclude"), "{text}");
    assert_eq!(cursor.map(|c| c.row), Some(0), "the cursor is in the query");
    search_for(&mut search, &mut harness, "needle");
    let (text, _, _) = harness.draw(&mut search, 70, 14);
    let rows = lines(&text);
    assert!(rows[2].contains("3 results in 2 files"), "{:?}", rows[2]);
    assert!(rows[3].contains("b.txt") && rows[3].trim_end().ends_with('1'), "{:?}", rows[3]);
    assert!(rows[4].trim_start().starts_with('2') && rows[4].contains("Needle again"), "{:?}", rows[4]);
    assert!(rows[5].contains("a.rs") && rows[5].contains("src") && rows[5].trim_end().ends_with('2'), "{:?}", rows[5]);
    assert!(rows[7].contains("let needle = 1;") && !rows[7].contains("    let"), "leading blanks are dropped: {:?}", rows[7]);
}

#[test]
fn the_number_column_is_right_aligned() {
    let dir = tempfile::tempdir().unwrap();
    let body: String = (1..=120).map(|i| if i == 9 || i == 100 { "hit\n".to_string() } else { "x\n".to_string() }).collect();
    std::fs::write(dir.path().join("n.txt"), body).unwrap();
    let mut harness = Harness::new(dir.path());
    let mut search = ProjectSearch::new(false, &mut harness.cx());
    search_for(&mut search, &mut harness, "hit");
    let (text, _, _) = harness.draw(&mut search, 60, 10);
    let rows = lines(&text);
    let nine = rows.iter().find(|r| r.contains("hit") && r.contains('9')).unwrap();
    let hundred = rows.iter().find(|r| r.contains("100")).unwrap();
    let end = |row: &str, needle: &str| row.find(needle).unwrap() + needle.len();
    assert_eq!(end(nine, "9"), end(hundred, "100"), "{text}");
}

#[test]
fn enter_previews_and_cmd_enter_focuses_the_editor() {
    let dir = dir();
    let mut harness = Harness::new(dir.path());
    let mut search = ProjectSearch::new(false, &mut harness.cx());
    search_for(&mut search, &mut harness, "let needle");
    assert!(act(&mut search, &mut harness, "menu::Confirm"), "enter moves into the results");
    assert_eq!(search.focus, Part::Results);
    assert!(harness.take_effects().is_empty());
    let mut contexts = Vec::new();
    search.contexts(&mut contexts);
    assert_eq!(contexts.iter().map(|c| c.name).collect::<Vec<_>>(), ["ProjectSearchView"]);
    assert!(act(&mut search, &mut harness, "menu::Confirm"));
    let path = dir.path().join("src/a.rs");
    assert_eq!(harness.take_effects(), vec![Effect::OpenFile { path: path.clone(), line: Some(2), column: Some(5), preview: true, focus: false }]);
    assert!(act(&mut search, &mut harness, "menu::SecondaryConfirm"));
    assert_eq!(harness.take_effects(), vec![Effect::OpenFile { path, line: Some(2), column: Some(5), preview: false, focus: true }]);
    assert_eq!(harness.project.search_history(SearchKind::Project), vec!["let needle".to_string()]);
}

#[test]
fn typing_searches_after_a_pause_and_a_new_query_replaces_the_old() {
    let dir = dir();
    let mut harness = Harness::new(dir.path());
    let mut search = ProjectSearch::new(false, &mut harness.cx());
    type_text(&mut search, &mut harness, "zzz");
    assert!(search.due.is_some() && search.state == State::Idle, "nothing runs on the first key");
    settle(&mut search, &mut harness);
    assert!(harness.draw(&mut search, 70, 8).0.contains("No results for \"zzz\""));
    assert!(harness.draw(&mut search, 70, 8).0.contains("Nothing in the project matches."));
    search.handle_action(&Action::new("editor::SelectAll"), &mut harness.cx());
    type_text(&mut search, &mut harness, "needle");
    settle(&mut search, &mut harness);
    assert_eq!(search.total_matches(), 3);
}

#[test]
fn options_and_errors_show_in_the_status() {
    let dir = dir();
    let mut harness = Harness::new(dir.path());
    let mut search = ProjectSearch::new(false, &mut harness.cx());
    search_for(&mut search, &mut harness, "needle");
    act(&mut search, &mut harness, "search::ToggleCaseSensitive");
    settle(&mut search, &mut harness);
    assert_eq!(search.total_matches(), 2, "case matters now");
    act(&mut search, &mut harness, "search::ToggleRegex");
    search_for(&mut search, &mut harness, "(unclosed");
    let text = harness.draw(&mut search, 80, 8).0;
    assert!(matches!(search.state, State::Failed(_)), "{:?}", search.state);
    assert!(text.contains("Invalid pattern: unclosed group"), "{text}");
    assert!(text.contains("not a valid regular expression"), "{text}");
    let mut contexts = Vec::new();
    search.contexts(&mut contexts);
    assert_eq!(contexts.iter().map(|c| c.name).collect::<Vec<_>>(), ["ProjectSearchView", "ProjectSearchBar", "Editor"]);
    assert_eq!(search.kind(), "project_search");
}

#[test]
fn include_and_exclude_limit_the_files() {
    let dir = dir();
    let mut harness = Harness::new(dir.path());
    let mut search = ProjectSearch::in_directory(&dir.path().join("src"), false, &mut harness.cx());
    assert_eq!(search.include.text(), "src");
    search_for(&mut search, &mut harness, "needle");
    assert_eq!(search.files.len(), 1);
    search.include.set_text("");
    search.exclude.set_text("*.rs, nothing");
    search.focus = Part::Exclude;
    search.edited(&mut harness.cx());
    settle(&mut search, &mut harness);
    assert_eq!(search.files.iter().map(|f| f.path.to_string_lossy().into_owned()).collect::<Vec<_>>(), ["b.txt"]);
    assert_eq!(include_for(Path::new("/p"), Path::new("/p")), "");
    assert_eq!(include_for(Path::new("/p/crates/app/"), Path::new("/p")), "crates/app");
}

#[test]
fn arrows_skip_headers_and_left_right_fold_files() {
    let dir = dir();
    let mut harness = Harness::new(dir.path());
    let mut search = ProjectSearch::new(false, &mut harness.cx());
    search_for(&mut search, &mut harness, "needle");
    act(&mut search, &mut harness, "project_search::ToggleFocus");
    assert_eq!(search.rows[search.selected], ResultRow::Match(0, 0));
    act(&mut search, &mut harness, "menu::SelectNext");
    assert_eq!(search.rows[search.selected], ResultRow::Match(1, 0), "the header of src/a.rs is skipped");
    act(&mut search, &mut harness, "project_search::CollapseEntry");
    assert_eq!(search.rows[search.selected], ResultRow::File(1), "left goes to the header");
    act(&mut search, &mut harness, "project_search::CollapseEntry");
    assert_eq!(search.rows.len(), 3, "src/a.rs is folded");
    let text = harness.draw(&mut search, 70, 10).0;
    assert!(text.contains("▸"), "{text}");
    act(&mut search, &mut harness, "menu::SelectPrevious");
    assert_eq!(search.rows[search.selected], ResultRow::Match(0, 0));
    act(&mut search, &mut harness, "menu::SelectNext");
    assert_eq!(search.rows[search.selected], ResultRow::File(1), "a folded header can be chosen");
    act(&mut search, &mut harness, "project_search::ExpandEntry");
    assert_eq!(search.rows.len(), 5);
    act(&mut search, &mut harness, "project_search::CollapseAllEntries");
    assert_eq!(search.rows.len(), 2);
    act(&mut search, &mut harness, "project_search::ExpandAllEntries");
    assert_eq!(search.rows.len(), 5);
    act(&mut search, &mut harness, "menu::SelectFirst");
    act(&mut search, &mut harness, "menu::SelectPrevious");
    assert_eq!(search.focus, Part::Query, "up from the first match returns to the query");
}

#[test]
fn history_steps_through_earlier_queries() {
    let dir = dir();
    let mut harness = Harness::new(dir.path());
    harness.project.remember_search(SearchKind::Project, "older");
    harness.project.remember_search(SearchKind::Project, "newer");
    let mut search = ProjectSearch::new(false, &mut harness.cx());
    type_text(&mut search, &mut harness, "dr");
    act(&mut search, &mut harness, "search::PreviousHistoryQuery");
    assert_eq!(search.query.text(), "newer");
    act(&mut search, &mut harness, "search::PreviousHistoryQuery");
    assert_eq!(search.query.text(), "older");
    act(&mut search, &mut harness, "search::PreviousHistoryQuery");
    assert_eq!(search.query.text(), "older", "stops at the oldest");
    act(&mut search, &mut harness, "search::NextHistoryQuery");
    act(&mut search, &mut harness, "search::NextHistoryQuery");
    assert_eq!(search.query.text(), "dr", "the draft comes back");
}

#[test]
fn replace_next_changes_one_match_and_moves_on() {
    let dir = dir();
    let mut harness = Harness::new(dir.path());
    let mut search = ProjectSearch::new(true, &mut harness.cx());
    search_for(&mut search, &mut harness, "needle");
    search.replacement.set_text("pin");
    search.focus = Part::Replace;
    let mut contexts = Vec::new();
    search.contexts(&mut contexts);
    assert!(contexts[1].flags.contains(&"in_replace"));
    assert!(act(&mut search, &mut harness, "search::ReplaceNext"));
    settle(&mut search, &mut harness);
    assert_eq!(std::fs::read_to_string(dir.path().join("b.txt")).unwrap(), "no match here\npin again\n");
    let effects = harness.take_effects();
    assert!(effects.contains(&Effect::PathsChanged(vec![dir.path().join("b.txt")])), "{effects:?}");
    assert_eq!(search.total_matches(), 2, "the replaced match left the list");
    assert_eq!(search.files.len(), 1);
    assert!(act(&mut search, &mut harness, "search::ReplaceNext"));
    settle(&mut search, &mut harness);
    assert_eq!(std::fs::read_to_string(dir.path().join("src/a.rs")).unwrap(), "fn pin() {}\n    let needle = 1;\n");
    let text = harness.draw(&mut search, 70, 10).0;
    assert!(text.contains("let needle = 1;") && !text.contains("fn needle"), "{text}");
}

#[test]
fn replace_all_asks_first_then_reports() {
    let dir = dir();
    let mut harness = Harness::new(dir.path());
    let mut search = ProjectSearch::new(true, &mut harness.cx());
    search_for(&mut search, &mut harness, "(?i)needle");
    act(&mut search, &mut harness, "search::ToggleRegex");
    settle(&mut search, &mut harness);
    search.replacement.set_text("pin");
    assert!(act(&mut search, &mut harness, "search::ReplaceAll"));
    let effects = harness.take_effects();
    let Some(Effect::Ask(question)) = effects.first() else { panic!("{effects:?}") };
    assert_eq!(question.message, "Replace 3 matches in 2 files with \"pin\"?");
    assert_eq!(std::fs::read_to_string(dir.path().join("b.txt")).unwrap(), "no match here\nNeedle again\n", "nothing is written before the answer");
    assert_eq!(question.answers[0].1, vec![Effect::Dispatch(Action::new("project_search::ConfirmReplaceAll"))]);
    assert!(question.answers[1].1.is_empty(), "the last button cancels");
    assert!(act(&mut search, &mut harness, "project_search::ConfirmReplaceAll"));
    settle(&mut search, &mut harness);
    assert_eq!(std::fs::read_to_string(dir.path().join("b.txt")).unwrap(), "no match here\npin again\n");
    assert_eq!(std::fs::read_to_string(dir.path().join("src/a.rs")).unwrap(), "fn pin() {}\n    let pin = 1;\n");
    let effects = harness.take_effects();
    assert!(effects.contains(&Effect::status("Replaced 3 matches in 2 files")), "{effects:?}");
    assert!(matches!(effects.first(), Some(Effect::PathsChanged(paths)) if paths.len() == 2), "{effects:?}");
}

#[test]
fn a_toggle_click_and_the_replace_chevron_work() {
    let dir = dir();
    let mut harness = Harness::new(dir.path());
    let mut search = ProjectSearch::new(false, &mut harness.cx());
    harness.draw(&mut search, 70, 10);
    let (rect, _) = *search.hits.iter().find(|(_, hit)| *hit == Hit::Toggle(Opt::Regex)).unwrap();
    let press = MouseEvent { kind: MouseKind::Press(MouseButton::Left), col: rect.x + 1, row: rect.y, mods: Mods::empty() };
    assert!(search.handle_mouse(&press, Rect::new(0, 0, 70, 10), &mut harness.cx()));
    assert!(search.regex);
    let (chevron, _) = *search.hits.iter().find(|(_, hit)| *hit == Hit::ReplaceRow).unwrap();
    let press = MouseEvent { kind: MouseKind::Press(MouseButton::Left), col: chevron.x + 1, row: chevron.y, mods: Mods::empty() };
    search.handle_mouse(&press, Rect::new(0, 0, 70, 10), &mut harness.cx());
    assert!(search.replace_open && search.focus == Part::Replace);
    assert!(harness.draw(&mut search, 70, 10).0.contains("Replace with"));
}

#[test]
fn clip_line_keeps_the_match_in_view() {
    let text = |cells: Vec<(char, bool)>| cells.iter().map(|c| c.0).collect::<String>();
    assert_eq!(text(clip_line("    let x = 1;", 0, &[8..9], 20)), "let x = 1;");
    let long = format!("{}needle{}", "a".repeat(60), "b".repeat(60));
    let cells = clip_line(&long, 0, &[60..66], 30);
    let shown = text(cells.clone());
    assert!(shown.starts_with('…') && shown.ends_with('…') && shown.contains("needle"), "{shown}");
    assert_eq!(shown.chars().count(), 30);
    assert_eq!(cells.iter().filter(|c| c.1).count(), 6);
    assert_eq!(text(clip_line("tail of a clamped line", 40, &[0..4], 30)), "…tail of a clamped line");
    assert_eq!(text(clip_line("abc", 0, &[], 0)), "");
    // Wide characters never overrun the room.
    let wide = "日本語日本語日本語";
    assert!(ui::width(&text(clip_line(wide, 0, &[0..1], 7))) <= 7);
}

#[test]
fn formats_counts() {
    assert_eq!(thousands(10_000), "10,000");
    assert_eq!(plural(1, "result", "results"), "1 result");
    assert_eq!(plural(1234, "result", "results"), "1,234 results");
}

#[test]
fn the_built_in_keymap_binds_fold_keys_only_in_the_results() {
    use bakedit_keymap::{KeyContext, Keymap, KeymapSource};
    let mut keymap = Keymap::new();
    assert!(keymap.load(crate::keys::DEFAULT_KEYMAP, KeymapSource::Default).is_empty());
    let dir = dir();
    let mut harness = Harness::new(dir.path());
    let mut search = ProjectSearch::new(false, &mut harness.cx());
    let resolve = |search: &ProjectSearch, key: Key| {
        let mut views = Vec::new();
        search.contexts(&mut views);
        let mut stack = vec![KeyContext::new("Workspace"), KeyContext::new("Pane")];
        for view in views {
            let mut context = KeyContext::new(view.name);
            for flag in view.flags {
                context = context.add(flag);
            }
            for (key, value) in view.values {
                context = context.set(key, value);
            }
            stack.push(context);
        }
        let (bindings, _) = keymap.bindings_for_input(&[KeyEvent::new(key, Mods::empty())], &stack);
        bindings.first().and_then(|b| b.action()).map(|a| a.name)
    };
    assert_eq!(resolve(&search, Key::Left).as_deref(), Some("editor::MoveLeft"), "in the query left moves the cursor");
    search.focus = Part::Results;
    assert_eq!(resolve(&search, Key::Left).as_deref(), Some("project_search::CollapseEntry"));
    assert_eq!(resolve(&search, Key::Right).as_deref(), Some("project_search::ExpandEntry"));
    assert_eq!(resolve(&search, Key::Down).as_deref(), Some("menu::SelectNext"));
}
