//! Sequences as real terminals and their libraries write them, taken from the encoders of
//! Ghostty and WezTerm and from the parser tests of crossterm and termina. The table in
//! `table.rs` follows the specifications; this one checks that what is actually sent means
//! what the user did.

use bakedit_core::{InputEvent, Key, KeyKind, ModKey, Mods, MouseButton, MouseKind};

use super::{alt_typed, key, mouse, parse, typed, Expect, Parser};

type Case = (&'static [u8], Vec<InputEvent>);

/// What Ghostty writes with kitty keyboard flags active.
#[rustfmt::skip]
fn ghostty_kitty() -> Vec<Case> {
    let release = KeyKind::Release;
    let shift = Mods::SHIFT;
    let modifier = |key: ModKey| Expect::key(Key::Modifier(key));
    vec![
        (b"\x1b[13u", vec![key(Key::Enter, Mods::empty())]),
        (b"\x1b[13;1:3u", vec![Expect::key(Key::Enter).kind(release).event()]),
        (b"\x1b[127u", vec![key(Key::Backspace, Mods::empty())]),
        (b"\x1b[127;1:3u", vec![Expect::key(Key::Backspace).kind(release).event()]),
        (b"\x1b[9;1:3u", vec![Expect::key(Key::Tab).kind(release).event()]),
        (b"\x1b[127;2u", vec![key(Key::Backspace, shift)]),
        (b"\x1b[127;3u", vec![key(Key::Backspace, Mods::ALT)]),
        (b"\x1b[13;2u", vec![key(Key::Enter, shift)]),
        (b"\x1b[9;2u", vec![key(Key::Tab, shift)]),
        (b"\x1b[3~", vec![key(Key::Delete, Mods::empty())]),
        // Modifier keys, with and without their own bit in the modifiers.
        (b"\x1b[57441u", vec![modifier(ModKey::LeftShift).event()]),
        (b"\x1b[57441;2u", vec![modifier(ModKey::LeftShift).mods(shift).event()]),
        (b"\x1b[57442;5u", vec![modifier(ModKey::LeftCtrl).mods(Mods::CTRL).event()]),
        (b"\x1b[57442;5:3u", vec![modifier(ModKey::LeftCtrl).mods(Mods::CTRL).kind(release).event()]),
        // Alternate keys and associated text.
        (b"\x1b[97:65;2u", vec![Expect::ch('a').mods(shift).shifted('A').text("A").event()]),
        (b"\x1b[65::97;2u", vec![Expect::ch('a').mods(shift).shifted('A').base('a').text("A").event()]),
        (b"\x1b[59:58;2;58u", vec![Expect::ch(';').mods(shift).shifted(':').text(":").event()]),
        (b"\x1b[106:74;2;74u", vec![Expect::ch('j').mods(shift).shifted('J').text("J").event()]),
        (b"\x1b[106:74;2:3u", vec![Expect::ch('j').mods(shift).shifted('J').kind(release).event()]),
        (b"\x1b[106;65;74u", vec![Expect::ch('j').caps_lock().text("J").event()]),
        (b"\x1b[106;5u", vec![key(Key::Char('j'), Mods::CTRL)]),
        // A Russian layout: the key is ч, at the position of the US `;`.
        (b"\x1b[1095::59;;1095u", vec![Expect::ch('ч').base(';').text("ч").event()]),
        (b"\x1b[1095:1063:59;2;1063u", vec![Expect::ch('ч').mods(shift).shifted('Ч').base(';').text("Ч").event()]),
        (b"\x1b[1095::59;65;1063u", vec![Expect::ch('ч').base(';').caps_lock().text("Ч").event()]),
        (b"\x1b[337::91;5:3u", vec![Expect::ch('ő').mods(Mods::CTRL).base('[').kind(release).event()]),
        // Option on a Mac: a modifier, a way to type a character, or both at once.
        (b"\x1b[119;3u", vec![key(Key::Char('w'), Mods::ALT)]),
        (b"\x1b[119;3;8721u", vec![Expect::ch('w').mods(Mods::ALT).text("∑").event()]),
        (b"\x1b[119;;8721u", vec![Expect::ch('w').text("∑").event()]),
        (b"\x1b[57400;;49u", vec![Expect::ch('1').keypad().text("1").event()]),
        // A dead key before Backspace: the key arrives with the pending text.
        (b"\x1b[127;;65u", vec![Expect::key(Key::Backspace).text("A").event()]),
        (b"\x1b[127;2;65u", vec![Expect::key(Key::Backspace).mods(shift).text("A").event()]),
        (b"\x1b[127;1:3;65u", vec![Expect::key(Key::Backspace).kind(release).event()]),
        // Keys that keep their legacy form.
        (b"\x1b[A", vec![key(Key::Up, Mods::empty())]),
        (b"\x1b[1;2A", vec![key(Key::Up, shift)]),
        (b"\x1b[1;2:3A", vec![Expect::key(Key::Up).mods(shift).kind(release).event()]),
        (b"\x1b[1;1:1A", vec![key(Key::Up, Mods::empty())]),
        (b"\x1b[1;5P", vec![key(Key::F(1), Mods::CTRL)]),
        (b"\x1b[1;5Q", vec![key(Key::F(2), Mods::CTRL)]),
        (b"\x1b[13;5~", vec![key(Key::F(3), Mods::CTRL)]),
        (b"\x1b[1;5S", vec![key(Key::F(4), Mods::CTRL)]),
        (b"\x1b[15;5~", vec![key(Key::F(5), Mods::CTRL)]),
        // Shortcuts that legacy bytes cannot tell apart from other keys.
        (b"\x1b[105;5u", vec![key(Key::Char('i'), Mods::CTRL)]),
        (b"\x1b[109;5u", vec![key(Key::Char('m'), Mods::CTRL)]),
        (b"\x1b[91;5u", vec![key(Key::Char('['), Mods::CTRL)]),
        (b"\x1b[64;5u", vec![key(Key::Char('@'), Mods::CTRL)]),
        (b"\x1b[109;6u", vec![key(Key::Char('m'), Mods::CTRL | shift)]),
    ]
}

/// What Ghostty writes in legacy mode.
#[rustfmt::skip]
fn ghostty_legacy() -> Vec<Case> {
    let ctrl = Mods::CTRL;
    let alt = Mods::ALT;
    let none = Mods::empty();
    vec![
        (b"\r", vec![key(Key::Enter, none)]),
        (b"\t", vec![key(Key::Tab, none)]),
        (b"\x7f", vec![key(Key::Backspace, none)]),
        (b"\x03", vec![key(Key::Char('c'), ctrl)]),
        (b"\x00", vec![key(Key::Char(' '), ctrl)]),
        (b"\x08", vec![key(Key::Char('h'), ctrl)]),
        (b"\x10", vec![key(Key::Char('p'), ctrl)]),
        (b"\x1f", vec![key(Key::Char('-'), ctrl)]),
        (b"\x1bc", vec![key(Key::Char('c'), alt)]),
        (b"\x1be", vec![key(Key::Char('e'), alt)]),
        (b"\x1b>", vec![alt_typed('>')]),
        (b"\x1b\x03", vec![key(Key::Char('c'), ctrl | alt)]),
        (b"\x1b\x08", vec![key(Key::Char('h'), ctrl | alt)]),
        (b"\x1b\r", vec![key(Key::Enter, alt)]),
        (b"\x1b\t", vec![key(Key::Tab, alt)]),
        (b"\x1b\x7f", vec![key(Key::Backspace, alt)]),
        ("\x1b界".as_bytes(), vec![key(Key::Char('界'), alt)]),
        ("\x1b😀".as_bytes(), vec![key(Key::Char('😀'), alt)]),
        ("\x1bф".as_bytes(), vec![key(Key::Char('ф'), alt)]),
        ("û".as_bytes(), vec![typed('û')]),
        (b"\x1b[Z", vec![key(Key::Tab, Mods::SHIFT)]),
        (b"\x1bOq", vec![Expect::ch('1').keypad().text("1").event()]),
        // xterm's modifyOtherKeys form for what has no legacy bytes of its own.
        (b"\x1b[27;3;27~", vec![key(Key::Escape, alt)]),
        (b"\x1b[27;8;127~", vec![key(Key::Backspace, Mods::SHIFT | alt | ctrl)]),
        (b"\x1b[27;6;72~", vec![Expect::ch('h').mods(ctrl | Mods::SHIFT).shifted('H').event()]),
        (b"\x1b[27;5;112~", vec![key(Key::Char('p'), ctrl)]),
        (b"\x1b[27;3;56~", vec![key(Key::Char('8'), alt)]),
        (b"\x1b[27;16;13~", vec![key(Key::Enter, Mods::SHIFT | alt | ctrl | Mods::SUPER)]),
        (b"\x1b[27;9;9~", vec![key(Key::Tab, Mods::SUPER)]),
        // The function keys beyond F12.
        (b"\x1b[25;2~", vec![key(Key::F(13), Mods::SHIFT)]),
        (b"\x1b[34;5~", vec![key(Key::F(20), ctrl)]),
        (b"\x1b[42;2~", vec![key(Key::F(21), Mods::SHIFT)]),
        (b"\x1b[46;3~", vec![key(Key::F(25), alt)]),
        (b"\x1b[1;5E", vec![Expect::key(Key::Other(57427)).keypad().mods(ctrl).event()]),
    ]
}

/// What WezTerm writes with kitty keyboard flags active, and in its other modes.
#[rustfmt::skip]
fn wezterm() -> Vec<Case> {
    let release = KeyKind::Release;
    let ctrl = Mods::CTRL;
    let alt = Mods::ALT;
    let shift = Mods::SHIFT;
    vec![
        // It spells out the default modifiers.
        (b"\x1b[11;1~", vec![key(Key::F(1), Mods::empty())]),
        (b"\x1b[11;1:3~", vec![Expect::key(Key::F(1)).kind(release).event()]),
        (b"\x1b[111;1:3u", vec![Expect::ch('o').kind(release).event()]),
        (b"\x1b[105;4u", vec![key(Key::Char('i'), shift | alt)]),
        (b"\x1b[49;4u", vec![key(Key::Char('1'), shift | alt)]),
        (b"\x1b[105;6u", vec![key(Key::Char('i'), shift | ctrl)]),
        (b"\x1b[105;8u", vec![key(Key::Char('i'), shift | alt | ctrl)]),
        (b"\x1b[127;5u", vec![key(Key::Backspace, ctrl)]),
        (b"\x1b[E", vec![Expect::key(Key::Other(57427)).keypad().event()]),
        // A Russian layout again, with text reported even for a shortcut.
        (b"\x1b[1092::97;5u", vec![Expect::ch('ф').mods(ctrl).base('a').event()]),
        (b"\x1b[1092:1060:97;6u", vec![Expect::ch('ф').mods(ctrl | shift).shifted('Ф').base('a').event()]),
        (b"\x1b[1092::97;5;1092u", vec![Expect::ch('ф').mods(ctrl).base('a').text("ф").event()]),
        (b"\x1b[1092:1060:97;6;1060u", vec![Expect::ch('ф').mods(ctrl | shift).shifted('Ф').base('a').text("Ф").event()]),
        // Its legacy and modifyOtherKeys output.
        (b"\x1b[9;5u", vec![key(Key::Tab, ctrl)]),
        (b"\x1b[1;5Z", vec![key(Key::Tab, ctrl | shift)]),
        (b"\x1b[27;5;13~", vec![key(Key::Enter, ctrl)]),
        (b"\x1b[27;6;13~", vec![key(Key::Enter, ctrl | shift)]),
        (b"\x1b[27;5;9~", vec![key(Key::Tab, ctrl)]),
        (b"\x1b[27;6;9~", vec![key(Key::Tab, ctrl | shift)]),
        (b"\x1b[27;5;99~", vec![key(Key::Char('c'), ctrl)]),
        (b"\x1b[27;5;49~", vec![key(Key::Char('1'), ctrl)]),
        (b"\x1b[27;5;44~", vec![key(Key::Char(','), ctrl)]),
        (b"\x1b[D", vec![key(Key::Left, Mods::empty())]),
        (b"\x1b[1;3D", vec![key(Key::Left, alt)]),
        (b"\x1b[1;3H", vec![key(Key::Home, alt)]),
        (b"\x1b[1;3F", vec![key(Key::End, alt)]),
        (b"\x1b[5;3~", vec![key(Key::PageUp, alt)]),
    ]
}

/// Inputs other parsers are tested with.
#[rustfmt::skip]
fn other_parsers() -> Vec<Case> {
    let release = KeyKind::Release;
    let left = MouseButton::Left;
    vec![
        (b"\x1b[20;10R", vec![]),
        (b"\x1b[2D", vec![key(Key::Left, Mods::SHIFT)]),
        (b"\x1b[3;2~", vec![key(Key::Delete, Mods::SHIFT)]),
        (b"\x1b[97;7u", vec![key(Key::Char('a'), Mods::ALT | Mods::CTRL)]),
        (b"\x1b\x14", vec![key(Key::Char('t'), Mods::ALT | Mods::CTRL)]),
        (b"\x1bH", vec![alt_typed('H')]),
        (b"\x1b[57:40;4u", vec![Expect::ch('9').mods(Mods::SHIFT | Mods::ALT).shifted('(').event()]),
        (b"\x1b[45:95;4u", vec![Expect::ch('-').mods(Mods::SHIFT | Mods::ALT).shifted('_').event()]),
        (b"\x1b[;1:3B", vec![Expect::key(Key::Down).kind(release).event()]),
        (b"\x1b[1;1:3B", vec![Expect::key(Key::Down).kind(release).event()]),
        (b"\x1b[5;1:3~", vec![Expect::key(Key::PageUp).kind(release).event()]),
        (b"\x1b[6;5:3~", vec![Expect::key(Key::PageDown).mods(Mods::CTRL).kind(release).event()]),
        (b"\x1b[57449u", vec![Expect::key(Key::Modifier(ModKey::RightAlt)).event()]),
        (b"\x1b[57449;3:3u", vec![Expect::key(Key::Modifier(ModKey::RightAlt)).mods(Mods::ALT).kind(release).event()]),
        (b"\x1b[49;129u", vec![Expect::ch('1').num_lock().text("1").event()]),
        (b"\x1b[M0\x60\x70", vec![mouse(MouseKind::Press(left), 63, 79, Mods::CTRL)]),
        (b"\x1b[<0;20;10M", vec![mouse(MouseKind::Press(left), 19, 9, Mods::empty())]),
        (b"\x1b[<0;20;10;M", vec![mouse(MouseKind::Press(left), 19, 9, Mods::empty())]),
        (b"\x1b[<0;20;10m", vec![mouse(MouseKind::Release(left), 19, 9, Mods::empty())]),
        (b"\x1b[32;30;40;M", vec![]),
        (b"\x1b[200~on and on and on\x1b[201~", vec![InputEvent::Paste("on and on and on".into())]),
        (b"\x1b[200~o\x1b[2D\x1b[201~", vec![InputEvent::Paste("o\x1b[2D".into())]),
    ]
}

fn all() -> Vec<Case> {
    [
        ghostty_kitty(),
        ghostty_legacy(),
        wezterm(),
        other_parsers(),
    ]
    .concat()
}

#[test]
fn what_terminals_send_means_what_the_user_did() {
    for (bytes, expected) in all() {
        assert_eq!(
            parse(bytes),
            expected,
            "input {:?}",
            String::from_utf8_lossy(bytes)
        );
    }
}

#[test]
fn and_it_means_the_same_cut_anywhere() {
    for (bytes, expected) in all() {
        for cut in 1..bytes.len() {
            let mut parser = Parser::new();
            let mut events = parser.feed(&bytes[..cut]);
            events.extend(parser.feed(&bytes[cut..]));
            let input = String::from_utf8_lossy(bytes);
            assert_eq!(events, expected, "input {input:?} cut at {cut}");
        }
    }
}

#[test]
fn composed_text_from_a_terminal_in_all_keys_mode_is_text_without_a_key() {
    // Ghostty writes composed and input-method text as plain UTF-8 even when it reports
    // every key as an escape code.
    let mut parser = Parser::new();
    parser.set_kitty_flags(crate::host::KITTY_ALL_FLAGS);
    let expected = Expect::key(Key::Other(0)).text("û").event();
    assert_eq!(parser.feed("û".as_bytes()), vec![expected]);
}

#[test]
fn alt_escape_in_legacy_mode_needs_the_timeout() {
    let mut parser = Parser::new();
    assert_eq!(parser.feed(b"\x1b\x1b"), vec![]);
    assert_eq!(parser.flush(), vec![key(Key::Escape, Mods::ALT)]);
}
