//! Commands that change the selections without editing.

use super::{apply_all, check, check_with, editor, state, type_text};
use crate::command::{Command, Motion};
use crate::editor::EditSettings;
use crate::pairs::PairKind;
use crate::selection::Selection;

const NEXT: Command = Command::SelectNext {
    replace_newest: false,
};
const PREVIOUS: Command = Command::SelectPrevious {
    replace_newest: false,
};
const BELOW: Command = Command::AddSelectionBelow {
    skip_soft_wrap: true,
};
const ABOVE: Command = Command::AddSelectionAbove {
    skip_soft_wrap: true,
};

#[test]
fn select_all() {
    check("abˇc\ndef", Command::SelectAll, "«abc\ndefˇ»");
    check("ˇaˇb«cˇ»", Command::SelectAll, "«abcˇ»");
    check("ˇ", Command::SelectAll, "ˇ");
}

#[test]
fn select_line_takes_whole_lines_and_grows() {
    let mut e = editor("ab\ncˇd\nef\ngh");
    apply_all(&mut e, [Command::SelectLine]);
    assert_eq!(state(&e), "ab\n«cd\nˇ»ef\ngh");
    apply_all(&mut e, [Command::SelectLine]);
    assert_eq!(state(&e), "ab\n«cd\nef\nˇ»gh");
    apply_all(&mut e, [Command::SelectLine]);
    assert_eq!(state(&e), "ab\n«cd\nef\nghˇ»");
    apply_all(&mut e, [Command::SelectLine]);
    assert_eq!(state(&e), "ab\n«cd\nef\nghˇ»");
}

#[test]
fn select_line_shapes() {
    check("ab\ncˇd", Command::SelectLine, "ab\n«cdˇ»");
    check("a«b\ncˇ»d\nef", Command::SelectLine, "«ab\ncd\nˇ»ef");
    check("a«ˇb\nc»d\nef", Command::SelectLine, "«ab\ncd\nˇ»ef");
    check("aˇb\ncd\neˇf", Command::SelectLine, "«ab\nˇ»cd\n«efˇ»");
    check("aˇb\ncˇd\nef", Command::SelectLine, "«ab\nˇ»«cd\nˇ»ef");
    check("aˇbˇ\ncd", Command::SelectLine, "«ab\nˇ»cd");
    check("ˇ", Command::SelectLine, "ˇ");
    check("ab\nˇ", Command::SelectLine, "ab\nˇ");
}

#[test]
fn cancel_drops_extra_cursors_then_the_selection() {
    let mut e = editor("one two one two one");
    e.select_range(8, 11);
    apply_all(&mut e, [NEXT, NEXT]);
    assert_eq!(state(&e), "«oneˇ» two «oneˇ» two «oneˇ»");
    apply_all(&mut e, [Command::Cancel]);
    assert_eq!(
        state(&e),
        "one two «oneˇ» two one",
        "the selection the others were added to stays"
    );
    apply_all(&mut e, [Command::Cancel]);
    assert_eq!(state(&e), "one two oneˇ two one");
    let outcome = e.apply(Command::Cancel);
    assert!(outcome.is_noop());

    check("a«ˇbc»d", Command::Cancel, "aˇbcd");
    check("aˇbcˇd", Command::Cancel, "aˇbcd");
}

#[test]
fn select_next_from_a_cursor_selects_whole_words() {
    let mut e = editor("foo bar fˇoo foobar foo");
    apply_all(&mut e, [NEXT]);
    assert_eq!(
        state(&e),
        "foo bar «fooˇ» foobar foo",
        "the first use only selects the word"
    );
    apply_all(&mut e, [NEXT]);
    assert_eq!(
        state(&e),
        "foo bar «fooˇ» foobar «fooˇ»",
        "foobar is not a whole-word match"
    );
    assert_eq!(
        e.selections().primary(),
        Selection::new(19, 22),
        "the newest selection is the primary"
    );
    apply_all(&mut e, [NEXT]);
    assert_eq!(
        state(&e),
        "«fooˇ» bar «fooˇ» foobar «fooˇ»",
        "the search wraps around"
    );
    assert_eq!(e.selections().primary(), Selection::new(0, 3));
    let outcome = e.apply(NEXT);
    assert!(outcome.is_noop(), "every occurrence is selected");
}

