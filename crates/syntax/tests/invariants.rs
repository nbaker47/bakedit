//! What must hold for any input, however broken: spans are well formed, the per-line view
//! agrees with them, bracket marks make sense, and nothing panics.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bakedit_syntax::{
    bracket_marks, languages, outline, spans_in, BracketMark, HighlightSpan, Highlighter, Language,
    LineBreaks, LineHighlights, SyntaxError,
};
use common::{language, Random, SAMPLES};

/// Check everything the documentation promises about a list of spans.
fn assert_spans_are_well_formed(context: &str, text: &str, spans: &[HighlightSpan]) {
    let mut previous: Option<&HighlightSpan> = None;
    for span in spans {
        assert!(span.start < span.end, "{context}: empty span {span:?}");
        assert!(
            span.end <= text.len(),
            "{context}: {span:?} runs past the end ({})",
            text.len()
        );
        assert!(
            text.is_char_boundary(span.start) && text.is_char_boundary(span.end),
            "{context}: {span:?} splits a character"
        );
        assert!(
            !span.capture.name().is_empty(),
            "{context}: {span:?} has no capture name"
        );
        if let Some(previous) = previous {
            assert!(
                previous.end <= span.start,
                "{context}: {previous:?} overlaps {span:?}"
            );
            assert!(
                previous.end < span.start || previous.capture != span.capture,
                "{context}: {previous:?} and {span:?} should have been joined"
            );
        }
        previous = Some(span);
    }
}

/// Check that the per-line view holds exactly the bytes the spans do, minus line breaks.
fn assert_lines_agree(context: &str, text: &str, spans: &[HighlightSpan], breaks: LineBreaks) {
    let lines = LineHighlights::with_line_breaks(text, spans, breaks);
    let line_starts = line_starts(text, breaks);
    assert_eq!(
        lines.line_count(),
        line_starts.len(),
        "{context}: line count"
    );
    let mut rebuilt: Vec<(usize, usize, &str)> = Vec::new();
    for (line, &(start, content_end)) in line_starts.iter().enumerate() {
        let mut column = 0;
        for (range, capture) in lines.spans_for_line(line) {
            assert!(
                range.start < range.end,
                "{context}: empty piece on line {line}"
            );
            assert!(
                range.start >= column,
                "{context}: pieces overlap on line {line}"
            );
            assert!(
                start + range.end <= content_end,
                "{context}: a piece leaves line {line}"
            );
            column = range.end;
            rebuilt.push((start + range.start, start + range.end, capture.name()));
        }
    }
    // The same bytes, cut at line ends, straight from the spans.
    let mut expected: Vec<(usize, usize, &str)> = Vec::new();
    for span in spans {
        for &(start, content_end) in &line_starts {
            let (from, to) = (span.start.max(start), span.end.min(content_end));
            if from < to {
                expected.push((from, to, span.capture.name()));
            }
        }
    }
    expected.sort_unstable();
    assert_eq!(
        rebuilt, expected,
        "{context}: the per-line view differs from the spans"
    );
}

/// Each line as `(start, end of content)`, worked out the slow, obvious way.
fn line_starts(text: &str, breaks: LineBreaks) -> Vec<(usize, usize)> {
    // Every line break and whether only a rope counts it.
    const BREAKS: &[(&str, bool)] = &[
        ("\r\n", false),
        ("\n", false),
        ("\r", true),
        ("\u{b}", true),
        ("\u{c}", true),
        ("\u{85}", true),
        ("\u{2028}", true),
        ("\u{2029}", true),
    ];
    let breaks_here = |at: usize| -> usize {
        BREAKS
            .iter()
            .filter(|(_, rope_only)| !rope_only || breaks == LineBreaks::Unicode)
            .find(|(line_break, _)| text[at..].starts_with(line_break))
            .map_or(0, |(line_break, _)| line_break.len())
    };
    let mut lines = Vec::new();
    let mut start = 0;
    let mut at = 0;
    while at < text.len() {
        if !text.is_char_boundary(at) {
            at += 1;
            continue;
        }
        let length = breaks_here(at);
        if length > 0 {
            lines.push((start, at));
            at += length;
            start = at;
        } else {
            at += 1;
        }
    }
    lines.push((start, text.len()));
    lines
}

