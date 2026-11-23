//! Typing, line breaks and deleting at the cursors.

use super::{apply_all, check, check_with, editor, state, type_text};
use crate::command::Command;
use crate::edit::Bias;
use crate::editor::EditSettings;

fn insert(text: &str) -> Command {
    Command::Insert(text.to_string())
}

fn no_autoclose() -> EditSettings {
    EditSettings {
        autoclose: false,
        ..EditSettings::default()
    }
}

fn no_auto_indent() -> EditSettings {
    EditSettings {
        auto_indent: false,
        ..EditSettings::default()
    }
}

fn hard_tabs() -> EditSettings {
    EditSettings {
        hard_tabs: true,
        ..EditSettings::default()
    }
}

const BACK_WORD: Command = Command::DeleteToPreviousWordStart {
    ignore_newlines: false,
    ignore_brackets: false,
};
const FORWARD_WORD: Command = Command::DeleteToNextWordEnd {
    ignore_newlines: false,
    ignore_brackets: false,
};

#[test]
fn typing_inserts_and_replaces() {
    check("abˇc", insert("x"), "abxˇc");
    check("ˇ", insert("x"), "xˇ");
    check("a«bcˇ»d", insert("x"), "axˇd");
    check("a«ˇbc»d", insert("x"), "axˇd");
    check("abˇ", insert("hello"), "abhelloˇ");
    check("aˇb", insert("世界"), "a世界ˇb");
    check("aˇb", insert("e\u{301}"), "ae\u{301}ˇb");
    check("aˇb", insert("x\r\ny\rz"), "ax\ny\nzˇb");
    check("a«bcˇ»d", insert(""), "aˇd");
}

#[test]
fn typing_at_several_cursors() {
    check("ˇa\nˇb\nˇc", insert("- "), "- ˇa\n- ˇb\n- ˇc");
    check("«oneˇ» two «oneˇ»", insert("1"), "1ˇ two 1ˇ");
    check("aˇbˇc", insert("--"), "a--ˇb--ˇc");
}

#[test]
fn typing_reports_what_changed() {
    let mut e = editor("abˇc");
    let outcome = e.apply(insert("xy"));
    assert!(outcome.text_changed() && outcome.selections_changed && !outcome.is_noop());
    assert_eq!(outcome.edits.map_offset(2, Bias::Left), 2);
    assert_eq!(outcome.edits.map_offset(2, Bias::Right), 4);
    assert_eq!(outcome.edits.map_offset(3, Bias::Left), 5);
    let edits = outcome.edits.input_edits();
    assert_eq!(edits.len(), 1);
    assert_eq!(
        (
            edits[0].start_byte,
            edits[0].old_end_byte,
            edits[0].new_end_byte
        ),
        (2, 2, 4)
    );
    assert_eq!(e.buffer().version(), 1);
}

#[test]
fn opening_brackets_and_quotes_are_closed() {
    check("ˇ", insert("("), "(ˇ)");
    check("ˇ", insert("["), "[ˇ]");
    check("ˇ", insert("{"), "{ˇ}");
    check("ˇ", insert("\""), "\"ˇ\"");
    check("ˇ", insert("'"), "'ˇ'");
    check("ˇ", insert("`"), "`ˇ`");
    check("foo(ˇ)", insert("["), "foo([ˇ])");
    check("ˇ;", insert("("), "(ˇ);");
    check("xˇ y", insert("("), "x(ˇ) y");
    check("f(ˇ, 1)", insert("\""), "f(\"ˇ\", 1)");
    check("ˇ\nnext", insert("{"), "{ˇ}\nnext");
    check("ˇa\nˇb", insert("("), "(ˇa\n(ˇb");
    check("ˇ\nˇ", insert("("), "(ˇ)\n(ˇ)");
}

#[test]
fn nothing_is_closed_in_front_of_a_word() {
    check("ˇfoo", insert("("), "(ˇfoo");
    check("ˇword", insert("\""), "\"ˇword");
    check("ˇ1", insert("["), "[ˇ1");
    check("ˇ\"x\"", insert("("), "(ˇ\"x\"");
}

#[test]
fn quotes_are_not_closed_next_to_a_word_or_another_quote() {
    check("donˇ", insert("'"), "don'ˇ");
    check("xˇ", insert("\""), "x\"ˇ");
    check("x_ˇ", insert("`"), "x_`ˇ");
    check("\"\"ˇ", insert("\""), "\"\"\"ˇ");
    // After punctuation or a space a quote is closed as usual.
    check("f(ˇ)", insert("'"), "f('ˇ')");
    check("x = ˇ", insert("\""), "x = \"ˇ\"");
    // Brackets do not mind a word before them.
    check("fooˇ", insert("("), "foo(ˇ)");
}

