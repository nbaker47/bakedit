//! Character classes and word boundaries.
//!
//! Word motions follow Zed: text is a sequence of runs of word characters, punctuation and
//! whitespace, and a motion stops where the run changes. Sub-word motions also stop inside
//! `camelCase` and `snake_case` identifiers.

use std::collections::VecDeque;

use ropey::iter::Chars;
use ropey::Rope;
use unicode_width::UnicodeWidthChar;

use crate::graphemes;

/// The three classes a character can belong to. The order matters: when a cursor sits
/// between two runs, the higher class wins (a word beats punctuation beats whitespace).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CharKind {
    /// Spaces, tabs, line breaks and every other Unicode whitespace.
    Whitespace,
    /// Anything that is neither whitespace nor a word character.
    Punctuation,
    /// Letters, digits, `_`, and the language's extra word characters.
    Word,
}

/// The class of `c`. `extra_word_chars` lists characters that count as part of a word on
/// top of letters, digits and `_` (for example `-` in CSS or `$` in JavaScript).
pub fn char_kind(c: char, extra_word_chars: &str) -> CharKind {
    if c.is_alphanumeric()
        || c == '_'
        || (!extra_word_chars.is_empty() && extra_word_chars.contains(c))
    {
        CharKind::Word
    } else if c.is_whitespace() {
        CharKind::Whitespace
    } else {
        CharKind::Punctuation
    }
}

/// Whether `c` takes no cell of its own (a combining mark, a joiner, a variation selector).
/// Such a character belongs to the character before it, so it takes that character's class.
fn is_zero_width(c: char) -> bool {
    c > '\u{7f}' && UnicodeWidthChar::width(c) == Some(0)
}

/// Characters after a position, each with its class. A zero-width character takes the class
/// of the character before it, so a letter and its accent stay in one run.
struct Forward<'a> {
    chars: Chars<'a>,
    extra: &'a str,
    last: Option<CharKind>,
}

impl<'a> Forward<'a> {
    fn new(rope: &'a Rope, pos: usize, extra: &'a str) -> Forward<'a> {
        let pos = pos.min(rope.len_chars());
        let last = kind_before(rope, pos, extra);
        Forward {
            chars: rope.chars_at(pos),
            extra,
            last,
        }
    }
}

impl Iterator for Forward<'_> {
    type Item = (char, CharKind);

    fn next(&mut self) -> Option<(char, CharKind)> {
        let c = self.chars.next()?;
        let kind = match self.last {
            Some(last) if is_zero_width(c) => last,
            _ => char_kind(c, self.extra),
        };
        self.last = Some(kind);
        Some((c, kind))
    }
}

/// Characters before a position, nearest first, each with its class (see [`Forward`]).
struct Backward<'a> {
    chars: Chars<'a>,
    extra: &'a str,
    /// Zero-width characters already read, waiting for the character that owns them.
    pending: VecDeque<char>,
    pending_kind: CharKind,
    /// The owner of the pending characters, yielded after them.
    owner: Option<(char, CharKind)>,
}

impl<'a> Backward<'a> {
    fn new(rope: &'a Rope, pos: usize, extra: &'a str) -> Backward<'a> {
        let pos = pos.min(rope.len_chars());
        Backward {
            chars: rope.chars_at(pos),
            extra,
            pending: VecDeque::new(),
            pending_kind: CharKind::Punctuation,
            owner: None,
        }
    }
}

impl Iterator for Backward<'_> {
    type Item = (char, CharKind);

    fn next(&mut self) -> Option<(char, CharKind)> {
        if let Some(c) = self.pending.pop_front() {
            return Some((c, self.pending_kind));
        }
        if let Some(owner) = self.owner.take() {
            return Some(owner);
        }
        let c = self.chars.prev()?;
        if !is_zero_width(c) {
            return Some((c, char_kind(c, self.extra)));
        }
        // Collect the run of zero-width characters, then find who owns it.
        loop {
            match self.chars.prev() {
                Some(next) if is_zero_width(next) => self.pending.push_back(next),
                Some(next) => {
                    let kind = char_kind(next, self.extra);
                    self.pending_kind = kind;
                    self.owner = Some((next, kind));
                    break;
                }
                None => {
                    self.pending_kind = char_kind(c, self.extra);
                    break;
                }
            }
        }
        Some((c, self.pending_kind))
    }
}

