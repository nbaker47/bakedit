//! Plain text with and without the kitty all-keys flag.

use bakedit_core::{InputEvent, Key, KeyEvent, Mods};

use super::{key, typed, Expect, Parser};
use crate::host;

/// The event for text that belongs to no key.
fn text_only(text: &str) -> InputEvent {
    Expect::key(Key::Other(0)).text(text).event()
}

fn all_keys_parser() -> Parser {
    let mut parser = Parser::new();
    parser.set_kitty_flags(host::KITTY_ALL_FLAGS);
    parser
}

#[test]
fn plain_text_is_typing_by_default() {
    let mut parser = Parser::new();
    assert_eq!(parser.kitty_flags(), 0);
    assert_eq!(
        parser.feed("a;É".as_bytes()),
        vec![typed('a'), typed(';'), typed('É')]
    );
}

#[test]
fn with_all_keys_plain_text_belongs_to_no_key() {
    let mut parser = all_keys_parser();
    assert_eq!(parser.kitty_flags(), host::KITTY_ALL_FLAGS);
    assert_eq!(
        parser.feed("a;A é日😀".as_bytes()),
        vec![
            text_only("a"),
            text_only(";"),
            text_only("A"),
            text_only(" "),
            text_only("é"),
            text_only("日"),
            text_only("😀"),
        ]
    );
    // No shift is made up for it, and nothing that would need a release.
    let InputEvent::Key(ev) = &parser.feed(b":")[0] else {
        panic!("expected a key event");
    };
    assert_eq!(
        ev,
        &KeyEvent::new(Key::Other(0), Mods::empty()).with_text(":")
    );
    assert!(ev.is_press() && ev.shifted.is_none());
}

#[test]
fn with_all_keys_everything_else_means_what_it_always_means() {
    let none = Mods::empty();
    let mut parser = all_keys_parser();
    // Control characters are still the keys that send them.
    assert_eq!(
        parser.feed(b"\r\t\x7f\x03\x00"),
        vec![
            key(Key::Enter, none),
            key(Key::Tab, none),
            key(Key::Backspace, none),
            key(Key::Char('c'), Mods::CTRL),
            key(Key::Char(' '), Mods::CTRL),
        ]
    );
    // ESC and a character is still alt + that key, and a lone ESC still Escape.
    assert_eq!(parser.feed(b"\x1ba"), vec![key(Key::Char('a'), Mods::ALT)]);
    assert_eq!(
        parser.feed("\x1bé".as_bytes()),
        vec![key(Key::Char('é'), Mods::ALT)]
    );
    assert_eq!(parser.feed(b"\x1b"), vec![]);
    assert_eq!(parser.flush(), vec![key(Key::Escape, none)]);
    assert_eq!(parser.feed(b"\x1b["), vec![]);
    assert_eq!(parser.flush().len(), 1);
    // Keys reported as escape codes are keys, with their text.
    assert_eq!(parser.feed(b"\x1b[97;;97u"), vec![typed('a')]);
    assert_eq!(parser.feed(b"\x1b[59u"), vec![typed(';')]);
    assert_eq!(parser.feed(b"\x1b[A"), vec![key(Key::Up, none)]);
    assert_eq!(parser.feed(b"\x1bOP"), vec![key(Key::F(1), none)]);
    // kitty's own text-only event has the same shape as plain text.
    assert_eq!(parser.feed(b"\x1b[0;;229u"), vec![text_only("å")]);
    assert_eq!(parser.feed("å".as_bytes()), vec![text_only("å")]);
    // A paste is a paste.
    assert_eq!(
        parser.feed(b"\x1b[200~a;b\x1b[201~"),
        vec![InputEvent::Paste("a;b".into())]
    );
}

#[test]
fn only_the_all_keys_flag_changes_text() {
    let without = host::KITTY_DISAMBIGUATE
        | host::KITTY_EVENT_TYPES
        | host::KITTY_ALTERNATE_KEYS
        | host::KITTY_TEXT;
    let mut parser = Parser::new();
    parser.set_kitty_flags(without);
    assert_eq!(parser.feed(b"a"), vec![typed('a')]);
    parser.set_kitty_flags(host::KITTY_ALL_KEYS);
    assert_eq!(parser.feed(b"a"), vec![text_only("a")]);
    parser.set_kitty_flags(0);
    assert_eq!(parser.feed(b"a"), vec![typed('a')]);
}

#[test]
fn the_flags_survive_a_reset() {
    let mut parser = all_keys_parser();
    parser.feed(b"\x1b[1;5");
    parser.reset();
    assert_eq!(parser.kitty_flags(), host::KITTY_ALL_FLAGS);
    assert_eq!(parser.feed(b"A"), vec![text_only("A")]);
}

#[test]
fn text_in_all_keys_mode_parses_the_same_however_it_is_cut() {
    let stream = "a;\x1b[97;;97u日本\x1b[A é\r\x1bx😀\x1b[0;;12371u".as_bytes();
    let whole = all_keys_parser().feed(stream);
    assert_eq!(whole.len(), 12);
    for size in 1..stream.len() {
        let mut parser = all_keys_parser();
        let mut events = Vec::new();
        for chunk in stream.chunks(size) {
            events.extend(parser.feed(chunk));
        }
        assert_eq!(events, whole, "chunks of {size}");
    }
}
