//! What `has_pending` and `flush` do with unfinished input, and the size limits.

use bakedit_core::{ColorSlot, InputEvent, Key, Mods, MouseButton, MouseKind, Rgb, TerminalReply};

use super::super::{MAX_CSI_LEN, MAX_STRING_LEN};
use super::{alt_typed, key, mouse, parse, parse_and_flush, reply, typed, Parser};

fn escape() -> InputEvent {
    key(Key::Escape, Mods::empty())
}

#[test]
fn a_lone_esc_waits_and_then_is_the_escape_key() {
    let mut parser = Parser::new();
    assert_eq!(parser.feed(b"\x1b"), vec![]);
    assert!(parser.has_pending());
    assert_eq!(parser.flush(), vec![escape()]);
    assert!(!parser.has_pending());
    // Whatever comes next is unrelated.
    assert_eq!(parser.feed(b"a"), vec![typed('a')]);
    assert_eq!(parser.feed(b"[A"), vec![typed('['), typed('A')]);
}

#[test]
fn a_new_parser_and_complete_input_have_nothing_pending() {
    let mut parser = Parser::new();
    assert!(!parser.has_pending());
    assert_eq!(parser.flush(), vec![]);
    assert_eq!(parser.feed(b""), vec![]);
    assert!(!parser.has_pending());
    parser.feed(b"abc\x1b[A\x1b]0;t\x07");
    assert!(!parser.has_pending());
    assert_eq!(parser.flush(), vec![]);
}

#[test]
fn esc_runs_resolve_to_escape_presses() {
    assert_eq!(
        parse_and_flush(b"\x1b\x1b"),
        vec![key(Key::Escape, Mods::ALT)]
    );
    assert_eq!(
        parse_and_flush(b"\x1b\x1b\x1b"),
        vec![escape(), key(Key::Escape, Mods::ALT)]
    );
    assert_eq!(parse_and_flush(b"a\x1b"), vec![typed('a'), escape()]);
    assert_eq!(
        parse_and_flush(b"\x1b[A\x1b"),
        vec![key(Key::Up, Mods::empty()), escape()]
    );
}

#[test]
fn esc_and_a_sequence_introducer_is_alt_plus_that_character() {
    for c in ['[', 'O', ']', 'P', '_', '^', 'X'] {
        let bytes = [0x1b, c as u8];
        assert_eq!(parse_and_flush(&bytes), vec![alt_typed(c)], "ESC {c}");
        // With an ESC in front, that ESC was a key press of its own.
        let bytes = [0x1b, 0x1b, c as u8];
        assert_eq!(
            parse_and_flush(&bytes),
            vec![escape(), alt_typed(c)],
            "ESC ESC {c}"
        );
    }
    // alt+^ is alt+shift+6 and alt+O is alt+shift+o.
    let InputEvent::Key(ev) = alt_typed('^') else {
        unreachable!()
    };
    assert_eq!(
        (ev.key, ev.mods, ev.text),
        (Key::Char('6'), Mods::ALT | Mods::SHIFT, None)
    );
    let InputEvent::Key(ev) = alt_typed('O') else {
        unreachable!()
    };
    assert_eq!(
        (ev.key, ev.mods, ev.text),
        (Key::Char('o'), Mods::ALT | Mods::SHIFT, None)
    );
}

#[test]
fn an_empty_string_followed_by_esc_is_alt_introducer_then_escape() {
    assert_eq!(
        parse_and_flush(b"\x1b]\x1b"),
        vec![alt_typed(']'), escape()]
    );
    assert_eq!(
        parse_and_flush(b"\x1bP\x1b"),
        vec![alt_typed('P'), escape()]
    );
}

#[test]
fn an_unfinished_csi_survives_one_flush_and_completes_late() {
    let mut parser = Parser::new();
    assert_eq!(parser.feed(b"\x1b[1;5"), vec![]);
    assert!(parser.has_pending());
    assert_eq!(parser.flush(), vec![]);
    assert!(!parser.has_pending(), "a flush must leave nothing pending");
    assert_eq!(parser.feed(b"A"), vec![key(Key::Up, Mods::CTRL)]);
    assert!(!parser.has_pending());

    // A mouse report cut in the middle on a slow link.
    assert_eq!(parser.feed(b"\x1b[<35;10"), vec![]);
    assert_eq!(parser.flush(), vec![]);
    assert_eq!(
        parser.feed(b";5M"),
        vec![mouse(MouseKind::Move, 9, 4, Mods::empty())]
    );
}

