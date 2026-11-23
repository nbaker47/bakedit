//! Undo, redo and how edits are grouped into steps.

use std::sync::Arc;

use super::{apply_all, editor, state, type_text};
use crate::clock::ManualClock;
use crate::command::{Command, Motion};
use crate::search::SearchQuery;

fn insert(text: &str) -> Command {
    Command::Insert(text.to_string())
}

/// An editor whose clock the test controls.
fn timed(marked: &str) -> (crate::Editor, Arc<ManualClock>) {
    let mut e = editor(marked);
    let clock = Arc::new(ManualClock::new(1_000));
    e.set_clock(clock.clone());
    (e, clock)
}

#[test]
fn undo_and_redo_restore_text_and_selections() {
    let mut e = editor("one «twoˇ» three");
    apply_all(&mut e, [insert("2")]);
    assert_eq!(state(&e), "one 2ˇ three");
    let outcome = e.apply(Command::Undo);
    assert!(outcome.text_changed() && outcome.selections_changed);
    assert_eq!(
        state(&e),
        "one «twoˇ» three",
        "the selection from before the edit is back"
    );
    apply_all(&mut e, [Command::Redo]);
    assert_eq!(state(&e), "one 2ˇ three");
    assert!(e.apply(Command::Redo).is_noop());
    apply_all(&mut e, [Command::Undo]);
    assert!(e.apply(Command::Undo).is_noop());
}

#[test]
fn fast_typing_is_one_undo_step() {
    let (mut e, clock) = timed("ˇ");
    for c in "hello".chars() {
        e.apply(insert(&c.to_string()));
        clock.advance(120);
    }
    assert_eq!(state(&e), "helloˇ");
    apply_all(&mut e, [Command::Undo]);
    assert_eq!(state(&e), "ˇ");
    apply_all(&mut e, [Command::Redo]);
    assert_eq!(state(&e), "helloˇ");
}

#[test]
fn a_pause_starts_a_new_undo_step() {
    let (mut e, clock) = timed("ˇ");
    type_text(&mut e, "ab");
    clock.advance(300);
    type_text(&mut e, "c");
    clock.advance(301);
    type_text(&mut e, "de");
    assert_eq!(state(&e), "abcdeˇ");
    apply_all(&mut e, [Command::Undo]);
    assert_eq!(
        state(&e),
        "abcˇ",
        "exactly 300 ms still merges, 301 ms does not"
    );
    apply_all(&mut e, [Command::Undo]);
    assert_eq!(state(&e), "ˇ");
}

#[test]
fn a_cursor_jump_starts_a_new_undo_step() {
    let (mut e, _clock) = timed("ˇ");
    type_text(&mut e, "ab");
    apply_all(
        &mut e,
        [Command::Move(Motion::Left), Command::Move(Motion::Right)],
    );
    type_text(&mut e, "cd");
    apply_all(&mut e, [Command::Undo]);
    assert_eq!(state(&e), "abˇ");

    // A click counts as a jump too, even back to the same place.
    let (mut e, _clock) = timed("ˇ");
    type_text(&mut e, "ab");
    e.set_cursor(2);
    type_text(&mut e, "cd");
    apply_all(&mut e, [Command::Undo]);
    assert_eq!(state(&e), "abˇ");

    // And so does an explicit break.
    let (mut e, _clock) = timed("ˇ");
    type_text(&mut e, "ab");
    e.break_undo_group();
    type_text(&mut e, "cd");
    apply_all(&mut e, [Command::Undo]);
    assert_eq!(state(&e), "abˇ");
}

#[test]
fn other_commands_are_steps_of_their_own() {
    let (mut e, _clock) = timed("ˇ");
    type_text(&mut e, "ab");
    apply_all(&mut e, [Command::Newline]);
    type_text(&mut e, "cd");
    assert_eq!(state(&e), "ab\ncdˇ");
    apply_all(&mut e, [Command::Undo]);
    assert_eq!(state(&e), "ab\nˇ");
    apply_all(&mut e, [Command::Undo]);
    assert_eq!(state(&e), "abˇ");
    apply_all(&mut e, [Command::Undo]);
    assert_eq!(state(&e), "ˇ");
}

