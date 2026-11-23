//! Key encoding: the bytes a program running in the terminal expects for a key event.
//!
//! Two encodings live here, chosen by the progressive enhancement flags the child has set:
//!
//! - **Legacy** (`kitty_flags == 0`): what xterm sends. Text as UTF-8, `ctrl` as C0 control
//!   bytes, `alt` as an `ESC` prefix, `CSI`/`SS3` sequences for the functional keys. A child
//!   in this mode has not said it understands anything else, so nothing else is ever sent:
//!   a key with no legacy form (`cmd-s`, a media key) encodes to nothing.
//! - **Kitty keyboard protocol** (any flag set), following
//!   <https://sw.kovidgoyal.net/kitty/keyboard-protocol/>: `CSI code[:shifted[:base]] ;
//!   mods[:event] [; text] u`, with the legacy-compatible `CSI 1;mods A` and `CSI n;mods ~`
//!   forms for the keys that have them.
//!
//! Everything is a pure function of the event and [`KeyModes`].

use bakedit_core::{us_shifted, Key, KeyEvent, KeyKind, Mods};

/// Kitty flag 1: report `Esc`, `alt+key` and `ctrl+key` as escape codes instead of the
/// ambiguous legacy bytes.
pub const KITTY_DISAMBIGUATE: u8 = 0b0_0001;
/// Kitty flag 2: report key repeat and release.
pub const KITTY_REPORT_EVENTS: u8 = 0b0_0010;
/// Kitty flag 4: add the shifted and base-layout keys to escape codes.
pub const KITTY_REPORT_ALTERNATES: u8 = 0b0_0100;
/// Kitty flag 8: report every key, text keys and modifier keys included, as an escape code.
pub const KITTY_REPORT_ALL: u8 = 0b0_1000;
/// Kitty flag 16: embed the text a key types in its escape code (only with flag 8).
pub const KITTY_REPORT_TEXT: u8 = 0b1_0000;

const KITTY_ALL_FLAGS: u8 = 0b1_1111;

/// Kitty modifier bits beyond [`Mods`]: the lock states.
const CAPS_LOCK_BIT: u32 = 64;
const NUM_LOCK_BIT: u32 = 128;
const LOCK_BITS: u32 = CAPS_LOCK_BIT | NUM_LOCK_BIT;

/// The kitty code of the keypad's centre key (5 without NumLock), which has no [`Key`].
const KP_BEGIN: u32 = 57427;

/// The parts of the child's terminal state that change how a key is encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeyModes {
    /// The kitty keyboard protocol flags the child has pushed (0 = legacy encoding).
    pub kitty_flags: u8,
    /// Application cursor keys (DECCKM): unmodified arrows, Home and End use `SS3` instead
    /// of `CSI` in the legacy encoding.
    pub app_cursor: bool,
    /// Application keypad (DECKPAM). Tracked for completeness but it does not change the
    /// encoding: keypad keys are sent as their ordinary equivalents, as the kitty protocol
    /// specifies for its legacy mode. Honouring it would break the number pad in every
    /// shell that turns on `smkx`.
    pub app_keypad: bool,
}

impl KeyModes {
    /// Legacy encoding with normal cursor keys: what a freshly started program expects.
    pub const LEGACY: KeyModes = KeyModes {
        kitty_flags: 0,
        app_cursor: false,
        app_keypad: false,
    };

    /// The kitty keyboard protocol with the given flags.
    pub fn kitty(flags: u8) -> KeyModes {
        KeyModes {
            kitty_flags: flags,
            ..KeyModes::LEGACY
        }
    }
}

/// Encode a key event for the child.
///
/// Returns an empty vector when the event produces no bytes: a release the child did not
/// ask for, a modifier key on its own, or a key the active encoding cannot represent.
///
/// `text` on the event wins when the child takes text as plain UTF-8, which is how composed
/// characters (dead keys, `option` on a Mac) get through. Text is ignored when `ctrl`, `cmd`,
/// `hyper` or `meta` is held, and when `alt` is held but the text is just the key's own
/// character, so a hand-built `alt-a` event still encodes as a shortcut. When `text` is
/// `None` and only `shift` is held, the text is derived from the key (US layout for the
/// shifted symbols unless the event says otherwise).
pub fn encode_key(event: &KeyEvent, modes: KeyModes) -> Vec<u8> {
    let input = Input::new(event);
    let flags = modes.kitty_flags & KITTY_ALL_FLAGS;
    if flags == 0 {
        encode_legacy(&input, modes.app_cursor)
    } else {
        encode_kitty(&input, flags, modes.app_cursor)
    }
}

/// A key event with its ambiguities resolved once, up front.
struct Input {
    key: Key,
    mods: Mods,
    kind: KeyKind,
    /// The character `shift` produces on this key when that is known for sure: the host
    /// terminal reported it, or the key is a letter.
    shifted: Option<char>,
    /// `shifted`, falling back to the US layout. Used where some character has to be sent.
    shifted_guess: Option<char>,
    /// The key at the same position on a US layout, when the host terminal reported it.
    base: Option<char>,
    /// The text this event types, if it types any.
    text: Option<String>,
    keypad: bool,
    /// Kitty modifier bits for the lock states.
    locks: u32,
}

impl Input {
    fn new(event: &KeyEvent) -> Input {
        let mut key = event.key;
        let mut mods = event.mods;
        let mut reported_shifted = event.shifted;
        // `Key::Char` is meant to hold the unshifted key. An upper-case letter is taken as
        // shift plus the lower-case key, which is the only reading the protocol allows.
        if let Key::Char(c) = key {
            if let Some(lower) = lower_case_of(c) {
                key = Key::Char(lower);
                mods |= Mods::SHIFT;
                reported_shifted.get_or_insert(c);
            }
        }
        let (shifted, shifted_guess) = match key {
            Key::Char(c) => {
                let sure = reported_shifted.or_else(|| upper_case_of(c));
                (sure, sure.or_else(|| us_shifted(c)))
            }
            _ => (None, None),
        };
        let text = typed_text(event, key, mods, shifted_guess);
        let mut locks = 0;
        if event.caps_lock {
            locks |= CAPS_LOCK_BIT;
        }
        if event.num_lock {
            locks |= NUM_LOCK_BIT;
        }
        Input {
            key,
            mods,
            kind: event.kind,
            shifted,
            shifted_guess,
            base: event.base,
            text,
            keypad: event.keypad,
            locks,
        }
    }
}

/// The lower-case form of an upper-case letter, when it is a single character.
fn lower_case_of(c: char) -> Option<char> {
    if !c.is_uppercase() {
        return None;
    }
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(l), None) if l != c => Some(l),
        _ => None,
    }
}

/// The upper-case form of a lower-case letter, when it is a single character.
fn upper_case_of(c: char) -> Option<char> {
    if !c.is_lowercase() {
        return None;
    }
    let mut upper = c.to_uppercase();
    match (upper.next(), upper.next()) {
        (Some(u), None) if u != c => Some(u),
        _ => None,
    }
}

fn is_control(c: char) -> bool {
    (c as u32) < 0x20 || (0x7f..=0x9f).contains(&(c as u32))
}

/// The text a key event types, or `None` for shortcuts, functional keys and releases.
fn typed_text(
    event: &KeyEvent,
    key: Key,
    mods: Mods,
    shifted_guess: Option<char>,
) -> Option<String> {
    if event.kind == KeyKind::Release {
        return None;
    }
    if mods.intersects(Mods::CTRL | Mods::SUPER | Mods::HYPER | Mods::META) {
        return None;
    }
    let shift = mods.contains(Mods::SHIFT);
    match event.text.as_deref() {
        Some(text) => {
            if text.chars().next().map_or(true, is_control) {
                return None;
            }
            // With alt held, text only counts if alt changed what the key types.
            if mods.contains(Mods::ALT) {
                if let Key::Char(c) = key {
                    let mut chars = text.chars();
                    if let (Some(only), None) = (chars.next(), chars.next()) {
                        if only == c
                            || Some(only) == shifted_guess
                            || Some(only) == upper_case_of(c)
                        {
                            return None;
                        }
                    }
                }
            }
            Some(text.to_string())
        }
        None => {
            let Key::Char(c) = key else { return None };
            if mods.contains(Mods::ALT) || is_control(c) {
                return None;
            }
            let typed = match upper_case_of(c) {
                // A cased letter: shift and caps lock cancel each other out.
                Some(upper) if shift != event.caps_lock => upper,
                Some(_) => c,
                None if shift => shifted_guess.unwrap_or(c),
                None => c,
            };
            Some(typed.to_string())
        }
    }
}

