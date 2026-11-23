//! The incremental parser for the bytes the host terminal sends.
//!
//! The parser is a byte-at-a-time state machine: every byte is looked at once and all the
//! state lives in the [`Parser`], so the events it produces never depend on how the input
//! was cut into `feed` calls.

mod csi;
mod keys;
mod mouse;
mod strings;
#[cfg(test)]
mod tests;

use bakedit_core::{InputEvent, Key, KeyEvent, Mods, MouseButton};

/// The longest OSC, DCS, APC, PM or SOS string the parser keeps while it waits for the
/// terminator. A longer one is discarded whole (its tail is still swallowed).
pub const MAX_STRING_LEN: usize = 64 * 1024;

/// The most bytes of one bracketed paste the parser keeps. Bytes beyond that are discarded
/// and the paste is delivered truncated when its end marker arrives.
pub const MAX_PASTE_LEN: usize = 64 * 1024 * 1024;

/// The longest CSI sequence the parser collects. Real ones are a few dozen bytes; the
/// longest legitimate case is a kitty key event carrying a sentence of input-method text.
const MAX_CSI_LEN: usize = 4096;

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;

/// The marker that ends a bracketed paste: `CSI 201 ~`.
const PASTE_END: &[u8] = b"\x1b[201~";

/// The string-type control sequences: everything between the introducer and ST.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StringKind {
    Osc,
    Dcs,
    Apc,
    Pm,
    Sos,
}

impl StringKind {
    /// The kind opened by `ESC` followed by `byte`, if any.
    fn from_introducer(byte: u8) -> Option<StringKind> {
        Some(match byte {
            b']' => StringKind::Osc,
            b'P' => StringKind::Dcs,
            b'_' => StringKind::Apc,
            b'^' => StringKind::Pm,
            b'X' => StringKind::Sos,
            _ => return None,
        })
    }

