//! Delimiter pairs: brackets and quotes.
//!
//! Two things live here.
//!
//! The first is a faithful port of the pairing rules of the `zed-select-between` script
//! ([`find_bracket_pairs`], [`find_quote_pairs`], [`select_between`]), which look at the
//! whole text.
//!
//! The second is a set of local queries built on the same rules that only look near a
//! position, cheap enough to run on every cursor move: [`matching_bracket`],
//! [`enclosing_bracket`] and [`enclosing_quote`].
//!
//! There is no syntax tree, so strings are recognised by a simple rule shared by both: a
//! `"` or `'` opens a string that ends at the same quote or at the end of the line, a
//! backslash escapes the next character, and an apostrophe between two letters or digits
//! (`don't`) is not a quote. A bracket inside a string only pairs with a bracket inside the
//! same string.

use std::borrow::Cow;
use std::cmp::Reverse;
use std::ops::Range;

use ropey::Rope;

use crate::buffer::Buffer;

/// The kinds of delimiter `select_between` understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PairKind {
    /// `(` and `)`.
    Paren,
    /// `[` and `]`.
    Bracket,
    /// `{` and `}`.
    Brace,
    /// `<` and `>`.
    Angle,
    /// `"`.
    DoubleQuote,
    /// `'`.
    SingleQuote,
    /// `` ` ``.
    Backtick,
}

impl PairKind {
    /// Every kind, in a fixed order.
    pub const ALL: [PairKind; 7] = [
        PairKind::Paren,
        PairKind::Bracket,
        PairKind::Brace,
        PairKind::Angle,
        PairKind::DoubleQuote,
        PairKind::SingleQuote,
        PairKind::Backtick,
    ];

    /// The kind named `name`: `paren`, `bracket`, `brace`, `angle`, `dquote`, `squote` or
    /// `backtick` (the names the script takes). Anything else is `None`.
    pub fn from_name(name: &str) -> Option<PairKind> {
        PairKind::ALL.into_iter().find(|kind| kind.name() == name)
    }

    /// The kind's name, as accepted by [`PairKind::from_name`].
    pub fn name(self) -> &'static str {
        match self {
            PairKind::Paren => "paren",
            PairKind::Bracket => "bracket",
            PairKind::Brace => "brace",
            PairKind::Angle => "angle",
            PairKind::DoubleQuote => "dquote",
            PairKind::SingleQuote => "squote",
            PairKind::Backtick => "backtick",
        }
    }

    /// The opening and closing delimiter. They are the same character for quotes.
    pub fn delimiters(self) -> (char, char) {
        match self {
            PairKind::Paren => ('(', ')'),
            PairKind::Bracket => ('[', ']'),
            PairKind::Brace => ('{', '}'),
            PairKind::Angle => ('<', '>'),
            PairKind::DoubleQuote => ('"', '"'),
            PairKind::SingleQuote => ('\'', '\''),
            PairKind::Backtick => ('`', '`'),
        }
    }

    /// True for the three quote kinds.
    pub fn is_quote(self) -> bool {
        matches!(
            self,
            PairKind::DoubleQuote | PairKind::SingleQuote | PairKind::Backtick
        )
    }
}

/// A matched pair of delimiters, as char offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DelimPair {
    /// Offset of the opening delimiter.
    pub open: usize,
    /// Length of each delimiter: 1, or 3 for a triple quote.
    pub delim_len: usize,
    /// Offset of the closing delimiter.
    pub close: usize,
}

impl DelimPair {
    /// The text between the delimiters.
    pub fn inner(&self) -> Range<usize> {
        self.open + self.delim_len..self.close
    }

    /// The text including both delimiters.
    pub fn outer(&self) -> Range<usize> {
        self.open..self.close + self.delim_len
    }
}

/// What [`select_between`] decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BetweenTarget {
    /// Select this range (the text between a pair's delimiters, possibly empty), with the
    /// cursor at its end.
    Select(Range<usize>),
    /// Put the cursor here: just inside the first pair of the text.
    Cursor(usize),
    /// The text has no delimiter of this kind at all.
    Nothing,
}

/// Whether the byte at `index` is escaped: preceded by an odd number of backslashes.
fn is_escaped(bytes: &[u8], index: usize) -> bool {
    let backslashes = bytes[..index]
        .iter()
        .rev()
        .take_while(|&&b| b == b'\\')
        .count();
    backslashes % 2 == 1
}

/// Whether the character ending just before byte `index` is a letter or a digit.
fn alnum_before(text: &str, index: usize) -> bool {
    text[..index]
        .chars()
        .next_back()
        .is_some_and(char::is_alphanumeric)
}

/// Whether the character starting at byte `index` is a letter or a digit.
fn alnum_at(text: &str, index: usize) -> bool {
    text.get(index..)
        .and_then(|rest| rest.chars().next())
        .is_some_and(char::is_alphanumeric)
}

/// Whether the `'` at byte `index` sits between two letters or digits, as in `don't`.
fn is_apostrophe(text: &str, index: usize) -> bool {
    index > 0 && index + 1 < text.len() && alnum_before(text, index) && alnum_at(text, index + 1)
}

/// The script's ordering of pairs: by opening offset, the larger pair first on a tie.
fn sort_pairs(pairs: &mut [(usize, usize, usize)]) {
    pairs.sort_by_key(|&(open, _, close)| (open, Reverse(close - open)));
}

/// Bracket pairs of one kind in `text`, as byte offsets `(open, 1, close)`, plus the first
/// opening bracket seen. A direct port of the script's `find_bracket_pairs`.
fn bracket_pairs_in(
    text: &str,
    open: u8,
    close: u8,
) -> (Vec<(usize, usize, usize)>, Option<usize>) {
    let bytes = text.as_bytes();
    let mut pairs = Vec::new();
    let mut code_stack: Vec<usize> = Vec::new();
    let mut string_stack: Vec<usize> = Vec::new();
    let mut in_string: Option<u8> = None;
    let mut first_open: Option<usize> = None;
    let mut backslashes = 0usize;
    for (index, &byte) in bytes.iter().enumerate() {
        let escaped = backslashes % 2 == 1;
        backslashes = if byte == b'\\' { backslashes + 1 } else { 0 };

        if byte == b'\n' && in_string.is_some() {
            in_string = None;
            string_stack.clear();
            continue;
        }
        if let Some(quote) = in_string {
            if byte == quote && !escaped {
                in_string = None;
                string_stack.clear();
                continue;
            }
            if !escaped {
                if byte == open {
                    first_open.get_or_insert(index);
                    string_stack.push(index);
                } else if byte == close {
                    if let Some(opened) = string_stack.pop() {
                        pairs.push((opened, 1, index));
                    }
                }
            }
            continue;
        }
        if (byte == b'"' || byte == b'\'')
            && !escaped
            && !(byte == b'\'' && is_apostrophe(text, index))
        {
            in_string = Some(byte);
            string_stack.clear();
            continue;
        }
        if escaped {
            continue;
        }
        if byte == open {
            first_open.get_or_insert(index);
            code_stack.push(index);
        } else if byte == close {
            if let Some(opened) = code_stack.pop() {
                pairs.push((opened, 1, index));
            }
        }
    }
    sort_pairs(&mut pairs);
    (pairs, first_open)
}

