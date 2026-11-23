//! Moving and extending selections.

use super::{apply_all, check, check_with, editor, editor_with, state};
use crate::command::{Command, Motion};
use crate::editor::{DisplayPosition, EditSettings};

fn mv(motion: Motion) -> Command {
    Command::Move(motion)
}

fn sel(motion: Motion) -> Command {
    Command::Select(motion)
}

fn wrapped(width: usize) -> EditSettings {
    EditSettings {
        wrap_width: Some(width),
        ..EditSettings::default()
    }
}

#[test]
fn left_and_right_step_by_grapheme() {
    check("abˇc", mv(Motion::Left), "aˇbc");
    check("abˇc", mv(Motion::Right), "abcˇ");
    check("ˇabc", mv(Motion::Left), "ˇabc");
    check("abcˇ", mv(Motion::Right), "abcˇ");
    check("ab\nˇcd", mv(Motion::Left), "abˇ\ncd");
    check("abˇ\ncd", mv(Motion::Right), "ab\nˇcd");
    check("e\u{301}ˇx", mv(Motion::Left), "ˇe\u{301}x");
    check("ˇe\u{301}x", mv(Motion::Right), "e\u{301}ˇx");
    check("ˇ👨‍👩‍👧x", mv(Motion::Right), "👨‍👩‍👧ˇx");
    check("🇨🇦🇹🇼ˇ", mv(Motion::Left), "🇨🇦ˇ🇹🇼");
    check("ˇ", mv(Motion::Left), "ˇ");
    check("ˇ", mv(Motion::Right), "ˇ");
}

#[test]
fn left_and_right_leave_a_selection_at_its_edge() {
    check("a«bcˇ»d", mv(Motion::Left), "aˇbcd");
    check("a«bcˇ»d", mv(Motion::Right), "abcˇd");
    check("a«ˇbc»d", mv(Motion::Left), "aˇbcd");
    check("a«ˇbc»d", mv(Motion::Right), "abcˇd");
}

#[test]
fn several_cursors_move_together_and_merge() {
    check("aˇbcˇd", mv(Motion::Left), "ˇabˇcd");
    check("aˇbcˇd", mv(Motion::Right), "abˇcdˇ");
    check("ˇaˇbc", mv(Motion::Left), "ˇabc");
    check("abˇcˇ", mv(Motion::Right), "abcˇ");
    check("ˇab\ncˇd\nef", mv(Motion::End), "ab\ncd\nefˇ");
}

#[test]
fn selecting_left_and_right() {
    check("abˇc", sel(Motion::Left), "a«ˇb»c");
    check("abˇc", sel(Motion::Right), "ab«cˇ»");
    check("a«bcˇ»d", sel(Motion::Left), "a«bˇ»cd");
    check("a«ˇbc»d", sel(Motion::Right), "ab«ˇc»d");
    let mut e = editor("a«bˇ»c");
    apply_all(&mut e, [sel(Motion::Left)]);
    assert_eq!(state(&e), "aˇbc");
    apply_all(&mut e, [sel(Motion::Left)]);
    assert_eq!(state(&e), "«ˇa»bc");
    check("«abˇ»c«ˇde»", sel(Motion::Right), "«abcˇ»d«ˇe»");
    check("«abˇ»cd«ˇef»", sel(Motion::Left), "«aˇ»bc«ˇdef»");
    check("«abˇ»c«ˇde»", mv(Motion::Left), "ˇabcˇde");
    // Selections that grow into each other merge.
    check("aˇbcˇd", sel(Motion::End), "a«bcdˇ»");
    check("aˇbcˇd", sel(Motion::Beginning), "«ˇabc»d");
}

#[test]
fn up_and_down_keep_the_column() {
    check("abc\ndeˇf\nghi", mv(Motion::Up), "abˇc\ndef\nghi");
    check("abc\ndeˇf\nghi", mv(Motion::Down), "abc\ndef\nghˇi");
    check("abc\ndef\nghiˇ", mv(Motion::Up), "abc\ndefˇ\nghi");
    check("ˇ\nabc", mv(Motion::Down), "\nˇabc");
}