/// The class of the character just before `pos`, or `None` at the start of the text.
pub fn kind_before(rope: &Rope, pos: usize, extra_word_chars: &str) -> Option<CharKind> {
    Backward::new(rope, pos, extra_word_chars)
        .next()
        .map(|(_, kind)| kind)
}

/// The class of the character at `pos`, or `None` at the end of the text.
pub fn kind_at(rope: &Rope, pos: usize, extra_word_chars: &str) -> Option<CharKind> {
    Forward::new(rope, pos, extra_word_chars)
        .next()
        .map(|(_, kind)| kind)
}

/// Walk forward from `pos`. The first character is always passed; after that the walk stops
/// at the first pair of neighbours for which `is_boundary(left, right, after_right)` holds.
/// Returns the position between that pair, or the end of the text.
fn scan_forward(
    rope: &Rope,
    pos: usize,
    extra: &str,
    mut is_boundary: impl FnMut((char, CharKind), (char, CharKind), Option<char>) -> bool,
) -> usize {
    let mut offset = pos.min(rope.len_chars());
    let mut iter = Forward::new(rope, offset, extra).peekable();
    let mut prev: Option<(char, CharKind)> = None;
    while let Some(current) = iter.next() {
        if let Some(left) = prev {
            let after = iter.peek().map(|(c, _)| *c);
            if is_boundary(left, current, after) {
                break;
            }
        }
        offset += 1;
        prev = Some(current);
    }
    graphemes::ceil_boundary(rope, offset)
}

/// Walk backward from `pos`, the mirror image of [`scan_forward`]. `is_boundary` receives
/// `(left, right, after_right)` in reading order.
fn scan_backward(
    rope: &Rope,
    pos: usize,
    extra: &str,
    mut is_boundary: impl FnMut((char, CharKind), (char, CharKind), Option<char>) -> bool,
) -> usize {
    let mut offset = pos.min(rope.len_chars());
    let mut after_right: Option<char> = rope.get_char(offset);
    let mut right: Option<(char, CharKind)> = None;
    for left in Backward::new(rope, offset, extra) {
        if let Some(right) = right {
            if is_boundary(left, right, after_right) {
                break;
            }
            after_right = Some(right.0);
        }
        offset -= 1;
        right = Some(left);
    }
    graphemes::floor_boundary(rope, offset)
}

/// Where `editor::MoveToPreviousWordStart` goes from `pos`: the start of the word the cursor
/// is in or just after. Punctuation directly before the cursor is skipped together with the
/// word before it (`hello.|` goes to `|hello.`), and the walk never crosses an empty line.
pub fn previous_word_start(rope: &Rope, pos: usize, extra_word_chars: &str) -> usize {
    let mut first = true;
    scan_backward(rope, pos, extra_word_chars, |left, right, _| {
        let skip = first
            && right.1 == CharKind::Punctuation
            && left.1 != CharKind::Punctuation
            && left.0 != '\n';
        first = false;
        if skip {
            return false;
        }
        (left.1 != right.1 && right.1 != CharKind::Whitespace) || left.0 == '\n'
    })
}

/// Like [`previous_word_start`] but also stops on either side of a line break. Word deletion
/// uses this so one keystroke never removes more than a line break.
pub fn previous_word_start_or_newline(rope: &Rope, pos: usize, extra_word_chars: &str) -> usize {
    scan_backward(rope, pos, extra_word_chars, |left, right, _| {
        (left.1 != right.1 && right.1 != CharKind::Whitespace) || left.0 == '\n' || right.0 == '\n'
    })
}