/// Whether a line ends just after the byte at `index`, by Python's `str.splitlines` rules
/// (the script pairs quotes line by line with it). Carriage returns never occur here.
fn ends_python_line(bytes: &[u8], index: usize) -> bool {
    match bytes[index] {
        b'\n' | 0x0b | 0x0c | 0x1c | 0x1d | 0x1e | b'\r' => true,
        // U+0085 is C2 85; U+2028 and U+2029 are E2 80 A8 and E2 80 A9.
        0x85 => index >= 1 && bytes[index - 1] == 0xc2,
        0xa8 | 0xa9 => index >= 2 && bytes[index - 2] == 0xe2 && bytes[index - 1] == 0x80,
        _ => false,
    }
}

/// Quote pairs of one kind in `text`, as byte offsets `(open, delimiter length, close)`,
/// plus the first opening quote seen. A direct port of the script's `find_quote_pairs`:
/// triple quotes first (they may span lines), then single quotes paired line by line.
fn quote_pairs_in(text: &str, quote: u8) -> (Vec<(usize, usize, usize)>, Option<usize>) {
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut pairs = Vec::new();
    let mut first_open: Option<usize> = None;

    let triple = [quote; 3];
    let finder = memchr::memmem::Finder::new(&triple);
    let find_from = |from: usize| -> Option<usize> {
        if from > len {
            return None;
        }
        finder.find(&bytes[from..]).map(|found| found + from)
    };
    let mut triple_spans: Vec<Range<usize>> = Vec::new();
    let mut search_from = 0;
    while search_from < len {
        let Some(start) = find_from(search_from) else {
            break;
        };
        if is_escaped(bytes, start) {
            search_from = start + 1;
            continue;
        }
        first_open.get_or_insert(start);
        let mut end = find_from(start + 3);
        while let Some(candidate) = end {
            if !is_escaped(bytes, candidate) {
                break;
            }
            end = find_from(candidate + 1);
        }
        let Some(end) = end else {
            break;
        };
        pairs.push((start, 3, end));
        triple_spans.push(start..end + 3);
        search_from = end + 3;
    }
    let in_triple = |position: usize| -> bool {
        let index = triple_spans.partition_point(|span| span.end <= position);
        triple_spans
            .get(index)
            .is_some_and(|span| span.start <= position)
    };

    let mut open_index: Option<usize> = None;
    for index in memchr::memchr_iter(quote, bytes) {
        // Quotes pair up within one line; anything left open is dropped at a line end.
        if let Some(opened) = open_index {
            if (opened..index).any(|between| ends_python_line(bytes, between)) {
                open_index = None;
            }
        }
        if in_triple(index) || is_escaped(bytes, index) {
            continue;
        }
        if quote == b'\'' && open_index.is_none() && is_apostrophe(text, index) {
            continue;
        }
        first_open.get_or_insert(index);
        match open_index {
            None => open_index = Some(index),
            Some(opened) => {
                pairs.push((opened, 1, index));
                open_index = None;
            }
        }
    }
    sort_pairs(&mut pairs);
    (pairs, first_open)
}

/// The rope's text in one piece: borrowed when the rope is a single chunk.
fn whole_text(rope: &Rope) -> Cow<'_, str> {
    rope.slice(..).into()
}

/// Turn byte-offset pairs into char-offset [`DelimPair`]s.
fn to_char_pairs(
    rope: &Rope,
    found: (Vec<(usize, usize, usize)>, Option<usize>),
) -> (Vec<DelimPair>, Option<usize>) {
    let ascii = rope.len_bytes() == rope.len_chars();
    let to_char = |byte: usize| if ascii { byte } else { rope.byte_to_char(byte) };
    let pairs = found
        .0
        .into_iter()
        .map(|(open, delim_len, close)| DelimPair {
            open: to_char(open),
            delim_len,
            close: to_char(close),
        })
        .collect();
    (pairs, found.1.map(to_char))
}

/// Every matched `open`/`close` bracket pair in the text, sorted by opening offset, and the
/// offset of the first opening bracket whether matched or not.
///
/// This is the script's `find_bracket_pairs`: brackets outside strings pair up across the
/// whole text; brackets inside a single-line `"..."` or `'...'` string pair up only within
/// that string, so an unmatched bracket in a string never disturbs the code around it.
/// Escaped brackets are ignored, and a closing bracket with nothing to close is skipped.
/// `open` and `close` must be ASCII.
pub fn find_bracket_pairs(rope: &Rope, open: char, close: char) -> (Vec<DelimPair>, Option<usize>) {
    if !open.is_ascii() || !close.is_ascii() {
        return (Vec::new(), None);
    }
    let text = whole_text(rope);
    to_char_pairs(rope, bracket_pairs_in(&text, open as u8, close as u8))
}

/// Every pair of `quote` characters in the text, sorted by opening offset (on a tie the
/// larger pair first), and the offset of the first opening quote whether closed or not.
///
/// This is the script's `find_quote_pairs`: triple quotes (`"""`) are matched first and may
/// span lines; inside them nothing else pairs. The remaining quotes are paired two by two
/// within each line, so a stray quote or apostrophe on one line never flips the pairing of
/// the lines below. Escaped quotes are ignored, and for `'` an apostrophe between two
/// letters or digits is not an opening quote. `quote` must be ASCII.
pub fn find_quote_pairs(rope: &Rope, quote: char) -> (Vec<DelimPair>, Option<usize>) {
    if !quote.is_ascii() {
        return (Vec::new(), None);
    }
    let text = whole_text(rope);
    to_char_pairs(rope, quote_pairs_in(&text, quote as u8))
}