#[test]
fn the_goal_column_survives_short_lines() {
    let mut e = editor("abcdeˇf\nxy\n\nabcdefgh");
    apply_all(&mut e, [mv(Motion::Down)]);
    assert_eq!(state(&e), "abcdef\nxyˇ\n\nabcdefgh");
    apply_all(&mut e, [mv(Motion::Down)]);
    assert_eq!(state(&e), "abcdef\nxy\nˇ\nabcdefgh");
    apply_all(&mut e, [mv(Motion::Down)]);
    assert_eq!(state(&e), "abcdef\nxy\n\nabcdeˇfgh");
    apply_all(&mut e, [mv(Motion::Up), mv(Motion::Up), mv(Motion::Up)]);
    assert_eq!(state(&e), "abcdeˇf\nxy\n\nabcdefgh");

    // A sideways move forgets the goal.
    let mut e = editor("abcdeˇf\nxy\nabcdefgh");
    apply_all(
        &mut e,
        [mv(Motion::Down), mv(Motion::Left), mv(Motion::Down)],
    );
    assert_eq!(state(&e), "abcdef\nxy\naˇbcdefgh");
}

#[test]
fn up_on_the_first_line_and_down_on_the_last_go_to_the_ends() {
    check("abˇc\ndef", mv(Motion::Up), "ˇabc\ndef");
    check("abc\ndˇef", mv(Motion::Down), "abc\ndefˇ");
    check("ˇ", mv(Motion::Up), "ˇ");
    check("ˇ", mv(Motion::Down), "ˇ");
    // The goal column is still remembered, as in Zed.
    let mut e = editor("abˇc\ndef");
    apply_all(&mut e, [mv(Motion::Up), mv(Motion::Down)]);
    assert_eq!(state(&e), "abc\ndeˇf");
    check("abˇc\ndef", sel(Motion::Up), "«ˇab»c\ndef");
    check("abc\ndˇef", sel(Motion::Down), "abc\nd«efˇ»");
}

#[test]
fn up_and_down_start_from_the_edges_of_a_selection() {
    check("a«bc\ndˇ»ef\nghi", mv(Motion::Up), "ˇabc\ndef\nghi");
    check("a«bc\ndˇ»ef\nghi", mv(Motion::Down), "abc\ndef\ngˇhi");
    check("abc\nd«ˇef\ng»hi", mv(Motion::Up), "aˇbc\ndef\nghi");
    check("abc\ndeˇf", sel(Motion::Up), "ab«ˇc\nde»f");
    check("abˇc\ndef", sel(Motion::Down), "ab«c\ndeˇ»f");
}

#[test]
fn vertical_motion_uses_display_columns() {
    // The tab is 4 wide, so the cursor after "ab" is at column 6.
    check("\tabˇc\n世界x", mv(Motion::Down), "\tabc\n世界xˇ");
    check("abˇc\n世界", mv(Motion::Down), "abc\n世ˇ界");
    check("abcˇ\n世界", mv(Motion::Down), "abc\n世ˇ界");
    check("abcd\n世界ˇ", mv(Motion::Up), "abcdˇ\n世界");
    check("abcdef\n\tˇx", mv(Motion::Up), "abcdˇef\n\tx");
    check("aˇbcdef\n\tx", mv(Motion::Down), "abcdef\nˇ\tx");
    check(
        "e\u{301}e\u{301}ˇz\nabc",
        mv(Motion::Down),
        "e\u{301}e\u{301}z\nabˇc",
    );
}

#[test]
fn moving_by_several_lines() {
    let text = "l0\nl1\nl2\nl3\nl4\nl5\nl6\nlˇ7\nl8\nl9";
    check(
        text,
        mv(Motion::UpByLines { lines: 5 }),
        "l0\nl1\nlˇ2\nl3\nl4\nl5\nl6\nl7\nl8\nl9",
    );
    check(
        text,
        mv(Motion::DownByLines { lines: 2 }),
        "l0\nl1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nlˇ9",
    );
    // Running out of lines stops on the first or last one.
    check(
        text,
        mv(Motion::UpByLines { lines: 50 }),
        "lˇ0\nl1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9",
    );
    check(
        text,
        mv(Motion::DownByLines { lines: 50 }),
        "l0\nl1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nlˇ9",
    );
    // From the edge line it goes to the very start or end.
    check("lˇ0\nl1", mv(Motion::UpByLines { lines: 5 }), "ˇl0\nl1");
    check("l0\nlˇ1", mv(Motion::DownByLines { lines: 5 }), "l0\nl1ˇ");
    // Zero lines is no movement at all.
    check(text, mv(Motion::UpByLines { lines: 0 }), text);
    check(text, mv(Motion::DownByLines { lines: 0 }), text);
    check(
        text,
        sel(Motion::UpByLines { lines: 2 }),
        "l0\nl1\nl2\nl3\nl4\nl«ˇ5\nl6\nl»7\nl8\nl9",
    );
}