#[test]
fn select_next_from_a_selection_matches_anywhere() {
    let mut e = editor("«fooˇ» foobar foo");
    apply_all(&mut e, [NEXT]);
    assert_eq!(state(&e), "«fooˇ» «fooˇ»bar foo");
    apply_all(&mut e, [NEXT]);
    assert_eq!(state(&e), "«fooˇ» «fooˇ»bar «fooˇ»");

    // Case matters, and so does every character of a multi-line selection.
    check("Foo «fooˇ» FOO foo", NEXT, "Foo «fooˇ» FOO «fooˇ»");
    check("«a\nbˇ» a b a\nb", NEXT, "«a\nbˇ» a b «a\nbˇ»");
    check("«a.bˇ» axb a.b", NEXT, "«a.bˇ» axb «a.bˇ»");
}

#[test]
fn select_next_on_punctuation_and_whitespace() {
    let mut e = editor("a =ˇ> b => c");
    apply_all(&mut e, [NEXT, NEXT]);
    assert_eq!(state(&e), "a «=>ˇ» b «=>ˇ» c");
    // Nothing to select in the middle of whitespace.
    let mut e = editor("a  ˇ  b");
    let outcome = e.apply(NEXT);
    assert!(outcome.is_noop());
    assert_eq!(state(&e), "a  ˇ  b");
}

#[test]
fn select_next_with_several_cursors_selects_each_word() {
    let mut e = editor("oˇne two\nthrˇee one");
    apply_all(&mut e, [NEXT]);
    assert_eq!(state(&e), "«oneˇ» two\n«threeˇ» one");
    // The search then goes on with the primary selection's word.
    apply_all(&mut e, [NEXT]);
    assert_eq!(
        state(&e),
        "«oneˇ» two\n«threeˇ» one",
        "three occurs only once"
    );
}

#[test]
fn select_next_replacing_the_newest() {
    let replace = Command::SelectNext {
        replace_newest: true,
    };
    check("«fooˇ» foo foo", replace.clone(), "foo «fooˇ» foo");
    let mut e = editor("«fooˇ» foo foo foo");
    apply_all(&mut e, [NEXT, replace.clone()]);
    assert_eq!(
        state(&e),
        "«fooˇ» foo «fooˇ» foo",
        "the second selection moved on"
    );
    apply_all(&mut e, [replace]);
    assert_eq!(state(&e), "«fooˇ» foo foo «fooˇ»");
}

#[test]
fn select_previous() {
    let mut e = editor("foo foo fˇoo");
    apply_all(&mut e, [PREVIOUS]);
    assert_eq!(state(&e), "foo foo «fooˇ»");
    apply_all(&mut e, [PREVIOUS]);
    assert_eq!(state(&e), "foo «fooˇ» «fooˇ»");
    assert_eq!(e.selections().primary(), Selection::new(4, 7));
    apply_all(&mut e, [PREVIOUS]);
    assert_eq!(state(&e), "«fooˇ» «fooˇ» «fooˇ»");
    assert!(e.apply(PREVIOUS).is_noop());

    // Going backwards wraps to the end.
    check("«fooˇ» bar foo", PREVIOUS, "«fooˇ» bar «fooˇ»");
    let replace = Command::SelectPrevious {
        replace_newest: true,
    };
    check("foo foo «fooˇ»", replace, "foo «fooˇ» foo");
}

