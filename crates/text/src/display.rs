//! Display columns and soft wrap.
//!
//! Everything about "how wide is this text on screen" lives here, and both the editing
//! engine (vertical motion, column cursors) and the UI (drawing, mouse hit-testing) call the
//! same functions, so they cannot disagree.
//!
//! The rules:
//!
//! - Text is measured one grapheme cluster at a time; a cluster is never split.
//! - A tab runs to the next multiple of `tab_size`, counted from the start of its row.
//! - A cluster is as wide as `unicode-width` says its string is: 2 for CJK and emoji, 0 for
//!   a lone combining mark or joiner, 1 for almost everything else.
//! - Control characters other than tab take one cell, and so do the Unicode line and
//!   paragraph separators. The UI must draw a visible stand-in for them
//!   ([`control_picture`]), never the raw character.
//! - A line break takes no cell.
//!
//! Soft wrap splits one line into rows. A row is a char range of the line; the rows of a
//! line are contiguous and cover it. The position where one row ends and the next begins is
//! drawn at the start of the next row, so the last place a cursor can stand on a wrapped row
//! is before that row's final cluster.

use std::ops::Range;

use unicode_segmentation::{Graphemes, UnicodeSegmentation};
use unicode_width::UnicodeWidthStr;

use crate::chars::{char_kind, CharKind};

/// Width in cells of one grapheme cluster that starts at display column `col` of its row.
/// `col` only matters for a tab. A `tab_size` of 0 is treated as 1.
pub fn grapheme_width(grapheme: &str, col: usize, tab_size: usize) -> usize {
    match grapheme.as_bytes() {
        [b'\t'] => {
            let tab_size = tab_size.max(1);
            tab_size - (col % tab_size)
        }
        [b'\n'] | [b'\r', b'\n'] => 0,
        [byte] if *byte < 0x80 => 1,
        _ => UnicodeWidthStr::width(grapheme),
    }
}

/// The character the UI should draw in place of one that a terminal would interpret or
/// misplace: the Unicode control pictures for U+0000..U+001F and DEL, U+FFFD for the C1
/// controls, and `␤` and `¶` for the line and paragraph separators U+2028 and U+2029 (which
/// are ordinary one-cell characters inside a line here). `None` for every other character,
/// including tab and line feed, which the UI lays out itself. Each stand-in is one cell
/// wide, as wide as the engine takes the original to be.
pub fn control_picture(c: char) -> Option<char> {
    match c {
        '\t' | '\n' => None,
        '\u{0}'..='\u{1f}' => char::from_u32(0x2400 + c as u32),
        '\u{7f}' => Some('\u{2421}'),
        '\u{80}'..='\u{9f}' => Some('\u{fffd}'),
        '\u{2028}' => Some('\u{2424}'),
        '\u{2029}' => Some('\u{b6}'),
        _ => None,
    }
}

/// One grapheme cluster of a row with its place on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cluster<'a> {
    /// The cluster's text.
    pub text: &'a str,
    /// Char offset of the cluster within the measured string.
    pub char_start: usize,
    /// Number of chars in the cluster.
    pub char_len: usize,
    /// Display column of the cluster's first cell.
    pub col: usize,
    /// Number of cells the cluster takes (0 for a zero-width cluster).
    pub width: usize,
}

/// Iterator over the clusters of one row, left to right. See [`clusters`].
#[derive(Debug, Clone)]
pub struct Clusters<'a> {
    graphemes: Graphemes<'a>,
    char_start: usize,
    col: usize,
    tab_size: usize,
}

impl<'a> Iterator for Clusters<'a> {
    type Item = Cluster<'a>;

    fn next(&mut self) -> Option<Cluster<'a>> {
        let text = self.graphemes.next()?;
        let char_len = if text.len() == 1 {
            1
        } else {
            text.chars().count()
        };
        let width = grapheme_width(text, self.col, self.tab_size);
        let cluster = Cluster {
            text,
            char_start: self.char_start,
            char_len,
            col: self.col,
            width,
        };
        self.char_start += char_len;
        self.col += width;
        Some(cluster)
    }
}

