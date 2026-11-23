//! What a complete CSI sequence means: a key, a mouse report, a focus change, the start of
//! a paste, a reply to a query, or nothing bakedit cares about.

use bakedit_core::{InputEvent, Key, KeyEvent, KeyKind, Mods, TerminalReply};

use super::{keys, mouse};

/// The outcome of decoding one CSI sequence.
#[derive(Debug)]
pub(super) enum Decoded {
    /// A key event. An ESC in front of the sequence adds alt to it.
    Key(KeyEvent),
    /// Any other event.
    Event(InputEvent),
    /// `CSI 200 ~`: a bracketed paste begins.
    PasteStart,
    /// A well-formed sequence that is not input, or a malformed one.
    Ignore,
}

/// The parameters of a CSI sequence: fields separated by `;`, each made of sub-fields
/// separated by `:`. An empty sub-field is `None`.
struct Params {
    fields: Vec<Vec<Option<u32>>>,
}

impl Params {
    fn parse(bytes: &[u8]) -> Params {
        let fields = bytes
            .split(|&b| b == b';')
            .map(|field| field.split(|&b| b == b':').map(number).collect())
            .collect();
        Params { fields }
    }

    /// Sub-field `sub` of field `field`, if it is there and not empty.
    fn get(&self, field: usize, sub: usize) -> Option<u32> {
        *self.fields.get(field)?.get(sub)?
    }

    /// How many fields there are. Never zero: no parameters at all is one empty field.
    fn len(&self) -> usize {
        self.fields.len()
    }

    /// The first sub-field of every field, when every field has exactly one sub-field.
    fn simple(&self) -> Option<Vec<Option<u32>>> {
        self.fields
            .iter()
            .map(|field| {
                if field.len() == 1 {
                    Some(field[0])
                } else {
                    None
                }
            })
            .collect()
    }
}

/// A decimal number. Empty is `None`; one too large for `u32` saturates.
fn number(digits: &[u8]) -> Option<u32> {
    if digits.is_empty() {
        return None;
    }
    let value = digits.iter().fold(0u32, |acc, &d| {
        acc.saturating_mul(10).saturating_add(u32::from(d - b'0'))
    });
    Some(value)
}

/// Decode the sequence `CSI body final_byte`. `body` holds only bytes 0x20..=0x3f.
pub(super) fn decode(body: &[u8], final_byte: u8) -> Decoded {
    let (prefix, rest) = match body.first() {
        Some(&b) if (b'<'..=b'?').contains(&b) => (Some(b), &body[1..]),
        _ => (None, body),
    };
    let params_len = rest
        .iter()
        .take_while(|&&b| b.is_ascii_digit() || b == b':' || b == b';')
        .count();
    let (param_bytes, intermediates) = rest.split_at(params_len);
    if !intermediates.iter().all(|b| (0x20..=0x2f).contains(b)) {
        return Decoded::Ignore;
    }
    let params = Params::parse(param_bytes);
    let decoded = match (prefix, intermediates, final_byte) {
        (None, b"", b'u') => kitty_key(&params).map(Decoded::Key),
        (None, b"", b'~') => tilde(&params),
        (None, b"", b'Z') => letter_mods(&params)
            .map(|state| Decoded::Key(state.apply(KeyEvent::new(Key::Tab, Mods::SHIFT)))),
        (None, b"", b'I') if param_bytes.is_empty() => {
            Some(Decoded::Event(InputEvent::FocusGained))
        }
        (None, b"", b'O') if param_bytes.is_empty() => Some(Decoded::Event(InputEvent::FocusLost)),
        (None, b"", _) => letter(&params, final_byte).map(Decoded::Key),
        (Some(b'<'), b"", b'M' | b'm') => sgr_mouse(&params, final_byte == b'm'),
        (Some(b'?'), _, _) => reply(&params, param_bytes, intermediates, final_byte),
        _ => None,
    };
    decoded.unwrap_or(Decoded::Ignore)
}

/// The modifiers, lock states and event type of a key, as the kitty protocol encodes them
/// in `modifiers[:event-type]`.
struct ModState {
    mods: Mods,
    caps_lock: bool,
    num_lock: bool,
    kind: KeyKind,
}

impl ModState {
    /// From the wire values: `modifiers` is 1 + the bit field (absent means 1), `event` is
    /// 1 press, 2 repeat, 3 release (absent or unknown means press).
    fn new(modifiers: Option<u32>, event: Option<u32>) -> ModState {
        let bits = modifiers.unwrap_or(1).saturating_sub(1);
        ModState {
            mods: Mods::from_bits_truncate((bits & 0x3f) as u8),
            caps_lock: bits & 64 != 0,
            num_lock: bits & 128 != 0,
            kind: match event {
                Some(2) => KeyKind::Repeat,
                Some(3) => KeyKind::Release,
                _ => KeyKind::Press,
            },
        }
    }

    /// The modifiers of a plain key press.
    fn none() -> ModState {
        ModState::new(None, None)
    }

    fn apply(&self, mut ev: KeyEvent) -> KeyEvent {
        ev.mods |= self.mods;
        ev.caps_lock = self.caps_lock;
        ev.num_lock = self.num_lock;
        ev.with_kind(self.kind)
    }
}