#[test]
fn an_unfinished_csi_is_dropped_by_the_second_flush() {
    let mut parser = Parser::new();
    parser.feed(b"\x1b[1");
    assert_eq!(parser.flush(), vec![]);
    assert_eq!(parser.feed(b";"), vec![]);
    assert!(parser.has_pending(), "more of the late sequence arrived");
    assert_eq!(parser.flush(), vec![]);
    assert!(!parser.has_pending());
    assert_eq!(parser.feed(b"5A"), vec![typed('5'), typed('A')]);

    // Two flushes with nothing in between drop it too.
    parser.feed(b"\x1b[1;5");
    assert_eq!(parser.flush(), vec![]);
    assert_eq!(parser.flush(), vec![]);
    assert_eq!(parser.feed(b"A"), vec![typed('A')]);
}

#[test]
fn each_sequence_gets_its_own_grace_period() {
    let mut parser = Parser::new();
    for (tail, mods) in [
        (&b";5A"[..], Mods::CTRL),
        (&b";2A"[..], Mods::SHIFT),
        (&b";3A"[..], Mods::ALT),
    ] {
        parser.feed(b"\x1b[1");
        assert_eq!(parser.flush(), vec![]);
        assert_eq!(parser.feed(tail), vec![key(Key::Up, mods)]);
    }
}

#[test]
fn a_late_sequence_gives_way_to_a_new_one() {
    let mut parser = Parser::new();
    parser.feed(b"\x1b[1;5");
    parser.flush();
    assert_eq!(parser.feed(b"\x1b[B"), vec![key(Key::Down, Mods::empty())]);
    assert!(!parser.has_pending());

    parser.feed(b"\x1b]11;rgb:");
    parser.flush();
    assert_eq!(parser.feed(b"\x1b[97u"), vec![typed('a')]);
}

#[test]
fn an_unfinished_string_survives_one_flush_and_completes_late() {
    let mut parser = Parser::new();
    assert_eq!(parser.feed(b"\x1b]11;rgb:1a1a/"), vec![]);
    assert!(parser.has_pending());
    assert_eq!(parser.flush(), vec![]);
    assert!(!parser.has_pending());
    let color = reply(TerminalReply::Color {
        slot: ColorSlot::Background,
        rgb: Rgb::new(0x1a, 0x1b, 0x1c),
    });
    assert_eq!(parser.feed(b"1b1b/1c1c\x1b\\"), vec![color.clone()]);

    // Cut between the two bytes of the terminator.
    assert_eq!(parser.feed(b"\x1b]11;rgb:1a1a/1b1b/1c1c\x1b"), vec![]);
    assert_eq!(parser.flush(), vec![]);
    assert_eq!(parser.feed(b"\\"), vec![color]);
}

#[test]
fn an_unfinished_string_is_dropped_by_the_second_flush() {
    let mut parser = Parser::new();
    parser.feed(b"\x1bP>|ghos");
    assert_eq!(parser.flush(), vec![]);
    assert_eq!(parser.feed(b"tty"), vec![]);
    assert!(parser.has_pending());
    assert_eq!(parser.flush(), vec![]);
    assert!(!parser.has_pending());
    assert_eq!(parser.feed(b"x"), vec![typed('x')]);
}

#[test]
fn half_a_utf8_character_survives_one_flush() {
    let mut parser = Parser::new();
    assert_eq!(parser.feed(b"\xe6\x97"), vec![]);
    assert!(parser.has_pending());
    assert_eq!(parser.flush(), vec![]);
    assert!(!parser.has_pending());
    assert_eq!(parser.feed(b"\xa5"), vec![typed('日')]);

    parser.feed(b"\xe6\x97");
    parser.flush();
    parser.flush();
    assert_eq!(
        parser.feed(b"\xa5x"),
        vec![typed('x')],
        "the orphaned byte is not text"
    );

    // The same after ESC: alt + the character.
    assert_eq!(parser.feed(b"\x1b\xc3"), vec![]);
    assert_eq!(parser.flush(), vec![]);
    assert_eq!(parser.feed(b"\xa9"), vec![key(Key::Char('é'), Mods::ALT)]);
}

#[test]
fn an_unfinished_legacy_mouse_report_survives_one_flush() {
    let mut parser = Parser::new();
    assert_eq!(parser.feed(b"\x1b[M "), vec![]);
    assert!(parser.has_pending());
    assert_eq!(parser.flush(), vec![]);
    assert_eq!(
        parser.feed(b"!!"),
        vec![mouse(
            MouseKind::Press(MouseButton::Left),
            0,
            0,
            Mods::empty()
        )]
    );

    parser.feed(b"\x1b[M");
    parser.flush();
    parser.flush();
    assert_eq!(parser.feed(b" "), vec![typed(' ')]);
}

#[test]
fn a_paste_in_progress_is_not_pending_and_ignores_flush() {
    let mut parser = Parser::new();
    assert_eq!(parser.feed(b"\x1b[200~first "), vec![]);
    assert!(parser.in_paste());
    assert!(!parser.has_pending());
    assert_eq!(parser.flush(), vec![]);
    assert_eq!(parser.flush(), vec![]);
    assert!(parser.in_paste());
    // Even with half an end marker held back.
    assert_eq!(parser.feed(b"second\x1b[20"), vec![]);
    assert!(!parser.has_pending());
    assert_eq!(parser.flush(), vec![]);
    assert_eq!(
        parser.feed(b"1~"),
        vec![InputEvent::Paste("first second".into())]
    );
    assert!(!parser.in_paste());
}