#[test]
fn select_all_matches() {
    let mut e = editor("foo bar fˇoo foobar foo");
    apply_all(&mut e, [Command::SelectAllMatches]);
    assert_eq!(state(&e), "«fooˇ» bar «fooˇ» foobar «fooˇ»");
    assert_eq!(
        e.selections().primary(),
        Selection::new(8, 11),
        "the word under the cursor stays primary"
    );

    check(
        "«fooˇ» foobar",
        Command::SelectAllMatches,
        "«fooˇ» «fooˇ»bar",
    );
    check("«aˇ»aaa", Command::SelectAllMatches, "«aˇ»«aˇ»«aˇ»«aˇ»");
    check("a  ˇ  b", Command::SelectAllMatches, "a  ˇ  b");
    check("ˇ", Command::SelectAllMatches, "ˇ");
}

#[test]
fn the_search_is_forgotten_when_something_else_happens() {
    let mut e = editor("foo foobar fˇoo");
    apply_all(&mut e, [NEXT]);
    assert_eq!(state(&e), "foo foobar «fooˇ»");
    // Moving away and selecting text by hand starts a new, non-word search.
    apply_all(&mut e, [Command::Move(Motion::Beginning)]);
    e.select_range(0, 3);
    apply_all(&mut e, [NEXT]);
    assert_eq!(state(&e), "«fooˇ» «fooˇ»bar foo");
}

#[test]
fn add_selection_below_and_above() {
    let mut e = editor("abˇcd\nefgh\nijkl");
    apply_all(&mut e, [BELOW]);
    assert_eq!(state(&e), "abˇcd\nefˇgh\nijkl");
    assert_eq!(
        e.selections().primary().head,
        7,
        "the new cursor is the primary"
    );
    apply_all(&mut e, [BELOW]);
    assert_eq!(state(&e), "abˇcd\nefˇgh\nijˇkl");
    let outcome = e.apply(BELOW);
    assert!(outcome.is_noop(), "no row left below");

    // The opposite command takes the last cursor away again.
    apply_all(&mut e, [ABOVE]);
    assert_eq!(state(&e), "abˇcd\nefˇgh\nijkl");
    apply_all(&mut e, [ABOVE]);
    assert_eq!(state(&e), "abˇcd\nefgh\nijkl");
    // Back at the original cursor, it adds in its own direction (here there is no row).
    assert!(e.apply(ABOVE).is_noop());

    check("ab\ncˇd", ABOVE, "aˇb\ncˇd");
    let mut e = editor("ab\ncd\neˇf");
    apply_all(&mut e, [ABOVE, ABOVE, BELOW]);
    assert_eq!(state(&e), "ab\ncˇd\neˇf");
}

#[test]
fn add_selection_skips_rows_that_are_too_short() {
    check("abcdˇef\nxy\nabcdefgh", BELOW, "abcdˇef\nxy\nabcdˇefgh");
    check("abcdefgh\nxy\nabcdˇef", ABOVE, "abcdˇefgh\nxy\nabcdˇef");
    // A row exactly as long as the column still takes a cursor at its end.
    check("abˇ\ncd\nef", BELOW, "abˇ\ncdˇ\nef");
    check("abcˇ\n\nab", BELOW, "abcˇ\n\nab");
    // Columns are display columns: the tab is 4 wide.
    check("\tˇx\nabcdefgh", BELOW, "\tˇx\nabcdˇefgh");
    check("世界ˇx\nabcdefgh", BELOW, "世界ˇx\nabcdˇefgh");
}

#[test]
fn add_selection_with_a_range() {
    let mut e = editor("a«bcˇ»d\nxyzw\nq\nlmnop");
    apply_all(&mut e, [BELOW]);
    assert_eq!(state(&e), "a«bcˇ»d\nx«yzˇ»w\nq\nlmnop");
    apply_all(&mut e, [BELOW]);
    assert_eq!(
        state(&e),
        "a«bcˇ»d\nx«yzˇ»w\nq\nl«mnˇ»op",
        "the one-character row is skipped"
    );
    // A range is cut off at the end of a short row.
    check("a«bcdˇ»e\nxyz", BELOW, "a«bcdˇ»e\nx«yzˇ»");
    check("a«ˇbc»d\nxyzw", BELOW, "a«ˇbc»d\nx«ˇyz»w");
}