/// Check that bracket marks are in order, sit on brackets, and pair up consistently.
fn assert_brackets_make_sense(context: &str, text: &str, marks: &[BracketMark]) {
    let bytes = text.as_bytes();
    let mut open: Vec<(u8, u16)> = Vec::new();
    let mut previous = None;
    for mark in marks {
        assert!(
            previous < Some(mark.byte),
            "{context}: marks out of order at {}",
            mark.byte
        );
        previous = Some(mark.byte);
        let character = bytes[mark.byte];
        // A bar opens or closes depending on where it stands; the rest face one way.
        if character != b'|' {
            assert_eq!(
                mark.open,
                matches!(character, b'(' | b'[' | b'{' | b'<'),
                "{context}: {mark:?}"
            );
        }
        assert!(
            matches!(
                character,
                b'(' | b')' | b'[' | b']' | b'{' | b'}' | b'<' | b'>' | b'|'
            ),
            "{context}: {mark:?}"
        );
        if mark.open && mark.matched {
            open.push((character, mark.depth));
        } else if mark.matched {
            let (opener, depth) = open
                .pop()
                .unwrap_or_else(|| panic!("{context}: {mark:?} has no opener"));
            let pairs = matches!(
                (opener, character),
                (b'(', b')') | (b'[', b']') | (b'{', b'}') | (b'<', b'>') | (b'|', b'|')
            );
            assert!(pairs, "{context}: {mark:?} closes a {:?}", opener as char);
            assert_eq!(
                depth, mark.depth,
                "{context}: {mark:?} is not at its opener's depth"
            );
        }
    }
    assert!(
        open.is_empty(),
        "{context}: {} matched openers were never closed",
        open.len()
    );
}

/// Highlight `text` and check every invariant, in both line-break modes.
fn check_everything(
    highlighter: &mut Highlighter,
    language: &'static Language,
    context: &str,
    text: &str,
) {
    let spans = highlighter
        .highlight(language, text, None)
        .unwrap_or_else(|error| panic!("{context}: {error}"));
    assert_spans_are_well_formed(context, text, &spans);
    assert_lines_agree(context, text, &spans, LineBreaks::Unicode);
    assert_lines_agree(context, text, &spans, LineBreaks::Lf);
    assert_brackets_make_sense(context, text, &bracket_marks(text, &spans));
    assert_brackets_make_sense(context, text, &bracket_marks(text, &[]));
    for symbol in outline(language, text) {
        assert!(
            symbol.byte <= text.len() && text.is_char_boundary(symbol.byte),
            "{context}: {symbol:?}"
        );
        assert!(!symbol.name.is_empty(), "{context}: {symbol:?}");
    }
}

/// Text that tends to upset parsers, to splice into samples.
const SNIPPETS: &[&str] = &[
    "(",
    ")",
    "[",
    "]",
    "{",
    "}",
    "\"",
    "'",
    "`",
    "\\",
    "/*",
    "*/",
    "//",
    "#",
    "<",
    ">",
    "</",
    "\n",
    "\r\n",
    "\t",
    " ",
    "\u{0}",
    "\u{feff}",
    "é",
    "日本語",
    "🦀",
    "\u{2028}",
    "\u{c}",
    "```",
    "${",
    "#{",
    "<?php",
    "?>",
    "<script>",
    "</script>",
    "<!--",
    "-->",
    "'''",
    "\"\"\"",
    "<<EOF\n",
    "fn ",
    "if ",
    "end\n",
    "@",
    "$(",
    "=>",
    "::",
    "0x",
    "1e",
    "%",
    "\\n",
    "\\u{",
    "r#\"",
    "macro!(",
    "---\n",
    "+++\n",
    "> ",
    "- ",
    "# ",
];