    /// The character that follows `ESC` to open this kind of string.
    fn introducer(self) -> char {
        match self {
            StringKind::Osc => ']',
            StringKind::Dcs => 'P',
            StringKind::Apc => '_',
            StringKind::Pm => '^',
            StringKind::Sos => 'X',
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Between sequences.
    Ground,
    /// Inside a UTF-8 character of `len` bytes; `alt` when an ESC came right before it.
    Utf8 { len: usize, alt: bool },
    /// After ESC.
    Escape,
    /// After `ESC [`, collecting parameter and intermediate bytes.
    Csi,
    /// Inside a CSI sequence that grew past [`MAX_CSI_LEN`]: discard up to its final byte.
    CsiIgnore,
    /// After `ESC O`, waiting for the one byte that names the key.
    Ss3,
    /// After `CSI M`, collecting the three bytes of a legacy mouse report.
    X10Mouse,
    /// Inside a string sequence, collecting its body.
    Str(StringKind),
    /// Inside a string sequence, right after an ESC that may begin its terminator.
    StrEsc(StringKind),
    /// Inside a string sequence that grew past the cap: discard up to its terminator.
    StrIgnore(StringKind),
    /// [`State::StrIgnore`], right after an ESC.
    StrIgnoreEsc,
    /// Inside a bracketed paste.
    Paste,
}

/// Turns the bytes read from the host terminal into [`InputEvent`]s.
///
/// Feed it whatever `read` returned. The bytes may be cut anywhere, including in the middle
/// of an escape sequence or of a UTF-8 character: the unfinished tail is kept and completed
/// by the next [`feed`](Parser::feed).
///
/// # The escape timeout
///
/// A lone ESC byte is either the Escape key or the start of a sequence, and only time can
/// tell. When [`has_pending`](Parser::has_pending) is true and nothing more arrives for
/// about 25 ms, call [`flush`](Parser::flush):
///
/// - a lone ESC becomes an Escape press, `ESC ESC` an alt+Escape press;
/// - ESC followed by nothing but the character that opens a sequence (`ESC [`, `ESC O`,
///   `ESC ]`, `ESC P`, `ESC _`, `ESC ^`, `ESC X`) becomes alt + that character;
/// - an unfinished sequence that cannot be a key press (a CSI with parameters, a string
///   with content, a legacy mouse report, half a UTF-8 character) is kept through *one*
///   flush, because on a slow link its tail may simply be late, and typing that tail into
///   the editor as text would be worse than waiting. If a second flush finds it still
///   unfinished it is dropped.
///
/// `flush` always leaves `has_pending` false, so a timer loop cannot spin.
///
/// # Bracketed paste
///
/// Everything between `CSI 200 ~` and `CSI 201 ~` becomes one [`InputEvent::Paste`], however
/// many `feed` calls it takes. A paste in progress is not "pending" and `flush` leaves it
/// alone; see [`in_paste`](Parser::in_paste) and [`reset`](Parser::reset).
///
/// # Limits
///
/// Nothing the terminal sends makes the parser panic, loop or grow without bound: a CSI
/// sequence is cut off at 4 KiB, a string sequence at [`MAX_STRING_LEN`] and a paste at
/// [`MAX_PASTE_LEN`].
#[derive(Debug, Clone)]
pub struct Parser {
    state: State,
    /// The bytes collected so far for the sequence or character in progress.
    seq: Vec<u8>,
    /// An ESC came right before the ESC that opened the sequence in progress. It becomes
    /// alt on a key sequence and an Escape press of its own before anything else.
    esc_prefix: bool,
    /// The sequence in progress has already survived one flush.
    late: bool,
    /// No byte has arrived since the last flush.
    quiet: bool,
    /// The body of the paste in progress.
    paste: Vec<u8>,
    /// How many bytes of [`PASTE_END`] the end of the paste input matches so far.
    paste_match: usize,
    /// The paste in progress has lost bytes to the cap.
    paste_truncated: bool,
    /// The button of the last legacy mouse press: that protocol's release does not say.
    x10_button: MouseButton,
    max_string_len: usize,
    max_paste_len: usize,
}

impl Default for Parser {
    fn default() -> Parser {
        Parser::new()
    }
}

impl Parser {
    /// A parser with nothing buffered.
    pub fn new() -> Parser {
        Parser {
            state: State::Ground,
            seq: Vec::new(),
            esc_prefix: false,
            late: false,
            quiet: true,
            paste: Vec::new(),
            paste_match: 0,
            paste_truncated: false,
            x10_button: MouseButton::Left,
            max_string_len: MAX_STRING_LEN,
            max_paste_len: MAX_PASTE_LEN,
        }
    }

    /// Parse `bytes` and return the events they complete, in order.
    ///
    /// An unfinished sequence at the end stays buffered. Bytes that are not valid input
    /// (broken UTF-8, control characters inside a sequence, sequences bakedit does not
    /// know) produce no event and never turn into text.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<InputEvent> {
        let mut out = Vec::new();
        if bytes.is_empty() {
            return out;
        }
        self.quiet = false;
        let mut at = 0;
        while at < bytes.len() {
            if self.state == State::Paste {
                at += self.paste_bytes(&bytes[at..], &mut out);
            } else if self.step(bytes[at], &mut out) {
                at += 1;
            }
        }
        out
    }

    /// True when buffered input is waiting for more bytes and [`flush`](Parser::flush)
    /// would act on it. False in the middle of a bracketed paste, and false right after a
    /// flush even if that flush kept a late sequence (it becomes true again when more of it
    /// arrives).
    pub fn has_pending(&self) -> bool {
        match self.state {
            State::Ground | State::Paste => false,
            _ => !(self.late && self.quiet),
        }
    }

    /// True while the parser is inside a bracketed paste whose end marker has not arrived.
    pub fn in_paste(&self) -> bool {
        self.state == State::Paste
    }

    /// Resolve buffered input after the escape timeout; see the type's documentation for
    /// the rules. Returns the events that produces, usually none or one key press. A paste
    /// in progress is left untouched.
    pub fn flush(&mut self) -> Vec<InputEvent> {
        let mut out = Vec::new();
        match self.state {
            State::Ground | State::Paste => {}
            State::Escape => {
                let mods = if self.esc_prefix {
                    Mods::ALT
                } else {
                    Mods::empty()
                };
                out.push(InputEvent::Key(KeyEvent::new(Key::Escape, mods)));
                self.enter_ground();
            }
            State::Csi if self.seq.is_empty() => {
                self.emit_prefix(&mut out);
                out.push(alt_char('['));
                self.enter_ground();
            }
            State::Ss3 => {
                self.emit_prefix(&mut out);
                out.push(alt_char('O'));
                self.enter_ground();
            }
            State::Str(kind) if self.seq.is_empty() => {
                out.push(alt_char(kind.introducer()));
                self.enter_ground();
            }
            State::StrEsc(kind) if self.seq.is_empty() => {
                out.push(alt_char(kind.introducer()));
                out.push(escape_press());
                self.enter_ground();
            }
            _ => {
                if self.late {
                    self.enter_ground();
                } else {
                    self.late = true;
                }
            }
        }
        self.quiet = true;
        out
    }

    /// Forget everything buffered, including a paste in progress, and start clean. Nothing
    /// is emitted. For the app to call when the input stream is known to have restarted.
    pub fn reset(&mut self) {
        *self = Parser {
            max_string_len: self.max_string_len,
            max_paste_len: self.max_paste_len,
            ..Parser::new()
        };
    }

    /// Back to the ground state with nothing collected.
    fn enter_ground(&mut self) {
        self.state = State::Ground;
        self.seq.clear();
        self.esc_prefix = false;
        self.late = false;
    }

    /// Start over at an ESC that has just been consumed.
    fn enter_escape(&mut self) {
        self.enter_ground();
        self.state = State::Escape;
    }

    /// Emit the Escape press owed for an ESC that turned out not to prefix a key.
    fn emit_prefix(&mut self, out: &mut Vec<InputEvent>) {
        if self.esc_prefix {
            self.esc_prefix = false;
            out.push(escape_press());
        }
    }

    /// Emit a key that came from an escape sequence, with alt if an ESC preceded it.
    fn emit_sequence_key(&mut self, ev: KeyEvent, out: &mut Vec<InputEvent>) {
        let ev = if self.esc_prefix { with_alt(ev) } else { ev };
        self.esc_prefix = false;
        out.push(InputEvent::Key(ev));
    }

    /// Advance by one byte. Returns false when the byte ended the sequence in progress
    /// without belonging to it and has to be looked at again from the new state.
    fn step(&mut self, byte: u8, out: &mut Vec<InputEvent>) -> bool {
        match self.state {
            State::Ground => self.ground(byte, out),
            State::Utf8 { len, alt } => self.utf8(byte, len, alt, out),
            State::Escape => self.escape(byte, out),
            State::Csi => self.csi(byte, out),
            State::CsiIgnore => self.csi_ignore(byte),
            State::Ss3 => self.ss3(byte, out),
            State::X10Mouse => self.x10_mouse(byte, out),
            State::Str(kind) => self.string(kind, byte, out),
            State::StrEsc(kind) => self.string_esc(kind, byte, out),
            State::StrIgnore(kind) => self.string_ignore(kind, byte),
            State::StrIgnoreEsc => self.string_ignore_esc(byte),
            State::Paste => self.paste_bytes(&[byte], out) == 1,
        }
    }

    fn ground(&mut self, byte: u8, out: &mut Vec<InputEvent>) -> bool {
        match byte {
            ESC => self.enter_escape(),
            0x00..=0x7f => out.push(InputEvent::Key(keys::legacy_byte(byte))),
            _ => self.start_utf8(byte, false),
        }
        true
    }

    /// Begin a multi-byte UTF-8 character at its lead byte. A byte that cannot lead one is
    /// dropped.
    fn start_utf8(&mut self, lead: u8, alt: bool) {
        let len = match lead {
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => return,
        };
        self.seq.clear();
        self.seq.push(lead);
        self.state = State::Utf8 { len, alt };
    }

    fn utf8(&mut self, byte: u8, len: usize, alt: bool, out: &mut Vec<InputEvent>) -> bool {
        if !(0x80..=0xbf).contains(&byte) {
            // The character was cut short; whatever this byte is, it starts something new.
            self.enter_ground();
            return false;
        }
        self.seq.push(byte);
        if self.seq.len() < len {
            return true;
        }
        let decoded = std::str::from_utf8(&self.seq)
            .ok()
            .and_then(|s| s.chars().next());
        self.enter_ground();
        if let Some(c) = decoded.filter(|c| !c.is_control()) {
            let ev = KeyEvent::typed(c);
            out.push(InputEvent::Key(if alt { with_alt(ev) } else { ev }));
        }
        true
    }

    fn escape(&mut self, byte: u8, out: &mut Vec<InputEvent>) -> bool {
        match byte {
            ESC => {
                // Of a run of ESCs only the last two can still belong to a sequence.
                self.emit_prefix(out);
                self.esc_prefix = true;
            }
            b'[' => self.state = State::Csi,
            b'O' => self.state = State::Ss3,
            0x00..=0x7f => {
                self.emit_prefix(out);
                if let Some(kind) = StringKind::from_introducer(byte) {
                    self.state = State::Str(kind);
                } else {
                    out.push(InputEvent::Key(with_alt(keys::legacy_byte(byte))));
                    self.enter_ground();
                }
            }
            _ => {
                self.emit_prefix(out);
                self.enter_ground();
                self.start_utf8(byte, true);
            }
        }
        true
    }

    fn csi(&mut self, byte: u8, out: &mut Vec<InputEvent>) -> bool {
        match byte {
            0x20..=0x3f => {
                if self.seq.len() >= MAX_CSI_LEN {
                    self.seq.clear();
                    self.state = State::CsiIgnore;
                } else {
                    self.seq.push(byte);
                }
                true
            }
            b'M' if self.seq.is_empty() => {
                self.state = State::X10Mouse;
                true
            }
            0x40..=0x7e => {
                self.finish_csi(byte, out);
                true
            }
            _ => {
                // Not a byte a CSI sequence can hold. A bare `ESC [` was alt+[; anything
                // longer was a sequence cut short and is dropped.
                if self.seq.is_empty() {
                    self.emit_prefix(out);
                    out.push(alt_char('['));
                }
                self.enter_ground();
                false
            }
        }
    }

    fn csi_ignore(&mut self, byte: u8) -> bool {
        match byte {
            0x20..=0x3f => true,
            0x40..=0x7e => {
                self.enter_ground();
                true
            }
            _ => {
                self.enter_ground();
                false
            }
        }
    }

    fn finish_csi(&mut self, final_byte: u8, out: &mut Vec<InputEvent>) {
        let decoded = csi::decode(&self.seq, final_byte);
        match decoded {
            csi::Decoded::Key(ev) => self.emit_sequence_key(ev, out),
            csi::Decoded::Event(event) => {
                self.emit_prefix(out);
                out.push(event);
            }
            csi::Decoded::PasteStart => {
                self.emit_prefix(out);
                self.enter_ground();
                self.paste.clear();
                self.paste_match = 0;
                self.paste_truncated = false;
                self.state = State::Paste;
                return;
            }
            csi::Decoded::Ignore => self.emit_prefix(out),
        }
        self.enter_ground();
    }

    fn ss3(&mut self, byte: u8, out: &mut Vec<InputEvent>) -> bool {
        if let Some(ev) = keys::ss3(byte) {
            self.emit_sequence_key(ev, out);
            self.enter_ground();
            return true;
        }
        // No such SS3 key, so the `ESC O` was alt+shift+o and this byte is on its own.
        self.emit_prefix(out);
        out.push(alt_char('O'));
        self.enter_ground();
        false
    }

    fn x10_mouse(&mut self, byte: u8, out: &mut Vec<InputEvent>) -> bool {
        if byte < 0x20 {
            // The report's bytes are all offset by 32, so this one is not part of it.
            self.enter_ground();
            return false;
        }
        self.seq.push(byte);
        if self.seq.len() == 3 {
            let report = mouse::decode_x10(self.seq[0], self.seq[1], self.seq[2], self.x10_button);
            self.emit_prefix(out);
            if let Some((event, button)) = report {
                self.x10_button = button;
                out.push(InputEvent::Mouse(event));
            }
            self.enter_ground();
        }
        true
    }

    fn string(&mut self, kind: StringKind, byte: u8, out: &mut Vec<InputEvent>) -> bool {
        match byte {
            ESC => self.state = State::StrEsc(kind),
            BEL if kind == StringKind::Osc => self.finish_string(kind, out),
            _ if self.seq.len() >= self.max_string_len => {
                self.seq.clear();
                self.state = State::StrIgnore(kind);
            }
            _ => self.seq.push(byte),
        }
        true
    }

    fn string_esc(&mut self, kind: StringKind, byte: u8, out: &mut Vec<InputEvent>) -> bool {
        if byte == b'\\' {
            self.finish_string(kind, out);
            return true;
        }
        // The ESC began a new sequence instead of the terminator. An empty string was no
        // string at all but alt + the introducer character.
        if self.seq.is_empty() {
            out.push(alt_char(kind.introducer()));
        }
        self.enter_escape();
        false
    }

    fn string_ignore(&mut self, kind: StringKind, byte: u8) -> bool {
        match byte {
            ESC => self.state = State::StrIgnoreEsc,
            BEL if kind == StringKind::Osc => self.enter_ground(),
            _ => {}
        }
        true
    }

    fn string_ignore_esc(&mut self, byte: u8) -> bool {
        if byte == b'\\' {
            self.enter_ground();
            return true;
        }
        // As in `string_esc`, the ESC began something new.
        self.enter_escape();
        false
    }

    fn finish_string(&mut self, kind: StringKind, out: &mut Vec<InputEvent>) {
        strings::dispatch(kind, &self.seq, out);
        self.enter_ground();
    }

    /// Consume paste input from the front of `bytes` and return how many bytes that was
    /// (at least one). Runs of ordinary bytes are copied in one go.
    fn paste_bytes(&mut self, bytes: &[u8], out: &mut Vec<InputEvent>) -> usize {
        if self.paste_match == 0 {
            let plain = bytes.iter().position(|&b| b == ESC).unwrap_or(bytes.len());
            if plain > 0 {
                self.push_paste(&bytes[..plain]);
                return plain;
            }
        }
        let byte = bytes[0];
        if byte == PASTE_END[self.paste_match] {
            self.paste_match += 1;
            if self.paste_match == PASTE_END.len() {
                self.finish_paste(out);
            }
        } else {
            // What looked like the end marker was paste content after all. The marker has
            // an ESC only at its start, so a new match can only begin at this byte.
            let matched = self.paste_match;
            self.paste_match = 0;
            self.push_paste(&PASTE_END[..matched]);
            if byte == ESC {
                self.paste_match = 1;
            } else {
                self.push_paste(&[byte]);
            }
        }
        1
    }

    /// Append to the paste body, up to the cap.
    fn push_paste(&mut self, bytes: &[u8]) {
        let room = self.max_paste_len.saturating_sub(self.paste.len());
        let take = room.min(bytes.len());
        self.paste.extend_from_slice(&bytes[..take]);
        if take < bytes.len() {
            self.paste_truncated = true;
        }
    }

    fn finish_paste(&mut self, out: &mut Vec<InputEvent>) {
        let mut body = std::mem::take(&mut self.paste);
        if self.paste_truncated {
            strings::trim_partial_utf8(&mut body);
        }
        self.paste_match = 0;
        self.paste_truncated = false;
        self.enter_ground();
        out.push(InputEvent::Paste(strings::paste_text(body)));
    }
}

/// The same key with alt held. Alt combinations type nothing.
fn with_alt(mut ev: KeyEvent) -> KeyEvent {
    ev.mods |= Mods::ALT;
    ev.text = None;
    ev
}

/// The press of alt + the key that types `c`.
fn alt_char(c: char) -> InputEvent {
    InputEvent::Key(with_alt(KeyEvent::typed(c)))
}

fn escape_press() -> InputEvent {
    InputEvent::Key(KeyEvent::new(Key::Escape, Mods::empty()))
}