// ---------------------------------------------------------------------------------------
// Legacy encoding
// ---------------------------------------------------------------------------------------

fn encode_legacy(input: &Input, app_cursor: bool) -> Vec<u8> {
    if input.kind == KeyKind::Release {
        return Vec::new();
    }
    if let Some(text) = &input.text {
        return text.as_bytes().to_vec();
    }
    // The legacy encoding has no way to say cmd, hyper or meta.
    if input
        .mods
        .intersects(Mods::SUPER | Mods::HYPER | Mods::META)
    {
        return Vec::new();
    }
    let mods = input.mods;
    let ctrl = mods.contains(Mods::CTRL);
    let shift = mods.contains(Mods::SHIFT);
    match input.key {
        Key::Char(c) => {
            legacy_text_key(c, input.shifted_guess, input.base, mods, false).unwrap_or_default()
        }
        Key::Enter => alt_prefixed(mods, b"\r"),
        Key::Escape => alt_prefixed(mods, b"\x1b"),
        Key::Backspace => alt_prefixed(mods, if ctrl { b"\x08" } else { b"\x7f" }),
        Key::Tab => alt_prefixed(mods, if shift { b"\x1b[Z" } else { b"\t" }),
        key => legacy_functional(key, mods, app_cursor),
    }
}

fn alt_prefixed(mods: Mods, bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 1);
    if mods.contains(Mods::ALT) {
        out.push(0x1b);
    }
    out.extend_from_slice(bytes);
    out
}

/// xterm's sequences for the functional keys: `CSI A`, `SS3 A` in application cursor mode,
/// `CSI 1;m A` with modifiers, `CSI n ~` and `CSI n;m ~` for the rest.
fn legacy_functional(key: Key, mods: Mods, app_cursor: bool) -> Vec<u8> {
    let (number, last, cursor_key): (u32, char, bool) = match key {
        Key::Up => (1, 'A', true),
        Key::Down => (1, 'B', true),
        Key::Right => (1, 'C', true),
        Key::Left => (1, 'D', true),
        Key::Home => (1, 'H', true),
        Key::End => (1, 'F', true),
        Key::Other(KP_BEGIN) => (1, 'E', true),
        Key::F(1) => (1, 'P', false),
        Key::F(2) => (1, 'Q', false),
        Key::F(3) => (1, 'R', false),
        Key::F(4) => (1, 'S', false),
        Key::Insert => (2, '~', false),
        Key::Delete => (3, '~', false),
        Key::PageUp => (5, '~', false),
        Key::PageDown => (6, '~', false),
        Key::F(n @ 5..=20) => (LEGACY_F_KEY_NUMBERS[usize::from(n) - 5], '~', false),
        Key::Menu => (29, '~', false),
        _ => return Vec::new(),
    };
    let modifier = 1 + u32::from(mods.bits() & (Mods::SHIFT | Mods::ALT | Mods::CTRL).bits());
    let text = match (last, modifier) {
        ('~', 1) => format!("\x1b[{number}~"),
        ('~', _) => format!("\x1b[{number};{modifier}~"),
        (_, 1) if cursor_key && !app_cursor => format!("\x1b[{last}"),
        // F1-F4 are always SS3; the cursor keys are in application mode.
        (_, 1) => format!("\x1bO{last}"),
        _ => format!("\x1b[1;{modifier}{last}"),
    };
    text.into_bytes()
}

/// `CSI n ~` numbers for F5 to F20.
const LEGACY_F_KEY_NUMBERS: [u32; 16] = [
    15, 17, 18, 19, 20, 21, 23, 24, 25, 26, 28, 29, 31, 32, 33, 34,
];

/// The keys the kitty specification calls "legacy text keys", shifted forms included.
fn is_legacy_ascii_key(c: char) -> bool {
    c == ' ' || c.is_ascii_lowercase() || c.is_ascii_digit() || c.is_ascii_punctuation()
}

/// The specification's "legacy ctrl mapping of ASCII keys". Keys not in the table are left
/// untouched by ctrl.
fn ctrl_mapped(c: char) -> char {
    let byte: u8 = match c {
        ' ' | '2' | '@' => 0,
        'a'..='z' => c as u8 - b'a' + 1,
        '3' | '[' => 27,
        '4' | '\\' => 28,
        '5' | ']' => 29,
        '6' | '^' | '~' => 30,
        '7' | '/' | '_' => 31,
        '8' | '?' => 127,
        _ => return c,
    };
    byte as char
}

/// A text key with modifiers in the legacy encoding: `ESC` for alt, the control byte for
/// ctrl, the shifted character for shift.
///
/// With `strict` set this is exactly the algorithm in the kitty specification, which only
/// covers shift, alt, ctrl, shift+alt and ctrl+alt (plus ctrl+shift+space); any other
/// combination is `None` and the caller sends a `CSI u` code. Without it the remaining
/// shift/alt/ctrl combinations follow xterm (ctrl+shift+letter is the control byte), for
/// children that have not asked for the kitty protocol at all.
fn legacy_text_key(
    key: char,
    shifted: Option<char>,
    base: Option<char>,
    mods: Mods,
    strict: bool,
) -> Option<Vec<u8>> {
    let relevant = Mods::SHIFT | Mods::ALT | Mods::CTRL;
    if mods.difference(relevant) != Mods::empty() {
        return None;
    }
    let mut key = key;
    let mut rest = mods;
    if !is_legacy_ascii_key(key) && !shifted.is_some_and(is_legacy_ascii_key) {
        // A key from another alphabet: with ctrl or alt, use the key at the same position
        // on a US layout, so ctrl+с on a Cyrillic layout is still ctrl+c.
        let plain_combo =
            rest == Mods::CTRL || rest == Mods::ALT || rest == (Mods::CTRL | Mods::ALT);
        match base {
            Some(base) if plain_combo && is_legacy_ascii_key(base) => key = base,
            _ if strict => return None,
            _ => {}
        }
    }
    if rest.contains(Mods::SHIFT) {
        if let Some(shifted) = shifted {
            // Shift is spent on producing the shifted character, except for ctrl+letter,
            // where the control byte is the same either way.
            if shifted != key && !(rest.contains(Mods::CTRL) && key.is_ascii_lowercase()) {
                key = shifted;
                rest.remove(Mods::SHIFT);
            }
        }
    }
    let ctrl = rest.contains(Mods::CTRL);
    let alt = rest.contains(Mods::ALT);
    if strict {
        let shift_left = rest.contains(Mods::SHIFT);
        let allowed = if mods == Mods::SHIFT {
            true
        } else if shift_left {
            // Only the space bar keeps its meaning with a leftover shift.
            key == ' ' && (ctrl != alt)
        } else {
            ctrl || alt
        };
        if !allowed {
            return None;
        }
    }
    let mut out = Vec::with_capacity(5);
    if alt {
        out.push(0x1b);
    }
    let typed = if ctrl { ctrl_mapped(key) } else { key };
    let mut buf = [0u8; 4];
    out.extend_from_slice(typed.encode_utf8(&mut buf).as_bytes());
    Some(out)
}

// ---------------------------------------------------------------------------------------
// Kitty keyboard protocol
// ---------------------------------------------------------------------------------------

/// The flags, decoded.
#[derive(Clone, Copy)]
struct Kitty {
    disambiguate: bool,
    report_events: bool,
    report_alternates: bool,
    report_all: bool,
    report_text: bool,
}

impl Kitty {
    fn new(flags: u8) -> Kitty {
        Kitty {
            disambiguate: flags & KITTY_DISAMBIGUATE != 0,
            report_events: flags & KITTY_REPORT_EVENTS != 0,
            report_alternates: flags & KITTY_REPORT_ALTERNATES != 0,
            report_all: flags & KITTY_REPORT_ALL != 0,
            report_text: flags & KITTY_REPORT_TEXT != 0,
        }
    }

    /// No flag that changes which keys become escape codes: functional keys keep their
    /// legacy bytes.
    fn legacy_functional_keys(self) -> bool {
        !self.disambiguate && !self.report_events && !self.report_all
    }
}

