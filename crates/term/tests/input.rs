//! What the user sends: keys, the mouse, pastes and focus changes, as real children on a
//! pty receive them.

mod common;

use bakedit_core::{Key, KeyEvent, KeyKind, Mods, MouseButton, MouseEvent, MouseKind};
use bakedit_term::{CursorShape, TermConfig};
use common::{echo, sh, start};

fn key(key: Key, mods: Mods) -> KeyEvent {
    KeyEvent::new(key, mods)
}

fn mouse(kind: MouseKind, col: u16, row: u16) -> MouseEvent {
    MouseEvent::new(kind, col, row)
}

#[test]
fn typed_input_round_trips_through_cat() {
    let mut session = start(TermConfig {
        program: Some("cat".to_string()),
        ..TermConfig::default()
    });
    let term = &session.term;

    for c in "hi é".chars() {
        assert!(term.send_key(&KeyEvent::typed(c)));
    }
    assert!(term.send_key(&key(Key::Enter, Mods::empty())));
    // Once from the tty's echo, once from cat.
    session.wait_until("the typed line twice", |term| {
        term.row_text(0) == "hi é" && term.row_text(1) == "hi é"
    });

    assert!(term.send_text("raw text\r"));
    session.wait_until("the raw line twice", |term| {
        term.row_text(2) == "raw text" && term.row_text(3) == "raw text"
    });

    // An unbracketed paste turns line breaks into Enter. Both lines arrive at once, so
    // the tty echoes both before cat prints both.
    assert!(term.send_paste("one\ntwo\r\n"));
    session.wait_until("both pasted lines twice", |term| {
        term.row_text(6) == "one" && term.row_text(7) == "two"
    });
    assert_eq!(term.row_text(4), "one");
    assert_eq!(term.row_text(5), "two");

    // Things that produce no bytes say so, and send nothing.
    assert!(!term.send_key(&KeyEvent::typed('x').with_kind(KeyKind::Release)));
    assert!(!term.send_key(&key(Key::Char('s'), Mods::SUPER)));
    assert!(!term.send_key(&key(Key::CapsLock, Mods::empty())));
    assert!(!term.send_paste(""));
    assert!(!term.send_text(""));
    assert!(!term.write(b""));
    assert!(term.write(b"bytes\n"));
    session.wait_until("the written line twice", |term| {
        term.row_text(8) == "bytes" && term.row_text(9) == "bytes"
    });
    assert_eq!(term.row_text(10), "");

    // ctrl-d on an empty line is end of input: cat exits cleanly.
    assert!(term.send_key(&key(Key::Char('d'), Mods::CTRL)));
    assert_eq!(session.wait_for_exit(), Some(0));
    assert!(!session.term.is_running());
}

#[test]
fn keys_are_legacy_until_the_child_asks_for_more() {
    let session = start(echo(""));
    session.wait_ready();
    let term = &session.term;
    assert_eq!(term.modes().kitty_flags, 0);

    let mut expected: Vec<u8> = Vec::new();
    let mut check = |event: KeyEvent, bytes: &[u8]| {
        assert!(term.send_key(&event), "{event:?}");
        expected.extend_from_slice(bytes);
        session.wait_received(&expected);
    };
    check(KeyEvent::typed('a'), b"a");
    check(key(Key::Char('c'), Mods::CTRL), b"\x03");
    check(key(Key::Char('x'), Mods::ALT), b"\x1bx");
    check(key(Key::Escape, Mods::empty()), b"\x1b");
    check(key(Key::Enter, Mods::SHIFT), b"\r");
    check(key(Key::Tab, Mods::SHIFT), b"\x1b[Z");
    check(key(Key::Backspace, Mods::empty()), b"\x7f");
    check(key(Key::Up, Mods::empty()), b"\x1b[A");
    check(key(Key::Left, Mods::CTRL), b"\x1b[1;5D");
    check(key(Key::F(5), Mods::empty()), b"\x1b[15~");
    check(key(Key::Delete, Mods::empty()), b"\x1b[3~");

    // Application cursor keys.
    session.child_writes("\x1b[?1h");
    session.wait_until("application cursor mode", |term| term.modes().app_cursor);
    check(key(Key::Up, Mods::empty()), b"\x1bOA");
    check(key(Key::Up, Mods::SHIFT), b"\x1b[1;2A");
}

