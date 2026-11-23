//! Commands that work on whole lines.

use super::{apply_all, check, check_with, editor, state};
use crate::command::Command;
use crate::editor::EditSettings;

fn hard_tabs() -> EditSettings {
    EditSettings {
        hard_tabs: true,
        ..EditSettings::default()
    }
}

fn comment(prefix: &str) -> Command {
    Command::ToggleComments {
        prefix: prefix.to_string(),
        comment_empty_lines: false,
        advance_downwards: false,
    }
}

fn comment_all(prefix: &str) -> Command {
    Command::ToggleComments {
        prefix: prefix.to_string(),
        comment_empty_lines: true,
        advance_downwards: false,
    }
}

fn block(start: &str, end: &str) -> Command {
    Command::ToggleBlockComments {
        start: start.to_string(),
        end: end.to_string(),
    }
}

#[test]
fn delete_line() {
    check("ab\ncˇd\nef", Command::DeleteLine, "ab\neˇf");
    check("ab\ncˇd", Command::DeleteLine, "aˇb");
    check("abˇc", Command::DeleteLine, "ˇ");
    check("ˇ", Command::DeleteLine, "ˇ");
    check("abˇc\n", Command::DeleteLine, "ˇ");
    check("a\nbcdeˇf\nxy\nz", Command::DeleteLine, "a\nxyˇ\nz");
    check("a«b\ncˇ»d\nef", Command::DeleteLine, "eˇf");
    check("a«ˇb\nc»d\nef", Command::DeleteLine, "eˇf");
    // A selection that stops at the start of a line leaves that line alone.
    check("«ab\nˇ»cd", Command::DeleteLine, "ˇcd");
    check("ab\n«cd\nefˇ»", Command::DeleteLine, "abˇ");
}

#[test]
fn delete_line_at_several_cursors() {
    check("aˇb\ncd\neˇf", Command::DeleteLine, "cˇd");
    check("aˇb\ncˇd\nef", Command::DeleteLine, "eˇf");
    check("aˇb\ncd\neˇf\ngh", Command::DeleteLine, "cˇd\ngˇh");
    check("aˇbˇ\ncd", Command::DeleteLine, "cˇdˇ");
}

#[test]
fn tab_at_a_cursor_goes_to_the_next_stop() {
    check("ˇfoo", Command::Tab, "    ˇfoo");
    check("abˇc", Command::Tab, "ab  ˇc");
    check("abcdˇ", Command::Tab, "abcd    ˇ");
    check("世ˇ", Command::Tab, "世  ˇ");
    check_with(&hard_tabs(), "abˇc", Command::Tab, "ab\tˇc");
    let two = EditSettings {
        tab_size: 2,
        ..EditSettings::default()
    };
    check_with(&two, "aˇ", Command::Tab, "a ˇ");
    check_with(&two, "abˇ", Command::Tab, "ab  ˇ");
    // Cursors on one line each count what the ones before them added.
    check("ˇabˇcd", Command::Tab, "    ˇab  ˇcd");
    check("ˇa\nˇb", Command::Tab, "    ˇa\n    ˇb");
}

#[test]
fn tab_with_a_selection_indents_its_lines() {
    check("«ab\ncˇ»d", Command::Tab, "    «ab\n    cˇ»d");
    check("a«bcˇ»d", Command::Tab, "    a«bcˇ»d");
    check("«a\n\nbˇ»", Command::Tab, "    «a\n\n    bˇ»");
    check("  «abˇ»", Command::Tab, "    «abˇ»");
    check("«ab\nˇ»cd", Command::Tab, "    «ab\nˇ»cd");
    check_with(&hard_tabs(), "«ab\ncdˇ»", Command::Tab, "\t«ab\n\tcdˇ»");
    // A cursor elsewhere still gets its own tab.
    check("«abˇ»\ncˇd", Command::Tab, "    «abˇ»\nc   ˇd");
}

