//! Tests for the editor's commands.
//!
//! Most tests describe a buffer and its selections in one marked string, in the notation
//! Zed's own tests use: `ˇ` is a cursor, `«textˇ»` a selection with its head at the end and
//! `«ˇtext»` one with its head at the start. The last selection in the string is the
//! primary one.

mod clipboard;
mod fuzz;
mod history;
mod lines;
mod motion;
mod select;
mod typing;

use std::sync::Arc;

use crate::clock::ManualClock;
use crate::command::Command;
use crate::editor::{EditSettings, Editor};
use crate::selection::{Selection, Selections};

/// Split a marked string into its text and its selections.
pub(crate) fn parse(marked: &str) -> (String, Vec<Selection>) {
    let mut text = String::new();
    let mut selections = Vec::new();
    let mut chars = 0;
    let mut open: Option<usize> = None;
    let mut head_at_start = false;
    let mut previous_was_open = false;
    for c in marked.chars() {
        match c {
            '«' => {
                open = Some(chars);
                head_at_start = false;
                previous_was_open = true;
                continue;
            }
            'ˇ' => match open {
                Some(_) if previous_was_open => head_at_start = true,
                Some(_) => {}
                None => selections.push(Selection::cursor(chars)),
            },
            '»' => {
                let start = open.take().expect("» without «");
                selections.push(if head_at_start {
                    Selection::new(chars, start)
                } else {
                    Selection::new(start, chars)
                });
            }
            _ => {
                text.push(c);
                chars += 1;
            }
        }
        previous_was_open = false;
    }
    assert!(open.is_none(), "« without » in {marked:?}");
    (text, selections)
}

/// An editor holding the marked text and selections, with default settings and a clock
/// that only moves when a test moves it.
pub(crate) fn editor(marked: &str) -> Editor {
    editor_with(marked, EditSettings::default())
}

/// An editor holding the marked text and selections, with the given settings.
pub(crate) fn editor_with(marked: &str, settings: EditSettings) -> Editor {
    let (text, selections) = parse(marked);
    let mut editor = Editor::with_settings(crate::Buffer::from_str(&text), settings);
    editor.set_clock(Arc::new(ManualClock::new(0)));
    if !selections.is_empty() {
        let primary = selections.len() - 1;
        editor.set_selections(Selections::new(selections, primary));
    }
    editor
}

/// The editor's text with its selections marked.
pub(crate) fn state(editor: &Editor) -> String {
    let mut marks: Vec<(usize, u8, &str)> = Vec::new();
    for selection in editor.selections().iter() {
        if selection.is_empty() {
            marks.push((selection.head, 1, "ˇ"));
        } else if selection.reversed() {
            marks.push((selection.start(), 2, "«ˇ"));
            marks.push((selection.end(), 0, "»"));
        } else {
            marks.push((selection.start(), 2, "«"));
            marks.push((selection.end(), 0, "ˇ»"));
        }
    }
    marks.sort_by_key(|&(position, order, _)| (position, order));
    let mut out = String::new();
    let mut next = marks.iter().peekable();
    let text: Vec<char> = editor.buffer().to_string().chars().collect();
    for (index, c) in text.iter().enumerate() {
        while let Some((_, _, mark)) = next.next_if(|(position, _, _)| *position == index) {
            out.push_str(mark);
        }
        out.push(*c);
    }
    for (_, _, mark) in next {
        out.push_str(mark);
    }
    out
}

/// Apply `command` to the marked `before` and compare with the marked `after`.
#[track_caller]
pub(crate) fn check(before: &str, command: Command, after: &str) {
    let mut editor = editor(before);
    editor.apply(command);
    assert_eq!(state(&editor), after, "after the command on {before:?}");
}

/// Like [`check`] with custom settings.
#[track_caller]
pub(crate) fn check_with(settings: &EditSettings, before: &str, command: Command, after: &str) {
    let mut editor = editor_with(before, settings.clone());
    editor.apply(command);
    assert_eq!(state(&editor), after, "after the command on {before:?}");
}

/// Apply several commands in a row.
pub(crate) fn apply_all(editor: &mut Editor, commands: impl IntoIterator<Item = Command>) {
    for command in commands {
        editor.apply(command);
    }
}

/// Type `text` one character at a time.
pub(crate) fn type_text(editor: &mut Editor, text: &str) {
    for c in text.chars() {
        editor.apply(Command::Insert(c.to_string()));
    }
}

#[test]
fn the_marked_notation_round_trips() {
    for marked in [
        "ˇ",
        "abˇc",
        "«abˇ»c",
        "«ˇab»c",
        "ˇa«bˇ»c«ˇd»eˇ",
        "one\n«two\nthˇ»ree",
        "x«aˇ»«bˇ»y",
    ] {
        assert_eq!(state(&editor(marked)), marked);
    }
    let (text, selections) = parse("a«bcˇ»d«ˇef»gˇ");
    assert_eq!(text, "abcdefg");
    assert_eq!(
        selections,
        vec![
            Selection::new(1, 3),
            Selection::new(6, 4),
            Selection::cursor(7)
        ]
    );
    assert_eq!(
        state(&editor("no marks")),
        "ˇno marks",
        "an editor starts with a cursor at 0"
    );
}

#[test]
fn editors_are_send() {
    fn assert_send<T: Send>() {}
    assert_send::<Editor>();
}