#[test]
fn typing_a_closer_that_is_already_there_steps_over_it() {
    check("(ˇ)", insert(")"), "()ˇ");
    check("[xˇ]", insert("]"), "[x]ˇ");
    check("\"abcˇ\"", insert("\""), "\"abc\"ˇ");
    check("'ˇ'", insert("'"), "''ˇ");
    // Otherwise the character is simply typed.
    check("(ˇ", insert(")"), "()ˇ");
    check("(ˇ]", insert(")"), "()ˇ]");
    check("(ˇ)", insert("]"), "(]ˇ)");
    let mut e = editor("(ˇ)");
    let outcome = e.apply(insert(")"));
    assert!(!outcome.text_changed() && outcome.selections_changed);
}

#[test]
fn typing_a_whole_call_with_autoclose() {
    let mut e = editor("ˇ");
    type_text(&mut e, "f(a[0], \"s\")");
    assert_eq!(state(&e), "f(a[0], \"s\")ˇ");
    let mut e = editor("ˇ");
    type_text(&mut e, "if x { y(); }");
    assert_eq!(state(&e), "if x { y(); }ˇ");
}

#[test]
fn typing_an_opener_wraps_a_selection() {
    check("a«bcˇ»d", insert("("), "a(«bcˇ»)d");
    check("a«ˇbc»d", insert("["), "a[«ˇbc»]d");
    check("a«bcˇ»d", insert("\""), "a\"«bcˇ»\"d");
    check("«oneˇ» «twoˇ»", insert("{"), "{«oneˇ»} {«twoˇ»}");
    check("«abˇ»«cdˇ»", insert("("), "(«abˇ»)(«cdˇ»)");
    check("«a\nbˇ»", insert("("), "(«a\nbˇ»)");
    // A character that opens nothing replaces the selection.
    check("a«bcˇ»d", insert(")"), "a)ˇd");
    check("a«bcˇ»d", insert("x"), "axˇd");
}

#[test]
fn autoclose_can_be_turned_off_or_changed() {
    let off = no_autoclose();
    check_with(&off, "ˇ", insert("("), "(ˇ");
    check_with(&off, "a«bcˇ»d", insert("("), "a(ˇd");
    check_with(&off, "(ˇ)", insert(")"), "()ˇ)");
    check_with(&off, "ˇ", insert("\""), "\"ˇ");

    let angle = EditSettings {
        autoclose_pairs: vec![('<', '>')],
        surround_pairs: vec![('<', '>')],
        ..EditSettings::default()
    };
    check_with(&angle, "ˇ", insert("<"), "<ˇ>");
    check_with(&angle, "<ˇ>", insert(">"), "<>ˇ");
    check_with(&angle, "ˇ", insert("("), "(ˇ");
    check_with(&angle, "a«bˇ»c", insert("<"), "a<«bˇ»>c");
    check_with(&angle, "a«bˇ»c", insert("("), "a(ˇc");
}

#[test]
fn pasted_or_composed_text_is_never_closed() {
    check("ˇ", insert("(("), "((ˇ");
    check("ˇ", insert("()"), "()ˇ");
    check("a«bcˇ»d", insert("(("), "a((ˇd");
}

#[test]
fn each_cursor_decides_for_itself() {
    // The first cursor steps over the bracket; the second has to type one.
    check("(ˇ) (ˇx", insert(")"), "()ˇ ()ˇx");
    check("ˇ ˇx", insert("("), "(ˇ) (ˇx");
}

#[test]
fn a_closing_bracket_on_its_own_line_lines_up_with_its_opener() {
    check(
        "if x {\n    foo();\n    ˇ",
        insert("}"),
        "if x {\n    foo();\n}ˇ",
    );
    check(
        "fn f() {\n    if x {\n        y();\n        ˇ",
        insert("}"),
        "fn f() {\n    if x {\n        y();\n    }ˇ",
    );
    check("foo(\n    a,\n    ˇ", insert(")"), "foo(\n    a,\n)ˇ");
    check("x = [\n    1,\n        ˇ", insert("]"), "x = [\n    1,\n]ˇ");
    check_with(
        &hard_tabs(),
        "\tif x {\n\t\ty\n\t\tˇ",
        insert("}"),
        "\tif x {\n\t\ty\n\t}ˇ",
    );
    // Several cursors, each with its own opener.
    check(
        "a {\n    x\n    ˇ\nb {\n        y\n        ˇ",
        insert("}"),
        "a {\n    x\n}ˇ\nb {\n        y\n}ˇ",
    );
    // It is one undo step with the typing.
    let mut e = editor("if x {\n    ˇ");
    apply_all(&mut e, [insert("}"), Command::Undo]);
    assert_eq!(state(&e), "if x {\n    ˇ");
}