#[test]
fn indent_and_outdent() {
    check("fˇoo", Command::Indent, "    fˇoo");
    check("  fˇoo", Command::Indent, "    fˇoo");
    check("ˇ", Command::Indent, "    ˇ");
    check("«a\n\nbˇ»", Command::Indent, "    «a\n\n    bˇ»");
    check("aˇ\nbˇ", Command::Indent, "    aˇ\n    bˇ");
    check_with(&hard_tabs(), "\tfˇoo", Command::Indent, "\t\tfˇoo");

    check("    fˇoo", Command::Outdent, "fˇoo");
    check("      fˇoo", Command::Outdent, "    fˇoo");
    check("  fˇoo", Command::Outdent, "fˇoo");
    check("fˇoo", Command::Outdent, "fˇoo");
    check("\t\tfˇoo", Command::Outdent, "\tfˇoo");
    check("  ˇ  foo", Command::Outdent, "ˇfoo");
    check(
        "«    a\n        b\n    cˇ»",
        Command::Outdent,
        "«a\n    b\ncˇ»",
    );
    check("    fˇoo", Command::Backtab, "fˇoo");
    assert!(editor("fˇoo").apply(Command::Outdent).is_noop());
}

#[test]
fn move_line_up_and_down() {
    check("a\nbˇ\nc", Command::MoveLineUp, "bˇ\na\nc");
    check("a\nbˇ\nc", Command::MoveLineDown, "a\nc\nbˇ");
    check("aˇ\nb", Command::MoveLineUp, "aˇ\nb");
    check("a\nbˇ", Command::MoveLineDown, "a\nbˇ");
    check("a\nbˇ", Command::MoveLineUp, "bˇ\na");
    check("aˇ\nb", Command::MoveLineDown, "b\naˇ");
    check("one\ntˇwo\n", Command::MoveLineDown, "one\n\ntˇwo");
    check("x\n  inˇner\ny", Command::MoveLineUp, "  inˇner\nx\ny");
}

#[test]
fn move_lines_carries_the_selection() {
    check("a\n«b\ncˇ»\nd", Command::MoveLineDown, "a\nd\n«b\ncˇ»");
    check("a\n«b\ncˇ»\nd", Command::MoveLineUp, "«b\ncˇ»\na\nd");
    check("«a\nˇ»b\nc", Command::MoveLineDown, "b\n«a\nˇ»c");
    check("a\n«b\nˇ»c", Command::MoveLineUp, "«b\nˇ»a\nc");
    check("«a\nˇ»b", Command::MoveLineDown, "b\n«aˇ»");
    // Cursors on neighbouring lines move as one block; separate blocks move separately.
    check("a\nbˇ\ncˇ\nd", Command::MoveLineDown, "a\nd\nbˇ\ncˇ");
    check("aˇ\nb\ncˇ\nd", Command::MoveLineDown, "b\naˇ\nd\ncˇ");
    check("a\nbˇ\nc\ndˇ", Command::MoveLineUp, "bˇ\na\ndˇ\nc");
    // A block at the edge stays, the other still moves.
    check("aˇ\nb\ncˇ", Command::MoveLineUp, "aˇ\ncˇ\nb");
}

#[test]
fn moving_a_line_there_and_back() {
    let mut e = editor("one\ntˇwo\nthree");
    apply_all(
        &mut e,
        [
            Command::MoveLineDown,
            Command::MoveLineUp,
            Command::MoveLineUp,
            Command::MoveLineDown,
        ],
    );
    assert_eq!(state(&e), "one\ntˇwo\nthree");
}