#[test]
fn keys_follow_the_kitty_flags_the_child_pushed() {
    // What herdr asks for: disambiguate, event types, alternate keys.
    let session = start(echo("\\x1b[>7u"));
    session.wait_ready();
    session.wait_until("the kitty keyboard flags", |term| {
        term.modes().kitty_flags == 7
    });
    let term = &session.term;

    let mut expected: Vec<u8> = Vec::new();
    let mut check = |event: KeyEvent, bytes: &[u8]| {
        assert!(term.send_key(&event), "{event:?}");
        expected.extend_from_slice(bytes);
        session.wait_received(&expected);
    };
    check(key(Key::Char('c'), Mods::CTRL), b"\x1b[99;5u");
    check(KeyEvent::typed('a'), b"a");
    check(
        KeyEvent::typed('a').with_kind(KeyKind::Release),
        b"\x1b[97;1:3u",
    );
    check(key(Key::Escape, Mods::empty()), b"\x1b[27u");
    check(key(Key::Enter, Mods::empty()), b"\r");
    check(key(Key::Enter, Mods::SHIFT), b"\x1b[13;2u");
    check(
        key(Key::Char('p'), Mods::SUPER | Mods::SHIFT),
        b"\x1b[112:80;10u",
    );
    check(
        key(Key::Up, Mods::empty()).with_kind(KeyKind::Repeat),
        b"\x1b[1;1:2A",
    );
    // Enter has no release event without "report all keys".
    assert!(!term.send_key(&key(Key::Enter, Mods::empty()).with_kind(KeyKind::Release)));

    // The child raises the flags to everything, then pops back to legacy.
    session.child_writes("\x1b[=31;1u");
    session.wait_until("all kitty flags", |term| term.modes().kitty_flags == 31);
    check(KeyEvent::typed('A'), b"\x1b[97:65;2;65u");
    check(
        key(Key::Enter, Mods::empty()).with_kind(KeyKind::Release),
        b"\x1b[13;1:3u",
    );

    session.child_writes("\x1b[<u");
    session.wait_until("legacy keys again", |term| term.modes().kitty_flags == 0);
    check(key(Key::Char('c'), Mods::CTRL), b"\x03");
    assert!(!term.send_key(&KeyEvent::typed('a').with_kind(KeyKind::Release)));
}

#[test]
fn the_keyboard_query_is_answered_with_the_flags_in_effect() {
    // The detection handshake from the kitty specification: ask for the flags, then for
    // the device attributes; a terminal without the protocol answers only the second.
    let session = start(echo("\\x1b[?u\\x1b[c"));
    session.wait_ready();
    session.wait_received(b"\x1b[?0u\x1b[?6c");
    session.child_writes("\x1b[>5u\x1b[?u");
    session.wait_received(b"\x1b[?0u\x1b[?6c\x1b[?5u");
}