#[test]
fn a_closing_bracket_stays_where_it_is_typed_otherwise() {
    // Text before it on the line.
    check("if x {\n    foo ˇ", insert("}"), "if x {\n    foo }ˇ");
    // Nothing to close, or the opener is on the same line.
    check("    ˇ", insert("}"), "    }ˇ");
    check("    {ˇ", insert("}"), "    {}ˇ");
    // The opener is indented at least as much.
    check("    if x {\n    ˇ", insert("}"), "    if x {\n    }ˇ");
    check(
        "        if x {\n    ˇ",
        insert("}"),
        "        if x {\n    }ˇ",
    );
    // Stepping over an existing closer edits nothing.
    check("{\n    ˇ}", insert("}"), "{\n    }ˇ");
    // Auto indent is off.
    check_with(
        &no_auto_indent(),
        "if x {\n    ˇ",
        insert("}"),
        "if x {\n    }ˇ",
    );
    // Other characters are not affected.
    check("if x {\n    ˇ", insert("x"), "if x {\n    xˇ");
}

#[test]
fn newline_keeps_the_indentation() {
    check("abˇc", Command::Newline, "ab\nˇc");
    check("ˇ", Command::Newline, "\nˇ");
    check("    fooˇ", Command::Newline, "    foo\n    ˇ");
    check("\tfooˇ", Command::Newline, "\tfoo\n\tˇ");
    check("    foo ˇbar", Command::Newline, "    foo \n    ˇbar");
    check(
        "  a\n    bˇ\n  c",
        Command::Newline,
        "  a\n    b\n    ˇ\n  c",
    );
    check("a«bcˇ»d", Command::Newline, "a\nˇd");
    check("    «fooˇ»", Command::Newline, "    \n    ˇ");
}

#[test]
fn newline_indents_after_an_opener_or_colon() {
    check("if x {ˇ", Command::Newline, "if x {\n    ˇ");
    check("    if x {ˇ", Command::Newline, "    if x {\n        ˇ");
    check("def f():ˇ", Command::Newline, "def f():\n    ˇ");
    check("call(ˇ", Command::Newline, "call(\n    ˇ");
    check("x = [ˇ", Command::Newline, "x = [\n    ˇ");
    check("if x {  ˇ", Command::Newline, "if x {  \n    ˇ");
    check("foo(ˇa, b)", Command::Newline, "foo(\n    ˇa, b)");
    check("x = 1;ˇ", Command::Newline, "x = 1;\nˇ");
    let two = EditSettings {
        tab_size: 2,
        ..EditSettings::default()
    };
    check_with(&two, "a {ˇ", Command::Newline, "a {\n  ˇ");
    let colon_only = EditSettings {
        indent_after: ":".to_string(),
        ..EditSettings::default()
    };
    check_with(&colon_only, "a {ˇ", Command::Newline, "a {\nˇ");
    check_with(&colon_only, "a:ˇ", Command::Newline, "a:\n    ˇ");
}

#[test]
fn newline_between_a_bracket_pair_opens_a_block() {
    check("fn f() {ˇ}", Command::Newline, "fn f() {\n    ˇ\n}");
    check(
        "    if x {ˇ}",
        Command::Newline,
        "    if x {\n        ˇ\n    }",
    );
    check("foo(ˇ)", Command::Newline, "foo(\n    ˇ\n)");
    check("x = [ˇ]", Command::Newline, "x = [\n    ˇ\n]");
    check("{ ˇ }", Command::Newline, "{\n    ˇ\n}");
    check("{ˇ} // done", Command::Newline, "{\n    ˇ\n} // done");
    check_with(
        &hard_tabs(),
        "\tif x {ˇ}",
        Command::Newline,
        "\tif x {\n\t\tˇ\n\t}",
    );
    // Not a pair: an ordinary line break.
    check("{ˇ]", Command::Newline, "{\n    ˇ]");
    check("\"ˇ\"", Command::Newline, "\"\nˇ\"");
    // Typing a brace and pressing enter gives the block in one go.
    let mut e = editor("fn f() ˇ");
    apply_all(&mut e, [insert("{"), Command::Newline]);
    assert_eq!(state(&e), "fn f() {\n    ˇ\n}");
}