#[test]
fn paging() {
    let text = "l0\nl1\nl2\nl3\nl4\nl5\nlˇ6\nl7\nl8\nl9";
    check(
        text,
        mv(Motion::PageUp { rows: 4 }),
        "l0\nl1\nlˇ2\nl3\nl4\nl5\nl6\nl7\nl8\nl9",
    );
    check(
        text,
        mv(Motion::PageDown { rows: 3 }),
        "l0\nl1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nlˇ9",
    );
    check(
        text,
        sel(Motion::PageUp { rows: 2 }),
        "l0\nl1\nl2\nl3\nl«ˇ4\nl5\nl»6\nl7\nl8\nl9",
    );
    // Without a row count the page size comes from the settings.
    let settings = EditSettings {
        page_rows: 5,
        ..EditSettings::default()
    };
    check_with(
        &settings,
        text,
        mv(Motion::PageUp { rows: 0 }),
        "l0\nlˇ1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9",
    );
    check_with(
        &settings,
        text,
        mv(Motion::PageDown { rows: 0 }),
        "l0\nl1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nlˇ9",
    );
    let none = EditSettings {
        page_rows: 0,
        ..EditSettings::default()
    };
    check_with(
        &none,
        text,
        mv(Motion::PageUp { rows: 0 }),
        "l0\nl1\nl2\nl3\nl4\nlˇ5\nl6\nl7\nl8\nl9",
    );
}

#[test]
fn vertical_motion_follows_soft_wrap_rows() {
    // "the quick brown fox" wraps at 10 into "the quick " and "brown fox".
    let wrap = wrapped(10);
    check_with(
        &wrap,
        "the quˇick brown fox\nnext",
        mv(Motion::Down),
        "the quick brown ˇfox\nnext",
    );
    check_with(
        &wrap,
        "the quick brown ˇfox\nnext",
        mv(Motion::Up),
        "the quˇick brown fox\nnext",
    );
    check_with(
        &wrap,
        "the quick brown ˇfox\nnext",
        mv(Motion::Down),
        "the quick brown fox\nnextˇ",
    );
    check_with(
        &wrap,
        "the quick brown fox\nneˇxt",
        mv(Motion::Up),
        "the quick brˇown fox\nnext",
    );
    check_with(
        &wrap,
        "the quick brown fox\nneˇxt",
        mv(Motion::UpByLines { lines: 2 }),
        "thˇe quick brown fox\nnext",
    );
    check_with(
        &wrap,
        "thˇe quick brown fox\nnext",
        mv(Motion::DownByLines { lines: 2 }),
        "the quick brown fox\nneˇxt",
    );
    // On the first row of a wrapped line, up still goes to the start of the text.
    check_with(
        &wrap,
        "the quˇick brown fox",
        mv(Motion::Up),
        "ˇthe quick brown fox",
    );
    check_with(
        &wrap,
        "the quick brown ˇfox",
        mv(Motion::Down),
        "the quick brown foxˇ",
    );
    // The same text without wrapping moves by whole lines.
    check(
        "the quˇick brown fox\nnext",
        mv(Motion::Down),
        "the quick brown fox\nnextˇ",
    );

    let mut e = editor_with("the quick brown ˇfox\nab\nthe quick brown fox", wrap);
    apply_all(&mut e, [mv(Motion::Down), mv(Motion::Down)]);
    assert_eq!(
        state(&e),
        "the quick brown fox\nab\nthe quˇick brown fox",
        "the goal column is kept across rows"
    );
}