/// Where `editor::MoveToNextWordEnd` goes from `pos`: the end of the word the cursor is in
/// or just before. Punctuation directly after the cursor is skipped together with the word
/// after it (`|.hello` goes to `.hello|`), and the walk stops at the end of a line.
pub fn next_word_end(rope: &Rope, pos: usize, extra_word_chars: &str) -> usize {
    let mut first = true;
    scan_forward(rope, pos, extra_word_chars, |left, right, _| {
        let skip = first
            && left.1 == CharKind::Punctuation
            && right.1 != CharKind::Punctuation
            && right.0 != '\n';
        first = false;
        if skip {
            return false;
        }
        (left.1 != right.1 && left.1 != CharKind::Whitespace) || right.0 == '\n'
    })
}

/// Like [`next_word_end`] but also stops on either side of a line break.
pub fn next_word_end_or_newline(rope: &Rope, pos: usize, extra_word_chars: &str) -> usize {
    scan_forward(rope, pos, extra_word_chars, |left, right, _| {
        (left.1 != right.1 && left.1 != CharKind::Whitespace) || left.0 == '\n' || right.0 == '\n'
    })
}

/// Whether a sub-word starts between `left` and `right`: after an underscore, at a lower to
/// upper case step (`camel|Case`), or before the last capital of an acronym (`HTTP|Server`).
fn is_subword_start(left: char, right: char, after_right: Option<char>) -> bool {
    (left == '_' && right != '_')
        || ((left.is_lowercase() || left.is_numeric()) && right.is_uppercase())
        || (left.is_uppercase()
            && right.is_uppercase()
            && after_right.is_some_and(char::is_lowercase))
}

/// Whether a sub-word ends between `left` and `right`: before an underscore, at a lower to
/// upper case step, or before the last capital of an acronym.
fn is_subword_end(left: char, right: char, after_right: Option<char>) -> bool {
    (left != '_' && right == '_')
        || ((left.is_lowercase() || left.is_numeric()) && right.is_uppercase())
        || (left.is_uppercase()
            && right.is_uppercase()
            && after_right.is_some_and(char::is_lowercase))
}

/// Where `editor::MoveToPreviousSubwordStart` goes from `pos`: like
/// [`previous_word_start`] without the punctuation skip, and stopping inside identifiers at
/// `snake_|case` and `camel|Case` steps.
pub fn previous_subword_start(rope: &Rope, pos: usize, extra_word_chars: &str) -> usize {
    scan_backward(rope, pos, extra_word_chars, |left, right, after| {
        let word_start = left.1 != right.1 && right.1 != CharKind::Whitespace;
        let subword_start = left.1 == CharKind::Word
            && right.1 == CharKind::Word
            && is_subword_start(left.0, right.0, after);
        word_start || subword_start || left.0 == '\n'
    })
}

/// Like [`previous_subword_start`] but also stops on either side of a line break.
pub fn previous_subword_start_or_newline(rope: &Rope, pos: usize, extra_word_chars: &str) -> usize {
    scan_backward(rope, pos, extra_word_chars, |left, right, after| {
        let word_start = left.1 != right.1 && right.1 != CharKind::Whitespace;
        let subword_start = left.1 == CharKind::Word
            && right.1 == CharKind::Word
            && is_subword_start(left.0, right.0, after);
        word_start || subword_start || left.0 == '\n' || right.0 == '\n'
    })
}

/// Where `editor::MoveToNextSubwordEnd` goes from `pos`: like [`next_word_end`] without the
/// punctuation skip, and stopping inside identifiers at `snake|_case` and `camel|Case`
/// steps.
pub fn next_subword_end(rope: &Rope, pos: usize, extra_word_chars: &str) -> usize {
    scan_forward(rope, pos, extra_word_chars, |left, right, after| {
        let word_end = left.1 != right.1 && left.1 != CharKind::Whitespace;
        let subword_end = left.1 == CharKind::Word
            && right.1 == CharKind::Word
            && is_subword_end(left.0, right.0, after);
        word_end || subword_end || right.0 == '\n'
    })
}

