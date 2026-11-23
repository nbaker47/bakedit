//! Random command sequences with a fixed seed.
//!
//! Whatever is thrown at the editor, nothing may panic, the selections must stay well
//! formed, every reported change must describe what really happened, and undoing everything
//! must give back the text the editor started with.

use std::sync::Arc;

use unicode_segmentation::UnicodeSegmentation;

use crate::clock::ManualClock;
use crate::command::{Clipboard, ClipboardPiece, Command, Motion};
use crate::edit::{Bias, BytePoint, Change, EditSummary};
use crate::editor::{EditSettings, Editor, Outcome};
use crate::pairs::PairKind;
use crate::search::SearchQuery;
use crate::selection::{Selection, Selections};

/// A small deterministic random number generator (xorshift).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound.max(1) as u64) as usize
    }

    fn chance(&mut self, one_in: usize) -> bool {
        self.below(one_in) == 0
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

/// Pieces of text that exercise words, brackets, quotes, blanks, wide and combining
/// characters, emoji, and the characters some rope builds take for line breaks.
const FRAGMENTS: [&str; 40] = [
    "a",
    "b",
    "Z",
    "fooBar",
    "snake_case",
    "x1",
    " ",
    " ",
    "  ",
    "    ",
    "\t",
    "\n",
    "\n",
    "\n\n",
    "(",
    ")",
    "[",
    "]",
    "{",
    "}",
    "<",
    ">",
    "\"",
    "'",
    "`",
    "\\",
    ".",
    ", ",
    ";",
    ":",
    "//",
    "# ",
    "é",
    "世界",
    "e\u{301}",
    "👍🏽",
    "🇨🇦",
    "\u{c}",
    "\u{2028}",
    "\u{85}",
];

fn random_text(rng: &mut Rng, max_fragments: usize) -> String {
    (0..rng.below(max_fragments + 1))
        .map(|_| *rng.pick(&FRAGMENTS))
        .collect()
}

fn random_motion(rng: &mut Rng) -> Motion {
    match rng.below(22) {
        0 => Motion::Left,
        1 => Motion::Right,
        2 => Motion::Up,
        3 => Motion::Down,
        4 => Motion::PreviousWordStart,
        5 => Motion::NextWordEnd,
        6 => Motion::NextWordStart,
        7 => Motion::PreviousSubwordStart,
        8 => Motion::NextSubwordEnd,
        9 => Motion::BeginningOfLine {
            stop_at_soft_wraps: rng.chance(2),
            stop_at_indent: rng.chance(2),
        },
        10 => Motion::EndOfLine {
            stop_at_soft_wraps: rng.chance(2),
        },
        11 => Motion::FirstNonBlank,
        12 => Motion::Beginning,
        13 => Motion::End,
        14 => Motion::StartOfParagraph,
        15 => Motion::EndOfParagraph,
        16 => Motion::PageUp { rows: rng.below(4) },
        17 => Motion::PageDown { rows: rng.below(4) },
        18 => Motion::UpByLines {
            lines: rng.below(4),
        },
        19 => Motion::DownByLines {
            lines: rng.below(4),
        },
        20 => Motion::EnclosingBracket,
        _ => Motion::GoToLine {
            line: rng.below(12),
            column: rng.below(12),
        },
    }
}

fn random_clipboard(rng: &mut Rng) -> Clipboard {
    match rng.below(4) {
        0 => Clipboard::plain(random_text(rng, 4)),
        1 => Clipboard::from_pieces(vec![ClipboardPiece {
            text: format!("{}\n", random_text(rng, 3)),
            linewise: true,
        }]),
        2 => Clipboard {
            text: random_text(rng, 3),
            linewise: true,
            pieces: Vec::new(),
        },
        _ => Clipboard::from_pieces(
            (0..1 + rng.below(3))
                .map(|_| {
                    let linewise = rng.chance(3);
                    let mut text = random_text(rng, 3);
                    if linewise {
                        text.push('\n');
                    }
                    ClipboardPiece { text, linewise }
                })
                .collect(),
        ),
    }
}

fn random_command(rng: &mut Rng, last_clipboard: &Option<Clipboard>) -> Command {
    match rng.below(64) {
        0..=7 => Command::Move(random_motion(rng)),
        8..=13 => Command::Select(random_motion(rng)),
        14..=23 => Command::Insert((*rng.pick(&FRAGMENTS)).to_string()),
        24 => Command::Insert(random_text(rng, 3)),
        25 => Command::SelectAll,
        26 => Command::SelectLine,
        27 => Command::SelectNext {
            replace_newest: rng.chance(3),
        },
        28 => Command::SelectPrevious {
            replace_newest: rng.chance(3),
        },
        29 => Command::SelectAllMatches,
        30 => Command::AddSelectionAbove {
            skip_soft_wrap: rng.chance(2),
        },
        31 => Command::AddSelectionBelow {
            skip_soft_wrap: rng.chance(2),
        },
        32 => Command::UndoSelection,
        33 => Command::RedoSelection,
        34 => Command::Cancel,
        35 => Command::SelectLargerSyntaxNode,
        36 => Command::SelectSmallerSyntaxNode,
        37 => Command::SelectBetween(*rng.pick(&PairKind::ALL)),
        38 => Command::Newline,
        39 => Command::NewlineAbove,
        40 => Command::NewlineBelow,
        41 | 42 => Command::Backspace,
        43 => Command::Delete,
        44 => Command::DeleteToPreviousWordStart {
            ignore_newlines: rng.chance(2),
            ignore_brackets: rng.chance(2),
        },
        45 => Command::DeleteToNextWordEnd {
            ignore_newlines: rng.chance(2),
            ignore_brackets: rng.chance(2),
        },
        46 => Command::DeleteToPreviousSubwordStart {
            ignore_newlines: rng.chance(2),
            ignore_brackets: rng.chance(2),
        },
        47 => Command::DeleteToNextSubwordEnd {
            ignore_newlines: rng.chance(2),
            ignore_brackets: rng.chance(2),
        },
        48 => Command::DeleteToBeginningOfLine {
            stop_at_indent: rng.chance(2),
        },
        49 => Command::DeleteToEndOfLine,
        50 => Command::DeleteLine,
        51 => Command::Tab,
        52 => rng
            .pick(&[Command::Backtab, Command::Indent, Command::Outdent])
            .clone(),
        53 => Command::MoveLineUp,
        54 => Command::MoveLineDown,
        55 => rng
            .pick(&[Command::DuplicateLineUp, Command::DuplicateLineDown])
            .clone(),
        56 => Command::JoinLines,
        57 => Command::Transpose,
        58 => Command::ToggleComments {
            prefix: (*rng.pick(&["//", "#", "// ", "--", ""])).to_string(),
            comment_empty_lines: rng.chance(2),
            advance_downwards: rng.chance(3),
        },
        59 => Command::ToggleBlockComments {
            start: "/*".to_string(),
            end: "*/".to_string(),
        },
        60 => rng
            .pick(&[
                Command::Cut,
                Command::Copy,
                Command::KillRingCut,
                Command::KillRingYank,
            ])
            .clone(),
        61 => match last_clipboard {
            Some(clipboard) if rng.chance(2) => Command::Paste(clipboard.clone()),
            _ => Command::Paste(random_clipboard(rng)),
        },
        62 => Command::Undo,
        _ => Command::Redo,
    }
}

fn random_settings(rng: &mut Rng) -> EditSettings {
    let mut settings = EditSettings {
        tab_size: *rng.pick(&[1, 2, 4, 4, 8]),
        hard_tabs: rng.chance(4),
        auto_indent: !rng.chance(4),
        autoclose: !rng.chance(4),
        wrap_width: *rng.pick(&[None, None, Some(1), Some(6), Some(20)]),
        page_rows: rng.below(6),
        ..EditSettings::default()
    };
    if rng.chance(4) {
        settings.word_chars = "-$".to_string();
    }
    if rng.chance(6) {
        settings.autoclose_pairs = vec![('<', '>'), ('(', ')')];
        settings.surround_pairs = Vec::new();
    }
    settings
}

fn random_selections(rng: &mut Rng, len: usize) -> Selections {
    let selections: Vec<Selection> = (0..1 + rng.below(4))
        .map(|_| {
            let anchor = rng.below(len + 3);
            if rng.chance(2) {
                Selection::cursor(anchor)
            } else {
                Selection::new(anchor, rng.below(len + 3))
            }
        })
        .collect();
    let primary = rng.below(selections.len());
    Selections::new(selections, primary)
}

/// The line and byte column of byte offset `byte` in `text`, computed the plain way.
fn byte_point(text: &str, byte: usize) -> BytePoint {
    let before = &text[..byte];
    BytePoint {
        line: before.matches('\n').count(),
        byte_col: byte - before.rfind('\n').map_or(0, |at| at + 1),
    }
}

fn chars_to_string(chars: &[char]) -> String {
    chars.iter().collect()
}

/// A summary with one batch must rebuild the new text from the old one, change by change,
/// and its parser edits must replay to the same result.
fn check_single_batch(old: &str, new: &str, changes: &[Change], summary: &EditSummary) {
    let old_chars: Vec<char> = old.chars().collect();
    let new_chars: Vec<char> = new.chars().collect();
    let mut rebuilt = String::new();
    let mut position = 0;
    for change in changes {
        assert!(
            change.old.start >= position && change.old.start <= change.old.end,
            "unsorted changes"
        );
        rebuilt.extend(&old_chars[position..change.old.start]);
        rebuilt.extend(&new_chars[change.new.clone()]);
        position = change.old.end;
        assert_eq!(
            &old[change.old_bytes.clone()],
            chars_to_string(&old_chars[change.old.clone()])
        );
        assert_eq!(
            &new[change.new_bytes.clone()],
            chars_to_string(&new_chars[change.new.clone()])
        );
        assert_eq!(
            change.old_start_point,
            byte_point(old, change.old_bytes.start)
        );
        assert_eq!(change.old_end_point, byte_point(old, change.old_bytes.end));
        assert_eq!(
            change.new_start_point,
            byte_point(new, change.new_bytes.start)
        );
        assert_eq!(change.new_end_point, byte_point(new, change.new_bytes.end));
    }
    rebuilt.extend(&old_chars[position..]);
    assert_eq!(
        rebuilt, new,
        "the changes do not turn the old text into the new one"
    );

    let mut current = old.to_string();
    for edit in summary.input_edits() {
        assert_eq!(edit.start_point, byte_point(&current, edit.start_byte));
        assert_eq!(edit.old_end_point, byte_point(&current, edit.old_end_byte));
        let inserted = &new[edit.start_byte..edit.new_end_byte];
        current.replace_range(edit.start_byte..edit.old_end_byte, inserted);
        assert_eq!(edit.new_end_point, byte_point(&current, edit.new_end_byte));
    }
    assert_eq!(
        current, new,
        "the parser edits do not replay to the new text"
    );
}

/// Everything that must hold after any operation.
fn check_state(editor: &Editor, context: &str) {
    let text = editor.buffer().to_string();
    let len = text.chars().count();
    assert!(
        !text.contains('\r'),
        "carriage return in the buffer {context}"
    );
    assert_eq!(editor.buffer().len_chars(), len);

    let lines: Vec<&str> = text.split('\n').collect();
    assert_eq!(
        editor.buffer().len_lines(),
        lines.len(),
        "line count {context}"
    );
    let mut start = 0;
    for (index, line) in lines.iter().enumerate() {
        assert_eq!(
            editor.buffer().line_to_char(index),
            start,
            "line start {context}"
        );
        assert_eq!(editor.buffer().line(index), *line, "line text {context}");
        start += line.chars().count() + 1;
    }

    let mut boundaries = vec![0];
    let mut chars = 0;
    for cluster in text.graphemes(true) {
        chars += cluster.chars().count();
        boundaries.push(chars);
    }
    let selections = editor.selections();
    for selection in selections.iter() {
        assert!(selection.end() <= len, "selection past the end {context}");
        for end in [selection.anchor, selection.head] {
            assert!(
                boundaries.binary_search(&end).is_ok(),
                "selection end {end} inside a cluster {context}"
            );
        }
    }
    for pair in selections.as_slice().windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let apart = a.end() < b.start() || (a.end() == b.start() && !a.is_empty() && !b.is_empty());
        assert!(
            apart,
            "selections {a:?} and {b:?} overlap or touch {context}"
        );
    }
    assert!(selections.primary_index() < selections.len());

    // The display queries agree with each other everywhere. (Two positions around a
    // zero-width cluster are drawn in the same cell, so the way back may pick the other.)
    for &position in &boundaries {
        let drawn = editor.display_position(position);
        let back = editor.position_at(drawn);
        assert_eq!(
            editor.display_position(back),
            drawn,
            "display round trip at {position} {context}"
        );
        assert!(
            boundaries.binary_search(&back).is_ok(),
            "position_at inside a cluster {context}"
        );
    }
}

