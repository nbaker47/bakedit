//! Parser tests. The helpers here build the expected events; the cases live in the
//! submodules.

mod flush;
mod fuzz;
mod paste;
mod table;
mod terminals;
mod text;

use bakedit_core::{
    InputEvent, Key, KeyEvent, KeyKind, Mods, MouseEvent, MouseKind, TerminalReply,
};

use super::Parser;

/// Builds the key event a test expects.
#[derive(Debug, Clone)]
struct Expect(KeyEvent);

impl Expect {
    fn key(key: Key) -> Expect {
        Expect(KeyEvent::new(key, Mods::empty()))
    }

    /// A character key with no text.
    fn ch(c: char) -> Expect {
        Expect::key(Key::Char(c))
    }

    fn mods(mut self, mods: Mods) -> Expect {
        self.0.mods = mods;
        self
    }

    fn kind(mut self, kind: KeyKind) -> Expect {
        self.0.kind = kind;
        self
    }

    fn text(mut self, text: &str) -> Expect {
        self.0.text = Some(text.to_string());
        self
    }

    fn shifted(mut self, c: char) -> Expect {
        self.0.shifted = Some(c);
        self
    }

    fn base(mut self, c: char) -> Expect {
        self.0.base = Some(c);
        self
    }

    fn keypad(mut self) -> Expect {
        self.0.keypad = true;
        self
    }

    fn caps_lock(mut self) -> Expect {
        self.0.caps_lock = true;
        self
    }

    fn num_lock(mut self) -> Expect {
        self.0.num_lock = true;
        self
    }

    fn event(self) -> InputEvent {
        InputEvent::Key(self.0)
    }
}

/// A press of `key` with `mods` and nothing else set.
fn key(key: Key, mods: Mods) -> InputEvent {
    Expect::key(key).mods(mods).event()
}

/// The press that typing `c` produces.
fn typed(c: char) -> InputEvent {
    InputEvent::Key(KeyEvent::typed(c))
}

/// The press that typing `c` with alt held produces: the same key, alt added, no text.
fn alt_typed(c: char) -> InputEvent {
    let mut ev = KeyEvent::typed(c);
    ev.mods |= Mods::ALT;
    ev.text = None;
    InputEvent::Key(ev)
}

fn mouse(kind: MouseKind, col: u16, row: u16, mods: Mods) -> InputEvent {
    InputEvent::Mouse(MouseEvent {
        kind,
        col,
        row,
        mods,
    })
}

fn reply(reply: TerminalReply) -> InputEvent {
    InputEvent::Reply(reply)
}

/// `ESC [` followed by `rest`.
fn csi(rest: &str) -> Vec<u8> {
    [b"\x1b[", rest.as_bytes()].concat()
}

/// Feed `bytes` to a new parser in one call. The input must be complete: nothing may be
/// left pending.
fn parse(bytes: &[u8]) -> Vec<InputEvent> {
    let mut parser = Parser::new();
    let events = parser.feed(bytes);
    assert!(!parser.has_pending(), "input left pending: {bytes:?}");
    assert!(!parser.in_paste(), "input left a paste open: {bytes:?}");
    events
}

/// Feed `bytes` to a new parser and then flush it, as the app does after the timeout.
fn parse_and_flush(bytes: &[u8]) -> Vec<InputEvent> {
    let mut parser = Parser::new();
    let mut events = parser.feed(bytes);
    events.extend(parser.flush());
    assert!(!parser.has_pending(), "flush left input pending: {bytes:?}");
    events
}