fn encode_kitty(input: &Input, flags: u8, app_cursor: bool) -> Vec<u8> {
    let kitty = Kitty::new(flags);
    let lock_or_modifier = matches!(
        input.key,
        Key::Modifier(_) | Key::CapsLock | Key::ScrollLock | Key::NumLock
    );
    if lock_or_modifier && !kitty.report_all {
        return Vec::new();
    }
    // Keys that type text send the text, unless the child asked for every key as a code.
    if !kitty.report_all {
        if let Some(text) = &input.text {
            return text.as_bytes().to_vec();
        }
    }
    if input.kind == KeyKind::Release && !kitty.report_events {
        return Vec::new();
    }
    let mods = u32::from(input.mods.bits()) | input.locks;
    let event = match input.kind {
        KeyKind::Repeat if kitty.report_events => Some(2),
        KeyKind::Release if kitty.report_events => Some(3),
        _ => None,
    };
    let text = if kitty.report_text {
        input.text.as_deref()
    } else {
        None
    };
    // Keypad keys are keys of their own once the child can tell them apart.
    let separate_keypad = input.keypad && (kitty.disambiguate || kitty.report_all);
    match input.key {
        Key::Char(c) if !separate_keypad || keypad_code(input.key).is_none() => {
            encode_kitty_text_key(c, input, kitty, mods, event, text)
        }
        _ => encode_kitty_functional(input, kitty, app_cursor, separate_keypad, mods, event, text),
    }
}

fn encode_kitty_text_key(
    c: char,
    input: &Input,
    kitty: Kitty,
    mods: u32,
    event: Option<u8>,
    text: Option<&str>,
) -> Vec<u8> {
    let needs_code = event.is_some() || text.is_some();
    if !needs_code {
        if mods == 0 {
            if !kitty.report_all {
                let mut buf = [0u8; 4];
                return c.encode_utf8(&mut buf).as_bytes().to_vec();
            }
        } else if !kitty.disambiguate && !kitty.report_all {
            let legacy_mods = Mods::from_bits_truncate((mods & !LOCK_BITS) as u8);
            // Lock states have no legacy encoding, so with one active this is a CSI u code.
            if mods & LOCK_BITS == 0 {
                if let Some(bytes) =
                    legacy_text_key(c, input.shifted_guess, input.base, legacy_mods, true)
                {
                    return bytes;
                }
            }
        }
    }
    let shifted = if input.mods.contains(Mods::SHIFT) {
        input.shifted.filter(|s| *s != c)
    } else {
        None
    };
    let base = input.base.filter(|b| *b != c);
    let alternates = if kitty.report_alternates && (shifted.is_some() || base.is_some()) {
        Some((shifted.map(u32::from), base.map(u32::from)))
    } else {
        None
    };
    csi(u32::from(c), alternates, mods, event, text, 'u')
}

fn encode_kitty_functional(
    input: &Input,
    kitty: Kitty,
    app_cursor: bool,
    separate_keypad: bool,
    mods: u32,
    event: Option<u8>,
    text: Option<&str>,
) -> Vec<u8> {
    let key = input.key;
    let release = input.kind == KeyKind::Release;
    let legacy_keys = kitty.legacy_functional_keys();
    // A keypad key reported as its own key has none of the legacy special cases below.
    let plain_key = !(separate_keypad && keypad_code(key).is_some());

    if plain_key {
        if mods == 0 {
            if app_cursor && legacy_keys {
                let last = match key {
                    Key::Up => Some('A'),
                    Key::Down => Some('B'),
                    Key::Right => Some('C'),
                    Key::Left => Some('D'),
                    Key::Other(KP_BEGIN) => Some('E'),
                    Key::End => Some('F'),
                    Key::Home => Some('H'),
                    _ => None,
                };
                if let Some(last) = last {
                    return format!("\x1bO{last}").into_bytes();
                }
            }
            if key == Key::Escape && !kitty.disambiguate && !kitty.report_all && !release {
                return vec![0x1b];
            }
            if legacy_keys {
                if let Key::F(n @ 1..=4) = key {
                    return vec![0x1b, b'O', b'P' + n - 1];
                }
            }
        } else if legacy_keys {
            let legacy_mods = Mods::from_bits_truncate((mods & !LOCK_BITS) as u8);
            let ctrl = legacy_mods.contains(Mods::CTRL);
            let shift = legacy_mods.contains(Mods::SHIFT);
            match key {
                Key::Enter => return alt_prefixed(legacy_mods, b"\r"),
                Key::Escape => return alt_prefixed(legacy_mods, b"\x1b"),
                Key::Backspace => {
                    return alt_prefixed(legacy_mods, if ctrl { b"\x08" } else { b"\x7f" })
                }
                Key::Tab => {
                    return alt_prefixed(legacy_mods, if shift { b"\x1b[Z" } else { b"\t" })
                }
                _ => {}
            }
        }
        // Enter, Tab and Backspace keep their legacy bytes, and have no release events, so
        // that a shell stays usable after a program leaves the protocol switched on.
        if mods & !LOCK_BITS == 0 && !kitty.report_all {
            let bytes: Option<&[u8]> = match key {
                Key::Enter => Some(b"\r"),
                Key::Backspace => Some(b"\x7f"),
                Key::Tab => Some(b"\t"),
                _ => None,
            };
            if let Some(bytes) = bytes {
                return if release { Vec::new() } else { bytes.to_vec() };
            }
        }
    }

    let Some((number, last)) = functional_code(key, separate_keypad, legacy_keys, text.is_some())
    else {
        return Vec::new();
    };
    csi(number, None, mods, event, text, last)
}

/// The number and final character of a functional key's escape code.
fn functional_code(
    key: Key,
    separate_keypad: bool,
    legacy_keys: bool,
    has_text: bool,
) -> Option<(u32, char)> {
    if separate_keypad {
        if let Some(code) = keypad_code(key) {
            return Some((code, 'u'));
        }
    }
    Some(match key {
        Key::Char(_) => return None,
        Key::Escape => (27, 'u'),
        Key::Enter => (13, 'u'),
        Key::Tab => (9, 'u'),
        Key::Backspace => (127, 'u'),
        Key::Insert => (2, '~'),
        Key::Delete => (3, '~'),
        Key::Left => (1, 'D'),
        Key::Right => (1, 'C'),
        Key::Up => (1, 'A'),
        Key::Down => (1, 'B'),
        Key::PageUp => (5, '~'),
        Key::PageDown => (6, '~'),
        Key::Home => (1, 'H'),
        Key::End => (1, 'F'),
        Key::F(1) => (1, 'P'),
        Key::F(2) => (1, 'Q'),
        Key::F(3) => (13, '~'),
        Key::F(4) => (1, 'S'),
        Key::F(n @ 5..=12) => (LEGACY_F_KEY_NUMBERS[usize::from(n) - 5], '~'),
        Key::F(n @ 13..=35) => (57376 + u32::from(n) - 13, 'u'),
        Key::F(_) => return None,
        Key::CapsLock => (57358, 'u'),
        Key::ScrollLock => (57359, 'u'),
        Key::NumLock => (57360, 'u'),
        Key::PrintScreen => (57361, 'u'),
        Key::Pause => (57362, 'u'),
        // xterm's code for this key, kept while functional keys are still legacy.
        Key::Menu if legacy_keys => (29, '~'),
        Key::Menu => (57363, 'u'),
        Key::Modifier(modifier) => (modifier.kitty_code(), 'u'),
        Key::Other(KP_BEGIN) => (1, 'E'),
        // Key number 0 is "text with no key behind it": only worth sending with its text.
        Key::Other(0) if !has_text => return None,
        Key::Other(code) => (code, 'u'),
    })
}

/// The dedicated kitty code of a key on the keypad.
fn keypad_code(key: Key) -> Option<u32> {
    Some(match key {
        Key::Char(c @ '0'..='9') => 57399 + (c as u32 - '0' as u32),
        Key::Char('.') => 57409,
        Key::Char('/') => 57410,
        Key::Char('*') => 57411,
        Key::Char('-') => 57412,
        Key::Char('+') => 57413,
        Key::Enter => 57414,
        Key::Char('=') => 57415,
        Key::Char(',') => 57416,
        Key::Left => 57417,
        Key::Right => 57418,
        Key::Up => 57419,
        Key::Down => 57420,
        Key::PageUp => 57421,
        Key::PageDown => 57422,
        Key::Home => 57423,
        Key::End => 57424,
        Key::Insert => 57425,
        Key::Delete => 57426,
        _ => return None,
    })
}