/// Every pair of delimiters of `kind` in the text (see [`find_bracket_pairs`] and
/// [`find_quote_pairs`]).
pub fn find_pairs(rope: &Rope, kind: PairKind) -> (Vec<DelimPair>, Option<usize>) {
    let (open, close) = kind.delimiters();
    if kind.is_quote() {
        find_quote_pairs(rope, open)
    } else {
        find_bracket_pairs(rope, open, close)
    }
}

/// Decide what "select between" does for a cursor at `cursor` whose selection is
/// `selected` (empty when nothing is selected). This is the choice logic of the script's
/// `run_worker`:
///
/// - If the cursor is inside one or more pairs of `kind` (between the delimiters, edges
///   included), the innermost pair's contents are selected.
/// - If exactly that text is already selected and a larger pair encloses the cursor, the
///   next larger pair's contents are selected instead.
/// - If the cursor is in no pair, it moves just inside the first pair of the text, or just
///   after the first opening delimiter when nothing is matched.
pub fn select_between(
    rope: &Rope,
    kind: PairKind,
    cursor: usize,
    selected: Range<usize>,
) -> BetweenTarget {
    let (pairs, first_open) = find_pairs(rope, kind);
    let mut enclosing: Vec<&DelimPair> = pairs
        .iter()
        .filter(|pair| pair.open + pair.delim_len <= cursor && cursor <= pair.close)
        .collect();
    enclosing.sort_by_key(|pair| (pair.close - pair.open, Reverse(pair.open)));

    if let Some(&innermost) = enclosing.first() {
        let mut chosen = innermost;
        let len = rope.len_chars();
        let selected = selected.start.min(len)..selected.end.min(len);
        if !selected.is_empty()
            && enclosing.len() > 1
            && rope.slice(selected) == rope.slice(chosen.inner())
        {
            chosen = enclosing[1];
        }
        return BetweenTarget::Select(chosen.inner());
    }
    match (pairs.first(), first_open) {
        (Some(first), _) => BetweenTarget::Cursor(first.open + first.delim_len),
        (None, Some(open)) => BetweenTarget::Cursor((open + 1).min(rope.len_chars())),
        (None, None) => BetweenTarget::Nothing,
    }
}

// ----- local queries -----------------------------------------------------------------

/// How many characters a local query may scan in each direction before it gives up.
pub const SCAN_LIMIT: usize = 1 << 20;

/// A bracket found while scanning one line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Token {
    /// Char offset in the buffer.
    pos: usize,
    byte: u8,
    /// 0 outside strings, otherwise the number of the string literal on its line.
    string: u32,
}

/// The brackets and string literals of one line.
#[derive(Debug, Default)]
struct LineScan {
    tokens: Vec<Token>,
    /// For each string literal, the char range of its contents (between the quotes, or up
    /// to the end of the line when it is not closed).
    strings: Vec<Range<usize>>,
}

impl LineScan {
    /// The number of the string literal whose contents hold both `start` and `end`.
    fn string_holding(&self, start: usize, end: usize) -> Option<u32> {
        self.strings
            .iter()
            .position(|contents| contents.start <= start && end <= contents.end)
            .map(|index| index as u32 + 1)
    }
}

fn is_bracket_byte(byte: u8) -> bool {
    matches!(byte, b'(' | b')' | b'[' | b']' | b'{' | b'}')
}

/// The opening and closing bracket of the pair `c` belongs to.
fn bracket_partner(c: char) -> Option<(u8, u8)> {
    match c {
        '(' | ')' => Some((b'(', b')')),
        '[' | ']' => Some((b'[', b']')),
        '{' | '}' => Some((b'{', b'}')),
        _ => None,
    }
}

/// Find the brackets of one line and which string literal each is in. `line_start` is the
/// char offset of the line in the buffer.
fn scan_line(text: &str, line_start: usize) -> LineScan {
    let bytes = text.as_bytes();
    let mut scan = LineScan::default();
    let mut in_string: Option<(u8, usize)> = None;
    let mut backslashes = 0usize;
    let mut chars = 0usize;
    for (index, &byte) in bytes.iter().enumerate() {
        if byte & 0xc0 == 0x80 {
            // A continuation byte: part of the character already counted.
            continue;
        }
        let pos = line_start + chars;
        chars += 1;
        let escaped = backslashes % 2 == 1;
        backslashes = if byte == b'\\' { backslashes + 1 } else { 0 };

        if let Some((quote, contents_start)) = in_string {
            if byte == quote && !escaped {
                scan.strings.push(contents_start..pos);
                in_string = None;
            } else if !escaped && is_bracket_byte(byte) {
                scan.tokens.push(Token {
                    pos,
                    byte,
                    string: scan.strings.len() as u32 + 1,
                });
            }
            continue;
        }
        if (byte == b'"' || byte == b'\'')
            && !escaped
            && !(byte == b'\'' && is_apostrophe(text, index))
        {
            in_string = Some((byte, pos + 1));
            continue;
        }
        if !escaped && is_bracket_byte(byte) {
            scan.tokens.push(Token {
                pos,
                byte,
                string: 0,
            });
        }
    }
    if let Some((_, contents_start)) = in_string {
        scan.strings.push(contents_start..line_start + chars);
    }
    scan
}

/// Scan line `line` of the buffer, unless it is longer than `budget` allows. The budget is
/// reduced by the line's length.
fn scan_buffer_line(buffer: &Buffer, line: usize, budget: &mut usize) -> Option<LineScan> {
    let cost = buffer.line_len(line) + 1;
    if cost > *budget {
        *budget = 0;
        return None;
    }
    *budget -= cost;
    Some(scan_line(
        &buffer.line_text(line),
        buffer.line_to_char(line),
    ))
}

/// The brackets outside strings at or after a position, in order, line by line.
struct CodeTokensAfter<'a> {
    buffer: &'a Buffer,
    line: usize,
    pending: std::vec::IntoIter<Token>,
    budget: usize,
}

impl<'a> CodeTokensAfter<'a> {
    fn new(buffer: &'a Buffer, from: usize) -> CodeTokensAfter<'a> {
        let line = buffer.char_to_line(from);
        let mut budget = SCAN_LIMIT;
        let tokens: Vec<Token> = scan_buffer_line(buffer, line, &mut budget)
            .map(|scan| {
                scan.tokens
                    .into_iter()
                    .filter(|token| token.string == 0 && token.pos >= from)
                    .collect()
            })
            .unwrap_or_default();
        CodeTokensAfter {
            buffer,
            line,
            pending: tokens.into_iter(),
            budget,
        }
    }
}

impl Iterator for CodeTokensAfter<'_> {
    type Item = Token;