#[test]
fn beginning_of_line() {
    let plain = Motion::BeginningOfLine {
        stop_at_soft_wraps: false,
        stop_at_indent: false,
    };
    let indent = Motion::BeginningOfLine {
        stop_at_soft_wraps: false,
        stop_at_indent: true,
    };
    check("    foo bˇar", mv(plain.clone()), "ˇ    foo bar");
    check("ab\ncdˇe", mv(plain.clone()), "ab\nˇcde");
    check("ˇ", mv(plain.clone()), "ˇ");
    check("    foo bˇar", sel(plain), "«ˇ    foo b»ar");

    // With stop_at_indent the key toggles between the first non-blank and column 0.
    let mut e = editor("    foo bˇar");
    apply_all(&mut e, [mv(indent.clone())]);
    assert_eq!(state(&e), "    ˇfoo bar");
    apply_all(&mut e, [mv(indent.clone())]);
    assert_eq!(state(&e), "ˇ    foo bar");
    apply_all(&mut e, [mv(indent.clone())]);
    assert_eq!(state(&e), "    ˇfoo bar");
    check("  ˇ  foo", mv(indent.clone()), "ˇ    foo");
    check("\t\tfoˇo", mv(indent.clone()), "\t\tˇfoo");
    check("foˇo", mv(indent.clone()), "ˇfoo");
    check("    foo bˇar", sel(indent), "    «ˇfoo b»ar");
}

#[test]
fn end_of_line() {
    let end = Motion::EndOfLine {
        stop_at_soft_wraps: false,
    };
    check("foˇo bar\nbaz", mv(end.clone()), "foo barˇ\nbaz");
    check("foo\nbˇaz", mv(end.clone()), "foo\nbazˇ");
    check("fooˇ\nbaz", mv(end.clone()), "fooˇ\nbaz");
    check("ˇ\nx", mv(end.clone()), "ˇ\nx");
    check("foˇo bar\nbaz", sel(end), "fo«o barˇ»\nbaz");
}

#[test]
fn line_edges_with_soft_wrap() {
    let wrap = wrapped(10);
    let start = Motion::BeginningOfLine {
        stop_at_soft_wraps: true,
        stop_at_indent: false,
    };
    let end = Motion::EndOfLine {
        stop_at_soft_wraps: true,
    };

    // On the continuation row "brown fox": first the row start, then the line start.
    let mut e = editor_with("the quick brown fˇox", wrap.clone());
    apply_all(&mut e, [mv(start.clone())]);
    assert_eq!(state(&e), "the quick ˇbrown fox");
    apply_all(&mut e, [mv(start.clone())]);
    assert_eq!(state(&e), "ˇthe quick brown fox");

    // On the first row: the last position drawn on the row, then the real line end.
    let mut e = editor_with("thˇe quick brown fox", wrap.clone());
    apply_all(&mut e, [mv(end.clone())]);
    assert_eq!(state(&e), "the quickˇ brown fox");
    apply_all(&mut e, [mv(end.clone())]);
    assert_eq!(state(&e), "the quick brown foxˇ");
    check_with(
        &wrap,
        "the quick brˇown fox",
        mv(end),
        "the quick brown foxˇ",
    );

    // Without the flag the wrap rows are ignored.
    let hard_start = Motion::BeginningOfLine {
        stop_at_soft_wraps: false,
        stop_at_indent: false,
    };
    let hard_end = Motion::EndOfLine {
        stop_at_soft_wraps: false,
    };
    check_with(
        &wrap,
        "the quick brown fˇox",
        mv(hard_start),
        "ˇthe quick brown fox",
    );
    check_with(
        &wrap,
        "thˇe quick brown fox",
        mv(hard_end),
        "the quick brown foxˇ",
    );
    // And with wrapping off the flag changes nothing.
    check("the quick brown fˇox", mv(start), "ˇthe quick brown fox");
}

#[test]
fn first_non_blank_and_document_edges() {
    check("    foo baˇr", mv(Motion::FirstNonBlank), "    ˇfoo bar");
    check("ˇ    foo", mv(Motion::FirstNonBlank), "    ˇfoo");
    check(" ˇ  ", mv(Motion::FirstNonBlank), "   ˇ");
    check("a\n\tˇb", mv(Motion::FirstNonBlank), "a\n\tˇb");
    check("ab\ncˇd\nef", mv(Motion::Beginning), "ˇab\ncd\nef");
    check("ab\ncˇd\nef", mv(Motion::End), "ab\ncd\nefˇ");
    check("ab\ncˇd\nef", sel(Motion::Beginning), "«ˇab\nc»d\nef");
    check("ab\ncˇd\nef", sel(Motion::End), "ab\nc«d\nefˇ»");
}