/// `CSI unicode-key-code[:shifted[:base]] [; modifiers[:event] [; text]] u`.
fn kitty_key(params: &Params) -> Option<KeyEvent> {
    let text = kitty_text(params);
    let code = params.get(0, 0).unwrap_or(0);
    let state = ModState::new(params.get(1, 0), params.get(1, 1));
    if code == 0 {
        // Text that belongs to no key, from an input method for example. Without text
        // there is nothing to report (a bare `CSI u` is not input at all).
        let ev = state.apply(KeyEvent::new(Key::Other(0), Mods::empty()));
        return text
            .map(|text| ev.with_text(text))
            .filter(KeyEvent::is_down);
    }
    let (key, keypad) = keys::kitty_key(code);
    let mut ev = KeyEvent::new(key, Mods::empty());
    ev.keypad = keypad;
    ev.shifted = params.get(0, 1).and_then(printable);
    ev.base = params.get(0, 2).and_then(printable);
    if let Key::Char(c) = key {
        // The key code is defined as the unshifted key. A terminal that sends the
        // upper-case letter anyway means shift + the lower-case key.
        if let Some(lower) = keys::lower_case(c) {
            ev.key = Key::Char(lower);
            ev.mods |= Mods::SHIFT;
            ev.shifted = ev.shifted.or(Some(c));
        }
    }
    let mut ev = state.apply(ev);
    if ev.is_down() {
        ev.text = text.or_else(|| keys::implied_text(&ev));
    }
    Some(ev)
}

/// The third field of a kitty key event: the text it types, as code points. Control
/// characters (which the protocol forbids there) are left out; `None` when the field is
/// absent or holds nothing printable.
fn kitty_text(params: &Params) -> Option<String> {
    let text: String = params
        .fields
        .get(2)?
        .iter()
        .filter_map(|&code| printable(code?))
        .collect();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// The character with this code point, unless there is none or it is a control character.
fn printable(code: u32) -> Option<char> {
    char::from_u32(code).filter(|c| !c.is_control())
}

/// The modifiers of a `CSI [1 ; mods] X` key, or `None` when the parameters are not that
/// shape. A single parameter other than 1 is the modifiers on its own, as old xterms sent.
fn letter_mods(params: &Params) -> Option<ModState> {
    match (params.len(), params.get(0, 0)) {
        (1, None | Some(1)) => Some(ModState::none()),
        (1, modifiers) => Some(ModState::new(modifiers, None)),
        (2, None | Some(1)) => Some(ModState::new(params.get(1, 0), params.get(1, 1))),
        _ => None,
    }
}

/// `CSI [1 ; mods[:event]] X` for the cursor and function keys.
fn letter(params: &Params, final_byte: u8) -> Option<KeyEvent> {
    let ev = keys::letter_key(final_byte)?;
    if final_byte == b'R' && params.len() == 1 && params.get(0, 0).is_some() {
        // F3 is `CSI R` or `CSI 1 ; mods R`. `CSI n R` is a cursor position report with
        // its column missing, and a full report (`CSI row ; column R` with a row other
        // than 1) is turned away by `letter_mods`.
        return None;
    }
    Some(letter_mods(params)?.apply(ev))
}

/// `CSI number [; mods[:event]] ~`, which also carries the paste markers and xterm's
/// modifyOtherKeys keys.
fn tilde(params: &Params) -> Option<Decoded> {
    let number = params.get(0, 0)?;
    match number {
        200 => Some(Decoded::PasteStart),
        // The end of a paste that never began.
        201 => None,
        27 if params.len() >= 3 => modify_other_keys(params).map(Decoded::Key),
        _ if params.len() <= 2 => {
            let state = ModState::new(params.get(1, 0), params.get(1, 1));
            keys::tilde_key(number).map(|ev| Decoded::Key(state.apply(ev)))
        }
        _ => None,
    }
}

/// xterm's `CSI 27 ; mods ; code ~`. The code is the character the key types with shift
/// applied, so it is taken apart the way typed text is.
fn modify_other_keys(params: &Params) -> Option<KeyEvent> {
    let code = params.get(2, 0)?;
    let state = ModState::new(params.get(1, 0), None);
    let ev = match code {
        13 => KeyEvent::new(Key::Enter, Mods::empty()),
        9 => KeyEvent::new(Key::Tab, Mods::empty()),
        27 => KeyEvent::new(Key::Escape, Mods::empty()),
        8 | 127 => KeyEvent::new(Key::Backspace, Mods::empty()),
        _ => KeyEvent::typed(printable(code)?),
    };
    let mut ev = state.apply(ev);
    ev.text = keys::implied_text(&ev);
    Some(ev)
}

/// `CSI < button ; column ; row M` (press, motion, wheel) or `m` (release).
fn sgr_mouse(params: &Params, release: bool) -> Option<Decoded> {
    let simple = params.simple()?;
    let [Some(code), Some(column), Some(row)] = simple[..] else {
        return None;
    };
    let event = mouse::decode(code, mouse::cell(column), mouse::cell(row), release)?;
    Some(Decoded::Event(InputEvent::Mouse(event)))
}

/// The `CSI ? ...` replies to bakedit's queries.
fn reply(
    params: &Params,
    param_bytes: &[u8],
    intermediates: &[u8],
    final_byte: u8,
) -> Option<Decoded> {
    let reply = match (intermediates, final_byte) {
        (b"", b'u') => {
            let flags = params.get(0, 0).unwrap_or(0);
            TerminalReply::KittyKeyboardFlags((flags & 0xff) as u8)
        }
        (b"", b'c') => {
            TerminalReply::DeviceAttributes(String::from_utf8_lossy(param_bytes).into_owned())
        }
        (b"", b'n') => match params.simple()?[..] {
            [Some(997), Some(1)] => TerminalReply::DarkMode(true),
            [Some(997), Some(2)] => TerminalReply::DarkMode(false),
            _ => return None,
        },
        (b"$", b'y') => match params.simple()?[..] {
            [Some(mode), Some(state)] => TerminalReply::Mode {
                mode: u16::try_from(mode).ok()?,
                state: u8::try_from(state).ok()?,
            },
            _ => return None,
        },
        _ => return None,
    };
    Some(Decoded::Event(InputEvent::Reply(reply)))
}
