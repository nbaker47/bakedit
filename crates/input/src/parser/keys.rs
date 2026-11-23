//! Key tables: what the legacy bytes, the function-key numbers and the kitty key codes mean.

use bakedit_core::{us_shifted, Key, KeyEvent, KeyKind, ModKey, Mods};

/// The modifiers that stop a key from typing text.
pub(super) const NON_TEXT_MODS: Mods = Mods::CTRL
    .union(Mods::ALT)
    .union(Mods::SUPER)
    .union(Mods::HYPER)
    .union(Mods::META);

/// The kitty code of the keypad's centre key (KP_BEGIN), which has no ordinary equivalent.
const KP_BEGIN: u32 = 57427;

fn press(key: Key) -> KeyEvent {
    KeyEvent::new(key, Mods::empty())
}

fn keypad(key: Key) -> KeyEvent {
    let mut ev = press(key);
    ev.keypad = true;
    ev
}

/// The press a single byte below 0x80 stands for in the legacy encoding: a control
/// character is the key that sends it (ctrl+letter, except that 0x0d is Enter, 0x09 Tab and
/// 0x7f Backspace), anything else is typed text.
pub(super) fn legacy_byte(byte: u8) -> KeyEvent {
    let ctrl = |c: char| KeyEvent::new(Key::Char(c), Mods::CTRL);
    match byte {
        0x0d => press(Key::Enter),
        0x09 => press(Key::Tab),
        0x7f => press(Key::Backspace),
        0x1b => press(Key::Escape),
        0x00 => ctrl(' '),
        0x01..=0x1a => ctrl(char::from(b'a' + byte - 1)),
        0x1c => ctrl('\\'),
        0x1d => ctrl(']'),
        // ctrl+^ and ctrl+_, reported as the unshifted keys they sit on.
        0x1e => ctrl('6'),
        0x1f => ctrl('-'),
        _ => KeyEvent::typed(char::from(byte)),
    }
}

/// The cursor and function keys named by the final byte of `CSI 1 ; mods X` and `SS3 X`.
pub(super) fn letter_key(final_byte: u8) -> Option<KeyEvent> {
    Some(match final_byte {
        b'A' => press(Key::Up),
        b'B' => press(Key::Down),
        b'C' => press(Key::Right),
        b'D' => press(Key::Left),
        b'H' => press(Key::Home),
        b'F' => press(Key::End),
        b'E' => keypad(Key::Other(KP_BEGIN)),
        b'P' => press(Key::F(1)),
        b'Q' => press(Key::F(2)),
        b'R' => press(Key::F(3)),
        b'S' => press(Key::F(4)),
        _ => return None,
    })
}

/// The key named by the byte after `ESC O`: the cursor and function keys, plus the keypad
/// keys a terminal left in application keypad mode sends.
pub(super) fn ss3(byte: u8) -> Option<KeyEvent> {
    if let Some(ev) = letter_key(byte) {
        return Some(ev);
    }
    let keypad_char = |c: char| keypad(Key::Char(c)).with_text(c);
    Some(match byte {
        b'M' => keypad(Key::Enter),
        b'I' => keypad(Key::Tab),
        b' ' => keypad_char(' '),
        b'X' => keypad_char('='),
        b'j' => keypad_char('*'),
        b'k' => keypad_char('+'),
        b'l' => keypad_char(','),
        b'm' => keypad_char('-'),
        b'n' => keypad_char('.'),
        b'o' => keypad_char('/'),
        b'p'..=b'y' => keypad_char(char::from(b'0' + (byte - b'p'))),
        _ => return None,
    })
}

/// The key named by the number in `CSI number ; mods ~`.
///
/// 29 is F16 here, as in xterm and terminfo (`kf16`). kitty also sends it for the Menu key,
/// but only when no keyboard enhancement is active; with any of them on Menu is `57363 u`.
pub(super) fn tilde_key(number: u32) -> Option<KeyEvent> {
    let function = |n: u32| press(Key::F(n as u8));
    Some(match number {
        1 | 7 => press(Key::Home),
        2 => press(Key::Insert),
        3 => press(Key::Delete),
        4 | 8 => press(Key::End),
        5 => press(Key::PageUp),
        6 => press(Key::PageDown),
        11..=15 => function(number - 10),
        17..=21 => function(number - 11),
        23..=26 => function(number - 12),
        28 | 29 => function(number - 13),
        31..=34 => function(number - 14),
        KP_BEGIN => keypad(Key::Other(KP_BEGIN)),
        _ => return None,
    })
}

