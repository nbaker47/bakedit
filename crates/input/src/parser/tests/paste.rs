//! Bracketed paste: arrival in pieces, newline normalisation and the size cap.

use bakedit_core::InputEvent;

use super::super::{strings, MAX_PASTE_LEN, MAX_STRING_LEN};
use super::{typed, Parser};

const START: &[u8] = b"\x1b[200~";
const END: &[u8] = b"\x1b[201~";

fn paste_events(text: &str) -> Vec<InputEvent> {
    vec![InputEvent::Paste(text.to_string())]
}

/// A parser whose paste cap is `cap` bytes.
fn capped(cap: usize) -> Parser {
    let mut parser = Parser::new();
    parser.max_paste_len = cap;
    parser
}

#[test]
fn the_limits_are_what_the_documentation_says() {
    assert_eq!(MAX_STRING_LEN, 64 * 1024);
    assert_eq!(MAX_PASTE_LEN, 64 * 1024 * 1024);
    let parser = Parser::new();
    assert_eq!(parser.max_string_len, MAX_STRING_LEN);
    assert_eq!(parser.max_paste_len, MAX_PASTE_LEN);
}

#[test]
fn a_paste_arrives_over_many_feeds() {
    let body = "fn main() {\r\n    println!(\"héllo\");\r\n}\r\n";
    let bytes = [START, body.as_bytes(), END].concat();
    for size in 1..=bytes.len() {
        let mut parser = Parser::new();
        let mut events = Vec::new();
        for chunk in bytes.chunks(size) {
            events.extend(parser.feed(chunk));
        }
        assert_eq!(
            events,
            paste_events("fn main() {\n    println!(\"héllo\");\n}\n"),
            "chunks of {size}"
        );
        assert!(!parser.in_paste());
    }
}

#[test]
fn a_megabyte_paste_comes_through_intact() {
    let line = "0123456789 the quick brown fox jumps over the lazy dog ñ 日本\r\n";
    let body = line.repeat(1024 * 1024 / line.len() + 1);
    let bytes = [START, body.as_bytes(), END].concat();
    let mut parser = Parser::new();
    let mut events = Vec::new();
    for chunk in bytes.chunks(4093) {
        events.extend(parser.feed(chunk));
    }
    assert_eq!(events, paste_events(&body.replace("\r\n", "\n")));
    // The buffer went with the event.
    assert_eq!(parser.paste.capacity(), 0);
}

#[test]
fn newlines_are_normalised_across_chunk_boundaries() {
    let mut parser = Parser::new();
    parser.feed(START);
    parser.feed(b"a\r");
    parser.feed(b"\nb\r");
    parser.feed(b"c\r");
    assert_eq!(parser.feed(END), paste_events("a\nb\nc\n"));
}

#[test]
fn a_paste_over_the_cap_is_truncated() {
    let mut parser = capped(10);
    let bytes = [START, b"0123456789abcdefghij", END, b"x"].concat();
    assert_eq!(
        parser.feed(&bytes),
        vec![InputEvent::Paste("0123456789".into()), typed('x')]
    );

    // The next paste starts from nothing and is not marked as truncated.
    let bytes = [START, b"short", END].concat();
    assert_eq!(parser.feed(&bytes), paste_events("short"));
    assert!(!parser.paste_truncated);

    // Exactly at the cap nothing is lost.
    let bytes = [START, b"0123456789", END].concat();
    assert_eq!(parser.feed(&bytes), paste_events("0123456789"));
}

#[test]
fn the_cap_holds_however_the_paste_is_chunked() {
    let body: Vec<u8> = (0..200u8).map(|i| b'a' + i % 26).collect();
    let bytes = [START, &body[..], END].concat();
    for size in 1..=40 {
        let mut parser = capped(37);
        let mut events = Vec::new();
        for chunk in bytes.chunks(size) {
            events.extend(parser.feed(chunk));
            assert!(parser.paste.len() <= 37);
        }
        let expected = String::from_utf8(body[..37].to_vec()).unwrap();
        assert_eq!(events, paste_events(&expected), "chunks of {size}");
    }
}

#[test]
fn end_marker_lookalikes_count_against_the_cap_too() {
    // Each `ESC [ 2 0` is held back as a possible end marker and then turns out to be content.
    let mut parser = capped(6);
    let bytes = [START, b"ab\x1b[20\x1b[20\x1b[20", END].concat();
    assert_eq!(parser.feed(&bytes), paste_events("ab\x1b[20"));
}

#[test]
fn truncation_does_not_leave_half_a_character() {
    // "日本" is six bytes; a cap of four cuts the second character after its first byte.
    let mut parser = capped(4);
    let bytes = [START, "日本".as_bytes(), END].concat();
    assert_eq!(parser.feed(&bytes), paste_events("日"));

    let mut parser = capped(5);
    let bytes = [START, "ab😀".as_bytes(), END].concat();
    assert_eq!(parser.feed(&bytes), paste_events("ab"));

    // A cut that falls between characters keeps everything before it.
    let mut parser = capped(3);
    let bytes = [START, "日本".as_bytes(), END].concat();
    assert_eq!(parser.feed(&bytes), paste_events("日"));
}

#[test]
fn trimming_a_partial_character() {
    let trimmed = |bytes: &[u8]| {
        let mut bytes = bytes.to_vec();
        strings::trim_partial_utf8(&mut bytes);
        bytes
    };
    assert_eq!(trimmed(b""), b"");
    assert_eq!(trimmed(b"abc"), b"abc");
    assert_eq!(trimmed("é".as_bytes()), "é".as_bytes());
    assert_eq!(trimmed(b"a\xc3"), b"a");
    assert_eq!(trimmed(b"a\xe6\x97"), b"a");
    assert_eq!(trimmed(b"a\xf0\x9f\x98"), b"a");
    assert_eq!(trimmed(b"a\xf0"), b"a");
    assert_eq!(trimmed("a😀".as_bytes()), "a😀".as_bytes());
    // Not the end of anything valid: left for the lossy conversion.
    assert_eq!(trimmed(b"\x80\x80\x80\x80\x80"), b"\x80\x80\x80\x80\x80");
    assert_eq!(trimmed(b"a\x80"), b"a\x80");
}

#[test]
fn a_paste_at_the_real_cap_is_delivered_truncated() {
    let mut parser = Parser::new();
    parser.feed(START);
    let chunk = vec![b'a'; 1024 * 1024];
    for _ in 0..(MAX_PASTE_LEN / chunk.len() + 1) {
        assert_eq!(parser.feed(&chunk), vec![]);
        assert!(parser.paste.len() <= MAX_PASTE_LEN);
    }
    let events = parser.feed(END);
    assert_eq!(events.len(), 1);
    let InputEvent::Paste(text) = &events[0] else {
        panic!("expected a paste, got {:?}", events[0]);
    };
    assert_eq!(text.len(), MAX_PASTE_LEN);
    assert!(text.bytes().all(|b| b == b'a'));
}