    fn next(&mut self) -> Option<Token> {
        loop {
            if let Some(token) = self.pending.next() {
                return Some(token);
            }
            if self.line + 1 >= self.buffer.len_lines() || self.budget == 0 {
                return None;
            }
            self.line += 1;
            let scan = scan_buffer_line(self.buffer, self.line, &mut self.budget)?;
            let tokens: Vec<Token> = scan
                .tokens
                .into_iter()
                .filter(|token| token.string == 0)
                .collect();
            self.pending = tokens.into_iter();
        }
    }
}

/// The brackets outside strings before a position, nearest first, line by line.
struct CodeTokensBefore<'a> {
    buffer: &'a Buffer,
    line: usize,
    pending: Vec<Token>,
    budget: usize,
}

impl<'a> CodeTokensBefore<'a> {
    fn new(buffer: &'a Buffer, before: usize) -> CodeTokensBefore<'a> {
        let line = buffer.char_to_line(before);
        let mut budget = SCAN_LIMIT;
        let pending: Vec<Token> = scan_buffer_line(buffer, line, &mut budget)
            .map(|scan| {
                scan.tokens
                    .into_iter()
                    .filter(|token| token.string == 0 && token.pos < before)
                    .collect()
            })
            .unwrap_or_default();
        CodeTokensBefore {
            buffer,
            line,
            pending,
            budget,
        }
    }
}

impl Iterator for CodeTokensBefore<'_> {
    type Item = Token;

    fn next(&mut self) -> Option<Token> {
        loop {
            if let Some(token) = self.pending.pop() {
                return Some(token);
            }
            if self.line == 0 || self.budget == 0 {
                return None;
            }
            self.line -= 1;
            let scan = scan_buffer_line(self.buffer, self.line, &mut self.budget)?;
            self.pending = scan
                .tokens
                .into_iter()
                .filter(|token| token.string == 0)
                .collect();
        }
    }
}

/// The closing partner of the opening bracket at `pos`.
fn find_close(buffer: &Buffer, pos: usize, open: u8, close: u8) -> Option<usize> {
    let line = buffer.char_to_line(pos);
    let mut budget = SCAN_LIMIT;
    let scan = scan_buffer_line(buffer, line, &mut budget)?;
    let index = scan.tokens.iter().position(|token| token.pos == pos)?;
    let string = scan.tokens[index].string;
    let mut depth = 1usize;
    let mut step = |token: &Token| -> bool {
        if token.byte == open {
            depth += 1;
        } else if token.byte == close {
            depth -= 1;
        }
        depth == 0
    };
    for token in scan.tokens[index + 1..]
        .iter()
        .filter(|token| token.string == string)
    {
        if step(token) {
            return Some(token.pos);
        }
    }
    if string != 0 {
        return None;
    }
    let line_end = buffer.line_end(line);
    CodeTokensAfter::new(buffer, line_end.saturating_add(1))
        .filter(|token| token.pos > line_end)
        .find(|token| step(token))
        .map(|token| token.pos)
}

/// The opening partner of the closing bracket at `pos`.
fn find_open(buffer: &Buffer, pos: usize, open: u8, close: u8) -> Option<usize> {
    let line = buffer.char_to_line(pos);
    let mut budget = SCAN_LIMIT;
    let scan = scan_buffer_line(buffer, line, &mut budget)?;
    let index = scan.tokens.iter().position(|token| token.pos == pos)?;
    let string = scan.tokens[index].string;
    let mut depth = 1usize;
    let mut step = |token: &Token| -> bool {
        if token.byte == close {
            depth += 1;
        } else if token.byte == open {
            depth -= 1;
        }
        depth == 0
    };
    for token in scan.tokens[..index]
        .iter()
        .rev()
        .filter(|token| token.string == string)
    {
        if step(token) {
            return Some(token.pos);
        }
    }
    if string != 0 {
        return None;
    }
    let line_start = buffer.line_to_char(line);
    CodeTokensBefore::new(buffer, line_start)
        .find(|token| step(token))
        .map(|token| token.pos)
}

/// The pair that the bracket character at exactly `at` belongs to, as the offsets of the
/// opening and the closing bracket. `None` when the character there is not one of
/// `()[]{}`, is escaped, has no partner, or the partner is further away than
/// [`SCAN_LIMIT`]. A bracket inside a string pairs only within that string.
pub fn bracket_pair_at(buffer: &Buffer, at: usize) -> Option<(usize, usize)> {
    let c = buffer.char_at(at)?;
    let (open, close) = bracket_partner(c)?;
    if c as u32 == u32::from(open) {
        find_close(buffer, at, open, close).map(|partner| (at, partner))
    } else {
        find_open(buffer, at, open, close).map(|partner| (partner, at))
    }
}

/// The bracket pair to highlight for a cursor at `pos`: the pair of the bracket right after
/// the cursor, or failing that of the bracket right before it (see [`bracket_pair_at`]).
/// Returns the offsets of the opening and the closing bracket.
pub fn matching_bracket(buffer: &Buffer, pos: usize) -> Option<(usize, usize)> {
    bracket_pair_at(buffer, pos).or_else(|| {
        pos.checked_sub(1)
            .and_then(|before| bracket_pair_at(buffer, before))
    })
}

/// The innermost pair of `open`/`close` brackets whose inside contains `range`.
fn enclosing_pair_of(
    buffer: &Buffer,
    range: &Range<usize>,
    open: u8,
    close: u8,
) -> Option<(usize, usize)> {
    let (start, end) = (range.start, range.end.max(range.start));
    let line = buffer.char_to_line(start);
    let mut budget = SCAN_LIMIT;
    let scan = scan_buffer_line(buffer, line, &mut budget)?;

    // Inside a string, pairs within that string come first.
    if let Some(string) = scan.string_holding(start, end) {
        let mut stack: Vec<usize> = Vec::new();
        let mut best: Option<(usize, usize)> = None;
        for token in scan.tokens.iter().filter(|token| token.string == string) {
            if token.byte == open {
                stack.push(token.pos);
            } else if token.byte == close {
                if let Some(opened) = stack.pop() {
                    let encloses = opened < start && token.pos >= end;
                    if encloses && best.is_none_or(|(best_open, _)| opened > best_open) {
                        best = Some((opened, token.pos));
                    }
                }
            }
        }
        if best.is_some() {
            return best;
        }
    }

    let mut before = CodeTokensBefore::new(buffer, start);
    let mut after = CodeTokensAfter::new(buffer, start);
    loop {
        let mut depth = 0usize;
        let opened = loop {
            let token = before.next()?;
            if token.byte == close {
                depth += 1;
            } else if token.byte == open {
                if depth == 0 {
                    break token.pos;
                }
                depth -= 1;
            }
        };
        let mut depth = 0usize;
        let closed = loop {
            let token = after.next()?;
            if token.byte == open {
                depth += 1;
            } else if token.byte == close {
                if depth == 0 {
                    break token.pos;
                }
                depth -= 1;
            }
        };
        if closed >= end {
            return Some((opened, closed));
        }
    }
}