#[test]
fn paragraphs() {
    // Lines: a, b, (blank), c, d, (blank), (blank), e.
    check(
        "a\nb\n\nc\nˇd\n\n\ne",
        mv(Motion::StartOfParagraph),
        "a\nb\nˇ\nc\nd\n\n\ne",
    );
    check(
        "a\nb\n\nc\nˇd\n\n\ne",
        mv(Motion::EndOfParagraph),
        "a\nb\n\nc\nd\nˇ\n\ne",
    );
    check(
        "a\nb\nˇ\nc\nd\n\n\ne",
        mv(Motion::StartOfParagraph),
        "ˇa\nb\n\nc\nd\n\n\ne",
    );
    check(
        "a\nb\nˇ\nc\nd\n\n\ne",
        mv(Motion::EndOfParagraph),
        "a\nb\n\nc\nd\nˇ\n\ne",
    );
    check("ˇa\nb\n\nc", mv(Motion::StartOfParagraph), "ˇa\nb\n\nc");
    check("a\nb\n\ncˇ", mv(Motion::EndOfParagraph), "a\nb\n\ncˇ");
    check(
        "a\nb\n\nc\nd\nˇ\n\ne",
        mv(Motion::EndOfParagraph),
        "a\nb\n\nc\nd\n\n\neˇ",
    );
    check("a\n  \t\nbˇ", mv(Motion::StartOfParagraph), "a\nˇ  \t\nb");
    check("a\nˇb\n\nc", sel(Motion::EndOfParagraph), "a\n«b\nˇ»\nc");
}

#[test]
fn word_motions() {
    check(
        "hello woˇrld",
        mv(Motion::PreviousWordStart),
        "hello ˇworld",
    );
    check(
        "hello ˇworld",
        mv(Motion::PreviousWordStart),
        "ˇhello world",
    );
    check("helˇlo world", mv(Motion::NextWordEnd), "helloˇ world");
    check("helloˇ world", mv(Motion::NextWordEnd), "hello worldˇ");
    check("ˇfooBar_baz", mv(Motion::NextSubwordEnd), "fooˇBar_baz");
    check(
        "fooBar_bazˇ",
        mv(Motion::PreviousSubwordStart),
        "fooBar_ˇbaz",
    );
    check("foˇo bar.baz", mv(Motion::NextWordStart), "foo ˇbar.baz");
    check("foo ˇbar.baz", mv(Motion::NextWordStart), "foo barˇ.baz");
    check(
        "hello woˇrld",
        sel(Motion::PreviousWordStart),
        "hello «ˇwo»rld",
    );
    check("helˇlo world", sel(Motion::NextWordEnd), "hel«loˇ» world");
    check("ˇa bˇc", mv(Motion::NextWordEnd), "aˇ bcˇ");

    // Extra word characters come from the settings.
    let css = EditSettings {
        word_chars: "-".to_string(),
        ..EditSettings::default()
    };
    check_with(
        &css,
        "ˇfont-size: 1",
        mv(Motion::NextWordEnd),
        "font-sizeˇ: 1",
    );
    check("ˇfont-size: 1", mv(Motion::NextWordEnd), "fontˇ-size: 1");
}

#[test]
fn enclosing_bracket() {
    check("fooˇ(bar)", mv(Motion::EnclosingBracket), "foo(bar)ˇ");
    check("foo(bar)ˇ", mv(Motion::EnclosingBracket), "fooˇ(bar)");
    check("foo(ˇbar)", mv(Motion::EnclosingBracket), "foo(barˇ)");
    check("foo(barˇ)", mv(Motion::EnclosingBracket), "foo(ˇbar)");
    check("foo(bˇar)", mv(Motion::EnclosingBracket), "foo(barˇ)");
    check("a[b(cˇ)d]", mv(Motion::EnclosingBracket), "a[b(ˇc)d]");
    check("a[bˇ(c)d]", mv(Motion::EnclosingBracket), "a[b(c)ˇd]");
    check("a[ˇb(c)d]", mv(Motion::EnclosingBracket), "a[b(c)dˇ]");
    check("{\n  xˇ\n}", mv(Motion::EnclosingBracket), "{\n  x\nˇ}");
    check("abˇc", mv(Motion::EnclosingBracket), "abˇc");
    check("(ˇ", mv(Motion::EnclosingBracket), "(ˇ");
    check("foo(ˇbar)", sel(Motion::EnclosingBracket), "foo(«barˇ»)");
}