#[test]
fn add_selection_splits_a_selection_that_spans_rows() {
    check("a«bc\ndeˇ»f\nghij", BELOW, "a«bˇ»c\nd«eˇ»f\ng«hˇ»ij");
    check("ghij\na«bc\ndeˇ»f", ABOVE, "g«hˇ»ij\na«bˇ»c\nd«eˇ»f");
}

#[test]
fn add_selection_from_several_cursors() {
    check("aˇbcˇd\nefgh", BELOW, "aˇbcˇd\neˇfgˇh");
    let mut e = editor("ab\ncˇd\nef");
    apply_all(&mut e, [BELOW, Command::Move(Motion::Left), ABOVE]);
    assert_eq!(
        state(&e),
        "ˇab\nˇcd\nˇef",
        "after another command each cursor starts a new column"
    );
    // Cancel goes back to where the column started.
    let mut e = editor("abˇcd\nefgh\nijkl");
    apply_all(&mut e, [BELOW, BELOW, Command::Cancel]);
    assert_eq!(state(&e), "abˇcd\nefgh\nijkl");
}

#[test]
fn add_selection_by_wrapped_rows() {
    let wrap = EditSettings {
        wrap_width: Some(10),
        ..EditSettings::default()
    };
    let by_row = Command::AddSelectionBelow {
        skip_soft_wrap: false,
    };
    check_with(
        &wrap,
        "the quˇick brown fox",
        by_row.clone(),
        "the quˇick brown ˇfox",
    );
    check_with(&wrap, "the quˇick brown fox", BELOW, "the quˇick brown fox");
    check_with(
        &wrap,
        "the quˇick brown fox\nnext line",
        BELOW,
        "the quˇick brown fox\nnext lˇine",
    );
    check_with(
        &wrap,
        "the quick brown ˇfox",
        Command::AddSelectionAbove {
            skip_soft_wrap: false,
        },
        "the quˇick brown ˇfox",
    );
    // Without wrapping both forms count buffer lines.
    check("abˇc\ndef", by_row, "abˇc\ndeˇf");
}

#[test]
fn typing_at_column_cursors() {
    let mut e = editor("ˇone\ntwo\nthree");
    apply_all(&mut e, [BELOW, BELOW]);
    type_text(&mut e, "- ");
    assert_eq!(state(&e), "- ˇone\n- ˇtwo\n- ˇthree");
    apply_all(&mut e, [Command::Cancel]);
    assert_eq!(state(&e), "- ˇone\n- two\n- three");
}

#[test]
fn undo_and_redo_selection() {
    let mut e = editor("abcdˇef");
    apply_all(
        &mut e,
        [Command::Move(Motion::Left), Command::Move(Motion::Left)],
    );
    assert_eq!(state(&e), "abˇcdef");
    apply_all(&mut e, [Command::UndoSelection]);
    assert_eq!(state(&e), "abcˇdef");
    apply_all(&mut e, [Command::UndoSelection]);
    assert_eq!(state(&e), "abcdˇef");
    apply_all(&mut e, [Command::RedoSelection]);
    assert_eq!(state(&e), "abcˇdef");
    apply_all(&mut e, [Command::RedoSelection]);
    assert_eq!(state(&e), "abˇcdef");
    assert!(e.apply(Command::RedoSelection).is_noop());

    // A new selection change drops what could be redone.
    apply_all(&mut e, [Command::UndoSelection, Command::Move(Motion::End)]);
    assert!(e.apply(Command::RedoSelection).is_noop());
    assert_eq!(state(&e), "abcdefˇ");
}