/// Write `CSI number[:shifted[:base]] [; mods[:event]] [; text] last`, leaving out every
/// field the specification allows to be left out.
fn csi(
    number: u32,
    alternates: Option<(Option<u32>, Option<u32>)>,
    mods: u32,
    event: Option<u8>,
    text: Option<&str>,
    last: char,
) -> Vec<u8> {
    use std::fmt::Write;

    let text: Vec<u32> = text
        .unwrap_or("")
        .chars()
        .filter(|c| !is_control(*c))
        .map(u32::from)
        .collect();
    let has_second = mods != 0 || event.is_some();
    let has_third = !text.is_empty();
    let mut out = String::from("\x1b[");
    if number != 1 || alternates.is_some() || has_second || has_third {
        let _ = write!(out, "{number}");
    }
    if let Some((shifted, base)) = alternates {
        out.push(':');
        if let Some(shifted) = shifted {
            let _ = write!(out, "{shifted}");
        }
        if let Some(base) = base {
            let _ = write!(out, ":{base}");
        }
    }
    if has_second || has_third {
        out.push(';');
        if has_second {
            let _ = write!(out, "{}", mods + 1);
        }
        if let Some(event) = event {
            let _ = write!(out, ":{event}");
        }
    }
    for (i, codepoint) in text.iter().enumerate() {
        out.push(if i == 0 { ';' } else { ':' });
        let _ = write!(out, "{codepoint}");
    }
    out.push(last);
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bakedit_core::ModKey;

    const NONE: Mods = Mods::empty();
    const SHIFT: Mods = Mods::SHIFT;
    const ALT: Mods = Mods::ALT;
    const CTRL: Mods = Mods::CTRL;
    const CMD: Mods = Mods::SUPER;

    fn key(key: Key, mods: Mods) -> KeyEvent {
        KeyEvent::new(key, mods)
    }

    fn ch(c: char, mods: Mods) -> KeyEvent {
        KeyEvent::new(Key::Char(c), mods)
    }

    fn enc(event: &KeyEvent, flags: u8) -> String {
        String::from_utf8(encode_key(event, KeyModes::kitty(flags))).unwrap()
    }

    fn legacy(event: &KeyEvent) -> String {
        enc(event, 0)
    }

    fn repeat(event: KeyEvent) -> KeyEvent {
        event.with_kind(KeyKind::Repeat)
    }

    fn release(event: KeyEvent) -> KeyEvent {
        event.with_kind(KeyKind::Release)
    }

    // ---- legacy: text ------------------------------------------------------------------

    #[test]
    fn legacy_plain_text_is_utf8() {
        assert_eq!(legacy(&KeyEvent::typed('a')), "a");
        assert_eq!(legacy(&KeyEvent::typed('A')), "A");
        assert_eq!(legacy(&KeyEvent::typed('{')), "{");
        assert_eq!(legacy(&KeyEvent::typed('é')), "é");
        assert_eq!(legacy(&KeyEvent::typed('日')), "日");
        assert_eq!(legacy(&KeyEvent::typed(' ')), " ");
    }

    #[test]
    fn legacy_text_is_derived_when_the_event_has_none() {
        assert_eq!(legacy(&ch('a', NONE)), "a");
        assert_eq!(legacy(&ch('a', SHIFT)), "A");
        assert_eq!(legacy(&ch('4', SHIFT)), "$");
        assert_eq!(legacy(&ch(' ', SHIFT)), " ");
        // The host's report of the shifted key beats the US table.
        assert_eq!(legacy(&ch('2', SHIFT).with_shifted('"')), "\"");
        // An upper-case key is shift plus the lower-case key.
        assert_eq!(legacy(&ch('G', NONE)), "G");
    }

    #[test]
    fn caps_lock_changes_derived_letters_only() {
        let mut event = ch('a', NONE);
        event.caps_lock = true;
        assert_eq!(legacy(&event), "A");
        let mut event = ch('a', SHIFT);
        event.caps_lock = true;
        assert_eq!(legacy(&event), "a");
        let mut event = ch('1', NONE);
        event.caps_lock = true;
        assert_eq!(legacy(&event), "1");
    }

    #[test]
    fn composed_text_wins_over_the_key() {
        // option-a on a Mac layout that composes: the host reports the text it produced.
        let event = ch('a', ALT).with_text("å");
        assert_eq!(legacy(&event), "å");
        assert_eq!(enc(&event, 0b111), "å");
        // A dead-key sequence with no key of its own.
        let event = key(Key::Other(0), NONE).with_text("ñ");
        assert_eq!(legacy(&event), "ñ");
    }

    #[test]
    fn text_is_ignored_when_it_cannot_be_what_the_key_typed() {
        // A typed 'c' that had modifiers bolted on afterwards is still the shortcut.
        let mut event = KeyEvent::typed('c');
        event.mods = CTRL;
        assert_eq!(legacy(&event), "\x03");
        let mut event = KeyEvent::typed('a');
        event.mods = ALT;
        assert_eq!(legacy(&event), "\x1ba");
        let mut event = KeyEvent::typed('A');
        event.mods |= ALT;
        assert_eq!(legacy(&event), "\x1bA");
        // Control characters are never text.
        assert_eq!(legacy(&key(Key::Enter, NONE).with_text("\r")), "\r");
        assert_eq!(legacy(&key(Key::Tab, NONE).with_text("\t")), "\t");
    }

    // ---- legacy: the specification's examples -------------------------------------------

    #[test]
    fn legacy_text_keys_match_the_specification_examples() {
        // "Example encodings" table: i, 3 and ;
        let cases: &[(char, Option<char>, Mods, &str)] = &[
            ('i', None, NONE, "i"),
            ('i', None, SHIFT, "I"),
            ('i', None, ALT, "\x1bi"),
            ('i', None, CTRL, "\t"),
            ('i', None, SHIFT | ALT, "\x1bI"),
            ('i', None, ALT | CTRL, "\x1b\t"),
            ('3', Some('#'), NONE, "3"),
            ('3', Some('#'), SHIFT, "#"),
            ('3', Some('#'), ALT, "\x1b3"),
            ('3', Some('#'), CTRL, "\x1b"),
            ('3', Some('#'), SHIFT | ALT, "\x1b#"),
            ('3', Some('#'), ALT | CTRL, "\x1b\x1b"),
            (';', Some(':'), NONE, ";"),
            (';', Some(':'), SHIFT, ":"),
            (';', Some(':'), ALT, "\x1b;"),
            (';', Some(':'), CTRL, ";"),
            (';', Some(':'), SHIFT | ALT, "\x1b:"),
            (';', Some(':'), ALT | CTRL, "\x1b;"),
        ];
        for &(c, shifted, mods, want) in cases {
            let mut event = ch(c, mods);
            event.shifted = shifted;
            assert_eq!(legacy(&event), want, "{c:?} with {mods:?}");
        }
    }

    #[test]
    fn legacy_ctrl_mapping_matches_the_specification_table() {
        let table: &[(char, u8)] = &[
            (' ', 0),
            ('/', 31),
            ('0', 48),
            ('1', 49),
            ('2', 0),
            ('3', 27),
            ('4', 28),
            ('5', 29),
            ('6', 30),
            ('7', 31),
            ('8', 127),
            ('9', 57),
            ('[', 27),
            ('\\', 28),
            (']', 29),
            ('a', 1),
            ('c', 3),
            ('d', 4),
            ('h', 8),
            ('i', 9),
            ('j', 10),
            ('m', 13),
            ('u', 21),
            ('z', 26),
            // Not in the table: untouched.
            ('`', b'`'),
            ('-', b'-'),
            ('=', b'='),
            (';', b';'),
            ('\'', b'\''),
            (',', b','),
            ('.', b'.'),
        ];
        for &(c, byte) in table {
            assert_eq!(
                encode_key(&ch(c, CTRL), KeyModes::LEGACY),
                vec![byte],
                "ctrl+{c:?}"
            );
            assert_eq!(
                encode_key(&ch(c, CTRL | ALT), KeyModes::LEGACY),
                vec![0x1b, byte],
                "ctrl+alt+{c:?}"
            );
        }
        // The shifted keys of the table are reached through shift.
        let shifted: &[(char, u8)] = &[('2', 0), ('6', 30), ('-', 31), ('/', 127), ('`', 30)];
        for &(c, byte) in shifted {
            assert_eq!(
                encode_key(&ch(c, CTRL | SHIFT), KeyModes::LEGACY),
                vec![byte],
                "ctrl+shift+{c:?}"
            );
        }
    }

    #[test]
    fn legacy_space_bar() {
        assert_eq!(legacy(&ch(' ', NONE)), " ");
        assert_eq!(legacy(&ch(' ', CTRL)), "\0");
        assert_eq!(legacy(&ch(' ', ALT)), "\x1b ");
        assert_eq!(legacy(&ch(' ', SHIFT)), " ");
        assert_eq!(legacy(&ch(' ', CTRL | ALT)), "\x1b\0");
        assert_eq!(legacy(&ch(' ', CTRL | SHIFT)), "\0");
        assert_eq!(legacy(&ch(' ', ALT | SHIFT)), "\x1b ");
    }

    #[test]
    fn legacy_ctrl_shift_letter_is_the_control_byte() {
        // xterm's convention; the child has not said it understands CSI u.
        assert_eq!(legacy(&ch('i', CTRL | SHIFT)), "\t");
        assert_eq!(legacy(&ch('a', CTRL | SHIFT)), "\x01");
        assert_eq!(legacy(&ch('a', CTRL | ALT | SHIFT)), "\x1b\x01");
        assert_eq!(legacy(&ch('C', CTRL)), "\x03");
    }

    #[test]
    fn legacy_alt_with_symbols_and_other_alphabets() {
        for c in "~!@#$%^&*()_+{}|:\"<>?".chars() {
            assert_eq!(legacy(&ch(c, ALT)), format!("\x1b{c}"));
        }
        assert_eq!(legacy(&ch('é', ALT)), "\x1bé");
        // ctrl leaves a key it has no mapping for untouched.
        assert_eq!(legacy(&ch('é', CTRL)), "é");
        // The base layout key makes ctrl work on a non-Latin layout.
        let mut event = ch('с', CTRL);
        event.base = Some('c');
        assert_eq!(legacy(&event), "\x03");
        let mut event = ch('с', ALT);
        event.base = Some('c');
        assert_eq!(legacy(&event), "\x1bc");
    }

    #[test]
    fn legacy_cannot_say_cmd_hyper_or_meta() {
        assert_eq!(legacy(&ch('s', CMD)), "");
        assert_eq!(legacy(&ch('s', CMD | SHIFT)), "");
        assert_eq!(legacy(&ch('a', Mods::HYPER)), "");
        assert_eq!(legacy(&ch('a', Mods::META | CTRL)), "");
        assert_eq!(legacy(&key(Key::Up, CMD)), "");
        assert_eq!(legacy(&key(Key::Enter, CMD)), "");
        assert_eq!(legacy(&key(Key::F(5), Mods::META)), "");
    }

    // ---- legacy: functional keys --------------------------------------------------------

    #[test]
    fn legacy_c0_keys_match_the_specification_table() {
        // Columns: none, ctrl, alt, shift, ctrl+shift, alt+shift, ctrl+alt
        let mods = [
            NONE,
            CTRL,
            ALT,
            SHIFT,
            CTRL | SHIFT,
            ALT | SHIFT,
            CTRL | ALT,
        ];
        let table: &[(Key, [&str; 7])] = &[
            (
                Key::Enter,
                ["\r", "\r", "\x1b\r", "\r", "\r", "\x1b\r", "\x1b\r"],
            ),
            (
                Key::Escape,
                [
                    "\x1b", "\x1b", "\x1b\x1b", "\x1b", "\x1b", "\x1b\x1b", "\x1b\x1b",
                ],
            ),
            (
                Key::Backspace,
                [
                    "\x7f", "\x08", "\x1b\x7f", "\x7f", "\x08", "\x1b\x7f", "\x1b\x08",
                ],
            ),
            (
                Key::Tab,
                [
                    "\t",
                    "\t",
                    "\x1b\t",
                    "\x1b[Z",
                    "\x1b[Z",
                    "\x1b\x1b[Z",
                    "\x1b\t",
                ],
            ),
        ];
        for (k, wants) in table {
            for (m, want) in mods.iter().zip(wants) {
                assert_eq!(legacy(&key(*k, *m)), *want, "{k:?} with {m:?}");
            }
        }
    }

    #[test]
    fn legacy_functional_keys_match_the_specification_table() {
        let table: &[(Key, &str)] = &[
            (Key::Insert, "\x1b[2~"),
            (Key::Delete, "\x1b[3~"),
            (Key::PageUp, "\x1b[5~"),
            (Key::PageDown, "\x1b[6~"),
            (Key::Up, "\x1b[A"),
            (Key::Down, "\x1b[B"),
            (Key::Right, "\x1b[C"),
            (Key::Left, "\x1b[D"),
            (Key::Home, "\x1b[H"),
            (Key::End, "\x1b[F"),
            (Key::F(1), "\x1bOP"),
            (Key::F(2), "\x1bOQ"),
            (Key::F(3), "\x1bOR"),
            (Key::F(4), "\x1bOS"),
            (Key::F(5), "\x1b[15~"),
            (Key::F(6), "\x1b[17~"),
            (Key::F(7), "\x1b[18~"),
            (Key::F(8), "\x1b[19~"),
            (Key::F(9), "\x1b[20~"),
            (Key::F(10), "\x1b[21~"),
            (Key::F(11), "\x1b[23~"),
            (Key::F(12), "\x1b[24~"),
            (Key::Menu, "\x1b[29~"),
            // xterm's codes for the keys past F12.
            (Key::F(13), "\x1b[25~"),
            (Key::F(20), "\x1b[34~"),
        ];
        for (k, want) in table {
            assert_eq!(legacy(&key(*k, NONE)), *want, "{k:?}");
        }
    }

    #[test]
    fn legacy_application_cursor_mode_uses_ss3_only_without_modifiers() {
        let app = KeyModes {
            app_cursor: true,
            ..KeyModes::LEGACY
        };
        let cases: &[(Key, &str)] = &[
            (Key::Up, "\x1bOA"),
            (Key::Down, "\x1bOB"),
            (Key::Right, "\x1bOC"),
            (Key::Left, "\x1bOD"),
            (Key::Home, "\x1bOH"),
            (Key::End, "\x1bOF"),
            (Key::Other(KP_BEGIN), "\x1bOE"),
            (Key::PageUp, "\x1b[5~"),
            (Key::F(5), "\x1b[15~"),
        ];
        for (k, want) in cases {
            assert_eq!(encode_key(&key(*k, NONE), app), want.as_bytes(), "{k:?}");
        }
        assert_eq!(encode_key(&key(Key::Up, SHIFT), app), b"\x1b[1;2A");
        assert_eq!(encode_key(&key(Key::Home, CTRL), app), b"\x1b[1;5H");
    }

    #[test]
    fn legacy_modified_functional_keys_use_the_xterm_parameter() {
        let cases: &[(Key, Mods, &str)] = &[
            (Key::Up, SHIFT, "\x1b[1;2A"),
            (Key::Up, ALT, "\x1b[1;3A"),
            (Key::Up, ALT | SHIFT, "\x1b[1;4A"),
            (Key::Left, CTRL, "\x1b[1;5D"),
            (Key::End, CTRL | SHIFT, "\x1b[1;6F"),
            (Key::Right, CTRL | ALT, "\x1b[1;7C"),
            (Key::Down, CTRL | ALT | SHIFT, "\x1b[1;8B"),
            (Key::Delete, ALT, "\x1b[3;3~"),
            (Key::Insert, SHIFT, "\x1b[2;2~"),
            (Key::PageUp, CTRL, "\x1b[5;5~"),
            (Key::F(1), SHIFT, "\x1b[1;2P"),
            // xterm-256color's terminfo: kf15 is \E[1;2R.
            (Key::F(3), SHIFT, "\x1b[1;2R"),
            (Key::F(4), CTRL, "\x1b[1;5S"),
            (Key::F(5), SHIFT, "\x1b[15;2~"),
            (Key::F(12), CTRL | SHIFT, "\x1b[24;6~"),
        ];
        for (k, m, want) in cases {
            assert_eq!(legacy(&key(*k, *m)), *want, "{k:?} with {m:?}");
        }
    }

    #[test]
    fn legacy_sends_nothing_for_keys_without_a_legacy_form() {
        for k in [
            Key::CapsLock,
            Key::NumLock,
            Key::ScrollLock,
            Key::PrintScreen,
            Key::Pause,
            Key::Modifier(ModKey::LeftShift),
            Key::Modifier(ModKey::RightCtrl),
            Key::Other(57428),
            Key::F(21),
            Key::F(0),
        ] {
            assert_eq!(legacy(&key(k, NONE)), "", "{k:?}");
        }
    }

    #[test]
    fn legacy_has_no_release_events_and_repeat_is_a_press() {
        assert_eq!(legacy(&release(KeyEvent::typed('a'))), "");
        assert_eq!(legacy(&release(key(Key::Enter, NONE))), "");
        assert_eq!(legacy(&release(key(Key::Up, NONE))), "");
        assert_eq!(legacy(&repeat(KeyEvent::typed('a'))), "a");
        assert_eq!(legacy(&repeat(key(Key::Up, NONE))), "\x1b[A");
    }

    #[test]
    fn keypad_keys_are_their_ordinary_equivalents_in_legacy_mode() {
        let keypad = |k: Key| {
            let mut event = key(k, NONE);
            event.keypad = true;
            event
        };
        let app = KeyModes {
            app_keypad: true,
            app_cursor: false,
            kitty_flags: 0,
        };
        for modes in [KeyModes::LEGACY, app] {
            assert_eq!(encode_key(&keypad(Key::Char('5')), modes), b"5");
            assert_eq!(encode_key(&keypad(Key::Char('+')), modes), b"+");
            assert_eq!(encode_key(&keypad(Key::Enter), modes), b"\r");
            assert_eq!(encode_key(&keypad(Key::Home), modes), b"\x1b[H");
            assert_eq!(encode_key(&keypad(Key::Delete), modes), b"\x1b[3~");
        }
    }

    // ---- kitty: flag 1, disambiguate ----------------------------------------------------

    #[test]
    fn disambiguate_keeps_text_and_the_three_c0_keys() {
        assert_eq!(enc(&KeyEvent::typed('a'), 1), "a");
        assert_eq!(enc(&KeyEvent::typed('A'), 1), "A");
        assert_eq!(enc(&ch('a', NONE), 1), "a");
        assert_eq!(enc(&key(Key::Enter, NONE), 1), "\r");
        assert_eq!(enc(&key(Key::Tab, NONE), 1), "\t");
        assert_eq!(enc(&key(Key::Backspace, NONE), 1), "\x7f");
    }

    #[test]
    fn disambiguate_turns_ambiguous_keys_into_csi_u() {
        assert_eq!(enc(&key(Key::Escape, NONE), 1), "\x1b[27u");
        assert_eq!(enc(&key(Key::Enter, SHIFT), 1), "\x1b[13;2u");
        assert_eq!(enc(&key(Key::Tab, SHIFT), 1), "\x1b[9;2u");
        assert_eq!(enc(&key(Key::Tab, CTRL | SHIFT), 1), "\x1b[9;6u");
        assert_eq!(enc(&key(Key::Backspace, CTRL), 1), "\x1b[127;5u");
        assert_eq!(enc(&ch('a', CTRL), 1), "\x1b[97;5u");
        assert_eq!(enc(&ch('a', ALT), 1), "\x1b[97;3u");
        assert_eq!(enc(&ch('a', CTRL | SHIFT), 1), "\x1b[97;6u");
        assert_eq!(enc(&ch('a', ALT | SHIFT), 1), "\x1b[97;4u");
        assert_eq!(enc(&ch(' ', CTRL), 1), "\x1b[32;5u");
        assert_eq!(enc(&ch('c', CTRL), 1), "\x1b[99;5u");
    }

    #[test]
    fn the_key_code_is_always_the_unshifted_key() {
        // "It must not be CSI 65; modifiers u."
        assert_eq!(enc(&ch('A', CTRL), 1), "\x1b[97;6u");
        assert_eq!(enc(&ch('a', CTRL | SHIFT), 1), "\x1b[97;6u");
        let mut event = KeyEvent::typed('A');
        event.mods |= CTRL;
        assert_eq!(enc(&event, 1), "\x1b[97;6u");
    }

    #[test]
    fn disambiguate_keeps_legacy_forms_for_functional_keys() {
        assert_eq!(enc(&key(Key::Up, NONE), 1), "\x1b[A");
        assert_eq!(enc(&key(Key::Up, CTRL), 1), "\x1b[1;5A");
        assert_eq!(enc(&key(Key::Home, NONE), 1), "\x1b[H");
        assert_eq!(enc(&key(Key::End, SHIFT), 1), "\x1b[1;2F");
        assert_eq!(enc(&key(Key::Delete, NONE), 1), "\x1b[3~");
        assert_eq!(enc(&key(Key::PageDown, ALT), 1), "\x1b[6;3~");
        assert_eq!(enc(&key(Key::F(1), NONE), 1), "\x1b[P");
        assert_eq!(enc(&key(Key::F(2), SHIFT), 1), "\x1b[1;2Q");
        assert_eq!(enc(&key(Key::F(3), NONE), 1), "\x1b[13~");
        assert_eq!(enc(&key(Key::F(3), SHIFT), 1), "\x1b[13;2~");
        assert_eq!(enc(&key(Key::F(4), NONE), 1), "\x1b[S");
        assert_eq!(enc(&key(Key::F(5), NONE), 1), "\x1b[15~");
        assert_eq!(enc(&key(Key::F(12), CTRL), 1), "\x1b[24;5~");
        assert_eq!(enc(&key(Key::F(13), NONE), 1), "\x1b[57376u");
        assert_eq!(enc(&key(Key::F(35), NONE), 1), "\x1b[57398u");
        assert_eq!(enc(&key(Key::Menu, NONE), 1), "\x1b[57363u");
        assert_eq!(enc(&key(Key::PrintScreen, NONE), 1), "\x1b[57361u");
        assert_eq!(enc(&key(Key::Pause, CTRL), 1), "\x1b[57362;5u");
        assert_eq!(enc(&key(Key::Other(57428), NONE), 1), "\x1b[57428u");
        assert_eq!(enc(&key(Key::Other(KP_BEGIN), NONE), 1), "\x1b[E");
    }

    #[test]
    fn application_cursor_mode_does_not_apply_once_keys_are_disambiguated() {
        let modes = KeyModes {
            kitty_flags: 1,
            app_cursor: true,
            app_keypad: false,
        };
        assert_eq!(encode_key(&key(Key::Up, NONE), modes), b"\x1b[A");
        // Flags that leave functional keys alone keep the legacy behaviour.
        let modes = KeyModes {
            kitty_flags: KITTY_REPORT_ALTERNATES,
            app_cursor: true,
            app_keypad: false,
        };
        assert_eq!(encode_key(&key(Key::Up, NONE), modes), b"\x1bOA");
        assert_eq!(encode_key(&key(Key::F(1), NONE), modes), b"\x1bOP");
        assert_eq!(encode_key(&key(Key::Menu, NONE), modes), b"\x1b[29~");
    }

    #[test]
    fn super_hyper_and_meta_are_reported() {
        assert_eq!(enc(&ch('s', CMD), 1), "\x1b[115;9u");
        assert_eq!(enc(&ch('s', CMD | SHIFT), 1), "\x1b[115;10u");
        assert_eq!(enc(&ch('a', Mods::HYPER), 1), "\x1b[97;17u");
        assert_eq!(enc(&ch('a', Mods::META), 1), "\x1b[97;33u");
        assert_eq!(
            enc(
                &ch('a', CTRL | ALT | SHIFT | CMD | Mods::HYPER | Mods::META),
                1
            ),
            "\x1b[97;64u"
        );
        assert_eq!(enc(&key(Key::Up, CMD), 1), "\x1b[1;9A");
        assert_eq!(enc(&key(Key::Enter, CMD), 1), "\x1b[13;9u");
    }

    #[test]
    fn lock_states_are_modifier_bits_on_escape_codes_only() {
        let mut event = ch('a', CTRL);
        event.caps_lock = true;
        assert_eq!(enc(&event, 1), "\x1b[97;69u");
        event.num_lock = true;
        assert_eq!(enc(&event, 1), "\x1b[97;197u");
        // Text is still text, and the three C0 keys still their bytes.
        let mut event = KeyEvent::typed('a');
        event.num_lock = true;
        assert_eq!(enc(&event, 1), "a");
        let mut event = key(Key::Enter, NONE);
        event.num_lock = true;
        event.caps_lock = true;
        assert_eq!(enc(&event, 0b11), "\r");
        assert_eq!(enc(&release(event), 0b11), "");
        // Legacy mode has no encoding for them.
        let mut event = ch(' ', CTRL);
        event.num_lock = true;
        event.caps_lock = true;
        assert_eq!(legacy(&event), "\0");
        let mut event = key(Key::Up, NONE);
        event.num_lock = true;
        assert_eq!(legacy(&event), "\x1b[A");
        assert_eq!(enc(&event, 1), "\x1b[1;129A");
    }

    #[test]
    fn keypad_keys_get_their_own_codes_when_disambiguated() {
        let keypad = |k: Key, mods: Mods| {
            let mut event = key(k, mods);
            event.keypad = true;
            event
        };
        assert_eq!(enc(&keypad(Key::PageUp, NONE), 1), "\x1b[57421u");
        assert_eq!(enc(&keypad(Key::PageUp, CTRL), 1), "\x1b[57421;5u");
        assert_eq!(enc(&keypad(Key::Enter, NONE), 1), "\x1b[57414u");
        assert_eq!(enc(&keypad(Key::Char('0'), CTRL), 1), "\x1b[57399;5u");
        assert_eq!(enc(&keypad(Key::Char('9'), ALT), 1), "\x1b[57408;3u");
        assert_eq!(enc(&keypad(Key::Left, NONE), 1), "\x1b[57417u");
        assert_eq!(enc(&keypad(Key::Delete, NONE), 1), "\x1b[57426u");
        // A keypad key that types text is still text.
        assert_eq!(enc(&keypad(Key::Char('5'), NONE), 1), "5");
        assert_eq!(enc(&keypad(Key::Char('+'), NONE).with_text("+"), 1), "+");
        // Every key as a code: the keypad code, with the text if asked for.
        assert_eq!(enc(&keypad(Key::Char('5'), NONE), 0b1000), "\x1b[57404u");
        assert_eq!(
            enc(&keypad(Key::Char('5'), NONE), 0b11000),
            "\x1b[57404;;53u"
        );
        // Without disambiguation they are the ordinary keys.
        assert_eq!(enc(&keypad(Key::PageUp, NONE), 0b10), "\x1b[5~");
        assert_eq!(enc(&keypad(Key::Enter, NONE), 0b100), "\r");
    }

    // ---- kitty: flag 2, event types -----------------------------------------------------

    #[test]
    fn event_types_are_a_sub_field_of_the_modifiers() {
        // "If no modifiers are present, the modifiers field must have the value 1."
        assert_eq!(enc(&release(ch('a', NONE)), 0b10), "\x1b[97;1:3u");
        assert_eq!(enc(&release(ch('a', SHIFT)), 0b10), "\x1b[97;2:3u");
        assert_eq!(enc(&repeat(ch('a', CTRL)), 0b10), "\x1b[97;5:2u");
        assert_eq!(enc(&release(ch('a', CTRL)), 0b10), "\x1b[97;5:3u");
        // A press has the default type and says nothing.
        assert_eq!(enc(&ch('a', CTRL), 0b11), "\x1b[97;5u");
        assert_eq!(enc(&repeat(key(Key::Up, NONE)), 0b10), "\x1b[1;1:2A");
        assert_eq!(enc(&release(key(Key::Up, NONE)), 0b10), "\x1b[1;1:3A");
        assert_eq!(enc(&release(key(Key::Up, CTRL)), 0b11), "\x1b[1;5:3A");
        assert_eq!(enc(&release(key(Key::Delete, NONE)), 0b11), "\x1b[3;1:3~");
        assert_eq!(enc(&release(key(Key::F(1), NONE)), 0b11), "\x1b[1;1:3P");
        assert_eq!(enc(&release(key(Key::Escape, NONE)), 0b11), "\x1b[27;1:3u");
        assert_eq!(enc(&repeat(key(Key::Escape, NONE)), 0b11), "\x1b[27;1:2u");
    }

    #[test]
    fn text_keys_repeat_as_text_and_release_as_a_code() {
        assert_eq!(enc(&KeyEvent::typed('a'), 0b10), "a");
        assert_eq!(enc(&repeat(KeyEvent::typed('a')), 0b10), "a");
        assert_eq!(enc(&release(KeyEvent::typed('a')), 0b10), "\x1b[97;1:3u");
        assert_eq!(enc(&release(KeyEvent::typed('A')), 0b11), "\x1b[97;2:3u");
        assert_eq!(
            enc(&release(KeyEvent::typed('A')), 0b111),
            "\x1b[97:65;2:3u"
        );
    }

    #[test]
    fn enter_tab_and_backspace_have_no_release_without_report_all() {
        for k in [Key::Enter, Key::Tab, Key::Backspace] {
            assert_eq!(enc(&release(key(k, NONE)), 0b10), "", "{k:?}");
            assert_eq!(enc(&release(key(k, NONE)), 0b11), "", "{k:?}");
            assert_eq!(enc(&release(key(k, NONE)), 0b111), "", "{k:?}");
        }
        assert_eq!(enc(&key(Key::Backspace, NONE), 0b11), "\x7f");
        assert_eq!(enc(&repeat(key(Key::Backspace, NONE)), 0b11), "\x7f");
        assert_eq!(enc(&release(key(Key::Enter, NONE)), 0b1010), "\x1b[13;1:3u");
        assert_eq!(enc(&release(key(Key::Tab, NONE)), 0b1010), "\x1b[9;1:3u");
        assert_eq!(
            enc(&release(key(Key::Backspace, NONE)), 0b1010),
            "\x1b[127;1:3u"
        );
        // With a modifier they are escape codes anyway, so the release is reported.
        assert_eq!(enc(&release(key(Key::Enter, SHIFT)), 0b11), "\x1b[13;2:3u");
    }

    #[test]
    fn releases_need_the_event_types_flag() {
        for flags in [0b1, 0b100, 0b101, 0b1000, 0b11000, 0b1101] {
            assert_eq!(enc(&release(ch('a', NONE)), flags), "", "flags {flags:#b}");
            assert_eq!(enc(&release(ch('a', CTRL)), flags), "", "flags {flags:#b}");
            assert_eq!(
                enc(&release(key(Key::Up, NONE)), flags),
                "",
                "flags {flags:#b}"
            );
            assert_eq!(
                enc(&release(key(Key::Escape, NONE)), flags),
                "",
                "flags {flags:#b}"
            );
        }
    }

    #[test]
    fn event_types_alone_leave_presses_in_their_legacy_form() {
        assert_eq!(enc(&ch('a', CTRL), 0b10), "\x01");
        assert_eq!(enc(&ch('a', ALT), 0b10), "\x1ba");
        assert_eq!(enc(&key(Key::Escape, NONE), 0b10), "\x1b");
        assert_eq!(enc(&key(Key::Up, NONE), 0b10), "\x1b[A");
        // Outside the specification's legacy combinations: a CSI u code.
        assert_eq!(enc(&ch('i', CTRL | SHIFT), 0b10), "\x1b[105;6u");
    }

    // ---- kitty: flag 4, alternate keys --------------------------------------------------

    #[test]
    fn alternates_are_added_to_escape_codes() {
        // The shifted key only when shift is held.
        assert_eq!(enc(&ch('a', CTRL | SHIFT), 0b101), "\x1b[97:65;6u");
        assert_eq!(enc(&ch('a', CTRL), 0b101), "\x1b[97;5u");
        assert_eq!(enc(&ch('a', CTRL).with_shifted('A'), 0b101), "\x1b[97;5u");
        // A symbol's shifted form is only sent when the host terminal reported it.
        assert_eq!(enc(&ch('=', CTRL | SHIFT), 0b101), "\x1b[61;6u");
        assert_eq!(
            enc(&ch('=', CTRL | SHIFT).with_shifted('+'), 0b101),
            "\x1b[61:43;6u"
        );
        // The base layout key, with an empty shifted field when there is no shifted key.
        let mut event = ch('с', CTRL);
        event.base = Some('c');
        assert_eq!(enc(&event, 0b101), "\x1b[1089::99;5u");
        let mut event = ch('с', CTRL | SHIFT);
        event.base = Some('c');
        event.shifted = Some('С');
        assert_eq!(enc(&event, 0b101), "\x1b[1089:1057:99;6u");
        // Without the flag, neither.
        assert_eq!(enc(&event, 0b1), "\x1b[1089;6u");
    }

    #[test]
    fn alternates_do_not_turn_a_key_into_an_escape_code() {
        // "Only key events represented as escape codes due to the other enhancements in
        // effect will be affected by this enhancement."
        assert_eq!(enc(&KeyEvent::typed('a'), 0b100), "a");
        assert_eq!(enc(&KeyEvent::typed('A'), 0b100), "A");
        assert_eq!(enc(&KeyEvent::typed('A'), 0b101), "A");
        assert_eq!(enc(&ch('a', ALT | SHIFT), 0b100), "\x1bA");
        assert_eq!(enc(&ch('a', CTRL), 0b100), "\x01");
        // Functional keys have no alternates.
        assert_eq!(enc(&key(Key::Up, SHIFT), 0b101), "\x1b[1;2A");
        assert_eq!(enc(&key(Key::Enter, SHIFT), 0b101), "\x1b[13;2u");
    }

    // ---- kitty: flag 8, all keys as escape codes ----------------------------------------

    #[test]
    fn report_all_turns_text_keys_into_codes() {
        assert_eq!(enc(&KeyEvent::typed('a'), 0b1000), "\x1b[97u");
        assert_eq!(enc(&repeat(KeyEvent::typed('a')), 0b1000), "\x1b[97u");
        assert_eq!(enc(&repeat(KeyEvent::typed('a')), 0b1010), "\x1b[97;1:2u");
        assert_eq!(enc(&release(KeyEvent::typed('a')), 0b1010), "\x1b[97;1:3u");
        assert_eq!(enc(&KeyEvent::typed('A'), 0b1000), "\x1b[97;2u");
        assert_eq!(enc(&KeyEvent::typed('A'), 0b1100), "\x1b[97:65;2u");
        assert_eq!(enc(&ch('a', CTRL), 0b1000), "\x1b[97;5u");
        assert_eq!(enc(&KeyEvent::typed(' '), 0b1000), "\x1b[32u");
        assert_eq!(enc(&KeyEvent::typed('é'), 0b1000), "\x1b[233u");
        let mut event = KeyEvent::typed('a');
        event.caps_lock = true;
        assert_eq!(enc(&event, 0b1000), "\x1b[97;65u");
    }

    #[test]
    fn report_all_covers_enter_tab_backspace_and_modifier_keys() {
        assert_eq!(enc(&key(Key::Enter, NONE), 0b1000), "\x1b[13u");
        assert_eq!(enc(&key(Key::Enter, CTRL), 0b1000), "\x1b[13;5u");
        assert_eq!(enc(&key(Key::Tab, NONE), 0b1000), "\x1b[9u");
        assert_eq!(enc(&key(Key::Backspace, NONE), 0b1000), "\x1b[127u");
        assert_eq!(enc(&key(Key::Escape, NONE), 0b1000), "\x1b[27u");
        assert_eq!(enc(&key(Key::Up, NONE), 0b1000), "\x1b[A");
        assert_eq!(
            enc(&key(Key::Modifier(ModKey::LeftShift), NONE), 0b1000),
            "\x1b[57441u"
        );
        assert_eq!(
            enc(&key(Key::Modifier(ModKey::LeftCtrl), CTRL), 0b1000),
            "\x1b[57442;5u"
        );
        assert_eq!(
            enc(&release(key(Key::Modifier(ModKey::LeftCtrl), NONE)), 0b1010),
            "\x1b[57442;1:3u"
        );
        assert_eq!(
            enc(&key(Key::Modifier(ModKey::RightSuper), CMD), 0b1000),
            "\x1b[57450;9u"
        );
        assert_eq!(enc(&key(Key::CapsLock, NONE), 0b1000), "\x1b[57358u");
        assert_eq!(enc(&key(Key::NumLock, NONE), 0b1000), "\x1b[57360u");
    }

    #[test]
    fn modifier_and_lock_keys_need_report_all() {
        for flags in [0b0, 0b1, 0b10, 0b11, 0b100, 0b111, 0b10000] {
            for k in [
                Key::Modifier(ModKey::LeftShift),
                Key::Modifier(ModKey::RightAlt),
                Key::CapsLock,
                Key::NumLock,
                Key::ScrollLock,
            ] {
                assert_eq!(enc(&key(k, NONE), flags), "", "{k:?} with flags {flags:#b}");
                assert_eq!(
                    enc(&release(key(k, NONE)), flags),
                    "",
                    "{k:?} with flags {flags:#b}"
                );
            }
        }
    }

    // ---- kitty: flag 16, associated text ------------------------------------------------

    #[test]
    fn associated_text_is_a_third_field() {
        // "shift+a -> CSI 97 ; 2 ; 65 u"
        assert_eq!(enc(&KeyEvent::typed('A'), 0b11000), "\x1b[97;2;65u");
        assert_eq!(enc(&KeyEvent::typed('a'), 0b11000), "\x1b[97;;97u");
        assert_eq!(
            enc(&ch('a', SHIFT).with_text("AB"), 0b11000),
            "\x1b[97;2;65:66u"
        );
        // "alt+a -> CSI 0 ; ; 229 u": text with no key behind it uses key number 0.
        assert_eq!(
            enc(&key(Key::Other(0), NONE).with_text("å"), 0b11000),
            "\x1b[0;;229u"
        );
        // Everything at once.
        assert_eq!(
            enc(&repeat(KeyEvent::typed('A')), 0b11111),
            "\x1b[97:65;2:2;65u"
        );
        // Keys without text have no third field, and a release never has one.
        assert_eq!(enc(&ch('a', CTRL), 0b11000), "\x1b[97;5u");
        assert_eq!(enc(&key(Key::Enter, NONE), 0b11000), "\x1b[13u");
        assert_eq!(enc(&release(KeyEvent::typed('a')), 0b11010), "\x1b[97;1:3u");
    }

    #[test]
    fn associated_text_without_report_all_changes_nothing() {
        assert_eq!(enc(&KeyEvent::typed('a'), 0b10000), "a");
        assert_eq!(enc(&KeyEvent::typed('A'), 0b10001), "A");
        assert_eq!(enc(&key(Key::Up, NONE), 0b10000), "\x1b[A");
    }

    #[test]
    fn a_text_event_with_no_key_needs_the_text_flag() {
        let event = key(Key::Other(0), NONE).with_text("å");
        assert_eq!(enc(&event, 0b1), "å");
        assert_eq!(enc(&event, 0b1000), "");
        assert_eq!(enc(&key(Key::Other(0), NONE), 0b1), "");
    }

    // ---- what herdr asks for ------------------------------------------------------------

    #[test]
    fn herdr_flags_seven() {
        let flags = KITTY_DISAMBIGUATE | KITTY_REPORT_EVENTS | KITTY_REPORT_ALTERNATES;
        assert_eq!(flags, 7);
        assert_eq!(enc(&KeyEvent::typed('h'), flags), "h");
        assert_eq!(enc(&release(KeyEvent::typed('h')), flags), "\x1b[104;1:3u");
        assert_eq!(enc(&ch('b', CTRL), flags), "\x1b[98;5u");
        assert_eq!(enc(&release(ch('b', CTRL)), flags), "\x1b[98;5:3u");
        assert_eq!(enc(&key(Key::Escape, NONE), flags), "\x1b[27u");
        assert_eq!(enc(&key(Key::Enter, NONE), flags), "\r");
        assert_eq!(enc(&key(Key::Enter, SHIFT), flags), "\x1b[13;2u");
        assert_eq!(enc(&ch('p', CMD | SHIFT), flags), "\x1b[112:80;10u");
        assert_eq!(enc(&key(Key::Left, ALT), flags), "\x1b[1;3D");
        assert_eq!(enc(&repeat(key(Key::Down, NONE)), flags), "\x1b[1;1:2B");
    }

    #[test]
    fn unknown_flag_bits_are_ignored() {
        assert_eq!(enc(&ch('a', CTRL), 0b0010_0000), "\x01");
        assert_eq!(enc(&ch('a', CTRL), 0b0010_0001), "\x1b[97;5u");
    }
}