#[test]
fn go_to_line() {
    let text = "one\ntwo\nthˇree";
    check(
        text,
        mv(Motion::GoToLine { line: 2, column: 2 }),
        "one\ntˇwo\nthree",
    );
    check(
        text,
        mv(Motion::GoToLine { line: 1, column: 1 }),
        "ˇone\ntwo\nthree",
    );
    check(
        text,
        mv(Motion::GoToLine { line: 0, column: 0 }),
        "ˇone\ntwo\nthree",
    );
    check(
        text,
        mv(Motion::GoToLine {
            line: 2,
            column: 99,
        }),
        "one\ntwoˇ\nthree",
    );
    check(
        text,
        mv(Motion::GoToLine {
            line: 99,
            column: 3,
        }),
        "one\ntwo\nthˇree",
    );
    check(
        text,
        mv(Motion::GoToLine {
            line: 99,
            column: 99,
        }),
        "one\ntwo\nthreeˇ",
    );
    // A jump leaves a single cursor.
    check(
        "ˇone\ntˇwo\nthˇree",
        mv(Motion::GoToLine { line: 2, column: 1 }),
        "one\nˇtwo\nthree",
    );
    // Extending keeps the anchor.
    check(
        text,
        sel(Motion::GoToLine { line: 1, column: 2 }),
        "o«ˇne\ntwo\nth»ree",
    );
    // A column inside a grapheme cluster lands before the cluster.
    check(
        "ˇe\u{301}x",
        mv(Motion::GoToLine { line: 1, column: 2 }),
        "ˇe\u{301}x",
    );
}

#[test]
fn motions_report_whether_anything_moved() {
    let mut e = editor("ˇabc");
    let outcome = e.apply(mv(Motion::Left));
    assert!(outcome.is_noop());
    let outcome = e.apply(mv(Motion::Right));
    assert!(outcome.selections_changed && !outcome.text_changed() && !outcome.is_noop());
    assert!(outcome.clipboard.is_none());
}

#[test]
fn display_positions_round_trip() {
    let e = editor_with("ab\tc\nthe quick brown fox\n世界", wrapped(10));
    assert_eq!(
        e.display_position(0),
        DisplayPosition {
            line: 0,
            row: 0,
            col: 0
        }
    );
    assert_eq!(
        e.display_position(3),
        DisplayPosition {
            line: 0,
            row: 0,
            col: 4
        }
    );
    assert_eq!(
        e.display_position(5),
        DisplayPosition {
            line: 1,
            row: 0,
            col: 0
        }
    );
    assert_eq!(
        e.display_position(15),
        DisplayPosition {
            line: 1,
            row: 1,
            col: 0
        },
        "a row end is the next row's start"
    );
    assert_eq!(
        e.display_position(21),
        DisplayPosition {
            line: 1,
            row: 1,
            col: 6
        }
    );
    assert_eq!(
        e.display_position(26),
        DisplayPosition {
            line: 2,
            row: 0,
            col: 2
        }
    );
    assert_eq!(
        e.display_position(999),
        DisplayPosition {
            line: 2,
            row: 0,
            col: 4
        }
    );

    assert_eq!(
        e.position_at(DisplayPosition {
            line: 0,
            row: 0,
            col: 3
        }),
        2,
        "inside the tab"
    );
    assert_eq!(
        e.position_at(DisplayPosition {
            line: 1,
            row: 0,
            col: 99
        }),
        14,
        "stays on the wrapped row"
    );
    assert_eq!(
        e.position_at(DisplayPosition {
            line: 1,
            row: 1,
            col: 99
        }),
        24
    );
    assert_eq!(
        e.position_at(DisplayPosition {
            line: 1,
            row: 9,
            col: 2
        }),
        17,
        "rows past the end clamp"
    );
    assert_eq!(
        e.position_at(DisplayPosition {
            line: 2,
            row: 0,
            col: 3
        }),
        26,
        "second half of a wide char"
    );
    assert_eq!(
        e.position_at(DisplayPosition {
            line: 99,
            row: 0,
            col: 0
        }),
        25
    );
    for pos in 0..=e.buffer().len_chars() {
        assert_eq!(e.position_at(e.display_position(pos)), pos);
    }

    assert_eq!(e.line_rows(1), vec![0..10, 10..19]);
    assert_eq!(e.line_rows(0), vec![0..4]);
    assert_eq!(e.line_rows(99), vec![0..0]);
    assert_eq!(editor("the quick brown fox").line_rows(0), vec![0..19]);
}