#[test]
fn newline_inside_the_indentation_and_before_blanks() {
    // Inside the indentation the line below keeps all of its indentation.
    check("    ˇ    foo", Command::Newline, "    \n    ˇ    foo");
    check("ˇ    foo", Command::Newline, "\nˇ    foo");
    check("\tˇ\tfoo", Command::Newline, "\t\n\tˇ\tfoo");
    check("    ˇ", Command::Newline, "    \n    ˇ");
    check("fooˇ   bar", Command::Newline, "foo\nˇbar");
    check("    fooˇ  \n", Command::Newline, "    foo\n    ˇ\n");
}

#[test]
fn newline_without_auto_indent() {
    let plain = no_auto_indent();
    check_with(&plain, "    fooˇ", Command::Newline, "    foo\nˇ");
    check_with(&plain, "{ˇ}", Command::Newline, "{\nˇ}");
    check_with(&plain, "a {ˇ", Command::Newline, "a {\nˇ");
    check_with(&plain, "fooˇ  bar", Command::Newline, "foo\nˇ  bar");
}

#[test]
fn newline_at_several_cursors() {
    check("aˇb\n  cˇd", Command::Newline, "a\nˇb\n  c\n  ˇd");
    check("{ˇ} {ˇ}", Command::Newline, "{\n    ˇ\n} {\n    ˇ\n}");
}

#[test]
fn newline_below_and_above() {
    check("    foo bˇar", Command::NewlineBelow, "    foo bar\n    ˇ");
    check("    foo bˇar", Command::NewlineAbove, "    ˇ\n    foo bar");
    check("abˇc", Command::NewlineBelow, "abc\nˇ");
    check("abˇc", Command::NewlineAbove, "ˇ\nabc");
    check("fn f() {ˇ\n}", Command::NewlineBelow, "fn f() {\n    ˇ\n}");
    check("fn f() {\n}ˇ", Command::NewlineAbove, "fn f() {\n    ˇ\n}");
    check("a\n«b\ncˇ»", Command::NewlineBelow, "a\nb\nc\nˇ");
    check("ˇ", Command::NewlineBelow, "\nˇ");
    check("ˇ", Command::NewlineAbove, "ˇ\n");
    // Cursors on one line open one line; cursors on different lines one each.
    check("aˇbˇc", Command::NewlineBelow, "abc\nˇ");
    check("ˇa\n  bˇ", Command::NewlineBelow, "a\nˇ\n  b\n  ˇ");
    check("ˇa\n  bˇ", Command::NewlineAbove, "ˇ\na\n  ˇ\n  b");
    let plain = no_auto_indent();
    check_with(
        &plain,
        "    foo bˇar",
        Command::NewlineBelow,
        "    foo bar\nˇ",
    );
    check_with(
        &plain,
        "    foo bˇar",
        Command::NewlineAbove,
        "ˇ\n    foo bar",
    );
}

#[test]
fn backspace_deletes_a_grapheme_or_the_selection() {
    check("abˇc", Command::Backspace, "aˇc");
    check("ˇabc", Command::Backspace, "ˇabc");
    check("a«bcˇ»d", Command::Backspace, "aˇd");
    check("a«ˇbc»d", Command::Backspace, "aˇd");
    check("ab\nˇcd", Command::Backspace, "abˇcd");
    check("ae\u{301}ˇb", Command::Backspace, "aˇb");
    check("a👨‍👩‍👧ˇb", Command::Backspace, "aˇb");
    check("🇨🇦🇹🇼ˇ", Command::Backspace, "🇨🇦ˇ");
    check("世界ˇ", Command::Backspace, "世ˇ");
    check("ˇ", Command::Backspace, "ˇ");
    assert!(editor("ˇabc").apply(Command::Backspace).is_noop());
}

#[test]
fn backspace_deletes_an_empty_pair_together() {
    check("(ˇ)", Command::Backspace, "ˇ");
    check("f([ˇ])", Command::Backspace, "f(ˇ)");
    check("\"ˇ\"", Command::Backspace, "ˇ");
    check("{ˇ}", Command::Backspace, "ˇ");
    check("(ˇ]", Command::Backspace, "ˇ]");
    check("(xˇ)", Command::Backspace, "(ˇ)");
    check_with(&no_autoclose(), "(ˇ)", Command::Backspace, "ˇ)");
    // Typing a bracket by mistake is undone by one backspace.
    let mut e = editor("fooˇ");
    apply_all(&mut e, [insert("("), Command::Backspace]);
    assert_eq!(state(&e), "fooˇ");
}