#[test]
fn mouse_events_go_to_a_child_that_asked_for_them() {
    let session = start(echo("\\x1b[?1000h\\x1b[?1006h"));
    session.wait_ready();
    session.wait_until("mouse reporting", |term| {
        term.modes().mouse_reporting && term.modes().sgr_mouse
    });
    let term = &session.term;
    let left = MouseButton::Left;

    let mut expected: Vec<u8> = Vec::new();
    let mut check = |event: MouseEvent, bytes: &[u8]| {
        assert!(term.send_mouse(&event), "{event:?}");
        expected.extend_from_slice(bytes);
        session.wait_received(&expected);
    };
    check(mouse(MouseKind::Press(left), 2, 1), b"\x1b[<0;3;2M");
    check(mouse(MouseKind::Release(left), 2, 1), b"\x1b[<0;3;2m");
    // Click-only reporting: motion is the child's business but there is nothing to send.
    assert!(term.send_mouse(&mouse(MouseKind::Drag(left), 3, 1)));
    assert!(term.send_mouse(&mouse(MouseKind::Move, 4, 1)));
    check(mouse(MouseKind::ScrollDown, 5, 5), b"\x1b[<65;6;6M");
    let mut shifted = mouse(MouseKind::Press(MouseButton::Right), 0, 0);
    shifted.mods = Mods::SHIFT | Mods::CTRL;
    check(shifted, b"\x1b[<22;1;1M");
    // Positions past the edge are clamped to the last cell (the terminal is 200x50).
    check(
        mouse(MouseKind::Press(left), 9999, 9999),
        b"\x1b[<0;200;50M",
    );

    session.child_writes("\x1b[?1003h");
    session.wait_until("any-motion reporting", |term| term.modes().mouse_motion);
    check(mouse(MouseKind::Move, 7, 8), b"\x1b[<35;8;9M");
    check(mouse(MouseKind::Drag(left), 7, 9), b"\x1b[<32;8;10M");

    // Without SGR the report is the three-byte legacy form.
    session.child_writes("\x1b[?1006l");
    session.wait_until("legacy mouse encoding", |term| !term.modes().sgr_mouse);
    check(mouse(MouseKind::Press(left), 0, 0), b"\x1b[M\x20\x21\x21");
    check(mouse(MouseKind::Release(left), 0, 0), b"\x1b[M\x23\x21\x21");

    // Reporting off: the event is handed back to the application.
    session.child_writes("\x1b[?1003l");
    session.wait_until("no mouse reporting", |term| !term.modes().mouse_reporting);
    assert!(!term.send_mouse(&mouse(MouseKind::Press(left), 1, 1)));
    assert!(!term.send_mouse(&mouse(MouseKind::ScrollUp, 1, 1)));
    // Nothing more arrived: the next thing the child reads is this key.
    check_key(&session, &mut expected, b"z");
}

fn check_key(session: &common::Session, expected: &mut Vec<u8>, bytes: &[u8]) {
    for byte in bytes {
        assert!(session.term.send_key(&KeyEvent::typed(*byte as char)));
    }
    expected.extend_from_slice(bytes);
    session.wait_received(&*expected);
}

#[test]
fn a_shell_at_its_prompt_does_not_get_the_mouse() {
    let session = start(sh("printf 'no mouse here\\n'; exec cat"));
    session.wait_for_text("no mouse here");
    let term = &session.term;
    assert!(!term.modes().mouse_reporting);
    assert!(!term.modes().alt_screen);
    for kind in [
        MouseKind::Press(MouseButton::Left),
        MouseKind::Drag(MouseButton::Left),
        MouseKind::Move,
        MouseKind::ScrollUp,
        MouseKind::ScrollDown,
    ] {
        assert!(!term.send_mouse(&mouse(kind, 1, 0)), "{kind:?}");
    }
}

#[test]
fn the_wheel_sends_arrow_keys_to_a_full_screen_program() {
    let config = TermConfig {
        alt_scroll_lines: 2,
        ..echo("\\x1b[?1049h")
    };
    let session = start(config);
    session.wait_ready();
    session.wait_until("the alternate screen", |term| term.modes().alt_screen);
    let term = &session.term;
    assert!(term.modes().alternate_scroll);
    assert!(!term.modes().mouse_reporting);

    assert!(term.send_mouse(&mouse(MouseKind::ScrollUp, 3, 3)));
    session.wait_received(b"\x1b[A\x1b[A");
    assert!(term.send_mouse(&mouse(MouseKind::ScrollDown, 3, 3)));
    session.wait_received(b"\x1b[A\x1b[A\x1b[B\x1b[B");
    // Only the wheel, and only up and down.
    assert!(!term.send_mouse(&mouse(MouseKind::ScrollLeft, 3, 3)));
    assert!(!term.send_mouse(&mouse(MouseKind::Press(MouseButton::Left), 3, 3)));

    session.child_writes("\x1b[?1h");
    session.wait_until("application cursor mode", |term| term.modes().app_cursor);
    assert!(term.send_mouse(&mouse(MouseKind::ScrollUp, 3, 3)));
    session.wait_received(b"\x1b[A\x1b[A\x1b[B\x1b[B\x1bOA\x1bOA");

    // The child can switch alternate scroll off.
    session.child_writes("\x1b[?1007l");
    session.wait_until("alternate scroll off", |term| {
        !term.modes().alternate_scroll
    });
    assert!(!term.send_mouse(&mouse(MouseKind::ScrollUp, 3, 3)));
    // And the alternate screen has no scrollback to scroll instead.
    assert!(!term.scroll(bakedit_term::Scroll::PageUp));
    assert_eq!(term.history_len(), 0);
}