/// Like [`next_subword_end`] but also stops on either side of a line break.
pub fn next_subword_end_or_newline(rope: &Rope, pos: usize, extra_word_chars: &str) -> usize {
    scan_forward(rope, pos, extra_word_chars, |left, right, after| {
        let word_end = left.1 != right.1 && left.1 != CharKind::Whitespace;
        let subword_end = left.1 == CharKind::Word
            && right.1 == CharKind::Word
            && is_subword_end(left.0, right.0, after);
        word_end || subword_end || left.0 == '\n' || right.0 == '\n'
    })
}

/// Where `editor::MoveToNextWordStart` (vim's `w`) goes from `pos`: past the rest of the
/// current word or punctuation run, then past whitespace, to the start of the next run. An
/// empty line counts as a word, so the walk stops there. Returns the end of the text when
/// nothing follows.
pub fn next_word_start(rope: &Rope, pos: usize, extra_word_chars: &str) -> usize {
    let len = rope.len_chars();
    let mut offset = pos.min(len);
    let mut iter = Forward::new(rope, offset, extra_word_chars).peekable();
    let Some(&(_, start_kind)) = iter.peek() else {
        return len;
    };
    if start_kind != CharKind::Whitespace {
        while let Some(&(_, kind)) = iter.peek() {
            if kind != start_kind {
                break;
            }
            iter.next();
            offset += 1;
        }
    }
    let mut previous_was_newline = false;
    while let Some(&(c, kind)) = iter.peek() {
        if kind != CharKind::Whitespace {
            break;
        }
        if c == '\n' && previous_was_newline {
            // The cursor is at the start of an empty line: stop here.
            break;
        }
        previous_was_newline = c == '\n';
        iter.next();
        offset += 1;
    }
    graphemes::ceil_boundary(rope, offset)
}