/// What must hold between the state before and after one operation.
fn check_outcome(
    before_text: &str,
    before_selections: &Selections,
    before_version: u64,
    editor: &Editor,
    outcome: &Outcome,
    context: &str,
) {
    let after_text = editor.buffer().to_string();
    if !outcome.text_changed() {
        assert_eq!(
            before_text, after_text,
            "text changed without being reported {context}"
        );
        assert_eq!(
            editor.buffer().version(),
            before_version,
            "version moved without a change {context}"
        );
    } else {
        assert!(
            editor.buffer().version() > before_version,
            "version did not move {context}"
        );
    }
    assert_eq!(
        outcome.selections_changed,
        before_selections != editor.selections(),
        "selections_changed {context}"
    );
    let old_len = before_text.chars().count();
    let new_len = after_text.chars().count();
    assert_eq!(
        outcome.edits.map_offset(old_len, Bias::Right),
        new_len,
        "the end does not map to the end {context}"
    );
    assert_eq!(
        outcome.edits.map_offset(0, Bias::Left),
        0,
        "the start does not map to the start {context}"
    );
    if let [changes] = outcome.edits.batches() {
        check_single_batch(before_text, &after_text, changes, &outcome.edits);
    }
}

fn run(seed: u64, steps: usize) {
    let mut rng = Rng(seed);
    let original = random_text(&mut rng, 30);
    let settings = random_settings(&mut rng);
    let mut editor = Editor::with_settings(crate::Buffer::from_str(&original), settings.clone());
    let clock = Arc::new(ManualClock::new(0));
    editor.set_clock(clock.clone());
    let original = editor.buffer().to_string();
    check_state(&editor, "at the start");

    let mut last_clipboard: Option<Clipboard> = None;
    for step in 0..steps {
        if editor.buffer().len_chars() > 3_000 {
            break;
        }
        clock.advance(*rng.pick(&[0, 10, 100, 299, 300, 301, 1_000]));
        let before_text = editor.buffer().to_string();
        let before_selections = editor.selections().clone();
        let before_version = editor.buffer().version();
        let len = editor.buffer().len_chars();

        let (description, outcome) = match rng.below(14) {
            0 => {
                let selections = random_selections(&mut rng, len);
                (
                    format!("set_selections({selections:?})"),
                    editor.set_selections(selections),
                )
            }
            1 => {
                let head = rng.below(len + 3);
                (
                    format!("extend_primary_to({head})"),
                    editor.extend_primary_to(head),
                )
            }
            2 => {
                let mut cuts: Vec<usize> =
                    (0..rng.below(3) * 2).map(|_| rng.below(len + 3)).collect();
                cuts.sort_unstable();
                let edits: Vec<(std::ops::Range<usize>, String)> = cuts
                    .chunks(2)
                    .map(|pair| (pair[0]..pair[1], random_text(&mut rng, 3)))
                    .collect();
                (format!("edit({edits:?})"), editor.edit(&edits))
            }
            3 if rng.chance(3) => {
                let needle = *rng.pick(&["a", " ", "\n", "(", "fooBar", "世"]);
                let search = SearchQuery::text(needle).compile().unwrap();
                let replacement = random_text(&mut rng, 2);
                (
                    format!("replace_all({needle:?}, {replacement:?})"),
                    editor.replace_all(&search, &replacement),
                )
            }
            _ => {
                let command = random_command(&mut rng, &last_clipboard);
                (format!("{command:?}"), editor.apply(command))
            }
        };
        let context = format!(
            "(seed {seed}, step {step}: {description} on {before_text:?} with {before_selections:?} and {settings:?})"
        );
        check_state(&editor, &context);
        check_outcome(
            &before_text,
            &before_selections,
            before_version,
            &editor,
            &outcome,
            &context,
        );
        if outcome.clipboard.is_some() {
            last_clipboard = outcome.clipboard;
        }
    }

    // Undo everything, then redo everything.
    let end = editor.buffer().to_string();
    let mut undone = 0;
    while editor.buffer().can_undo() {
        let outcome = editor.apply(Command::Undo);
        assert!(
            !outcome.edits.batches().is_empty(),
            "an undo step without edits (seed {seed})"
        );
        check_state(&editor, &format!("(seed {seed}, undoing)"));
        undone += 1;
    }
    assert_eq!(
        editor.buffer().to_string(),
        original,
        "undoing everything did not restore the text (seed {seed})"
    );
    assert!(
        !editor.buffer().is_dirty(),
        "the original text is not clean (seed {seed})"
    );
    // Redoing exactly those steps leads back to the text the run ended with.
    for _ in 0..undone {
        editor.apply(Command::Redo);
    }
    check_state(&editor, &format!("(seed {seed}, redone)"));
    assert_eq!(
        editor.buffer().to_string(),
        end,
        "redoing everything did not restore the text (seed {seed})"
    );
}