/// The innermost `()`, `[]` or `{}` pair whose inside contains `range` (a cursor when the
/// range is empty). Returns the offsets of the opening and the closing bracket, so the
/// inside is `open + 1..close`. Text that sits right between the brackets counts as inside.
///
/// The same string and escape rules as [`matching_bracket`] apply. `None` when nothing
/// encloses the range within [`SCAN_LIMIT`].
pub fn enclosing_bracket(buffer: &Buffer, range: Range<usize>) -> Option<(usize, usize)> {
    [(b'(', b')'), (b'[', b']'), (b'{', b'}')]
        .into_iter()
        .filter_map(|(open, close)| enclosing_pair_of(buffer, &range, open, close))
        .max_by_key(|&(open, _)| open)
}

/// The pairs of `quote` on one line, paired two by two from the left, as buffer char
/// offsets. Escaped quotes and (for `'`) apostrophes inside words are skipped.
fn quote_pairs_on_line(text: &str, line_start: usize, quote: u8) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let ascii = text.is_ascii();
    let to_pos = |byte: usize| {
        line_start
            + if ascii {
                byte
            } else {
                text[..byte].chars().count()
            }
    };
    let mut pairs = Vec::new();
    let mut open_index: Option<usize> = None;
    for index in memchr::memchr_iter(quote, bytes) {
        if is_escaped(bytes, index) {
            continue;
        }
        if quote == b'\'' && open_index.is_none() && is_apostrophe(text, index) {
            continue;
        }
        match open_index {
            None => open_index = Some(index),
            Some(opened) => {
                pairs.push((to_pos(opened), to_pos(index)));
                open_index = None;
            }
        }
    }
    pairs
}