#[test]
fn undo_selection_takes_back_an_added_cursor() {
    let mut e = editor("foo «fooˇ» foo");
    apply_all(&mut e, [NEXT, NEXT]);
    assert_eq!(state(&e), "«fooˇ» «fooˇ» «fooˇ»");
    apply_all(&mut e, [Command::UndoSelection]);
    assert_eq!(state(&e), "foo «fooˇ» «fooˇ»");
    apply_all(&mut e, [Command::UndoSelection]);
    assert_eq!(state(&e), "foo «fooˇ» foo");
}

#[test]
fn the_selection_history_follows_the_text() {
    // Remember a cursor at the end, move to the middle and type there.
    let mut e = editor("hello worldˇ");
    apply_all(&mut e, [Command::Move(Motion::Beginning)]);
    type_text(&mut e, ">> ");
    assert_eq!(state(&e), ">> ˇhello world");
    // Typing is not a selection change of its own, so this goes straight back to the end,
    // which has moved along with the text.
    apply_all(&mut e, [Command::UndoSelection]);
    assert_eq!(state(&e), ">> hello worldˇ");
}

#[test]
fn undo_selection_with_nothing_to_undo() {
    let mut e = crate::editor::Editor::from_str("abc");
    assert!(e.apply(Command::UndoSelection).is_noop());
    assert!(e.apply(Command::RedoSelection).is_noop());
}

/// The selected text of the only selection.
fn selected(e: &crate::editor::Editor) -> String {
    e.buffer()
        .slice(e.selections().primary().range())
        .to_string()
}

#[test]
fn select_larger_grows_outwards_and_smaller_steps_back() {
    let mut e = editor("fn f() {\n    call(\"a bˇc\", d);\n}");
    let steps = [
        "bc",
        "a bc",
        "\"a bc\"",
        "\"a bc\", d",
        "(\"a bc\", d)",
        "call(\"a bc\", d);",
        "    call(\"a bc\", d);",
        "\n    call(\"a bc\", d);\n",
        "{\n    call(\"a bc\", d);\n}",
        "fn f() {\n    call(\"a bc\", d);\n}",
    ];
    for step in steps {
        apply_all(&mut e, [Command::SelectLargerSyntaxNode]);
        assert_eq!(selected(&e), step);
    }
    assert!(
        e.apply(Command::SelectLargerSyntaxNode).is_noop(),
        "nothing larger than everything"
    );

    for step in steps.iter().rev().skip(1) {
        apply_all(&mut e, [Command::SelectSmallerSyntaxNode]);
        assert_eq!(selected(&e), *step);
    }
    apply_all(&mut e, [Command::SelectSmallerSyntaxNode]);
    assert_eq!(
        state(&e),
        "fn f() {\n    call(\"a bˇc\", d);\n}",
        "back at the cursor"
    );
    assert!(e.apply(Command::SelectSmallerSyntaxNode).is_noop());
}

#[test]
fn select_larger_shapes() {
    check("foo(ˇ)", Command::SelectLargerSyntaxNode, "foo«()ˇ»");
    check("foo«()ˇ»", Command::SelectLargerSyntaxNode, "«foo()ˇ»");
    check(
        "a + (ˇ  ) + b",
        Command::SelectLargerSyntaxNode,
        "a + («  ˇ») + b",
    );
    check(
        "x = [1, «2ˇ», 3]",
        Command::SelectLargerSyntaxNode,
        "x = [«1, 2, 3ˇ»]",
    );
    check(
        "x = [«1, 2, 3ˇ»]",
        Command::SelectLargerSyntaxNode,
        "x = «[1, 2, 3]ˇ»",
    );
    check("  wordˇ  ", Command::SelectLargerSyntaxNode, "  «wordˇ»  ");
    check(
        "  «wordˇ»  ",
        Command::SelectLargerSyntaxNode,
        "«  word  ˇ»",
    );
    check("ˇ", Command::SelectLargerSyntaxNode, "ˇ");
    check(
        "a\n«b\ncˇ»\nd",
        Command::SelectLargerSyntaxNode,
        "«a\nb\nc\ndˇ»",
    );
    // Every cursor grows on its own.
    check(
        "(aˇb) [cˇd]",
        Command::SelectLargerSyntaxNode,
        "(«abˇ») [«cdˇ»]",
    );
}