#[test]
fn backspace_in_the_indentation_deletes_a_step() {
    check("        ˇfoo", Command::Backspace, "    ˇfoo");
    check("      ˇfoo", Command::Backspace, "    ˇfoo");
    check("    ˇfoo", Command::Backspace, "ˇfoo");
    check("      ˇ  foo", Command::Backspace, "    ˇ  foo");
    check("  ˇfoo", Command::Backspace, "ˇfoo");
    check("a\n    ˇ", Command::Backspace, "a\nˇ");
    let two = EditSettings {
        tab_size: 2,
        ..EditSettings::default()
    };
    check_with(&two, "    ˇfoo", Command::Backspace, "  ˇfoo");
    // After a tab only the spaces back to the tab stop go.
    check("\t  ˇx", Command::Backspace, "\tˇx");
    check("\t\tˇfoo", Command::Backspace, "\tˇfoo");
    // Outside the indentation it is one character at a time.
    check("ab    ˇc", Command::Backspace, "ab   ˇc");
    // With hard tabs spaces are alignment and go one at a time.
    check_with(&hard_tabs(), "    ˇfoo", Command::Backspace, "   ˇfoo");
    check_with(&hard_tabs(), "\t\tˇfoo", Command::Backspace, "\tˇfoo");
}

#[test]
fn backspace_at_several_cursors() {
    check("aˇbcˇd", Command::Backspace, "ˇbˇd");
    check("abˇcˇd", Command::Backspace, "aˇd");
    check("ˇaˇb", Command::Backspace, "ˇb");
    check("a«bˇ»cdˇe", Command::Backspace, "aˇcˇe");
}

#[test]
fn delete_forward() {
    check("aˇbc", Command::Delete, "aˇc");
    check("abcˇ", Command::Delete, "abcˇ");
    check("a«bcˇ»d", Command::Delete, "aˇd");
    check("abˇ\ncd", Command::Delete, "abˇcd");
    check("ˇe\u{301}x", Command::Delete, "ˇx");
    check("ˇ👍🏽x", Command::Delete, "ˇx");
    check("(ˇ)", Command::Delete, "(ˇ");
    check("aˇbcˇd", Command::Delete, "aˇcˇ");
    assert!(editor("abcˇ").apply(Command::Delete).is_noop());
}

#[test]
fn delete_to_previous_word_start() {
    check("hello worldˇ", BACK_WORD, "hello ˇ");
    check("hello ˇ", BACK_WORD, "ˇ");
    check("hello woˇrld", BACK_WORD, "hello ˇrld");
    check("foo.barˇ", BACK_WORD, "foo.ˇ");
    check("foo.ˇ", BACK_WORD, "fooˇ");
    check("a«bcˇ»d", BACK_WORD, "aˇd");
    check("ˇabc", BACK_WORD, "ˇabc");
    // Two or more blanks go on their own first.
    check("foo   ˇ", BACK_WORD, "fooˇ");
    check("foo   barˇ", BACK_WORD, "foo   ˇ");
    // A deletion stops at a line break...
    check("foo\nˇbar", BACK_WORD, "fooˇbar");
    check("foo\n    ˇbar", BACK_WORD, "foo\nˇbar");
    // ...and at a bracket.
    check("foo(ˇ", BACK_WORD, "fooˇ");
    check("x = foo()ˇ", BACK_WORD, "x = foo(ˇ");
    check("foo(barˇ", BACK_WORD, "foo(ˇ");
}

#[test]
fn delete_to_previous_word_start_with_the_limits_lifted() {
    let across_lines = Command::DeleteToPreviousWordStart {
        ignore_newlines: true,
        ignore_brackets: false,
    };
    check("foo\nˇbar", across_lines.clone(), "ˇbar");
    check("foo\n\nˇbar", across_lines, "foo\nˇbar");
    let across_brackets = Command::DeleteToPreviousWordStart {
        ignore_newlines: false,
        ignore_brackets: true,
    };
    check("foo();ˇ", BACK_WORD, "foo()ˇ");
    check("foo();ˇ", across_brackets.clone(), "fooˇ");
    check("x = foo()ˇ", across_brackets, "x = fooˇ");
}