/// The key a kitty `unicode-key-code` names, and whether it is a keypad key.
///
/// Functional keys come from the protocol's table; keypad keys become their ordinary
/// equivalents; every other private-use, control or invalid code is `Key::Other(code)`;
/// the rest are the character itself.
pub(super) fn kitty_key(code: u32) -> (Key, bool) {
    let main = |key: Key| (key, false);
    let pad = |key: Key| (key, true);
    match code {
        27 => main(Key::Escape),
        13 => main(Key::Enter),
        9 => main(Key::Tab),
        8 | 127 => main(Key::Backspace),
        57358 => main(Key::CapsLock),
        57359 => main(Key::ScrollLock),
        57360 => main(Key::NumLock),
        57361 => main(Key::PrintScreen),
        57362 => main(Key::Pause),
        57363 => main(Key::Menu),
        57376..=57398 => main(Key::F((code - 57376 + 13) as u8)),
        57399..=57408 => pad(Key::Char(char::from(b'0' + (code - 57399) as u8))),
        57409 => pad(Key::Char('.')),
        57410 => pad(Key::Char('/')),
        57411 => pad(Key::Char('*')),
        57412 => pad(Key::Char('-')),
        57413 => pad(Key::Char('+')),
        57414 => pad(Key::Enter),
        57415 => pad(Key::Char('=')),
        57416 => pad(Key::Char(',')),
        57417 => pad(Key::Left),
        57418 => pad(Key::Right),
        57419 => pad(Key::Up),
        57420 => pad(Key::Down),
        57421 => pad(Key::PageUp),
        57422 => pad(Key::PageDown),
        57423 => pad(Key::Home),
        57424 => pad(Key::End),
        57425 => pad(Key::Insert),
        57426 => pad(Key::Delete),
        KP_BEGIN => pad(Key::Other(KP_BEGIN)),
        _ => main(plain_key(code)),
    }
}

/// A key code outside the functional table: a modifier key, a character, or `Other`.
fn plain_key(code: u32) -> Key {
    if let Some(modifier) = ModKey::from_kitty_code(code) {
        return Key::Modifier(modifier);
    }
    match char::from_u32(code) {
        Some(c) if !c.is_control() && !is_private_use(c) => Key::Char(c),
        _ => Key::Other(code),
    }
}

/// The Basic Multilingual Plane's private use area, where kitty puts its functional keys.
fn is_private_use(c: char) -> bool {
    ('\u{e000}'..='\u{f8ff}').contains(&c)
}

/// The single lower-case form of an upper-case letter, if it has exactly one.
pub(super) fn lower_case(c: char) -> Option<char> {
    if !c.is_uppercase() {
        return None;
    }
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(l), None) if l != c => Some(l),
        _ => None,
    }
}

/// The text a key event types when the terminal did not send any.
///
/// Only a press or repeat of a character key with none of ctrl, alt, super, hyper or meta
/// types text. With shift it is the shifted key the terminal reported, else the upper-case
/// letter or the US layout's shifted symbol; without shift it is the character, upper-cased
/// when caps lock is on. A keypad key types its own character whatever shift says.
pub(super) fn implied_text(ev: &KeyEvent) -> Option<String> {
    let Key::Char(c) = ev.key else {
        return None;
    };
    if ev.kind == KeyKind::Release || ev.mods.intersects(NON_TEXT_MODS) {
        return None;
    }
    if ev.keypad {
        return Some(c.to_string());
    }
    if ev.mods.contains(Mods::SHIFT) {
        if let Some(shifted) = ev.shifted {
            return Some(shifted.to_string());
        }
        if c.is_lowercase() {
            return Some(c.to_uppercase().collect());
        }
        return Some(us_shifted(c).unwrap_or(c).to_string());
    }
    if ev.caps_lock && c.is_lowercase() {
        return Some(c.to_uppercase().collect());
    }
    Some(c.to_string())
}