/// The innermost pair of `"`, `'` or `` ` `` quotes on the line of `range` whose inside
/// contains `range`. Returns the offsets of the opening and the closing quote. Quotes are
/// paired two by two within the line; a range that spans lines has no enclosing quotes.
pub fn enclosing_quote(buffer: &Buffer, range: Range<usize>) -> Option<(usize, usize)> {
    let (start, end) = (range.start, range.end.max(range.start));
    let line = buffer.char_to_line(start);
    if buffer.char_to_line(end) != line || buffer.line_len(line) > SCAN_LIMIT {
        return None;
    }
    let text = buffer.line_text(line);
    let line_start = buffer.line_to_char(line);
    [b'"', b'\'', b'`']
        .into_iter()
        .flat_map(|quote| quote_pairs_on_line(&text, line_start, quote))
        .filter(|&(open, close)| open < start && close >= end)
        .max_by_key(|&(open, _)| open)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs_of(text: &str, kind: PairKind) -> Vec<(usize, usize, usize)> {
        find_pairs(&Rope::from_str(text), kind)
            .0
            .into_iter()
            .map(|p| (p.open, p.delim_len, p.close))
            .collect()
    }

    fn first_open(text: &str, kind: PairKind) -> Option<usize> {
        find_pairs(&Rope::from_str(text), kind).1
    }

    #[test]
    fn kind_names_round_trip() {
        for kind in PairKind::ALL {
            assert_eq!(PairKind::from_name(kind.name()), Some(kind));
        }
        assert_eq!(PairKind::from_name("paren"), Some(PairKind::Paren));
        assert_eq!(PairKind::from_name("dquote"), Some(PairKind::DoubleQuote));
        assert_eq!(PairKind::from_name("parens"), None);
        assert_eq!(PairKind::from_name(""), None);
        assert_eq!(PairKind::Brace.delimiters(), ('{', '}'));
        assert!(PairKind::Backtick.is_quote() && !PairKind::Angle.is_quote());
        let pair = DelimPair {
            open: 2,
            delim_len: 3,
            close: 9,
        };
        assert_eq!(pair.inner(), 5..9);
        assert_eq!(pair.outer(), 2..12);
    }

    #[test]
    fn nested_brackets() {
        assert_eq!(
            pairs_of("f(a, (b), c)", PairKind::Paren),
            vec![(1, 1, 11), (5, 1, 7)]
        );
        assert_eq!(
            pairs_of("x[1][2]", PairKind::Bracket),
            vec![(1, 1, 3), (4, 1, 6)]
        );
        assert_eq!(
            pairs_of("{\n  {}\n}", PairKind::Brace),
            vec![(0, 1, 7), (4, 1, 5)]
        );
        assert_eq!(
            pairs_of("Vec<Option<T>>", PairKind::Angle),
            vec![(3, 1, 13), (10, 1, 12)]
        );
        assert_eq!(pairs_of("no brackets", PairKind::Paren), vec![]);
        assert_eq!(first_open("no brackets", PairKind::Paren), None);
        assert_eq!(pairs_of("", PairKind::Paren), vec![]);
    }

    #[test]
    fn unmatched_brackets_are_skipped() {
        assert_eq!(pairs_of(") ( )", PairKind::Paren), vec![(2, 1, 4)]);
        assert_eq!(pairs_of("( ( )", PairKind::Paren), vec![(2, 1, 4)]);
        assert_eq!(first_open("( ( )", PairKind::Paren), Some(0));
        assert_eq!(pairs_of("(((", PairKind::Paren), vec![]);
        assert_eq!(first_open("  (((", PairKind::Paren), Some(2));
        assert_eq!(pairs_of(")))", PairKind::Paren), vec![]);
        assert_eq!(first_open(")))", PairKind::Paren), None);
    }

    #[test]
    fn brackets_in_strings_pair_only_within_the_string() {
        // An unmatched bracket inside a string does not disturb the code around it.
        assert_eq!(
            pairs_of(r#"f("cmd-k (", x)"#, PairKind::Paren),
            vec![(1, 1, 14)]
        );
        // Balanced brackets inside a string still match each other.
        assert_eq!(
            pairs_of(r#"f("hello (world)")"#, PairKind::Paren),
            vec![(1, 1, 17), (9, 1, 15)]
        );
        // Single quotes work the same way.
        assert_eq!(pairs_of("f('(', ')')", PairKind::Paren), vec![(1, 1, 10)]);
        // A string ends at the end of its line even without a closing quote.
        assert_eq!(
            pairs_of("a = \"(\nb = (1)", PairKind::Paren),
            vec![(11, 1, 13)]
        );
        // The first opening bracket is reported even inside a string.
        assert_eq!(first_open(r#""(" ()"#, PairKind::Paren), Some(1));
    }

    #[test]
    fn apostrophes_do_not_open_strings() {
        assert_eq!(
            pairs_of("don't (stop) can't", PairKind::Paren),
            vec![(6, 1, 11)]
        );
        assert_eq!(
            pairs_of("it's (a) 'quoted (' thing", PairKind::Paren),
            vec![(5, 1, 7)]
        );
    }

    #[test]
    fn escapes_hide_brackets_and_quotes() {
        assert_eq!(pairs_of(r"\( ( \) )", PairKind::Paren), vec![(3, 1, 8)]);
        assert_eq!(
            pairs_of(r"\\( )", PairKind::Paren),
            vec![(2, 1, 4)],
            "an escaped backslash escapes nothing"
        );
        assert_eq!(pairs_of(r#""a\"(" ()"#, PairKind::Paren), vec![(7, 1, 8)]);
    }

    #[test]
    fn quote_pairs_per_line() {
        assert_eq!(
            pairs_of(r#"a "b" c "d""#, PairKind::DoubleQuote),
            vec![(2, 1, 4), (8, 1, 10)]
        );
        assert_eq!(
            pairs_of("`x` and `y`", PairKind::Backtick),
            vec![(0, 1, 2), (8, 1, 10)]
        );
        // An unclosed quote does not flip the pairing on the next line.
        assert_eq!(
            pairs_of("say \"oops\nthen \"fine\"", PairKind::DoubleQuote),
            vec![(15, 1, 20)]
        );
        assert_eq!(
            first_open("say \"oops\nthen \"fine\"", PairKind::DoubleQuote),
            Some(4)
        );
        assert_eq!(
            pairs_of(r#""a\"b""#, PairKind::DoubleQuote),
            vec![(0, 1, 5)]
        );
        assert_eq!(pairs_of("", PairKind::DoubleQuote), vec![]);
        assert_eq!(first_open("none", PairKind::Backtick), None);
    }

    #[test]
    fn single_quotes_skip_apostrophes() {
        assert_eq!(
            pairs_of("don't 'quote' it's", PairKind::SingleQuote),
            vec![(6, 1, 12)]
        );
        assert_eq!(pairs_of("'a' b'c", PairKind::SingleQuote), vec![(0, 1, 2)]);
        // Once a quote is open, the next apostrophe closes it even inside a word.
        assert_eq!(pairs_of("'don't", PairKind::SingleQuote), vec![(0, 1, 4)]);
    }

    #[test]
    fn triple_quotes_span_lines_and_hide_single_quotes() {
        let text = "x = \"\"\"doc \"quoted\"\nmore\"\"\" + \"s\"";
        assert_eq!(
            pairs_of(text, PairKind::DoubleQuote),
            vec![(4, 3, 24), (30, 1, 32)]
        );
        assert_eq!(first_open(text, PairKind::DoubleQuote), Some(4));
        // An unclosed triple quote pairs nothing itself but is still the first opener.
        assert_eq!(
            pairs_of("\"\"\"never closed", PairKind::DoubleQuote),
            vec![(0, 1, 1)]
        );
        assert_eq!(
            first_open("  \"\"\"never closed", PairKind::DoubleQuote),
            Some(2)
        );
        assert_eq!(
            pairs_of("```\ncode\n```", PairKind::Backtick),
            vec![(0, 3, 9)]
        );
    }

    #[test]
    fn offsets_are_in_chars() {
        assert_eq!(pairs_of("é(世界)x", PairKind::Paren), vec![(1, 1, 4)]);
        assert_eq!(
            pairs_of("«\"héllo\"»", PairKind::DoubleQuote),
            vec![(1, 1, 7)]
        );
        assert_eq!(first_open("ééé(", PairKind::Paren), Some(3));
        assert_eq!(
            pairs_of("日本 '語'", PairKind::SingleQuote),
            vec![(3, 1, 5)]
        );
        // Letters of any script make an apostrophe: nothing opens here.
        assert_eq!(pairs_of("日本'語'", PairKind::SingleQuote), vec![]);
    }

    fn between(text: &str, kind: PairKind, cursor: usize, selected: Range<usize>) -> BetweenTarget {
        select_between(&Rope::from_str(text), kind, cursor, selected)
    }

    #[test]
    fn select_between_picks_the_innermost_pair() {
        let text = "f(a, (b), c)";
        assert_eq!(
            between(text, PairKind::Paren, 6, 6..6),
            BetweenTarget::Select(6..7)
        );
        assert_eq!(
            between(text, PairKind::Paren, 7, 7..7),
            BetweenTarget::Select(6..7),
            "at the closer"
        );
        assert_eq!(
            between(text, PairKind::Paren, 3, 3..3),
            BetweenTarget::Select(2..11)
        );
        assert_eq!(
            between(text, PairKind::Paren, 2, 2..2),
            BetweenTarget::Select(2..11),
            "just inside"
        );
        assert_eq!(
            between(text, PairKind::Paren, 11, 11..11),
            BetweenTarget::Select(2..11)
        );
    }

    #[test]
    fn select_between_expands_when_the_inside_is_already_selected() {
        let text = "f(a, (b), c)";
        assert_eq!(
            between(text, PairKind::Paren, 7, 6..7),
            BetweenTarget::Select(2..11)
        );
        // The outermost pair has nothing larger to grow into.
        assert_eq!(
            between(text, PairKind::Paren, 11, 2..11),
            BetweenTarget::Select(2..11)
        );
        // A different selection does not trigger the expansion.
        assert_eq!(
            between(text, PairKind::Paren, 7, 5..7),
            BetweenTarget::Select(6..7)
        );
        // The comparison is by text, as in the script.
        let text = "((b) b)";
        assert_eq!(
            between(text, PairKind::Paren, 2, 5..6),
            BetweenTarget::Select(1..6)
        );
    }

    #[test]
    fn select_between_snaps_into_the_first_pair() {
        let text = "let x = 1; call(a) and (b)";
        assert_eq!(
            between(text, PairKind::Paren, 3, 3..3),
            BetweenTarget::Cursor(16)
        );
        assert_eq!(
            between(text, PairKind::Paren, 26, 26..26),
            BetweenTarget::Cursor(16)
        );
        // Nothing matched: just after the first opening delimiter.
        assert_eq!(
            between("x ( y", PairKind::Paren, 0, 0..0),
            BetweenTarget::Cursor(3)
        );
        assert_eq!(
            between("x (", PairKind::Paren, 0, 0..0),
            BetweenTarget::Cursor(3)
        );
        assert_eq!(
            between("plain", PairKind::Paren, 2, 2..2),
            BetweenTarget::Nothing
        );
        assert_eq!(
            between("", PairKind::Brace, 0, 0..0),
            BetweenTarget::Nothing
        );
    }

    #[test]
    fn select_between_quotes() {
        let text = r#"print("hello", 'x')"#;
        assert_eq!(
            between(text, PairKind::DoubleQuote, 9, 9..9),
            BetweenTarget::Select(7..12)
        );
        assert_eq!(
            between(text, PairKind::SingleQuote, 16, 16..16),
            BetweenTarget::Select(16..17)
        );
        assert_eq!(
            between(text, PairKind::DoubleQuote, 2, 2..2),
            BetweenTarget::Cursor(7)
        );
        // An empty pair selects nothing but still places the cursor inside.
        assert_eq!(
            between("a = \"\"", PairKind::DoubleQuote, 5, 5..5),
            BetweenTarget::Select(5..5)
        );
        // Inside a triple-quoted block the whole block's contents are chosen.
        let text = "\"\"\"one\ntwo\"\"\"";
        assert_eq!(
            between(text, PairKind::DoubleQuote, 5, 5..5),
            BetweenTarget::Select(3..10)
        );
    }

    #[test]
    fn select_between_out_of_range_input_is_harmless() {
        assert_eq!(
            between("(a)", PairKind::Paren, 99, 50..80),
            BetweenTarget::Cursor(1)
        );
        assert_eq!(
            between("(a)", PairKind::Paren, 1, 0..999),
            BetweenTarget::Select(1..2)
        );
    }

    // ----- local queries -----

    fn buffer(text: &str) -> Buffer {
        Buffer::from_str(text)
    }

    #[test]
    fn matching_bracket_next_to_the_cursor() {
        let b = buffer("f(a, [b], {c})");
        assert_eq!(matching_bracket(&b, 1), Some((1, 13)), "cursor before (");
        assert_eq!(matching_bracket(&b, 2), Some((1, 13)), "cursor after (");
        assert_eq!(matching_bracket(&b, 13), Some((1, 13)), "cursor before )");
        assert_eq!(matching_bracket(&b, 14), Some((1, 13)), "cursor after )");
        assert_eq!(matching_bracket(&b, 5), Some((5, 7)));
        assert_eq!(matching_bracket(&b, 8), Some((5, 7)));
        assert_eq!(matching_bracket(&b, 10), Some((10, 12)));
        assert_eq!(
            matching_bracket(&b, 3),
            None,
            "no bracket next to the cursor"
        );
        assert_eq!(matching_bracket(&b, 99), None);
        assert_eq!(matching_bracket(&buffer(""), 0), None);
    }

    #[test]
    fn matching_bracket_prefers_the_bracket_after_the_cursor() {
        let b = buffer("(a)(b)");
        assert_eq!(matching_bracket(&b, 3), Some((3, 5)));
        let b = buffer("()");
        assert_eq!(matching_bracket(&b, 1), Some((0, 1)));
    }

    #[test]
    fn matching_bracket_across_lines_and_unmatched() {
        let b = buffer("fn main() {\n    if x {\n        y();\n    }\n}\n");
        assert_eq!(matching_bracket(&b, 10), Some((10, 42)));
        assert_eq!(matching_bracket(&b, 42), Some((10, 42)));
        assert_eq!(matching_bracket(&b, 43), Some((10, 42)));
        assert_eq!(matching_bracket(&b, 21), Some((21, 40)));
        assert_eq!(matching_bracket(&b, 41), Some((21, 40)));
        assert_eq!(matching_bracket(&buffer("( ( )"), 0), None);
        assert_eq!(matching_bracket(&buffer(") ( )"), 0), None);
        assert_eq!(
            matching_bracket(&buffer("(]"), 0),
            None,
            "kinds do not pair with each other"
        );
    }

    #[test]
    fn matching_bracket_respects_strings_and_escapes() {
        let b = buffer(r#"f("(", x)"#);
        assert_eq!(matching_bracket(&b, 1), Some((1, 8)));
        assert_eq!(
            matching_bracket(&b, 3),
            None,
            "a lone bracket in a string has no partner"
        );
        let b = buffer(r#"f("a (b) c")"#);
        assert_eq!(matching_bracket(&b, 5), Some((5, 7)));
        assert_eq!(matching_bracket(&b, 1), Some((1, 11)));
        let b = buffer(r"(a \) b)");
        assert_eq!(matching_bracket(&b, 0), Some((0, 7)));
        assert_eq!(
            matching_bracket(&b, 4),
            None,
            "an escaped bracket is not a bracket"
        );
    }

    #[test]
    fn enclosing_bracket_finds_the_innermost_pair() {
        let b = buffer("f(a, [b, {c}], d)");
        assert_eq!(enclosing_bracket(&b, 3..3), Some((1, 16)));
        assert_eq!(enclosing_bracket(&b, 7..7), Some((5, 12)));
        assert_eq!(enclosing_bracket(&b, 10..11), Some((9, 11)));
        assert_eq!(
            enclosing_bracket(&b, 10..10),
            Some((9, 11)),
            "just inside the opener"
        );
        assert_eq!(
            enclosing_bracket(&b, 11..11),
            Some((9, 11)),
            "just inside the closer"
        );
        assert_eq!(
            enclosing_bracket(&b, 9..12),
            Some((5, 12)),
            "a selected pair is enclosed by the next one"
        );
        assert_eq!(
            enclosing_bracket(&b, 6..13),
            Some((1, 16)),
            "reaching past ] leaves only the parens"
        );
        assert_eq!(enclosing_bracket(&b, 0..0), None);
        assert_eq!(enclosing_bracket(&b, 17..17), None);
        assert_eq!(enclosing_bracket(&b, 1..17), None);
    }

    #[test]
    fn enclosing_bracket_skips_pairs_that_do_not_hold_the_whole_range() {
        let b = buffer("(a (b) c (d) e)");
        // The range starts inside (b) and ends inside (d): only the outer pair holds it.
        assert_eq!(enclosing_bracket(&b, 4..10), Some((0, 14)));
        assert_eq!(enclosing_bracket(&b, 4..5), Some((3, 5)));
    }

    #[test]
    fn enclosing_bracket_inside_strings() {
        let b = buffer(r#"call("say (hi) now", x)"#);
        assert_eq!(
            enclosing_bracket(&b, 12..12),
            Some((10, 13)),
            "the pair inside the string"
        );
        assert_eq!(
            enclosing_bracket(&b, 7..7),
            Some((4, 22)),
            "in the string but in no inner pair"
        );
        let b = buffer(r#"call("a ( b", x)"#);
        assert_eq!(
            enclosing_bracket(&b, 10..10),
            Some((4, 15)),
            "the lone bracket in the string is ignored"
        );
    }

    #[test]
    fn enclosing_bracket_across_lines() {
        let b = buffer("fn main() {\n    let v = [\n        1,\n    ];\n}\n");
        assert_eq!(enclosing_bracket(&b, 35..35), Some((24, 41)));
        assert_eq!(enclosing_bracket(&b, 14..14), Some((10, 44)));
        assert_eq!(enclosing_bracket(&b, 26..40), Some((24, 41)));
    }

    #[test]
    fn enclosing_quotes_on_a_line() {
        let b = buffer(r#"x = "a 'b' c" + `d`"#);
        assert_eq!(enclosing_quote(&b, 6..6), Some((4, 12)));
        assert_eq!(enclosing_quote(&b, 8..9), Some((7, 9)));
        assert_eq!(enclosing_quote(&b, 8..8), Some((7, 9)));
        assert_eq!(
            enclosing_quote(&b, 5..12),
            Some((4, 12)),
            "the inside itself is enclosed"
        );
        assert_eq!(enclosing_quote(&b, 4..13), None);
        assert_eq!(enclosing_quote(&b, 17..18), Some((16, 18)));
        assert_eq!(enclosing_quote(&b, 1..1), None);
        let b = buffer("a \"b\nc\" d");
        assert_eq!(
            enclosing_quote(&b, 3..3),
            None,
            "quotes pair within a line only"
        );
        assert_eq!(enclosing_quote(&b, 3..6), None);
        let b = buffer("don't 'quote' it");
        assert_eq!(enclosing_quote(&b, 9..9), Some((6, 12)));
        assert_eq!(enclosing_quote(&b, 2..2), None);
    }

    #[test]
    fn local_queries_use_char_offsets() {
        let b = buffer("é(世界 [ö] \"ü(x)\")");
        assert_eq!(matching_bracket(&b, 1), Some((1, 15)));
        assert_eq!(matching_bracket(&b, 5), Some((5, 7)));
        assert_eq!(enclosing_bracket(&b, 6..6), Some((5, 7)));
        assert_eq!(enclosing_bracket(&b, 12..12), Some((11, 13)));
        assert_eq!(enclosing_quote(&b, 11..11), Some((9, 14)));
    }

    #[test]
    fn local_queries_agree_with_the_whole_text_pairs() {
        // Every pair found by scanning the whole text is found from both of its ends, and
        // the innermost enclosing pair agrees, for brackets outside and inside strings.
        let texts = [
            "f(a, (b), c)\n[x, {y: (z)}]\n",
            "a (\"(\" b) c ')' (d\n(e) \"f (g) h\" (i\n)) j",
            "{[()]}\n\n  ( [ { } ] )\n) unmatched ( open",
            "it's (don't) \"x (y\" z) 'q (r) s' (t)",
            "\\( (a \\) b) \"c \\\" (d)\" (e)",
        ];
        for text in texts {
            let b = buffer(text);
            let mut all: Vec<(usize, usize)> = Vec::new();
            for kind in [PairKind::Paren, PairKind::Bracket, PairKind::Brace] {
                for pair in find_pairs(b.rope(), kind).0 {
                    assert_eq!(
                        matching_bracket(&b, pair.open),
                        Some((pair.open, pair.close)),
                        "{text:?}"
                    );
                    assert_eq!(
                        matching_bracket(&b, pair.close),
                        Some((pair.open, pair.close)),
                        "{text:?}"
                    );
                    all.push((pair.open, pair.close));
                }
            }
            for pos in 0..=b.len_chars() {
                let expected = all
                    .iter()
                    .copied()
                    .filter(|&(open, close)| open < pos && pos <= close)
                    .max();
                assert_eq!(
                    enclosing_bracket(&b, pos..pos),
                    expected,
                    "enclosing at {pos} in {text:?}"
                );
            }
        }
    }

    #[test]
    fn a_very_long_line_is_not_scanned() {
        let long = format!("({})", "x".repeat(SCAN_LIMIT + 10));
        let b = buffer(&long);
        assert_eq!(matching_bracket(&b, 0), None);
        assert_eq!(enclosing_bracket(&b, 5..5), None);
        assert_eq!(enclosing_quote(&b, 5..5), None);
        // The whole-text search still works on it.
        assert_eq!(find_pairs(b.rope(), PairKind::Paren).0.len(), 1);
    }

    #[test]
    fn a_distant_partner_is_out_of_reach() {
        let far = format!("(\n{}\n)", "line of text\n".repeat(SCAN_LIMIT / 13 + 10));
        let b = buffer(&far);
        assert_eq!(matching_bracket(&b, 0), None);
        let near = format!("(\n{}\n)", "line of text\n".repeat(1000));
        let b = buffer(&near);
        assert_eq!(matching_bracket(&b, 0), Some((0, b.len_chars() - 1)));
        assert_eq!(
            matching_bracket(&b, b.len_chars()),
            Some((0, b.len_chars() - 1))
        );
        assert_eq!(
            enclosing_bracket(&b, 500..500),
            Some((0, b.len_chars() - 1))
        );
    }
}