#[test]
fn held_backspace_and_delete_are_one_step_each() {
    let (mut e, clock) = timed("abcdefˇ");
    for _ in 0..4 {
        e.apply(Command::Backspace);
        clock.advance(30);
    }
    assert_eq!(state(&e), "abˇ");
    apply_all(&mut e, [Command::Undo]);
    assert_eq!(state(&e), "abcdefˇ");

    let (mut e, clock) = timed("ˇabcdef");
    for _ in 0..3 {
        e.apply(Command::Delete);
        clock.advance(30);
    }
    apply_all(&mut e, [Command::Undo]);
    assert_eq!(state(&e), "ˇabcdef");

    // Typing after backspacing is a different kind of step.
    let (mut e, _clock) = timed("abcˇ");
    apply_all(&mut e, [Command::Backspace, Command::Backspace]);
    type_text(&mut e, "xy");
    assert_eq!(state(&e), "axyˇ");
    apply_all(&mut e, [Command::Undo]);
    assert_eq!(state(&e), "aˇ");
    apply_all(&mut e, [Command::Undo]);
    assert_eq!(state(&e), "abcˇ");
}

#[test]
fn typing_through_closed_brackets_stays_one_step() {
    let (mut e, clock) = timed("ˇ");
    for c in "f(x);".chars() {
        e.apply(insert(&c.to_string()));
        clock.advance(100);
    }
    assert_eq!(state(&e), "f(x);ˇ");
    apply_all(&mut e, [Command::Undo]);
    assert_eq!(
        state(&e),
        "ˇ",
        "stepping over the closing bracket does not split the step"
    );
}

#[test]
fn typing_at_several_cursors_merges_too() {
    let (mut e, _clock) = timed("ˇa\nˇb");
    type_text(&mut e, "xy");
    assert_eq!(state(&e), "xyˇa\nxyˇb");
    apply_all(&mut e, [Command::Undo]);
    assert_eq!(state(&e), "ˇa\nˇb");
}

#[test]
fn undo_makes_the_buffer_clean_again() {
    let mut e = editor("abˇc");
    assert!(!e.buffer().is_dirty());
    apply_all(&mut e, [insert("x")]);
    assert!(e.buffer().is_dirty());
    apply_all(&mut e, [Command::Undo]);
    assert!(!e.buffer().is_dirty());
    apply_all(&mut e, [Command::Redo]);
    assert!(e.buffer().is_dirty());
}

#[test]
fn a_new_edit_after_undo_drops_the_redo() {
    let mut e = editor("ˇ");
    apply_all(
        &mut e,
        [insert("a"), Command::Newline, Command::Undo, insert("b")],
    );
    assert_eq!(state(&e), "abˇ");
    assert!(e.apply(Command::Redo).is_noop());
    assert!(!e.buffer().can_redo());
}

#[test]
fn outside_edits_move_the_selections_and_undo_restores_them() {
    let mut e = editor("one tˇwo «threeˇ»");
    let outcome = e.edit(&[(0..3, "1"), (8..8, "+ ")]);
    assert!(outcome.text_changed());
    assert_eq!(state(&e), "1 tˇwo + «threeˇ»");
    apply_all(&mut e, [Command::Undo]);
    assert_eq!(state(&e), "one tˇwo «threeˇ»");
    apply_all(&mut e, [Command::Redo]);
    assert_eq!(state(&e), "1 tˇwo + «threeˇ»");

    // Text inserted exactly at a cursor pushes the cursor along.
    let mut e = editor("abˇc");
    e.edit(&[(2..2, "XY")]);
    assert_eq!(state(&e), "abXYˇc");
    // A deletion around a selection collapses it.
    let mut e = editor("a«bcˇ»d");
    e.edit(&[(0..4, "")]);
    assert_eq!(state(&e), "ˇ");
    assert!(e.edit::<&str>(&[]).is_noop());
}