/// How many seeds the main test runs. Set `BAKEDIT_FUZZ_SEEDS` for a deeper run.
fn seed_count() -> u64 {
    std::env::var("BAKEDIT_FUZZ_SEEDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(400)
}

#[test]
fn random_commands_keep_every_invariant() {
    for seed in 1..=seed_count() {
        run(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1, 60);
    }
}

#[test]
fn long_random_sessions() {
    for seed in 1..=12u64 {
        run(seed.wrapping_mul(0xD6E8_FEB8_6659_FD93) | 1, 600);
    }
}

#[test]
fn undo_everything_then_redo_everything_round_trips() {
    // Without Undo and Redo among the commands, undoing everything and redoing everything
    // uses up the whole history in both directions.
    for seed in 1..=150u64 {
        let mut rng = Rng(seed.wrapping_mul(0xA24B_AED4_963E_E407) | 1);
        let settings = random_settings(&mut rng);
        let mut editor = Editor::with_settings(
            crate::Buffer::from_str(&random_text(&mut rng, 30)),
            settings,
        );
        let original = editor.buffer().to_string();
        for _ in 0..50 {
            if editor.buffer().len_chars() > 3_000 {
                break;
            }
            let command = random_command(&mut rng, &None);
            if matches!(command, Command::Undo | Command::Redo) {
                continue;
            }
            editor.apply(command);
        }
        let end = editor.buffer().to_string();
        let mut steps = 0;
        while editor.buffer().can_undo() {
            editor.apply(Command::Undo);
            steps += 1;
        }
        assert_eq!(editor.buffer().to_string(), original, "seed {seed}");
        for _ in 0..steps {
            editor.apply(Command::Redo);
        }
        assert_eq!(editor.buffer().to_string(), end, "seed {seed}");
        assert!(!editor.buffer().can_redo(), "seed {seed}");
    }
}