/// Apply one random edit to `text`, keeping it valid UTF-8.
fn mangle(text: &mut String, random: &mut Random) {
    let boundary = |text: &str, random: &mut Random| -> usize {
        let mut at = random.below(text.len() + 1);
        while !text.is_char_boundary(at) {
            at -= 1;
        }
        at
    };
    match random.below(5) {
        // Delete a short range.
        0 => {
            let (a, b) = (boundary(text, random), boundary(text, random));
            let (start, end) = (a.min(b), a.max(b).min(a.min(b) + 40));
            let mut end = end;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.replace_range(start..end, "");
        }
        // Insert a snippet.
        1 | 2 => {
            let at = boundary(text, random);
            text.insert_str(at, SNIPPETS[random.below(SNIPPETS.len())]);
        }
        // Duplicate a range somewhere else.
        3 => {
            let (a, b) = (boundary(text, random), boundary(text, random));
            let piece = text[a.min(b)..a.max(b)]
                .chars()
                .take(60)
                .collect::<String>();
            let at = boundary(text, random);
            text.insert_str(at, &piece);
        }
        // Cut the text short.
        _ => {
            let at = boundary(text, random);
            if at > text.len() / 2 {
                text.truncate(at);
            }
        }
    }
}

#[test]
fn every_sample_gives_well_formed_spans() {
    let mut highlighter = Highlighter::new();
    for sample in SAMPLES {
        check_everything(
            &mut highlighter,
            language(sample.language),
            sample.language,
            sample.text,
        );
    }
}

#[test]
fn randomly_mangled_samples_give_well_formed_spans() {
    let mut highlighter = Highlighter::new();
    let mut random = Random::new(0x5EED_BA5E);
    for sample in SAMPLES {
        let language = language(sample.language);
        for round in 0..40 {
            let mut text = sample.text.to_owned();
            for _ in 0..1 + random.below(6) {
                mangle(&mut text, &mut random);
            }
            let context = format!("{} (round {round})", sample.language);
            check_everything(&mut highlighter, language, &context, &text);
        }
    }
}

#[test]
fn samples_survive_being_cut_off_anywhere() {
    // Typing a file from top to bottom passes through every prefix of it.
    let mut highlighter = Highlighter::new();
    for sample in SAMPLES {
        let language = language(sample.language);
        let step = (sample.text.len() / 60).max(1);
        for end in (0..sample.text.len())
            .step_by(step)
            .filter(|&end| sample.text.is_char_boundary(end))
        {
            let context = format!("{} cut at {end}", sample.language);
            check_everything(&mut highlighter, language, &context, &sample.text[..end]);
        }
    }
}

#[test]
fn every_sample_is_fine_in_every_language() {
    // The wrong grammar for a file is the most broken input there is.
    let mut highlighter = Highlighter::new();
    for language in languages() {
        for sample in SAMPLES.iter().step_by(3) {
            let context = format!("{} read as {}", sample.language, language.id);
            check_everything(&mut highlighter, language, &context, sample.text);
        }
    }
}

#[test]
fn degenerate_inputs_are_fine_in_every_language() {
    let deep_open = "(".repeat(3000);
    let deep_nest = format!("{}{}", "[{(".repeat(1000), ")}]".repeat(1000));
    let long_line = "x = 1; ".repeat(20_000);
    let many_lines = "\n".repeat(20_000);
    let inputs: Vec<(&str, &str)> = vec![
        ("empty", ""),
        ("one space", " "),
        ("one newline", "\n"),
        ("carriage returns", "\r\r\n\r"),
        ("a nul", "\u{0}"),
        ("nuls in text", "a\u{0}b\u{0}\u{0}c"),
        ("a byte order mark", "\u{feff}fn main() {}\n"),
        ("only multibyte", "日本語のテキスト🦀"),
        ("odd line ends", "a\u{2028}b\u{2029}c\u{85}d\u{b}e\u{c}f"),
        ("unclosed string", "\"never closed\n\n\n"),
        ("unclosed comment", "/* never closed\n\n"),
        ("unclosed fence", "```rust\nfn main() {\n"),
        ("deep open", &deep_open),
        ("deep nest", &deep_nest),
        ("long line", &long_line),
        ("many lines", &many_lines),
    ];
    let mut highlighter = Highlighter::new();
    for language in languages() {
        for (name, text) in &inputs {
            let context = format!("{name} as {}", language.id);
            check_everything(&mut highlighter, language, &context, text);
        }
    }
}