#[test]
fn pastes_are_bracketed_for_a_child_that_asked() {
    let session = start(echo("\\x1b[?2004h"));
    session.wait_ready();
    session.wait_until("bracketed paste mode", |term| term.modes().bracketed_paste);
    let term = &session.term;

    // The end marker hidden in the text cannot close the paste.
    assert!(term.send_paste("first\x1b[201~ line\nsecond"));
    let mut expected = b"\x1b[200~first line\nsecond\x1b[201~".to_vec();
    session.wait_received(&expected);

    session.child_writes("\x1b[?2004l");
    session.wait_until("plain paste mode", |term| !term.modes().bracketed_paste);
    assert!(term.send_paste("x\ny"));
    expected.extend_from_slice(b"x\ry");
    session.wait_received(&expected);
}

#[test]
fn focus_changes_are_reported_to_a_child_that_asked() {
    let session = start(echo("\\x1b[?1004h"));
    session.wait_ready();
    session.wait_until("focus reporting", |term| term.modes().focus_reporting);
    let term = &session.term;
    assert_ne!(
        term.cursor().map(|cursor| cursor.shape),
        Some(CursorShape::Hollow)
    );

    let generation = term.generation();
    assert!(term.send_focus(false));
    session.wait_received(b"\x1b[O");
    assert_eq!(
        term.cursor().map(|cursor| cursor.shape),
        Some(CursorShape::Hollow)
    );
    assert!(
        term.generation() > generation,
        "losing the focus changes what is drawn"
    );

    assert!(term.send_focus(true));
    session.wait_received(b"\x1b[O\x1b[I");
    assert_eq!(
        term.cursor().map(|cursor| cursor.shape),
        Some(CursorShape::Block)
    );

    // A child that did not ask is not told, but the cursor still follows the focus.
    session.child_writes("\x1b[?1004l");
    session.wait_until("no focus reporting", |term| !term.modes().focus_reporting);
    assert!(!term.send_focus(false));
    assert_eq!(
        term.cursor().map(|cursor| cursor.shape),
        Some(CursorShape::Hollow)
    );
    check_key(&session, &mut b"\x1b[O\x1b[I".to_vec(), b"k");
}

#[test]
fn typing_returns_to_the_live_screen_and_drops_the_selection() {
    let session = start(sh("seq 1 100; exec cat"));
    session.wait_for_text("100");
    let term = &session.term;
    assert!(term.scroll(bakedit_term::Scroll::Lines(30)));
    term.selection_start(0, 0, bakedit_term::SelectionKind::Line);
    assert!(term.selection_text().is_some());
    assert_eq!(term.scroll_offset(), 30);

    // A release goes nowhere and changes nothing.
    assert!(!term.send_key(&KeyEvent::typed('q').with_kind(KeyKind::Release)));
    assert_eq!(term.scroll_offset(), 30);
    assert!(term.selection_text().is_some());

    assert!(term.send_key(&KeyEvent::typed('q')));
    assert_eq!(term.scroll_offset(), 0);
    assert_eq!(term.selection_text(), None);
    session.wait_for_text("q");
}