#[test]
fn duplicate_lines() {
    check("a\nbˇc\nd", Command::DuplicateLineDown, "a\nbc\nbˇc\nd");
    check("a\nbˇc\nd", Command::DuplicateLineUp, "a\nbˇc\nbc\nd");
    check("a\nbˇc", Command::DuplicateLineDown, "a\nbc\nbˇc");
    check("a\nbˇc", Command::DuplicateLineUp, "a\nbˇc\nbc");
    check("ˇ", Command::DuplicateLineDown, "\nˇ");
    check("ˇ", Command::DuplicateLineUp, "ˇ\n");
    check("«a\nbˇ»\nc", Command::DuplicateLineDown, "a\nb\n«a\nbˇ»\nc");
    check("«a\nbˇ»\nc", Command::DuplicateLineUp, "«a\nbˇ»\na\nb\nc");
    // A whole-line selection stays a whole-line selection.
    check("«ab\nˇ»cd", Command::DuplicateLineUp, "«ab\nˇ»ab\ncd");
    check("«ab\nˇ»cd", Command::DuplicateLineDown, "ab\n«ab\nˇ»cd");
    // Two cursors on one line copy it once; cursors on different lines copy each.
    check("aˇbˇ\nc", Command::DuplicateLineDown, "ab\naˇbˇ\nc");
    check("aˇ\nbˇ", Command::DuplicateLineDown, "a\naˇ\nb\nbˇ");
    check("aˇ\nbˇ", Command::DuplicateLineUp, "aˇ\na\nbˇ\nb");
}

#[test]
fn join_lines() {
    check("abˇ\n    cd", Command::JoinLines, "abˇ cd");
    check("aˇb\ncd", Command::JoinLines, "abˇ cd");
    check("abˇ", Command::JoinLines, "abˇ");
    check("ab\ncˇd", Command::JoinLines, "ab\ncˇd");
    // No space when one side is empty or the first already ends in a blank.
    check("abˇ\n\ncd", Command::JoinLines, "abˇ\ncd");
    check("ab ˇ\ncd", Command::JoinLines, "ab ˇcd");
    check("ˇ\ncd", Command::JoinLines, "ˇcd");
    check("abˇ\n   \ncd", Command::JoinLines, "abˇ\ncd");
    // A selection joins everything it touches.
    check("«a\nb\ncˇ»\nd", Command::JoinLines, "«a b cˇ»\nd");
    check("«a\n   \ncˇ»", Command::JoinLines, "«a cˇ»");
    check("x«a\n    bˇ»y", Command::JoinLines, "x«a bˇ»y");
    // Cursors on consecutive lines join them all.
    check("aˇ\nbˇ\nc", Command::JoinLines, "aˇ bˇ c");
    check("aˇ\nb\ncˇ\nd", Command::JoinLines, "aˇ b\ncˇ d");
}

#[test]
fn toggle_comments_on_one_line() {
    check("fooˇ", comment("//"), "// fooˇ");
    check("// fooˇ", comment("//"), "fooˇ");
    check("//fooˇ", comment("//"), "fooˇ");
    check("foˇo", comment("//"), "// foˇo");
    check("    fooˇ", comment("//"), "    // fooˇ");
    check("    // fooˇ", comment("//"), "    fooˇ");
    check("xˇ = 1", comment("#"), "# xˇ = 1");
    check("fooˇ", comment("// "), "// fooˇ");
    check("-- selectˇ", comment("--"), "selectˇ");
    // A blank line gets a marker to type after.
    check("ˇ", comment("//"), "// ˇ");
    check("    ˇ", comment("//"), "    // ˇ");
    // Without a prefix nothing happens.
    assert!(editor("fooˇ").apply(comment("")).is_noop());
    assert!(editor("fooˇ").apply(comment("  ")).is_noop());
}

#[test]
fn toggle_comments_on_several_lines() {
    check(
        "«    a\n        b\n    cˇ»",
        comment("//"),
        "«    // a\n    //     b\n    // cˇ»",
    );
    check(
        "«    // a\n    //     b\n    // cˇ»",
        comment("//"),
        "«    a\n        b\n    cˇ»",
    );
    // Blank lines are skipped unless asked for.
    check("x«a\n\nbˇ»", comment("//"), "// x«a\n\n// bˇ»");
    check("x«a\n\nbˇ»", comment_all("//"), "// x«a\n//\n// bˇ»");
    // A selection that starts where the marker goes starts after it, as in Zed.
    check("«a\n\nbˇ»", comment("//"), "// «a\n\n// bˇ»");
    check(
        "«    a\n\n    bˇ»",
        comment_all("//"),
        "«    // a\n//\n    // bˇ»",
    );
    check("«// a\n//\n// bˇ»", comment_all("//"), "«a\n\nbˇ»");
    // Uncommenting needs every non-blank line to be commented.
    check("«// a\nbˇ»", comment("//"), "// «// a\n// bˇ»");
    check("«// a\n\n// bˇ»", comment("//"), "«a\n\nbˇ»");
    // A selection that stops at the start of a line leaves that line alone.
    check("«a\nˇ»b", comment("//"), "// «a\nˇ»b");
}

