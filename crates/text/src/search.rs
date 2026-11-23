//! In-buffer search and replace.
//!
//! A [`SearchQuery`] describes what to look for; compiling it gives a [`Search`] that can be
//! run many times. Matching follows Zed and VS Code:
//!
//! - A pattern is searched line by line: a match never spans a line break, and in regex
//!   mode `^` and `$` are the start and end of a line. Only a pattern that itself contains a
//!   line break (a real one, or `\n` in regex mode) is matched against the whole text.
//! - Empty matches are never reported.
//! - Whole-word matching accepts a match only when it does not continue a word on either
//!   side.
//!
//! The text is searched as UTF-8 bytes with the `regex` crate and only the offsets of the
//! matches are converted to chars, so searching a large buffer costs little more than one
//! pass over it.

use std::borrow::Cow;
use std::ops::Range;

use regex::{Regex, RegexBuilder};
use ropey::Rope;
use thiserror::Error;

use crate::buffer::Buffer;
use crate::edit::EditSummary;
use crate::file;

/// What to search for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchQuery {
    /// The text or regular expression to find. An empty pattern finds nothing.
    pub pattern: String,
    /// Treat `pattern` as a regular expression (the `regex` crate's syntax) instead of
    /// plain text.
    pub regex: bool,
    /// Distinguish upper and lower case.
    pub case_sensitive: bool,
    /// Only accept matches that are whole words.
    pub whole_word: bool,
}

/// Why a query could not be compiled.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SearchError {
    /// The pattern is not a valid regular expression (or is too large to compile).
    #[error("invalid regular expression: {0}")]
    InvalidRegex(String),
}

impl SearchQuery {
    /// A case-sensitive plain-text query.
    pub fn text(pattern: impl Into<String>) -> SearchQuery {
        SearchQuery {
            pattern: pattern.into(),
            regex: false,
            case_sensitive: true,
            whole_word: false,
        }
    }

    /// A case-sensitive regular expression query.
    pub fn regex(pattern: impl Into<String>) -> SearchQuery {
        SearchQuery {
            pattern: pattern.into(),
            regex: true,
            case_sensitive: true,
            whole_word: false,
        }
    }

    /// Prepare the query for searching. An invalid regular expression is an error, never a
    /// panic.
    pub fn compile(&self) -> Result<Search, SearchError> {
        Search::new(self)
    }

    /// Every match inside `range` (see [`Search::find_all`]).
    pub fn find_all(
        &self,
        rope: &Rope,
        range: Range<usize>,
    ) -> Result<Vec<Range<usize>>, SearchError> {
        Ok(self.compile()?.find_all(rope, range))
    }

    /// The first match at or after `from` (see [`Search::find_next`]).
    pub fn find_next(
        &self,
        rope: &Rope,
        from: usize,
        wrap: bool,
    ) -> Result<Option<Range<usize>>, SearchError> {
        Ok(self.compile()?.find_next(rope, from, wrap))
    }