#[test]
fn replace_all_is_one_undo_step_and_keeps_the_cursor_in_place() {
    let mut e = editor("foo bar fˇoo baz foo");
    let search = SearchQuery::text("foo").compile().unwrap();
    let outcome = e.replace_all(&search, "quux");
    assert_eq!(outcome.edits.batches().len(), 1);
    assert_eq!(state(&e), "quux bar qˇuux baz quux");
    apply_all(&mut e, [Command::Undo]);
    assert_eq!(state(&e), "foo bar fˇoo baz foo");
    assert!(!e.buffer().can_undo());

    let none = SearchQuery::text("zzz").compile().unwrap();
    assert!(e.replace_all(&none, "y").is_noop());
}

#[test]
fn replace_next_reports_where_the_replacement_is() {
    let mut e = editor("ˇa-b-c");
    let search = SearchQuery::regex("-(\\w)").compile().unwrap();
    let (replaced, outcome) = e.replace_next(&search, 0, false, "+$1$1");
    assert_eq!(replaced, Some(1..4));
    assert!(outcome.text_changed());
    assert_eq!(state(&e), "ˇa+bb-c");
    let (replaced, _) = e.replace_next(&search, 4, false, "!");
    assert_eq!(replaced, Some(4..5));
    assert_eq!(state(&e), "ˇa+bb!");
    let (replaced, outcome) = e.replace_next(&search, 0, true, "?");
    assert_eq!(replaced, None);
    assert!(outcome.is_noop());
    apply_all(&mut e, [Command::Undo, Command::Undo]);
    assert_eq!(state(&e), "ˇa-b-c");
}

#[test]
fn dragging_is_one_entry_in_the_selection_history() {
    let mut e = editor("hello world");
    e.set_cursor(2);
    for head in 3..9 {
        assert!(e.extend_primary_to(head).selections_changed);
    }
    assert_eq!(state(&e), "he«llo woˇ»rld");
    assert!(!e.extend_primary_to(8).selections_changed);
    apply_all(&mut e, [Command::UndoSelection]);
    assert_eq!(
        state(&e),
        "heˇllo world",
        "the whole drag is undone at once"
    );

    // Adding a cursor and extending it works on the newest selection.
    let mut e = editor("hello world");
    e.set_cursor(1);
    e.add_selection(6, 6);
    e.extend_primary_to(9);
    assert_eq!(state(&e), "hˇello «worˇ»ld");
    e.select_range(4, 0);
    assert_eq!(state(&e), "«ˇhell»o world");
}

#[test]
fn positions_from_outside_are_made_valid() {
    let mut e = editor("ae\u{301}b");
    e.set_cursor(2);
    assert_eq!(
        state(&e),
        "aˇe\u{301}b",
        "a position inside a cluster moves to its start"
    );
    e.set_cursor(999);
    assert_eq!(state(&e), "ae\u{301}bˇ");
    e.select_range(999, 2);
    assert_eq!(state(&e), "a«ˇe\u{301}b»");
    e.add_selection(0, 0);
    assert_eq!(state(&e), "ˇa«ˇe\u{301}b»");
}

#[test]
fn deleting_between_a_letter_and_its_accent_keeps_the_cursor_whole() {
    // Removing the x lets the accent attach to the e; the cursor may not stay between.
    let mut e = editor("ex\u{301}ˇ");
    apply_all(&mut e, [Command::Move(Motion::Left)]);
    assert_eq!(state(&e), "eˇx\u{301}");
    e.edit(&[(1..2, "")]);
    assert_eq!(state(&e), "ˇe\u{301}");
}