#[test]
fn crlf_line_ends_change_nothing_but_offsets() {
    let mut highlighter = Highlighter::new();
    for sample in SAMPLES {
        // Markdown, YAML and commit messages are line-oriented enough that a `\r` is content.
        if matches!(
            sample.language,
            "markdown" | "yaml" | "git_commit" | "make" | "diff" | "dockerfile"
        ) {
            continue;
        }
        let language = language(sample.language);
        let crlf = sample.text.replace('\n', "\r\n");
        check_everything(&mut highlighter, language, sample.language, &crlf);
        let unix = highlighter.highlight(language, sample.text, None).unwrap();
        let dos = highlighter.highlight(language, &crlf, None).unwrap();
        let words = |text: &str, spans: &[HighlightSpan]| -> Vec<(String, &'static str)> {
            spans
                .iter()
                .map(|span| {
                    (
                        text[span.range()].replace("\r\n", "\n"),
                        span.capture.name(),
                    )
                })
                .filter(|(piece, _)| !piece.trim().is_empty())
                .map(|(piece, name)| (piece.trim_end().to_owned(), name))
                .collect()
        };
        assert_eq!(
            words(sample.text, &unix),
            words(&crlf, &dos),
            "{}",
            sample.language
        );
    }
}

#[test]
fn highlighting_is_deterministic_and_a_highlighter_can_be_reused() {
    let mut shared = Highlighter::new();
    let first: Vec<Vec<HighlightSpan>> = SAMPLES
        .iter()
        .map(|sample| {
            shared
                .highlight(language(sample.language), sample.text, None)
                .unwrap()
        })
        .collect();
    // Again, in reverse order, with the same highlighter and with fresh ones.
    for (sample, expected) in SAMPLES.iter().zip(&first).rev() {
        let language = language(sample.language);
        assert_eq!(
            &shared.highlight(language, sample.text, None).unwrap(),
            expected,
            "{}",
            sample.language
        );
        assert_eq!(
            &Highlighter::new()
                .highlight(language, sample.text, None)
                .unwrap(),
            expected,
            "{}",
            sample.language
        );
    }
}