    /// The last match before `from` (see [`Search::find_prev`]).
    pub fn find_prev(
        &self,
        rope: &Rope,
        from: usize,
        wrap: bool,
    ) -> Result<Option<Range<usize>>, SearchError> {
        Ok(self.compile()?.find_prev(rope, from, wrap))
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// A compiled [`SearchQuery`]. Cheap to run repeatedly and safe to share between threads.
#[derive(Debug, Clone)]
pub struct Search {
    /// `None` for an empty pattern, which matches nothing.
    regex: Option<Regex>,
    is_regex: bool,
    whole_word: bool,
    /// The pattern can match across line breaks, so it is run on the whole text.
    multiline: bool,
    /// The pattern can be run over many lines at once with the same result as line by line.
    block_safe: bool,
}

/// How many chars of text [`Search::find_next`] and [`Search::find_prev`] look at per step.
const WINDOW_CHARS: usize = 64 * 1024;

impl Search {
    /// Compile `query`.
    pub fn new(query: &SearchQuery) -> Result<Search, SearchError> {
        let pattern: Cow<'_, str> = if query.regex {
            Cow::Borrowed(query.pattern.as_str())
        } else {
            file::normalize_newlines(&query.pattern)
        };
        let multiline = pattern.contains('\n') || (query.regex && pattern.contains("\\n"));
        let mut search = Search {
            regex: None,
            is_regex: query.regex,
            whole_word: query.whole_word,
            multiline,
            block_safe: false,
        };
        if pattern.is_empty() {
            return Ok(search);
        }
        let mut source = if query.regex {
            pattern.to_string()
        } else {
            regex::escape(&pattern)
        };
        if query.whole_word {
            // A boundary is only required on a side where the pattern visibly starts or
            // ends with a word character; `accepts` checks the rest on each match.
            let leading = pattern.chars().next().is_some_and(is_word_char);
            let trailing = pattern.chars().next_back().is_some_and(is_word_char);
            source = format!(
                "{}(?:{}){}",
                if leading { "\\b" } else { "" },
                source,
                if trailing { "\\b" } else { "" }
            );
        }
        let regex = RegexBuilder::new(&source)
            .case_insensitive(!query.case_sensitive)
            .multi_line(multiline)
            .build()
            .map_err(|error| SearchError::InvalidRegex(error.to_string()))?;
        search.regex = Some(regex);
        search.block_safe = !query.regex && !multiline;
        Ok(search)
    }

    /// Whether a match of `text[start..end]` passes the whole-word rule: it must not extend
    /// a word on either side.
    fn accepts(&self, text: &str, start: usize, end: usize) -> bool {
        if start == end {
            return false;
        }
        if !self.whole_word {
            return true;
        }
        let joins_left = text[..start].chars().next_back().is_some_and(is_word_char)
            && text[start..end].chars().next().is_some_and(is_word_char);
        let joins_right = text[end..].chars().next().is_some_and(is_word_char)
            && text[start..end]
                .chars()
                .next_back()
                .is_some_and(is_word_char);
        !joins_left && !joins_right
    }

    /// Find the matches in `hay`, a piece of the text that starts at char `base` of the
    /// rope and consists of whole lines. With `template`, each match also gets its
    /// replacement text.
    fn matches_in(
        &self,
        hay: &str,
        base: usize,
        ascii: bool,
        template: Option<&str>,
    ) -> Vec<Found> {
        let Some(regex) = &self.regex else {
            return Vec::new();
        };
        let mut found = Vec::new();
        let mut counter = CharCounter {
            hay,
            ascii,
            byte: 0,
            chars: base,
        };
        let expand = template.filter(|_| self.is_regex).map(unescape_template);
        let mut scan = |text: &str, offset: usize| match &expand {
            Some(expand) => {
                for captures in regex.captures_iter(text) {
                    let Some(whole) = captures.get(0) else {
                        continue;
                    };
                    if self.accepts(text, whole.start(), whole.end()) {
                        let mut replacement = String::new();
                        captures.expand(expand, &mut replacement);
                        let range = counter.char_at(offset + whole.start())
                            ..counter.char_at(offset + whole.end());
                        found.push(Found {
                            range,
                            replacement: Some(replacement),
                        });
                    }
                }
            }
            None => {
                for whole in regex.find_iter(text) {
                    if self.accepts(text, whole.start(), whole.end()) {
                        let range = counter.char_at(offset + whole.start())
                            ..counter.char_at(offset + whole.end());
                        found.push(Found {
                            range,
                            replacement: template.map(str::to_string),
                        });
                    }
                }
            }
        };
        if self.multiline || self.block_safe {
            scan(hay, 0);
        } else {
            let mut line_start = 0;
            for line_end in
                memchr::memchr_iter(b'\n', hay.as_bytes()).chain(std::iter::once(hay.len()))
            {
                scan(&hay[line_start..line_end], line_start);
                line_start = line_end + 1;
            }
        }
        found
    }

    /// Matches in the char range `lines` of the rope, which must start at a line start and
    /// end at a line end (or cover the whole text).
    fn matches_in_lines(
        &self,
        rope: &Rope,
        lines: Range<usize>,
        template: Option<&str>,
    ) -> Vec<Found> {
        if self.regex.is_none() || lines.start > lines.end {
            return Vec::new();
        }
        let ascii = rope.len_bytes() == rope.len_chars();
        let hay: Cow<'_, str> = rope.slice(lines.clone()).into();
        self.matches_in(&hay, lines.start, ascii, template)
    }

    /// Matches fully inside `range`, with replacements when `template` is given.
    fn collect(&self, rope: &Rope, range: Range<usize>, template: Option<&str>) -> Vec<Found> {
        let len = rope.len_chars();
        let end = range.end.min(len);
        let start = range.start.min(end);
        let lines = if self.multiline {
            0..len
        } else {
            line_start(rope, start)..line_end(rope, end)
        };
        let mut found = self.matches_in_lines(rope, lines, template);
        found.retain(|m| m.range.start >= start && m.range.end <= end);
        found
    }

    /// Every match that lies fully inside the char range `range`, in order, as char ranges.
    /// The range is clamped to the text. Matches never overlap and are never empty.
    pub fn find_all(&self, rope: &Rope, range: Range<usize>) -> Vec<Range<usize>> {
        self.collect(rope, range, None)
            .into_iter()
            .map(|found| found.range)
            .collect()
    }

    /// The first match in document order that starts in `starts` and that `accept` likes.
    /// The text is searched a window at a time so a nearby match is found quickly.
    fn first_match(
        &self,
        rope: &Rope,
        starts: Range<usize>,
        accept: impl Fn(&Range<usize>) -> bool,
    ) -> Option<Range<usize>> {
        let len = rope.len_chars();
        if self.multiline {
            return self
                .matches_in_lines(rope, 0..len, None)
                .into_iter()
                .map(|found| found.range)
                .find(|range| range.start >= starts.start && accept(range));
        }
        let mut window_start = line_start(rope, starts.start.min(len));
        while window_start <= starts.end.min(len) {
            let window_end = line_end(rope, window_start.saturating_add(WINDOW_CHARS).min(len));
            let found = self
                .matches_in_lines(rope, window_start..window_end, None)
                .into_iter()
                .map(|found| found.range)
                .find(|range| range.start >= starts.start && accept(range));
            if found.is_some() || window_end >= len {
                return found;
            }
            window_start = window_end + 1;
        }
        None
    }

    /// The last match in document order that starts in `starts` and that `accept` likes.
    fn last_match(
        &self,
        rope: &Rope,
        starts: Range<usize>,
        accept: impl Fn(&Range<usize>) -> bool,
    ) -> Option<Range<usize>> {
        let len = rope.len_chars();
        if self.multiline {
            return self
                .matches_in_lines(rope, 0..len, None)
                .into_iter()
                .map(|found| found.range)
                .rfind(|range| range.start <= starts.end && accept(range));
        }
        let mut window_end = line_end(rope, starts.end.min(len));
        loop {
            let window_start = line_start(rope, window_end.saturating_sub(WINDOW_CHARS));
            let found = self
                .matches_in_lines(rope, window_start..window_end, None)
                .into_iter()
                .map(|found| found.range)
                .rfind(|range| range.start <= starts.end && accept(range));
            if found.is_some() || window_start == 0 || window_start <= starts.start {
                return found;
            }
            window_end = window_start - 1;
        }
    }

    /// The first match that starts at or after char `from`. With `wrap`, when there is none
    /// the search continues from the start of the text and returns the first match before
    /// `from`. `None` when nothing matches.
    pub fn find_next(&self, rope: &Rope, from: usize, wrap: bool) -> Option<Range<usize>> {
        let len = rope.len_chars();
        let from = from.min(len);
        self.first_match(rope, from..len, |_| true).or_else(|| {
            if wrap && from > 0 {
                self.first_match(rope, 0..from, |range| range.start < from)
            } else {
                None
            }
        })
    }

    /// The last match that starts before char `from`. With `wrap`, when there is none the
    /// search continues from the end of the text and returns the last match at or after
    /// `from`. `None` when nothing matches.
    pub fn find_prev(&self, rope: &Rope, from: usize, wrap: bool) -> Option<Range<usize>> {
        let len = rope.len_chars();
        let from = from.min(len);
        let before = if from == 0 {
            None
        } else {
            self.last_match(rope, 0..from, |range| range.start < from)
        };
        before.or_else(|| {
            if wrap {
                self.last_match(rope, from..len, |range| range.start >= from)
            } else {
                None
            }
        })
    }

    /// The text that replaces the match at `found` for the replacement `template`.
    ///
    /// For a plain-text search the template is used as it is. For a regex search `$1`,
    /// `${1}` and `$name` insert capture groups, `$$` is a dollar sign, and `\n`, `\t` and
    /// `\\` in the template are a line break, a tab and a backslash. Returns `None` when
    /// `found` is not a match of this search.
    pub fn replacement(&self, rope: &Rope, found: Range<usize>, template: &str) -> Option<String> {
        self.collect(rope, found.clone(), Some(template))
            .into_iter()
            .find(|candidate| candidate.range == found)
            .and_then(|candidate| candidate.replacement)
    }

    /// Replace every match inside `range` of the buffer by `template` (see
    /// [`Search::replacement`]). All replacements are one edit batch: a single undo step,
    /// unless a transaction is already open. Returns what changed; map selections through
    /// it. The summary has no batch when nothing matched.
    pub fn replace_all(
        &self,
        buffer: &mut Buffer,
        range: Range<usize>,
        template: &str,
    ) -> EditSummary {
        let edits: Vec<(Range<usize>, String)> = self
            .collect(buffer.rope(), range, Some(template))
            .into_iter()
            .map(|found| (found.range, found.replacement.unwrap_or_default()))
            .collect();
        buffer.edit(&edits)
    }

    /// Replace the first match at or after `from` (wrapping around when `wrap` is set) by
    /// `template`. Returns the range the replacement text now occupies and what changed, or
    /// `None` when nothing matched.
    pub fn replace_next(
        &self,
        buffer: &mut Buffer,
        from: usize,
        wrap: bool,
        template: &str,
    ) -> Option<(Range<usize>, EditSummary)> {
        let found = self.find_next(buffer.rope(), from, wrap)?;
        let replacement = self.replacement(buffer.rope(), found.clone(), template)?;
        let summary = buffer.edit(&[(found, replacement)]);
        let replaced = summary.batches().first()?.first()?.new.clone();
        Some((replaced, summary))
    }
}

/// One match: where it is and, when a template was given, what replaces it.
#[derive(Debug)]
struct Found {
    range: Range<usize>,
    replacement: Option<String>,
}

/// Converts increasing byte offsets of a haystack to char offsets of the rope.
struct CharCounter<'a> {
    hay: &'a str,
    ascii: bool,
    /// The last byte offset converted, and the char offset it maps to.
    byte: usize,
    chars: usize,
}