/// Lay out `text` as one row starting at display column 0: every grapheme cluster with its
/// char offset, column and width. This is the single source of truth for drawing a row.
pub fn clusters(text: &str, tab_size: usize) -> Clusters<'_> {
    Clusters {
        graphemes: text.graphemes(true),
        char_start: 0,
        col: 0,
        tab_size,
    }
}

/// Total width in cells of `text` laid out as one row from column 0.
pub fn str_width(text: &str, tab_size: usize) -> usize {
    if is_plain_ascii(text) {
        return text.len();
    }
    clusters(text, tab_size)
        .last()
        .map_or(0, |cluster| cluster.col + cluster.width)
}

/// Printable ASCII only: one byte, one char, one cell each.
fn is_plain_ascii(text: &str) -> bool {
    text.bytes().all(|b| (0x20..0x7f).contains(&b))
}

/// Display column of the position `char_col` chars into `text`, with `text` laid out as one
/// row. A position inside a cluster reports the cluster's first column; a position past the
/// end reports the total width.
pub fn char_col_to_display_col(text: &str, char_col: usize, tab_size: usize) -> usize {
    if is_plain_ascii(text) {
        return char_col.min(text.len());
    }
    let mut end = 0;
    for cluster in clusters(text, tab_size) {
        if char_col < cluster.char_start + cluster.char_len {
            return cluster.col;
        }
        end = cluster.col + cluster.width;
    }
    end
}

/// The char offset in `text` (laid out as one row) of the cluster that covers display column
/// `display_col`: a column in the middle of a tab or a wide character maps to the start of
/// that cluster. A column past the end maps to the end of the text.
pub fn display_col_to_char_col(text: &str, display_col: usize, tab_size: usize) -> usize {
    if is_plain_ascii(text) {
        return display_col.min(text.len());
    }
    let mut end = 0;
    for cluster in clusters(text, tab_size) {
        if cluster.col + cluster.width > display_col || cluster.col >= display_col {
            return cluster.char_start;
        }
        end = cluster.char_start + cluster.char_len;
    }
    end
}

/// The part of `text` covered by a char range. Out-of-range ends are clamped.
pub fn slice_chars(text: &str, chars: Range<usize>) -> &str {
    if text.is_ascii() {
        let end = chars.end.min(text.len());
        let start = chars.start.min(end);
        return &text[start..end];
    }
    let mut indices = text.char_indices().map(|(byte, _)| byte);
    let start = indices.nth(chars.start).unwrap_or(text.len());
    let end = if chars.end <= chars.start {
        start
    } else {
        indices
            .nth(chars.end - chars.start - 1)
            .unwrap_or(text.len())
    };
    &text[start..end]
}

/// Whether a row may be broken between two neighbouring clusters of these classes: after
/// whitespace, and between punctuation and a word in either order.
fn is_break_opportunity(before: CharKind, after: CharKind) -> bool {
    match (before, after) {
        (CharKind::Whitespace, CharKind::Whitespace) => false,
        (CharKind::Whitespace, _) => true,
        (CharKind::Punctuation, CharKind::Word) | (CharKind::Word, CharKind::Punctuation) => true,
        _ => false,
    }
}