/// The run of same-class characters around `pos`, as Zed's "surrounding word": when the
/// cursor is between two runs the higher class wins (word, then punctuation, then
/// whitespace). The run never crosses a line break. Returns the range and its class; the
/// range is empty only in an empty text or on an empty line.
pub fn surrounding_run(
    rope: &Rope,
    pos: usize,
    extra_word_chars: &str,
) -> (std::ops::Range<usize>, CharKind) {
    let pos = pos.min(rope.len_chars());
    let before = Backward::new(rope, pos, extra_word_chars)
        .next()
        .filter(|(c, _)| *c != '\n');
    let after = Forward::new(rope, pos, extra_word_chars)
        .next()
        .filter(|(c, _)| *c != '\n');
    let kind = match (before, after) {
        (Some((_, a)), Some((_, b))) => a.max(b),
        (Some((_, a)), None) => a,
        (None, Some((_, b))) => b,
        (None, None) => return (pos..pos, CharKind::Whitespace),
    };
    let mut start = pos;
    for (c, k) in Backward::new(rope, pos, extra_word_chars) {
        if k != kind || c == '\n' {
            break;
        }
        start -= 1;
    }
    let mut end = pos;
    for (c, k) in Forward::new(rope, pos, extra_word_chars) {
        if k != kind || c == '\n' {
            break;
        }
        end += 1;
    }
    (
        graphemes::floor_boundary(rope, start)..graphemes::ceil_boundary(rope, end),
        kind,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Split a marked string ("foo |bar") into the text and the marker's char offset.
    fn marked(text: &str) -> (Rope, usize) {
        let pos = text.chars().position(|c| c == '|').expect("a | marker");
        (Rope::from_str(&text.replacen('|', "", 1)), pos)
    }

    /// Put the marker back at `pos`.
    fn show(rope: &Rope, pos: usize) -> String {
        let mut out: String = rope.chars().take(pos).collect();
        out.push('|');
        out.extend(rope.chars().skip(pos));
        out
    }

    fn check(f: fn(&Rope, usize, &str) -> usize, before: &str, after: &str) {
        let (rope, pos) = marked(before);
        assert_eq!(show(&rope, f(&rope, pos, "")), after, "from {before:?}");
    }

    #[test]
    fn kinds() {
        assert_eq!(char_kind('a', ""), CharKind::Word);
        assert_eq!(char_kind('9', ""), CharKind::Word);
        assert_eq!(char_kind('_', ""), CharKind::Word);
        assert_eq!(char_kind('é', ""), CharKind::Word);
        assert_eq!(char_kind('世', ""), CharKind::Word);
        assert_eq!(char_kind('-', ""), CharKind::Punctuation);
        assert_eq!(char_kind('-', "-$"), CharKind::Word);
        assert_eq!(char_kind('$', "-$"), CharKind::Word);
        assert_eq!(char_kind(' ', ""), CharKind::Whitespace);
        assert_eq!(char_kind('\n', ""), CharKind::Whitespace);
        assert_eq!(char_kind('\t', ""), CharKind::Whitespace);
        assert_eq!(char_kind('🙂', ""), CharKind::Punctuation);
        assert!(
            CharKind::Word > CharKind::Punctuation && CharKind::Punctuation > CharKind::Whitespace
        );
    }

    #[test]
    fn previous_word_start_matches_zed() {
        check(previous_word_start, "hello world|", "hello |world");
        check(previous_word_start, "hello |world", "|hello world");
        check(previous_word_start, "hello wor|ld", "hello |world");
        check(previous_word_start, "hello.|", "|hello.");
        check(previous_word_start, "a.b.|", "a.|b.");
        check(previous_word_start, "foo  ..|", "foo  |..");
        check(previous_word_start, "foo\n|bar", "|foo\nbar");
        check(previous_word_start, "foo\n\n|bar", "foo\n|\nbar");
        check(previous_word_start, "|foo", "|foo");
        check(previous_word_start, "x = foo(|", "x = |foo(");
        check(previous_word_start, "   |", "|   ");
    }

    #[test]
    fn next_word_end_matches_zed() {
        check(next_word_end, "|hello world", "hello| world");
        check(next_word_end, "hello| world", "hello world|");
        check(next_word_end, "|.hello", ".hello|");
        check(next_word_end, "|..hello", "..|hello");
        check(next_word_end, "foo|\nbar", "foo\nbar|");
        check(next_word_end, "|foo\nbar", "foo|\nbar");
        check(next_word_end, "foo|", "foo|");
        check(next_word_end, "foo|   ", "foo   |");
        check(next_word_end, "a|\n\nb", "a\n|\nb");
    }

    #[test]
    fn word_deletion_stops_at_line_breaks() {
        check(previous_word_start_or_newline, "foo\n|bar", "foo|\nbar");
        check(previous_word_start_or_newline, "foo |\nbar", "|foo \nbar");
        check(previous_word_start_or_newline, "foo\n  |bar", "foo\n|  bar");
        check(next_word_end_or_newline, "foo|\nbar", "foo\n|bar");
        check(next_word_end_or_newline, "foo\n|bar baz", "foo\nbar| baz");
        check(next_word_end_or_newline, "foo|  \nbar", "foo  |\nbar");
    }

    #[test]
    fn subword_motions() {
        check(next_subword_end, "|fooBarBaz", "foo|BarBaz");
        check(next_subword_end, "foo|BarBaz", "fooBar|Baz");
        check(next_subword_end, "|foo_bar", "foo|_bar");
        check(next_subword_end, "foo|_bar", "foo_bar|");
        check(next_subword_end, "|HTTPServer x", "HTTP|Server x");
        check(next_subword_end, "HTTP|Server x", "HTTPServer| x");
        check(next_subword_end, "|foo2Bar", "foo2|Bar");
        check(next_subword_end, "|foo.bar", "foo|.bar");
        check(next_subword_end, "|ABC", "ABC|");

        check(previous_subword_start, "fooBarBaz|", "fooBar|Baz");
        check(previous_subword_start, "fooBar|Baz", "foo|BarBaz");
        check(previous_subword_start, "foo_bar|", "foo_|bar");
        check(previous_subword_start, "foo_|bar", "|foo_bar");
        check(previous_subword_start, "HTTPServer|", "HTTP|Server");
        check(previous_subword_start, "HTTP|Server", "|HTTPServer");
        check(previous_subword_start, "foo.bar|", "foo.|bar");
        check(previous_subword_start, "a\n|b", "|a\nb");
        check(previous_subword_start_or_newline, "a\n|b", "a|\nb");
        check(next_subword_end_or_newline, "a|\nbC", "a\n|bC");
    }

    #[test]
    fn next_word_start_is_vims_w() {
        check(next_word_start, "|foo bar", "foo |bar");
        check(next_word_start, "fo|o bar", "foo |bar");
        check(next_word_start, "foo| bar", "foo |bar");
        check(next_word_start, "|foo.bar", "foo|.bar");
        check(next_word_start, "foo|.bar", "foo.|bar");
        check(next_word_start, "foo |  bar", "foo   |bar");
        check(next_word_start, "foo|\n  bar", "foo\n  |bar");
        check(next_word_start, "foo|\n\nbar", "foo\n|\nbar");
        check(next_word_start, "foo\n|\nbar", "foo\n\n|bar");
        check(next_word_start, "foo|", "foo|");
        check(next_word_start, "foo|  ", "foo  |");
    }

    #[test]
    fn extra_word_characters_join_words() {
        let (rope, pos) = marked("|foo-bar baz");
        assert_eq!(next_word_end(&rope, pos, ""), 3);
        assert_eq!(next_word_end(&rope, pos, "-"), 7);
        assert_eq!(previous_word_start(&rope, 7, "-"), 0);
    }

    #[test]
    fn combining_marks_and_emoji_stay_whole() {
        // "résumé" written with combining accents is one word.
        let text = "re\u{301}sume\u{301} x";
        let rope = Rope::from_str(text);
        assert_eq!(next_word_end(&rope, 0, ""), 8);
        assert_eq!(previous_word_start(&rope, 8, ""), 0);
        assert_eq!(next_subword_end(&rope, 0, ""), 8);
        assert_eq!(surrounding_run(&rope, 3, ""), (0..8, CharKind::Word));

        // A family emoji is one punctuation run, never split at its joiners.
        let rope = Rope::from_str("a 👨‍👩‍👧 b");
        assert_eq!(next_word_end(&rope, 1, ""), 7);
        assert_eq!(previous_word_start(&rope, 7, ""), 2);
        assert_eq!(next_word_start(&rope, 2, ""), 8);
    }

    #[test]
    fn surrounding_run_prefers_the_higher_class() {
        let rope = Rope::from_str("foo.bar  baz\n\nq");
        assert_eq!(surrounding_run(&rope, 1, ""), (0..3, CharKind::Word));
        assert_eq!(surrounding_run(&rope, 3, ""), (0..3, CharKind::Word));
        assert_eq!(surrounding_run(&rope, 4, ""), (4..7, CharKind::Word));
        assert_eq!(surrounding_run(&rope, 8, ""), (7..9, CharKind::Whitespace));
        assert_eq!(surrounding_run(&rope, 12, ""), (9..12, CharKind::Word));
        assert_eq!(
            surrounding_run(&rope, 13, ""),
            (13..13, CharKind::Whitespace)
        );
        assert_eq!(surrounding_run(&rope, 15, ""), (14..15, CharKind::Word));
        let rope = Rope::from_str("a => b");
        assert_eq!(surrounding_run(&rope, 3, ""), (2..4, CharKind::Punctuation));
        assert_eq!(
            surrounding_run(&Rope::new(), 0, ""),
            (0..0, CharKind::Whitespace)
        );
    }

    #[test]
    fn out_of_range_positions_are_clamped() {
        let rope = Rope::from_str("ab cd");
        assert_eq!(previous_word_start(&rope, 99, ""), 3);
        assert_eq!(next_word_end(&rope, 99, ""), 5);
        assert_eq!(next_word_start(&rope, 99, ""), 5);
        assert_eq!(kind_before(&rope, 0, ""), None);
        assert_eq!(kind_at(&rope, 5, ""), None);
        assert_eq!(kind_at(&rope, 2, ""), Some(CharKind::Whitespace));
    }
}