#[test]
fn highlighters_on_several_threads_agree() {
    let expected: Vec<Vec<HighlightSpan>> = SAMPLES
        .iter()
        .map(|sample| common::highlight(sample.language, sample.text))
        .collect();
    let expected = Arc::new(expected);
    let threads: Vec<_> = (0..6)
        .map(|offset| {
            let expected = Arc::clone(&expected);
            std::thread::spawn(move || {
                let mut highlighter = Highlighter::new();
                for round in 0..3 {
                    for index in 0..SAMPLES.len() {
                        let index = (index + offset * 5 + round) % SAMPLES.len();
                        let sample = &SAMPLES[index];
                        let spans = highlighter
                            .highlight(language(sample.language), sample.text, None)
                            .unwrap();
                        assert_eq!(spans, expected[index], "{}", sample.language);
                    }
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
}

#[test]
fn a_language_without_a_grammar_gives_no_spans() {
    let mut highlighter = Highlighter::new();
    for language in languages()
        .iter()
        .filter(|language| !language.has_grammar())
    {
        assert_eq!(
            highlighter.highlight(language, "fn main() { \"x\" }\n# c\n", None),
            Ok(Vec::new())
        );
        assert!(outline(language, "fn main() {}\n").is_empty());
    }
}

#[test]
fn a_cancelled_call_returns_the_typed_error_and_the_highlighter_recovers() {
    let rust = language("rust");
    let mut highlighter = Highlighter::new();
    let cancelled = AtomicBool::new(true);
    assert_eq!(
        highlighter.highlight(rust, common::RUST, Some(&cancelled)),
        Err(SyntaxError::Cancelled)
    );
    // Even for an empty text, and for every language with a grammar.
    for language in languages().iter().filter(|language| language.has_grammar()) {
        assert_eq!(
            highlighter.highlight(language, "", Some(&cancelled)),
            Err(SyntaxError::Cancelled)
        );
    }
    // A flag that stays clear changes nothing.
    let clear = AtomicBool::new(false);
    let expected = Highlighter::new()
        .highlight(rust, common::RUST, None)
        .unwrap();
    assert_eq!(
        highlighter
            .highlight(rust, common::RUST, Some(&clear))
            .unwrap(),
        expected
    );
    // And after a cancellation the same highlighter gives the full answer again.
    assert_eq!(
        highlighter.highlight(rust, common::RUST, Some(&cancelled)),
        Err(SyntaxError::Cancelled)
    );
    assert_eq!(
        highlighter.highlight(rust, common::RUST, None).unwrap(),
        expected
    );
}

#[test]
fn cancelling_from_another_thread_stops_a_long_call_promptly() {
    let rust = language("rust");
    let big: String = (0..60_000)
        .map(|i| {
            format!("fn f{i}(a: u32) -> u32 {{ let b = a + {i}; println!(\"{{}}\", b); b }}\n")
        })
        .collect();
    let cancel = Arc::new(AtomicBool::new(false));
    let setter = {
        let cancel = Arc::clone(&cancel);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            cancel.store(true, Ordering::Relaxed);
        })
    };
    let mut highlighter = Highlighter::new();
    let started = Instant::now();
    let result = highlighter.highlight(rust, &big, Some(&cancel));
    let elapsed = started.elapsed();
    setter.join().unwrap();
    assert_eq!(
        result,
        Err(SyntaxError::Cancelled),
        "the call finished in {elapsed:?} before it could be cancelled"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "cancelling took {elapsed:?}"
    );
    // The interrupted parse must not leak into the next call.
    let small = "fn after() {}\n";
    let spans = highlighter.highlight(rust, small, None).unwrap();
    assert_eq!(
        common::capture_marked(small, &spans, "fn «after»"),
        "function"
    );
}

#[test]
fn spans_in_agrees_with_a_linear_scan() {
    let mut random = Random::new(7);
    for sample in SAMPLES {
        let spans = common::highlight(sample.language, sample.text);
        for _ in 0..50 {
            let (a, b) = (
                random.below(sample.text.len() + 20),
                random.below(sample.text.len() + 20),
            );
            let range = a.min(b)..a.max(b);
            let expected: Vec<&HighlightSpan> = spans
                .iter()
                .filter(|span| span.end > range.start && span.start < range.end)
                .collect();
            let found: Vec<&HighlightSpan> = spans_in(&spans, range.clone()).iter().collect();
            assert_eq!(found, expected, "{} {range:?}", sample.language);
        }
    }
}

#[test]
fn real_code_has_balanced_brackets_once_strings_and_comments_are_set_aside() {
    // Samples whose brackets all pair up when the highlights are used to skip the ones in
    // strings and comments.
    for id in [
        "rust",
        "python",
        "javascript",
        "typescript",
        "tsx",
        "json",
        "go",
        "c",
        "cpp",
        "java",
        "swift",
        "shell",
    ] {
        let sample = SAMPLES.iter().find(|sample| sample.language == id).unwrap();
        let spans = common::highlight(id, sample.text);
        let marks = bracket_marks(sample.text, &spans);
        let unmatched: Vec<&BracketMark> = marks.iter().filter(|mark| !mark.matched).collect();
        assert!(
            unmatched.is_empty(),
            "{id}: unmatched brackets at {unmatched:?}"
        );
        assert!(marks.len() >= 6, "{id}: only {} brackets", marks.len());
    }
    // The C sample has a `(` in a character literal and the shell sample two `)` that end
    // `case` patterns: without the highlights those would be brackets.
    for id in ["c", "shell"] {
        let sample = SAMPLES.iter().find(|sample| sample.language == id).unwrap();
        assert!(
            bracket_marks(sample.text, &[])
                .iter()
                .any(|mark| !mark.matched),
            "{id}"
        );
    }
}