#[test]
fn select_smaller_only_follows_select_larger() {
    let mut e = editor("foo(baˇr)");
    assert!(e.apply(Command::SelectSmallerSyntaxNode).is_noop());
    apply_all(&mut e, [Command::SelectLargerSyntaxNode]);
    assert_eq!(state(&e), "foo(«barˇ»)");
    apply_all(&mut e, [Command::SelectLargerSyntaxNode]);
    assert_eq!(state(&e), "foo«(bar)ˇ»");
    apply_all(&mut e, [Command::SelectSmallerSyntaxNode]);
    assert_eq!(state(&e), "foo(«barˇ»)");
    // Anything else in between makes the steps unusable.
    apply_all(&mut e, [Command::Select(Motion::Left)]);
    assert_eq!(state(&e), "foo(«baˇ»r)");
    assert!(e.apply(Command::SelectSmallerSyntaxNode).is_noop());
}

#[test]
fn select_between() {
    let paren = Command::SelectBetween(PairKind::Paren);
    let mut e = editor("f(a, (bˇ), c)");
    apply_all(&mut e, [paren.clone()]);
    assert_eq!(state(&e), "f(a, («bˇ»), c)");
    apply_all(&mut e, [paren.clone()]);
    assert_eq!(
        state(&e),
        "f(«a, (b), cˇ»)",
        "already selected: grow to the next pair"
    );
    apply_all(&mut e, [paren.clone()]);
    assert_eq!(state(&e), "f(«a, (b), cˇ»)", "the outermost pair stays");

    // Outside every pair the cursor jumps into the first one of the text.
    check("ˇx = f(a) + g(b)", paren.clone(), "x = f(ˇa) + g(b)");
    check("x = f(a) + g(b)ˇ", paren.clone(), "x = f(ˇa) + g(b)");
    check("plˇain", paren.clone(), "plˇain");
    check("x (ˇ", paren.clone(), "x (ˇ");
    check("ˇx ( y", paren.clone(), "x (ˇ y");
    // Only the primary selection counts, and one selection comes out.
    check("ˇf(a) (bˇ)", paren, "f(a) («bˇ»)");
}

#[test]
fn select_between_every_kind() {
    check(
        "a[1, ˇ2]",
        Command::SelectBetween(PairKind::Bracket),
        "a[«1, 2ˇ»]",
    );
    check(
        "{ a: ˇ1 }",
        Command::SelectBetween(PairKind::Brace),
        "{« a: 1 ˇ»}",
    );
    check(
        "Vec<Stˇring>",
        Command::SelectBetween(PairKind::Angle),
        "Vec<«Stringˇ»>",
    );
    check(
        "say \"helˇlo\" now",
        Command::SelectBetween(PairKind::DoubleQuote),
        "say \"«helloˇ»\" now",
    );
    check(
        "say 'helˇlo' now",
        Command::SelectBetween(PairKind::SingleQuote),
        "say '«helloˇ»' now",
    );
    check(
        "run `lˇs -la` now",
        Command::SelectBetween(PairKind::Backtick),
        "run `«ls -laˇ»` now",
    );
    check(
        "don't 'quoˇte'",
        Command::SelectBetween(PairKind::SingleQuote),
        "don't '«quoteˇ»'",
    );
    check(
        "a = \"\"ˇ",
        Command::SelectBetween(PairKind::DoubleQuote),
        "a = \"ˇ\"",
    );
    // A bracket inside a string does not confuse the pairs around it.
    check(
        "f(\"(\", ˇx)",
        Command::SelectBetween(PairKind::Paren),
        "f(«\"(\", xˇ»)",
    );
}