/// Split one line into soft-wrap rows no wider than `width` cells.
///
/// `line` is the line's text without its line break. The result is the char range of each
/// row; there is always at least one row (an empty line is the single row `0..0`), the rows
/// are contiguous and they cover the whole line.
///
/// A row is broken at the last word boundary that fits: after whitespace, or between
/// punctuation and a word. A token longer than the row is broken wherever the row is full.
/// A cluster is never split, so a row holding a single cluster wider than `width` (a wide
/// character with `width` 1) overflows. Leading indentation is never left alone on the
/// first row. Continuation rows get no extra indent, and tab stops count from the start of
/// each row.
///
/// `width` 0 or `usize::MAX` means no wrapping: the whole line is one row.
pub fn wrap_line(line: &str, width: usize, tab_size: usize) -> Vec<Range<usize>> {
    if width == 0 || width == usize::MAX {
        let chars = if line.is_ascii() {
            line.len()
        } else {
            line.chars().count()
        };
        return single_row(chars);
    }
    if line.len() <= width && is_plain_ascii(line) {
        return single_row(line.len());
    }

    struct Item<'a> {
        text: &'a str,
        char_start: usize,
        kind: CharKind,
    }
    let mut items: Vec<Item<'_>> = Vec::new();
    let mut char_count = 0;
    for text in line.graphemes(true) {
        let first = text.chars().next().unwrap_or(' ');
        items.push(Item {
            text,
            char_start: char_count,
            kind: char_kind(first, ""),
        });
        char_count += if text.len() == 1 {
            1
        } else {
            text.chars().count()
        };
    }

    let mut rows = Vec::new();
    let mut row_start = 0;
    let mut index = 0;
    let mut col = 0;
    let mut last_break: Option<usize> = None;
    let mut row_has_content = false;
    while index < items.len() {
        let item = &items[index];
        let may_break_here = index > row_start
            && (row_has_content || !rows.is_empty())
            && is_break_opportunity(items[index - 1].kind, item.kind);
        if may_break_here {
            last_break = Some(index);
        }
        let item_width = grapheme_width(item.text, col, tab_size);
        if col + item_width > width && index > row_start {
            let at = last_break.unwrap_or(index);
            rows.push(items[row_start].char_start..items[at].char_start);
            row_start = at;
            index = at;
            col = 0;
            last_break = None;
            row_has_content = false;
            continue;
        }
        col += item_width;
        row_has_content |= item.kind != CharKind::Whitespace;
        index += 1;
    }
    let last_start = items
        .get(row_start)
        .map_or(char_count, |item| item.char_start);
    rows.push(last_start..char_count);
    rows
}

fn single_row(chars: usize) -> Vec<Range<usize>> {
    std::iter::once(0..chars).collect()
}

/// Where the position `char_col` chars into `line` is drawn: `(row, display_col)`, given the
/// rows from [`wrap_line`] for the same line and tab size.
///
/// The position where a row ends is the start of the next row, so it is reported there. A
/// position past the end of the line is reported at the end of the last row. With no rows at
/// all the answer is `(0, 0)`.
pub fn char_col_to_row_col(
    line: &str,
    rows: &[Range<usize>],
    char_col: usize,
    tab_size: usize,
) -> (usize, usize) {
    let Some(last) = rows.len().checked_sub(1) else {
        return (0, 0);
    };
    let row = rows
        .iter()
        .position(|range| char_col < range.end)
        .unwrap_or(last);
    let range = &rows[row];
    let text = slice_chars(line, range.clone());
    let local = char_col.saturating_sub(range.start).min(range.len());
    (row, char_col_to_display_col(text, local, tab_size))
}

/// The char offset in `line` drawn at `(row, display_col)`, given the rows from
/// [`wrap_line`] for the same line and tab size.
///
/// `row` past the last row means the last row. A column in the middle of a tab or a wide
/// character maps to the start of that cluster. A column past the end of the last row maps
/// to the end of the line; past the end of an earlier row it maps to the position before
/// that row's final cluster, the last position still drawn on that row. With no rows at all
/// the answer is 0.
pub fn row_col_to_char_col(
    line: &str,
    rows: &[Range<usize>],
    row: usize,
    display_col: usize,
    tab_size: usize,
) -> usize {
    let Some(last) = rows.len().checked_sub(1) else {
        return 0;
    };
    let row = row.min(last);
    let range = &rows[row];
    let text = slice_chars(line, range.clone());
    let local = display_col_to_char_col(text, display_col, tab_size);
    if row < last && range.start + local >= range.end {
        return range.start + last_cluster_start(text);
    }
    range.start + local
}