#[test]
fn delete_to_next_word_end() {
    check("ˇhello world", FORWARD_WORD, "ˇ world");
    check("helˇlo world", FORWARD_WORD, "helˇ world");
    check("helloˇ world", FORWARD_WORD, "helloˇ");
    check("ˇ   foo", FORWARD_WORD, "ˇfoo");
    check("fooˇ\nbar", FORWARD_WORD, "fooˇbar");
    check("ˇ(foo)", FORWARD_WORD, "ˇfoo)");
    check("ˇfoo(bar)", FORWARD_WORD, "ˇ(bar)");
    check("a«bcˇ»d", FORWARD_WORD, "aˇd");
    check("abcˇ", FORWARD_WORD, "abcˇ");
    let free = Command::DeleteToNextWordEnd {
        ignore_newlines: true,
        ignore_brackets: true,
    };
    check("fooˇ\nbar baz", free.clone(), "fooˇ baz");
    check("ˇ(foo)", free, "ˇ)");
}

#[test]
fn delete_by_subword() {
    let back = Command::DeleteToPreviousSubwordStart {
        ignore_newlines: false,
        ignore_brackets: false,
    };
    let forward = Command::DeleteToNextSubwordEnd {
        ignore_newlines: false,
        ignore_brackets: false,
    };
    check("fooBarBazˇ", back.clone(), "fooBarˇ");
    check("foo_bar_bazˇ", back.clone(), "foo_bar_ˇ");
    check("a\nˇfooBar", back, "aˇfooBar");
    check("ˇfooBarBaz", forward.clone(), "ˇBarBaz");
    check("ˇfoo_bar", forward.clone(), "ˇ_bar");
    check("fooˇ\nbar", forward, "fooˇbar");
}

#[test]
fn word_deletion_at_several_cursors_merges_overlaps() {
    check("foo bˇarˇ", BACK_WORD, "foo ˇ");
    check("oneˇ twoˇ", BACK_WORD, "ˇ ˇ");
    check("ˇone ˇtwo", FORWARD_WORD, "ˇ ˇ");
}

#[test]
fn delete_to_beginning_of_line() {
    let plain = Command::DeleteToBeginningOfLine {
        stop_at_indent: false,
    };
    let indent = Command::DeleteToBeginningOfLine {
        stop_at_indent: true,
    };
    check("foo bˇar", plain.clone(), "ˇar");
    check("    foo bˇar", plain.clone(), "ˇar");
    check("ab\nˇcd", plain.clone(), "abˇcd");
    check("ˇab", plain.clone(), "ˇab");
    check("ab«cdˇ»ef", plain.clone(), "ˇef");
    check("ab\ncd«ef\nghˇ»ij", plain.clone(), "ab\nˇij");
    check("aˇb\ncˇd", plain, "ˇb\nˇd");
    check("    foo bˇar", indent.clone(), "    ˇar");
    check("    ˇfoo", indent.clone(), "ˇfoo");
    check("ab\nˇ    cd", indent, "abˇ    cd");
}

#[test]
fn delete_to_end_of_line() {
    check("foˇo bar\nx", Command::DeleteToEndOfLine, "foˇ\nx");
    check("fooˇ\nbar", Command::DeleteToEndOfLine, "fooˇbar");
    check("fooˇ", Command::DeleteToEndOfLine, "fooˇ");
    check("a«bcˇ»def\nx", Command::DeleteToEndOfLine, "aˇ\nx");
    check("aˇb\ncˇd", Command::DeleteToEndOfLine, "aˇ\ncˇ");
    check("ˇ\n\nx", Command::DeleteToEndOfLine, "ˇ\nx");
}

#[test]
fn transpose() {
    check("abˇcd", Command::Transpose, "acbˇd");
    check("abcdˇ", Command::Transpose, "abdcˇ");
    check("abˇ\ncd", Command::Transpose, "baˇ\ncd");
    check("ˇabc", Command::Transpose, "ˇabc");
    check("aˇ", Command::Transpose, "aˇ");
    check("ˇ", Command::Transpose, "ˇ");
    check("a«bcˇ»d", Command::Transpose, "a«bcˇ»d");
    check("e\u{301}ˇx", Command::Transpose, "xe\u{301}ˇ");
    check("世ˇ界", Command::Transpose, "界世ˇ");
    check("aˇbcˇd", Command::Transpose, "baˇdcˇ");
    // Typing "teh" and transposing at the end fixes it.
    let mut e = editor("tehˇ");
    apply_all(&mut e, [Command::Transpose]);
    assert_eq!(state(&e), "tehˇ".replace("eh", "he"));
}