impl CharCounter<'_> {
    fn char_at(&mut self, byte: usize) -> usize {
        if self.ascii {
            return self.chars + byte;
        }
        if byte > self.byte {
            self.chars += self.hay[self.byte..byte].chars().count();
            self.byte = byte;
        }
        self.chars
    }
}

/// Turn `\n`, `\t` and `\\` in a regex replacement template into the characters they name.
fn unescape_template(template: &str) -> String {
    let mut out = String::with_capacity(template.len());
    let mut chars = template.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// The char offset where the line containing `char_idx` starts: just after the previous
/// `\n`, or 0.
fn line_start(rope: &Rope, char_idx: usize) -> usize {
    let mut position = char_idx.min(rope.len_chars());
    let mut chars = rope.chars_at(position);
    while let Some(c) = chars.prev() {
        if c == '\n' {
            break;
        }
        position -= 1;
    }
    position
}

/// The char offset where the line containing `char_idx` ends: at the next `\n`, or the end
/// of the text.
fn line_end(rope: &Rope, char_idx: usize) -> usize {
    let mut position = char_idx.min(rope.len_chars());
    for c in rope.chars_at(position) {
        if c == '\n' {
            break;
        }
        position += 1;
    }
    position
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(pattern: &str) -> SearchQuery {
        SearchQuery::text(pattern)
    }

    fn regex(pattern: &str) -> SearchQuery {
        SearchQuery::regex(pattern)
    }

    fn all(query: &SearchQuery, haystack: &str) -> Vec<Range<usize>> {
        let rope = Rope::from_str(haystack);
        query.find_all(&rope, 0..rope.len_chars()).unwrap()
    }

    /// No matches, with the element type spelled out for `assert_eq!`.
    fn none() -> Vec<Range<usize>> {
        Vec::new()
    }

    /// The matched strings, for readable assertions.
    fn found(query: &SearchQuery, haystack: &str) -> Vec<String> {
        let chars: Vec<char> = haystack.chars().collect();
        all(query, haystack)
            .into_iter()
            .map(|range| chars[range].iter().collect())
            .collect()
    }

    #[test]
    fn plain_text_matches() {
        assert_eq!(
            all(&text("ab"), "ab cab abab"),
            vec![0..2, 4..6, 7..9, 9..11]
        );
        assert_eq!(all(&text("zz"), "ab cab"), none());
        assert_eq!(
            all(&text("a.b"), "a.b axb"),
            vec![0..3],
            "a dot is just a dot"
        );
        assert_eq!(all(&text("(x)"), "f(x) (x)"), vec![1..4, 5..8]);
        assert_eq!(all(&text(""), "anything"), none());
        assert_eq!(all(&text("a"), ""), none());
    }

    #[test]
    fn case_sensitivity() {
        let mut query = text("hello");
        assert_eq!(found(&query, "Hello hello HELLO"), vec!["hello"]);
        query.case_sensitive = false;
        assert_eq!(
            found(&query, "Hello hello HELLO"),
            vec!["Hello", "hello", "HELLO"]
        );
        let mut query = regex("h[a-z]+");
        query.case_sensitive = false;
        assert_eq!(found(&query, "Hey HO"), vec!["Hey", "HO"]);
        let mut query = text("straße");
        query.case_sensitive = false;
        assert_eq!(found(&query, "STRASSE Straße"), vec!["Straße"]);
    }

    #[test]
    fn whole_word() {
        let mut query = text("cat");
        query.whole_word = true;
        assert_eq!(
            all(&query, "cat concat cat_ cat, (cat)"),
            vec![0..3, 16..19, 22..25]
        );
        // A pattern with punctuation at its edge needs no boundary on that side.
        let mut query = text("foo(");
        query.whole_word = true;
        assert_eq!(all(&query, "foo() xfoo() foo(1"), vec![0..4, 13..17]);
        let mut query = text("->");
        query.whole_word = true;
        assert_eq!(all(&query, "a->b -> x"), vec![1..3, 5..7]);
        // Regex mode: classes and alternations are checked on the actual match.
        let mut query = regex("[a-z]+");
        query.whole_word = true;
        assert_eq!(found(&query, "foo1bar baz qux2"), vec!["baz"]);
        let mut query = regex("foo|foobar");
        query.whole_word = true;
        assert_eq!(found(&query, "foobar foo"), vec!["foobar", "foo"]);
        let mut query = regex(r"foo\(");
        query.whole_word = true;
        assert_eq!(found(&query, "foo() afoo("), vec!["foo("]);
        let mut query = text("日本");
        query.whole_word = true;
        assert_eq!(all(&query, "日本 日本語 x日本"), vec![0..2]);
    }

    #[test]
    fn regex_matches_and_anchors_are_per_line() {
        assert_eq!(
            found(&regex(r"\d+"), "a1 b22\nc333"),
            vec!["1", "22", "333"]
        );
        assert_eq!(all(&regex("^a"), "a\nba\na"), vec![0..1, 5..6]);
        assert_eq!(all(&regex("a$"), "a\nab\nba"), vec![0..1, 6..7]);
        assert_eq!(
            found(&regex("^.*$"), "one\n\ntwo"),
            vec!["one", "two"],
            "the empty line is skipped"
        );
        // Whitespace classes do not reach across a line break.
        assert_eq!(
            found(&regex(r"a\s+b"), "a b\na\nb a\t b"),
            vec!["a b", "a\t b"]
        );
        assert_eq!(found(&regex(r"[^x]+"), "ab\ncd"), vec!["ab", "cd"]);
        assert_eq!(all(&regex(r"\bfoo\b"), "foo\nfoo"), vec![0..3, 4..7]);
    }

    #[test]
    fn patterns_with_a_line_break_match_across_lines() {
        assert_eq!(all(&text("a\nb"), "a\nb a\nb"), vec![0..3, 4..7]);
        assert_eq!(all(&regex(r"a\nb"), "a\nb xa\nb"), vec![0..3, 5..8]);
        assert_eq!(
            found(&regex(r"^b\n"), "a\nb\nc"),
            vec!["b\n"],
            "^ still means line start"
        );
        assert_eq!(
            found(&regex(r";\n\s*}"), "x;\n  }\ny;\n}"),
            vec![";\n  }", ";\n}"]
        );
        // A pasted Windows line ending still finds the buffer's line feed.
        assert_eq!(all(&text("a\r\nb"), "a\nb"), vec![0..3]);
    }

    #[test]
    fn empty_matches_are_skipped() {
        assert_eq!(all(&regex("x*"), "axxb"), vec![1..3]);
        assert_eq!(all(&regex("^"), "a\nb"), none());
        assert_eq!(all(&regex("$"), "a\nb"), none());
    }

    #[test]
    fn invalid_regex_is_an_error() {
        let error = regex("(unclosed").compile().unwrap_err();
        assert!(matches!(error, SearchError::InvalidRegex(_)));
        assert!(error.to_string().starts_with("invalid regular expression"));
        assert!(regex("[z-a]").find_all(&Rope::from_str("x"), 0..1).is_err());
        assert!(regex("a{2,1}")
            .find_next(&Rope::from_str("x"), 0, true)
            .is_err());
        assert!(regex("*").find_prev(&Rope::from_str("x"), 0, true).is_err());
        // The same text is fine as plain text.
        assert_eq!(all(&text("(unclosed"), "an (unclosed paren"), vec![3..12]);
    }

    #[test]
    fn offsets_are_chars_not_bytes() {
        assert_eq!(all(&text("x"), "éx世x"), vec![1..2, 3..4]);
        assert_eq!(all(&text("世界"), "héllo 世界, 世界!"), vec![6..8, 10..12]);
        assert_eq!(all(&regex("é+"), "aéé\nbé"), vec![1..3, 5..6]);
        assert_eq!(all(&text("🙂"), "a🙂b🙂"), vec![1..2, 3..4]);
    }

    #[test]
    fn find_all_respects_the_range() {
        let rope = Rope::from_str("ab ab ab ab");
        let search = text("ab").compile().unwrap();
        assert_eq!(search.find_all(&rope, 3..8), vec![3..5, 6..8]);
        assert_eq!(
            search.find_all(&rope, 4..8),
            vec![6..8],
            "a match cut by the range start is left out"
        );
        assert_eq!(search.find_all(&rope, 3..7), vec![3..5]);
        assert_eq!(search.find_all(&rope, 9..999), vec![9..11]);
        assert_eq!(search.find_all(&rope, 50..60), none());
        // The range limits the matches, not the context: the anchor still sees its line.
        let rope = Rope::from_str("xab\nab");
        let search = regex("^ab").compile().unwrap();
        assert_eq!(search.find_all(&rope, 1..6), vec![4..6]);
        let search = regex(r"\bab").compile().unwrap();
        assert_eq!(search.find_all(&rope, 1..6), vec![4..6]);
    }

    #[test]
    fn find_next_and_wrap() {
        let rope = Rope::from_str("ab cd ab cd ab");
        let search = text("ab").compile().unwrap();
        assert_eq!(search.find_next(&rope, 0, false), Some(0..2));
        assert_eq!(search.find_next(&rope, 1, false), Some(6..8));
        assert_eq!(search.find_next(&rope, 6, false), Some(6..8));
        assert_eq!(search.find_next(&rope, 7, false), Some(12..14));
        assert_eq!(search.find_next(&rope, 13, false), None);
        assert_eq!(search.find_next(&rope, 13, true), Some(0..2));
        assert_eq!(search.find_next(&rope, 99, true), Some(0..2));
        assert_eq!(search.find_next(&rope, 0, true), Some(0..2));
        assert_eq!(text("zz").find_next(&rope, 3, true).unwrap(), None);
        assert_eq!(text("").find_next(&rope, 3, true).unwrap(), None);
        assert_eq!(text("a").find_next(&Rope::new(), 0, true).unwrap(), None);
    }

    #[test]
    fn find_prev_and_wrap() {
        let rope = Rope::from_str("ab cd ab cd ab");
        let search = text("ab").compile().unwrap();
        assert_eq!(search.find_prev(&rope, 14, false), Some(12..14));
        assert_eq!(
            search.find_prev(&rope, 13, false),
            Some(12..14),
            "a match under the cursor counts"
        );
        assert_eq!(search.find_prev(&rope, 12, false), Some(6..8));
        assert_eq!(search.find_prev(&rope, 6, false), Some(0..2));
        assert_eq!(search.find_prev(&rope, 0, false), None);
        assert_eq!(search.find_prev(&rope, 0, true), Some(12..14));
        assert_eq!(search.find_prev(&rope, 99, false), Some(12..14));
        assert_eq!(text("zz").find_prev(&rope, 3, true).unwrap(), None);
    }

    #[test]
    fn next_and_prev_with_multiline_patterns() {
        let rope = Rope::from_str("a\nb x a\nb y");
        let search = text("a\nb").compile().unwrap();
        assert_eq!(search.find_next(&rope, 0, false), Some(0..3));
        assert_eq!(search.find_next(&rope, 1, false), Some(6..9));
        assert_eq!(search.find_next(&rope, 7, true), Some(0..3));
        assert_eq!(search.find_next(&rope, 7, false), None);
        assert_eq!(search.find_prev(&rope, 6, false), Some(0..3));
        assert_eq!(search.find_prev(&rope, 0, true), Some(6..9));
    }

    #[test]
    fn next_and_prev_search_far_beyond_one_window() {
        // Matches sit several windows apart, and the lines are of very different lengths.
        let mut haystack = String::new();
        haystack.push_str("needle\n");
        for i in 0..6000 {
            haystack.push_str(&"filler ".repeat(1 + i % 9));
            haystack.push('\n');
        }
        haystack.push_str("x needle\n");
        haystack.push_str(&"long line without a break ".repeat(8000));
        haystack.push_str("\nneedle");
        let rope = Rope::from_str(&haystack);
        let search = text("needle").compile().unwrap();
        let expected = search.find_all(&rope, 0..rope.len_chars());
        assert_eq!(expected.len(), 3);
        assert_eq!(search.find_next(&rope, 0, false), Some(expected[0].clone()));
        assert_eq!(search.find_next(&rope, 1, false), Some(expected[1].clone()));
        assert_eq!(
            search.find_next(&rope, expected[1].start + 1, false),
            Some(expected[2].clone())
        );
        assert_eq!(search.find_next(&rope, expected[2].start + 1, false), None);
        assert_eq!(
            search.find_next(&rope, expected[2].start + 1, true),
            Some(expected[0].clone())
        );
        assert_eq!(
            search.find_prev(&rope, rope.len_chars(), false),
            Some(expected[2].clone())
        );
        assert_eq!(
            search.find_prev(&rope, expected[2].start, false),
            Some(expected[1].clone())
        );
        assert_eq!(
            search.find_prev(&rope, expected[1].start, false),
            Some(expected[0].clone())
        );
        assert_eq!(search.find_prev(&rope, expected[0].start, false), None);
        assert_eq!(
            search.find_prev(&rope, expected[0].start, true),
            Some(expected[2].clone())
        );

        // The regex path gives the same answers.
        let search = regex("ne+dle").compile().unwrap();
        assert_eq!(search.find_all(&rope, 0..rope.len_chars()), expected);
        assert_eq!(search.find_next(&rope, 1, false), Some(expected[1].clone()));
        assert_eq!(
            search.find_prev(&rope, expected[2].start, false),
            Some(expected[1].clone())
        );
    }

    #[test]
    fn replacement_text() {
        let rope = Rope::from_str("let foo_bar = baz(1, 2);");
        let search = regex(r"(\w+)\((\d+), (\d+)\)").compile().unwrap();
        let found = search.find_next(&rope, 0, false).unwrap();
        assert_eq!(found, 14..23);
        assert_eq!(
            search
                .replacement(&rope, found.clone(), "$1($3, $2)")
                .as_deref(),
            Some("baz(2, 1)")
        );
        assert_eq!(
            search
                .replacement(&rope, found.clone(), "${2}x$$")
                .as_deref(),
            Some("1x$")
        );
        assert_eq!(
            search
                .replacement(&rope, found.clone(), r"a\nb\tc\\d\q")
                .as_deref(),
            Some("a\nb\tc\\d\\q")
        );
        assert_eq!(
            search.replacement(&rope, found.clone(), "").as_deref(),
            Some("")
        );
        assert_eq!(
            search.replacement(&rope, 0..3, "x"),
            None,
            "not a match of this search"
        );

        // Plain text: the template is taken literally.
        let search = text("foo_bar").compile().unwrap();
        assert_eq!(
            search.replacement(&rope, 4..11, r"$1 \n").as_deref(),
            Some(r"$1 \n")
        );

        // Named groups.
        let search = regex(r"(?P<first>\w)(?P<rest>\w*)").compile().unwrap();
        assert_eq!(
            search.replacement(&rope, 0..3, "$rest$first").as_deref(),
            Some("etl")
        );
    }

    #[test]
    fn replacement_sees_the_match_in_context() {
        // `\B` only holds because of the character after the match.
        let rope = Rope::from_str("foobar foo");
        let search = regex(r"(fo)o\B").compile().unwrap();
        assert_eq!(search.find_all(&rope, 0..10), vec![0..3]);
        assert_eq!(
            search.replacement(&rope, 0..3, "<$1>").as_deref(),
            Some("<fo>")
        );
    }

    #[test]
    fn replace_all_is_one_undo_step() {
        let mut buffer = Buffer::from_str("one two one\ntwo one");
        let search = text("one").compile().unwrap();
        let len = buffer.len_chars();
        let summary = search.replace_all(&mut buffer, 0..len, "1");
        assert_eq!(buffer.to_string(), "1 two 1\ntwo 1");
        assert_eq!(summary.batches().len(), 1);
        assert_eq!(summary.batches()[0].len(), 3);
        assert_eq!(summary.map_offset(4, crate::edit::Bias::Left), 2);
        buffer.undo().unwrap();
        assert_eq!(buffer.to_string(), "one two one\ntwo one");
        assert!(!buffer.can_undo());

        // Limited to a range.
        search.replace_all(&mut buffer, 4..len, "X");
        assert_eq!(buffer.to_string(), "one two X\ntwo X");

        // Nothing to replace changes nothing.
        let version = buffer.version();
        let summary = text("zzz")
            .compile()
            .unwrap()
            .replace_all(&mut buffer, 0..99, "y");
        assert!(summary.is_empty() && summary.batches().is_empty());
        assert_eq!(buffer.version(), version);
    }

    #[test]
    fn replace_all_with_captures_and_growth() {
        let mut buffer = Buffer::from_str("a=1\nbb=22\nccc=333\n");
        let search = regex(r"(\w+)=(\d+)").compile().unwrap();
        search.replace_all(&mut buffer, 0..usize::MAX, "$2 <- ${1}!");
        assert_eq!(buffer.to_string(), "1 <- a!\n22 <- bb!\n333 <- ccc!\n");
        let search = regex("!$").compile().unwrap();
        search.replace_all(&mut buffer, 0..usize::MAX, r"\n");
        assert_eq!(buffer.to_string(), "1 <- a\n\n22 <- bb\n\n333 <- ccc\n\n");
    }

    #[test]
    fn replace_next_returns_the_new_range() {
        let mut buffer = Buffer::from_str("x = 1; x = 2;");
        let search = text("x").compile().unwrap();
        let (replaced, summary) = search.replace_next(&mut buffer, 1, false, "value").unwrap();
        assert_eq!(buffer.to_string(), "x = 1; value = 2;");
        assert_eq!(replaced, 7..12);
        assert!(!summary.is_empty());
        // Wrapping reaches the match before the starting point.
        let (replaced, _) = search.replace_next(&mut buffer, 12, true, "y").unwrap();
        assert_eq!(buffer.to_string(), "y = 1; value = 2;");
        assert_eq!(replaced, 0..1);
        assert!(search.replace_next(&mut buffer, 0, true, "z").is_none());
        buffer.undo().unwrap();
        buffer.undo().unwrap();
        assert_eq!(buffer.to_string(), "x = 1; x = 2;");
    }

    #[test]
    fn unescaping_templates() {
        assert_eq!(unescape_template(r"a\nb"), "a\nb");
        assert_eq!(unescape_template(r"\t\\n"), "\t\\n");
        assert_eq!(unescape_template(r"end\"), "end\\");
        assert_eq!(unescape_template(r"\d $1"), "\\d $1");
        assert_eq!(unescape_template(""), "");
    }

    #[test]
    fn line_helpers() {
        let rope = Rope::from_str("ab\ncd\n\nef");
        assert_eq!(line_start(&rope, 0), 0);
        assert_eq!(line_start(&rope, 2), 0);
        assert_eq!(line_start(&rope, 3), 3);
        assert_eq!(line_start(&rope, 6), 6);
        assert_eq!(line_start(&rope, 9), 7);
        assert_eq!(line_start(&rope, 99), 7);
        assert_eq!(line_end(&rope, 0), 2);
        assert_eq!(line_end(&rope, 2), 2);
        assert_eq!(line_end(&rope, 3), 5);
        assert_eq!(line_end(&rope, 6), 6);
        assert_eq!(line_end(&rope, 7), 9);
        assert_eq!(line_end(&rope, 99), 9);
    }

    #[test]
    fn large_buffer_search_is_fast() {
        // About 40 MB of text; a per-character conversion would make this crawl.
        let line = "the quick brown fox jumps over the lazy dog, again and again and again\n";
        let lines = 40 * 1024 * 1024 / line.len();
        let mut haystack = line.repeat(lines);
        haystack.push_str("the unique needle is here\n");
        let rope = Rope::from_str(&haystack);
        let start = std::time::Instant::now();
        let search = text("unique needle").compile().unwrap();
        let found = search.find_all(&rope, 0..rope.len_chars());
        assert_eq!(found, vec![lines * line.len() + 4..lines * line.len() + 17]);
        assert_eq!(search.find_next(&rope, 0, false), Some(found[0].clone()));
        assert_eq!(
            search.find_prev(&rope, rope.len_chars(), false),
            Some(found[0].clone())
        );
        let search = regex(r"uniq\w+ need\w+").compile().unwrap();
        assert_eq!(search.find_all(&rope, 0..rope.len_chars()), found);
        assert!(
            start.elapsed() < std::time::Duration::from_secs(20),
            "took {:?}",
            start.elapsed()
        );
    }
}