/// The char offset of the last position drawn on a wrapped row: before the row's final
/// cluster. 0 for an empty row.
pub fn last_cluster_start(row_text: &str) -> usize {
    match row_text.graphemes(true).next_back() {
        Some(last) => {
            let before = &row_text[..row_text.len() - last.len()];
            if before.is_ascii() {
                before.len()
            } else {
                before.chars().count()
            }
        }
        None => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows_of(line: &str, width: usize, tab_size: usize) -> Vec<String> {
        wrap_line(line, width, tab_size)
            .into_iter()
            .map(|range| slice_chars(line, range).to_string())
            .collect()
    }

    /// The rows must be contiguous, cover the line, and each fit the width unless it is a
    /// single cluster.
    fn check_invariants(line: &str, width: usize, tab_size: usize) {
        let rows = wrap_line(line, width, tab_size);
        assert!(!rows.is_empty());
        assert_eq!(rows[0].start, 0);
        assert_eq!(rows.last().unwrap().end, line.chars().count());
        for pair in rows.windows(2) {
            assert_eq!(pair[0].end, pair[1].start);
            assert!(
                pair[0].start < pair[0].end,
                "empty row in {line:?} at {width}"
            );
        }
        if width == 0 || width == usize::MAX {
            assert_eq!(rows.len(), 1);
            return;
        }
        for range in &rows {
            let text = slice_chars(line, range.clone());
            let cluster_count = text.graphemes(true).count();
            assert!(
                str_width(text, tab_size) <= width || cluster_count <= 1,
                "row {text:?} of {line:?} is wider than {width}"
            );
            assert_eq!(text.graphemes(true).collect::<String>(), text);
        }
        // Rows never split a cluster of the whole line.
        let mut boundaries = vec![0];
        let mut chars = 0;
        for cluster in line.graphemes(true) {
            chars += cluster.chars().count();
            boundaries.push(chars);
        }
        for range in &rows {
            assert!(
                boundaries.contains(&range.start),
                "row starts inside a cluster in {line:?}"
            );
        }
    }

    #[test]
    fn widths_of_clusters() {
        assert_eq!(grapheme_width("a", 0, 4), 1);
        assert_eq!(grapheme_width("\t", 0, 4), 4);
        assert_eq!(grapheme_width("\t", 1, 4), 3);
        assert_eq!(grapheme_width("\t", 3, 4), 1);
        assert_eq!(grapheme_width("\t", 4, 4), 4);
        assert_eq!(grapheme_width("\t", 5, 0), 1);
        assert_eq!(grapheme_width("世", 0, 4), 2);
        assert_eq!(grapheme_width("🙂", 0, 4), 2);
        assert_eq!(grapheme_width("👨‍👩‍👧", 0, 4), 2);
        assert_eq!(grapheme_width("🇨🇦", 0, 4), 2);
        assert_eq!(grapheme_width("e\u{301}", 0, 4), 1);
        assert_eq!(grapheme_width("\u{301}", 0, 4), 0);
        assert_eq!(grapheme_width("\u{200b}", 0, 4), 0);
        assert_eq!(grapheme_width("\n", 3, 4), 0);
        assert_eq!(grapheme_width("\r\n", 3, 4), 0);
        assert_eq!(grapheme_width("\u{1b}", 0, 4), 1);
        assert_eq!(grapheme_width("\u{7f}", 0, 4), 1);
        assert_eq!(grapheme_width("\u{85}", 0, 4), 1);
    }

    #[test]
    fn control_pictures() {
        assert_eq!(control_picture('\u{1b}'), Some('␛'));
        assert_eq!(control_picture('\u{0}'), Some('␀'));
        assert_eq!(control_picture('\u{c}'), Some('␌'));
        assert_eq!(control_picture('\u{7f}'), Some('␡'));
        assert_eq!(control_picture('\u{85}'), Some('\u{fffd}'));
        assert_eq!(control_picture('\t'), None);
        assert_eq!(control_picture('\n'), None);
        assert_eq!(control_picture('a'), None);
        assert_eq!(control_picture('\u{2028}'), Some('␤'));
        assert_eq!(control_picture('\u{2029}'), Some('¶'));
        // Every stand-in is exactly as wide as the engine assumes the control is.
        for code in (0..0xa0u32).chain([0x2028, 0x2029]) {
            let c = char::from_u32(code).unwrap();
            if let Some(picture) = control_picture(c) {
                let mut buf = [0u8; 4];
                assert_eq!(grapheme_width(picture.encode_utf8(&mut buf), 0, 4), 1);
                let mut buf = [0u8; 4];
                assert_eq!(
                    grapheme_width(c.encode_utf8(&mut buf), 0, 4),
                    1,
                    "U+{code:04X}"
                );
            }
        }
    }

    #[test]
    fn cluster_layout() {
        let laid: Vec<(String, usize, usize, usize)> = clusters("a\t世e\u{301}!", 4)
            .map(|c| (c.text.to_string(), c.char_start, c.col, c.width))
            .collect();
        assert_eq!(
            laid,
            vec![
                ("a".to_string(), 0, 0, 1),
                ("\t".to_string(), 1, 1, 3),
                ("世".to_string(), 2, 4, 2),
                ("e\u{301}".to_string(), 3, 6, 1),
                ("!".to_string(), 5, 7, 1),
            ]
        );
        assert_eq!(str_width("a\t世e\u{301}!", 4), 8);
        assert_eq!(str_width("", 4), 0);
        assert_eq!(str_width("abc", 4), 3);
        assert_eq!(str_width("\t\t", 8), 16);
    }

    #[test]
    fn columns_round_trip() {
        let text = "a\t世e\u{301}!";
        assert_eq!(char_col_to_display_col(text, 0, 4), 0);
        assert_eq!(char_col_to_display_col(text, 1, 4), 1);
        assert_eq!(char_col_to_display_col(text, 2, 4), 4);
        assert_eq!(char_col_to_display_col(text, 3, 4), 6);
        assert_eq!(char_col_to_display_col(text, 4, 4), 6, "inside a cluster");
        assert_eq!(char_col_to_display_col(text, 5, 4), 7);
        assert_eq!(char_col_to_display_col(text, 6, 4), 8);
        assert_eq!(char_col_to_display_col(text, 60, 4), 8);

        assert_eq!(display_col_to_char_col(text, 0, 4), 0);
        assert_eq!(display_col_to_char_col(text, 1, 4), 1);
        assert_eq!(display_col_to_char_col(text, 2, 4), 1, "inside the tab");
        assert_eq!(display_col_to_char_col(text, 3, 4), 1);
        assert_eq!(display_col_to_char_col(text, 4, 4), 2);
        assert_eq!(
            display_col_to_char_col(text, 5, 4),
            2,
            "second half of a wide char"
        );
        assert_eq!(display_col_to_char_col(text, 6, 4), 3);
        assert_eq!(display_col_to_char_col(text, 7, 4), 5);
        assert_eq!(display_col_to_char_col(text, 8, 4), 6);
        assert_eq!(display_col_to_char_col(text, 99, 4), 6);

        assert_eq!(char_col_to_display_col("hello", 3, 4), 3);
        assert_eq!(char_col_to_display_col("hello", 9, 4), 5);
        assert_eq!(display_col_to_char_col("hello", 3, 4), 3);
        assert_eq!(display_col_to_char_col("hello", 9, 4), 5);
        assert_eq!(display_col_to_char_col("", 3, 4), 0);
        // A zero-width cluster shares its column with what follows; the column maps to it.
        assert_eq!(display_col_to_char_col("a\u{200b}b", 1, 4), 1);
    }

    #[test]
    fn slicing_by_chars() {
        assert_eq!(slice_chars("hello", 1..3), "el");
        assert_eq!(slice_chars("hello", 3..99), "lo");
        assert_eq!(slice_chars("hello", 9..12), "");
        assert_eq!(slice_chars("héllo wörld", 1..4), "éll");
        assert_eq!(slice_chars("héllo wörld", 7..99), "örld");
        assert_eq!(slice_chars("héllo", 3..3), "");
        assert_eq!(slice_chars("héllo", Range { start: 4, end: 2 }), "");
        assert_eq!(slice_chars("世界", 1..2), "界");
        assert_eq!(slice_chars("世界", 2..2), "");
        assert_eq!(slice_chars("世界", 5..9), "");
    }

    #[test]
    fn no_wrap_widths() {
        assert_eq!(wrap_line("hello world", 0, 4), vec![0..11]);
        assert_eq!(wrap_line("hello world", usize::MAX, 4), vec![0..11]);
        assert_eq!(wrap_line("", 0, 4), vec![0..0]);
        assert_eq!(wrap_line("", 10, 4), vec![0..0]);
        assert_eq!(wrap_line("héllo", 0, 4), vec![0..5]);
        assert_eq!(wrap_line("short", 80, 4), vec![0..5]);
    }

    #[test]
    fn wraps_after_whitespace() {
        assert_eq!(
            rows_of("the quick brown fox", 10, 4),
            vec!["the quick ", "brown fox"]
        );
        assert_eq!(
            rows_of("the quick brown fox", 9, 4),
            vec!["the ", "quick ", "brown fox"]
        );
        assert_eq!(rows_of("aaa bbb ccc", 7, 4), vec!["aaa ", "bbb ccc"]);
        assert_eq!(rows_of("aaa bbb ccc", 8, 4), vec!["aaa bbb ", "ccc"]);
        assert_eq!(rows_of("a  b", 2, 4), vec!["a ", " b"]);
    }

    #[test]
    fn wraps_between_punctuation_and_words() {
        assert_eq!(rows_of("foo.bar.baz", 5, 4), vec!["foo.", "bar.", "baz"]);
        assert_eq!(
            rows_of("call(argument)", 6, 4),
            vec!["call(", "argume", "nt)"]
        );
        assert_eq!(rows_of("a->b->c", 4, 4), vec!["a->b", "->c"]);
    }

    #[test]
    fn hard_breaks_long_tokens() {
        assert_eq!(rows_of("abcdefghij", 4, 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(
            rows_of("x abcdefghij", 4, 4),
            vec!["x ", "abcd", "efgh", "ij"]
        );
        assert_eq!(rows_of("hello world", 5, 4), vec!["hello", " ", "world"]);
    }

    #[test]
    fn indentation_is_not_left_alone_on_the_first_row() {
        assert_eq!(rows_of("    foobar", 8, 4), vec!["    foob", "ar"]);
        assert_eq!(rows_of("    foo bar", 8, 4), vec!["    foo ", "bar"]);
        assert_eq!(rows_of("\tfoo", 6, 4), vec!["\tfo", "o"]);
    }

    #[test]
    fn wide_characters_and_clusters_are_never_split() {
        assert_eq!(rows_of("abcd世界", 5, 4), vec!["abcd", "世界"]);
        assert_eq!(rows_of("世界你好", 4, 4), vec!["世界", "你好"]);
        assert_eq!(rows_of("世界你好", 5, 4), vec!["世界", "你好"]);
        assert_eq!(
            rows_of("世界", 1, 4),
            vec!["世", "界"],
            "a wide char overflows a 1-cell row"
        );
        assert_eq!(rows_of("ab👨‍👩‍👧cd", 3, 4), vec!["ab", "👨‍👩‍👧", "cd"]);
        assert_eq!(rows_of("ab👨‍👩‍👧👨‍👩‍👧", 3, 4), vec!["ab", "👨‍👩‍👧", "👨‍👩‍👧"]);
        assert_eq!(
            rows_of("e\u{301}e\u{301}e\u{301}", 2, 4),
            vec!["e\u{301}e\u{301}", "e\u{301}"]
        );
        assert_eq!(rows_of("🇨🇦🇹🇼🇯🇵", 4, 4), vec!["🇨🇦🇹🇼", "🇯🇵"]);
    }

    #[test]
    fn tabs_count_from_the_row_start() {
        // "ab\tcd": the tab runs from column 2 to 4, so the row is 6 wide.
        assert_eq!(rows_of("ab\tcd", 6, 4), vec!["ab\tcd"]);
        assert_eq!(rows_of("ab\tcd", 5, 4), vec!["ab\t", "cd"]);
        // After a wrap the tab starts its row and is a full stop wide.
        assert_eq!(rows_of("abcd\tef", 4, 4), vec!["abcd", "\t", "ef"]);
        assert_eq!(rows_of("abcd\tef", 6, 4), vec!["abcd", "\tef"]);
    }

    #[test]
    fn invariants_hold_for_many_shapes() {
        let lines = [
            "",
            "a",
            "the quick brown fox jumps over the lazy dog",
            "    indented   with   runs   of   spaces    ",
            "\t\ttabs\tand spaces \t mixed",
            "foo.bar(baz, qux)->quux::<T>[0]{a: b}",
            "世界你好，这是一个很长的句子。",
            "emoji 👨‍👩‍👧‍👦 and 🇨🇦 flags 👍🏽 with e\u{301} marks",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "x\u{200b}y\u{feff}z",
            "control \u{1b}[0m chars\u{7f}",
        ];
        for line in lines {
            for width in [0, 1, 2, 3, 4, 5, 7, 8, 10, 13, 20, 40, 200, usize::MAX] {
                for tab_size in [1, 2, 4, 8] {
                    check_invariants(line, width, tab_size);
                }
            }
        }
    }

    #[test]
    fn row_and_column_of_a_position() {
        let line = "the quick brown fox";
        let rows = wrap_line(line, 10, 4);
        assert_eq!(rows, vec![0..10, 10..19]);
        assert_eq!(char_col_to_row_col(line, &rows, 0, 4), (0, 0));
        assert_eq!(char_col_to_row_col(line, &rows, 9, 4), (0, 9));
        assert_eq!(
            char_col_to_row_col(line, &rows, 10, 4),
            (1, 0),
            "a row end is the next row's start"
        );
        assert_eq!(char_col_to_row_col(line, &rows, 15, 4), (1, 5));
        assert_eq!(char_col_to_row_col(line, &rows, 19, 4), (1, 9));
        assert_eq!(char_col_to_row_col(line, &rows, 99, 4), (1, 9));
        assert_eq!(char_col_to_row_col(line, &[], 3, 4), (0, 0));

        assert_eq!(row_col_to_char_col(line, &rows, 0, 0, 4), 0);
        assert_eq!(row_col_to_char_col(line, &rows, 0, 4, 4), 4);
        assert_eq!(row_col_to_char_col(line, &rows, 0, 9, 4), 9);
        assert_eq!(
            row_col_to_char_col(line, &rows, 0, 10, 4),
            9,
            "stays on the wrapped row"
        );
        assert_eq!(row_col_to_char_col(line, &rows, 0, 99, 4), 9);
        assert_eq!(row_col_to_char_col(line, &rows, 1, 0, 4), 10);
        assert_eq!(row_col_to_char_col(line, &rows, 1, 99, 4), 19);
        assert_eq!(
            row_col_to_char_col(line, &rows, 7, 3, 4),
            13,
            "rows past the end clamp"
        );
        assert_eq!(row_col_to_char_col(line, &[], 0, 3, 4), 0);
    }

    #[test]
    fn row_and_column_with_wide_text() {
        let line = "ab世界cd";
        let rows = wrap_line(line, 4, 4);
        assert_eq!(rows, vec![0..3, 3..6]);
        assert_eq!(char_col_to_row_col(line, &rows, 2, 4), (0, 2));
        assert_eq!(char_col_to_row_col(line, &rows, 3, 4), (1, 0));
        assert_eq!(char_col_to_row_col(line, &rows, 4, 4), (1, 2));
        assert_eq!(char_col_to_row_col(line, &rows, 6, 4), (1, 4));
        assert_eq!(
            row_col_to_char_col(line, &rows, 0, 3, 4),
            2,
            "middle of the wide char"
        );
        assert_eq!(
            row_col_to_char_col(line, &rows, 0, 4, 4),
            2,
            "before the row's last cluster"
        );
        assert_eq!(row_col_to_char_col(line, &rows, 1, 1, 4), 3);
        assert_eq!(row_col_to_char_col(line, &rows, 1, 2, 4), 4);
        assert_eq!(row_col_to_char_col(line, &rows, 1, 4, 4), 6);
    }

    #[test]
    fn every_position_round_trips_through_rows() {
        let line = "fn main() {\tprintln!(\"héllo 世界 👍🏽\"); }";
        for width in [3, 5, 8, 12, 40] {
            let rows = wrap_line(line, width, 4);
            let mut boundaries = vec![0];
            let mut chars = 0;
            for cluster in line.graphemes(true) {
                chars += cluster.chars().count();
                boundaries.push(chars);
            }
            for &pos in &boundaries {
                let (row, col) = char_col_to_row_col(line, &rows, pos, 4);
                assert_eq!(
                    row_col_to_char_col(line, &rows, row, col, 4),
                    pos,
                    "pos {pos} at width {width}"
                );
            }
        }
    }

    #[test]
    fn last_cluster_of_a_row() {
        assert_eq!(last_cluster_start(""), 0);
        assert_eq!(last_cluster_start("a"), 0);
        assert_eq!(last_cluster_start("abc "), 3);
        assert_eq!(last_cluster_start("世界"), 1);
        assert_eq!(last_cluster_start("ae\u{301}"), 1);
    }
}
