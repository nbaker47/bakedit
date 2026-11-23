//! Deterministic fuzzing: pseudo-random input must never panic, hang, grow the parser
//! without bound or produce an event that breaks the parser's promises, and must parse the
//! same however it is cut up.

use bakedit_core::{InputEvent, Key, KeyKind};

use super::super::{MAX_CSI_LEN, MAX_STRING_LEN};
use super::Parser;

/// xorshift64*: small, fast and the same on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// A number in `0..bound`.
    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }

    fn byte(&mut self) -> u8 {
        (self.next() >> 32) as u8
    }
}

/// The pieces escape sequences are made of. A stream built from these reaches the parser's
/// deeper states far more often than uniformly random bytes do.
const FRAGMENTS: &[&[u8]] = &[
    b"\x1b",
    b"\x1b",
    b"\x1b[",
    b"\x1b[",
    b"\x1b[<",
    b"\x1b[?",
    b"\x1b[>",
    b"\x1bO",
    b"\x1b]",
    b"\x1bP",
    b"\x1b_",
    b"\x1b^",
    b"\x1bX",
    b"\x1b\\",
    b"\x07",
    b"\x1b[200~",
    b"\x1b[201~",
    b"\x1b[M",
    b"0",
    b"1",
    b"2",
    b"5",
    b"9",
    b"27",
    b"97",
    b"201",
    b"997",
    b"57399",
    b"57441",
    b"4294967295",
    b"99999999999999999999",
    b";",
    b";",
    b":",
    b"<",
    b"?",
    b">",
    b"=",
    b"$",
    b" ",
    b"!",
    b"u",
    b"~",
    b"A",
    b"M",
    b"m",
    b"R",
    b"Z",
    b"I",
    b"O",
    b"c",
    b"n",
    b"y",
    b"q",
    b"a",
    b"x",
    b"\r",
    b"\n",
    b"\t",
    b"\x00",
    b"\x7f",
    b"\x18",
    b"10;rgb:",
    b"11;rgb:12/34/56",
    b"4;1;",
    b">|",
    b"ffff/",
    b"#abc",
    "é".as_bytes(),
    "日".as_bytes(),
    "😀".as_bytes(),
    b"\xc3",
    b"\xe6\x97",
    b"\xf0\x9f",
    b"\x80",
    b"\xff",
    b"\xc2\x9b",
];

/// A stream of about `len` bytes made of fragments, with the odd random byte thrown in.
fn fragment_stream(rng: &mut Rng, len: usize) -> Vec<u8> {
    let mut stream = Vec::with_capacity(len + 32);
    while stream.len() < len {
        if rng.below(10) == 0 {
            stream.push(rng.byte());
        } else {
            stream.extend_from_slice(FRAGMENTS[rng.below(FRAGMENTS.len())]);
        }
    }
    stream
}

/// The promises every event keeps, whatever the input was.
fn check_event(event: &InputEvent) {
    let InputEvent::Key(ev) = event else {
        return;
    };
    if let Key::Char(c) = ev.key {
        assert!(!c.is_control(), "control character as a key: {ev:?}");
    }
    if let Some(text) = &ev.text {
        assert!(!text.is_empty(), "empty text: {ev:?}");
        assert!(
            !text.chars().any(char::is_control),
            "control character in text: {ev:?}"
        );
        assert_ne!(ev.kind, KeyKind::Release, "a release with text: {ev:?}");
    }
}

/// The promises the parser's own state keeps between calls.
fn check_parser(parser: &Parser) {
    assert!(parser.seq.len() <= MAX_STRING_LEN.max(MAX_CSI_LEN));
    assert!(parser.paste.len() <= parser.max_paste_len);
    assert!(parser.paste_match < super::super::PASTE_END.len());
}

/// Feed `stream` in random chunks, flushing now and then as the app's timer would.
fn run(rng: &mut Rng, parser: &mut Parser, stream: &[u8]) -> usize {
    let mut events = 0;
    let mut at = 0;
    while at < stream.len() {
        let size = 1 + rng.below(64).min(stream.len() - at - 1);
        for event in parser.feed(&stream[at..at + size]) {
            check_event(&event);
            events += 1;
        }
        at += size;
        check_parser(parser);
        if rng.below(8) == 0 {
            for event in parser.flush() {
                check_event(&event);
                events += 1;
            }
            assert!(!parser.has_pending(), "a flush must leave nothing pending");
            check_parser(parser);
        }
    }
    events
}

#[test]
fn random_bytes_do_not_panic() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let stream: Vec<u8> = (0..400_000).map(|_| rng.byte()).collect();
    let mut parser = Parser::new();
    let events = run(&mut rng, &mut parser, &stream);
    assert!(
        events > 100_000,
        "random bytes are mostly keys, got {events} events"
    );
}

#[test]
fn random_escape_fragments_do_not_panic() {
    let mut rng = Rng(0x0123_4567_89ab_cdef);
    let stream = fragment_stream(&mut rng, 400_000);
    let mut parser = Parser::new();
    // A small paste cap, so the cap itself is exercised too.
    parser.max_paste_len = 512;
    let events = run(&mut rng, &mut parser, &stream);
    assert!(events > 10_000, "got {events} events");
}

#[test]
fn random_input_parses_the_same_however_it_is_cut() {
    let mut rng = Rng(0xdead_beef_cafe_f00d);
    for round in 0..40 {
        let stream = if round % 4 == 0 {
            (0..5_000).map(|_| rng.byte()).collect()
        } else {
            fragment_stream(&mut rng, 5_000)
        };
        let whole = Parser::new().feed(&stream);

        let mut parser = Parser::new();
        let mut pieces = Vec::new();
        let mut at = 0;
        while at < stream.len() {
            let size = 1 + rng.below(16).min(stream.len() - at - 1);
            pieces.extend(parser.feed(&stream[at..at + size]));
            at += size;
        }
        assert_eq!(pieces, whole, "round {round}");

        let mut parser = Parser::new();
        let mut single = Vec::new();
        for byte in &stream {
            single.extend(parser.feed(std::slice::from_ref(byte)));
        }
        assert_eq!(single, whole, "round {round}, byte by byte");
    }
}

#[test]
fn every_two_and_three_byte_input_is_handled() {
    // Exhaustive over the bytes that steer the state machine, in every state a short
    // prefix can reach.
    let steering: Vec<u8> = (0u8..=255)
        .filter(|b| {
            matches!(
                b,
                0x00 | 0x07 | 0x18 | 0x1b | 0x20 | 0x7f | 0x80 | 0xc3 | 0xe6 | 0xf0 | 0xff
            ) || (0x30..=0x7e).contains(b)
        })
        .collect();
    let prefixes: &[&[u8]] = &[
        b"",
        b"\x1b",
        b"\x1b\x1b",
        b"\x1b[",
        b"\x1b[1",
        b"\x1b[<",
        b"\x1bO",
        b"\x1b]",
        b"\x1b]1",
        b"\x1bP",
        b"\x1b[M",
        b"\x1b[200~",
        b"\xe6",
    ];
    for prefix in prefixes {
        for &first in &steering {
            for &second in &steering {
                let mut parser = Parser::new();
                let mut events = parser.feed(prefix);
                events.extend(parser.feed(&[first, second]));
                events.extend(parser.flush());
                assert!(!parser.has_pending());
                events.extend(parser.flush());
                events.iter().for_each(check_event);
                check_parser(&parser);
            }
        }
    }
}