#[test]
fn toggle_comments_at_several_cursors() {
    check("// aˇ\nbˇ", comment("//"), "aˇ\n// bˇ");
    check("aˇbˇ", comment("//"), "// aˇbˇ");
    check("aˇ\nb\ncˇ", comment("#"), "# aˇ\nb\n# cˇ");
}

#[test]
fn toggle_comments_and_advance() {
    let advance = Command::ToggleComments {
        prefix: "//".to_string(),
        comment_empty_lines: false,
        advance_downwards: true,
    };
    let mut e = editor("ˇone\ntwo\nthree");
    apply_all(&mut e, [advance.clone(), advance.clone()]);
    assert_eq!(state(&e), "// one\n// two\nthreeˇ");
    apply_all(&mut e, [advance.clone()]);
    assert_eq!(
        state(&e),
        "// one\n// two\n// threeˇ",
        "on the last line the cursor stays"
    );
    // A selection is not moved.
    check("«oneˇ»\ntwo", advance, "// «oneˇ»\ntwo");
}

#[test]
fn toggle_block_comments_around_a_selection() {
    check("a «bcˇ» d", block("/*", "*/"), "a /* «bcˇ» */ d");
    check("a /* «bcˇ» */ d", block("/*", "*/"), "a «bcˇ» d");
    check("a «/* bc */ˇ» d", block("/*", "*/"), "a «bcˇ» d");
    check("a «/*bc*/ˇ» d", block("/*", "*/"), "a «bcˇ» d");
    check("a /*«bcˇ»*/ d", block("/*", "*/"), "a «bcˇ» d");
    check("«one\ntwoˇ»", block("/*", "*/"), "/* «one\ntwoˇ» */");
    check(
        "<p>«textˇ»</p>",
        block("<!--", "-->"),
        "<p><!-- «textˇ» --></p>",
    );
    check(
        "<p><!-- «textˇ» --></p>",
        block("<!--", "-->"),
        "<p>«textˇ»</p>",
    );
    check("«aˇ» «bˇ»", block("/*", "*/"), "/* «aˇ» */ /* «bˇ» */");
    check("a «ˇbc» d", block(" /* ", " */ "), "a /* «ˇbc» */ d");
}

#[test]
fn toggle_block_comments_at_a_cursor() {
    check("  fooˇ bar  ", block("/*", "*/"), "  /* fooˇ bar */  ");
    check("  /* fooˇ bar */", block("/*", "*/"), "  fooˇ bar");
    check("ˇ", block("/*", "*/"), "/* ˇ */");
    check("    ˇ", block("/*", "*/"), "    /* ˇ */");
    check("a\nbˇc\nd", block("/*", "*/"), "a\n/* bˇc */\nd");
    check("aˇbˇc", block("/*", "*/"), "/* aˇbˇc */");
    assert!(editor("fooˇ").apply(block("", "*/")).is_noop());
    assert!(editor("fooˇ").apply(block("/*", "")).is_noop());
}

#[test]
fn line_commands_are_single_undo_steps() {
    let commands = [
        Command::DeleteLine,
        Command::Tab,
        Command::Indent,
        Command::MoveLineDown,
        Command::DuplicateLineUp,
        Command::JoinLines,
        comment("//"),
        block("/*", "*/"),
    ];
    for command in commands {
        let mut e = editor("  one\n«  two\n  thrˇ»ee\n  four");
        let before = state(&e);
        let outcome = e.apply(command.clone());
        assert!(outcome.text_changed(), "{command:?} changes the text");
        e.apply(Command::Undo);
        assert_eq!(
            state(&e),
            before,
            "undoing {command:?} restores text and selection"
        );
        assert!(!e.buffer().can_undo());
    }
}