#[test]
fn reset_forgets_pending_input_and_an_open_paste() {
    let mut parser = Parser::new();
    parser.feed(b"\x1b[1;5");
    parser.reset();
    assert!(!parser.has_pending());
    assert_eq!(parser.feed(b"A"), vec![typed('A')]);

    parser.feed(b"\x1b[200~half a paste");
    assert!(parser.in_paste());
    parser.reset();
    assert!(!parser.in_paste());
    assert_eq!(parser.feed(b"x\x1b[201~"), vec![typed('x')]);

    parser.feed(b"\x1b");
    parser.reset();
    assert_eq!(parser.flush(), vec![]);
}

#[test]
fn an_oversized_csi_is_swallowed_whole() {
    let mut bytes = b"\x1b[".to_vec();
    bytes.extend(std::iter::repeat_n(b'1', MAX_CSI_LEN + 1000));
    bytes.extend(b";5Ax");
    assert_eq!(parse(&bytes), vec![typed('x')]);

    // Exactly at the limit it is still read; one byte more and it is not.
    let padded = |zeros: usize| {
        let mut bytes = b"\x1b[".to_vec();
        bytes.extend(std::iter::repeat_n(b'0', zeros));
        bytes.extend(b"1;5A");
        bytes
    };
    assert_eq!(
        parse(&padded(MAX_CSI_LEN - 3)),
        vec![key(Key::Up, Mods::CTRL)]
    );
    assert_eq!(parse(&padded(MAX_CSI_LEN - 2)), vec![]);

    // While it is being swallowed the usual two-flush rule applies.
    let mut parser = Parser::new();
    parser.feed(b"\x1b[");
    parser.feed(&vec![b'1'; MAX_CSI_LEN + 10]);
    assert!(parser.has_pending());
    assert_eq!(parser.flush(), vec![]);
    assert!(!parser.has_pending());
    assert_eq!(parser.feed(b"2A"), vec![]);
    parser.feed(b"\x1b[");
    parser.feed(&vec![b'1'; MAX_CSI_LEN + 10]);
    parser.flush();
    parser.flush();
    assert_eq!(parser.feed(b"A"), vec![typed('A')]);
}

#[test]
fn a_string_at_the_limit_is_kept_and_one_byte_more_is_dropped() {
    let version = |len: usize| {
        let mut bytes = b"\x1bP>|".to_vec();
        bytes.extend(std::iter::repeat_n(b'v', len));
        bytes.extend(b"\x1b\\x");
        bytes
    };
    // The body is `>|` plus the text.
    let events = parse(&version(MAX_STRING_LEN - 2));
    assert_eq!(events.len(), 2);
    assert!(
        matches!(&events[0], InputEvent::Reply(TerminalReply::Version(v)) if v.len() == MAX_STRING_LEN - 2)
    );
    assert_eq!(events[1], typed('x'));

    assert_eq!(parse(&version(MAX_STRING_LEN - 1)), vec![typed('x')]);
    assert_eq!(parse(&version(MAX_STRING_LEN * 3)), vec![typed('x')]);
}

#[test]
fn an_oversized_osc_is_dropped_whichever_way_it_ends() {
    let body = vec![b'a'; MAX_STRING_LEN + 1];
    for terminator in [&b"\x07"[..], &b"\x1b\\"[..]] {
        let bytes = [b"\x1b]0;", &body[..], terminator, b"x"].concat();
        assert_eq!(parse(&bytes), vec![typed('x')]);
    }
    // Broken off by a new sequence instead of a terminator.
    let bytes = [b"\x1b]0;", &body[..], b"\x1b[A"].concat();
    assert_eq!(parse(&bytes), vec![key(Key::Up, Mods::empty())]);
    // In the other string kinds BEL does not terminate, oversized or not.
    let bytes = [b"\x1b_", &body[..], b"\x07still inside\x1b\\x"].concat();
    assert_eq!(parse(&bytes), vec![typed('x')]);
}

#[test]
fn an_oversized_string_does_not_keep_its_bytes() {
    let mut parser = Parser::new();
    parser.feed(b"\x1b]0;");
    for _ in 0..64 {
        parser.feed(&[b'a'; 4096]);
    }
    assert!(
        parser.seq.is_empty(),
        "the body is discarded once it is over the limit"
    );
    assert!(parser.seq.capacity() <= 2 * MAX_STRING_LEN);
    assert!(parser.has_pending());
    assert_eq!(parser.flush(), vec![]);
    assert!(!parser.has_pending());
    assert_eq!(parser.feed(b"tail\x07y"), vec![typed('y')]);
}
