//! Highlights by line.
//!
//! The highlighter works in byte offsets into the whole text; a view draws one line at a
//! time. [`LineHighlights`] is the same spans cut at line ends and filed by line, so drawing
//! a line is one slice lookup.

use std::ops::Range;

use crate::capture::CaptureId;
use crate::highlight::HighlightSpan;

/// Which characters end a line.
///
/// Line numbers only mean something if this crate and the text buffer count lines the same
/// way, so pick the variant that matches the buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LineBreaks {
    /// Every break a `ropey` rope counts with its default features: `\n`, `\r\n` (as one
    /// break), a lone `\r`, vertical tab, form feed, NEL (U+0085), LS (U+2028) and PS
    /// (U+2029). This is the default because bakedit's buffers are ropes.
    #[default]
    Unicode,
    /// `\n` only, which is how tree-sitter and Zed count. A `\r` just before the `\n` belongs
    /// to the line end, not to the line.
    Lf,
}

impl LineBreaks {
    /// The length in bytes of the line break starting at `bytes[at]`, or 0 if there is none.
    fn len_at(self, bytes: &[u8], at: usize) -> usize {
        let unicode = self == LineBreaks::Unicode;
        match bytes[at] {
            b'\n' => 1,
            b'\r' if bytes.get(at + 1) == Some(&b'\n') => 2,
            b'\r' | 0x0B | 0x0C if unicode => 1,
            0xC2 if unicode && bytes.get(at + 1) == Some(&0x85) => 2,
            0xE2 if unicode
                && bytes.get(at + 1) == Some(&0x80)
                && matches!(bytes.get(at + 2), Some(0xA8 | 0xA9)) =>
            {
                3
            }
            _ => 0,
        }
    }
}

/// The lines of a text, each as the byte range of its content without the line break. There
/// is always at least one line, and a text that ends with a line break has an empty last
/// line, as in a rope.
pub(crate) fn line_ranges(
    text: &str,
    breaks: LineBreaks,
) -> impl Iterator<Item = Range<usize>> + '_ {
    let bytes = text.as_bytes();
    let mut next_start = Some(0);
    std::iter::from_fn(move || {
        let start = next_start?;
        let mut at = start;
        while at < bytes.len() {
            let break_len = breaks.len_at(bytes, at);
            if break_len > 0 {
                next_start = Some(at + break_len);
                return Some(start..at);
            }
            at += 1;
        }
        next_start = None;
        Some(start..bytes.len())
    })
}

/// The 0-based line each of `offsets` falls on. `offsets` must be ascending; the answer for
/// an offset inside a line break is the line the break ends.
pub(crate) fn lines_of_offsets(text: &str, offsets: &[usize], breaks: LineBreaks) -> Vec<usize> {
    // The lines after the first: each one that starts at or before an offset moves it down.
    let mut later_lines = line_ranges(text, breaks).skip(1).peekable();
    let mut line = 0;
    offsets
        .iter()
        .map(|&offset| {
            while later_lines.next_if(|next| next.start <= offset).is_some() {
                line += 1;
            }
            line
        })
        .collect()
}

/// Spans filed by line, with columns counted in bytes from the start of the line.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LineHighlights {
    /// `spans[line_starts[n]..line_starts[n + 1]]` are line `n`'s spans.
    line_starts: Vec<u32>,
    spans: Vec<(Range<usize>, CaptureId)>,
}

impl LineHighlights {
    /// File `spans` by line, counting lines the default way ([`LineBreaks::Unicode`]).
    ///
    /// `spans` must be sorted and must not overlap, which is what
    /// [`Highlighter::highlight`](crate::Highlighter::highlight) returns. A span that crosses
    /// a line end is cut into one piece per line, and the line break itself is never part of
    /// a piece.
    pub fn new(text: &str, spans: &[HighlightSpan]) -> LineHighlights {
        LineHighlights::with_line_breaks(text, spans, LineBreaks::default())
    }

    /// As [`LineHighlights::new`], counting lines the way `breaks` says.
    pub fn with_line_breaks(
        text: &str,
        spans: &[HighlightSpan],
        breaks: LineBreaks,
    ) -> LineHighlights {
        let mut line_starts = Vec::new();
        let mut pieces: Vec<(Range<usize>, CaptureId)> = Vec::with_capacity(spans.len());
        let mut first = 0;
        for line in line_ranges(text, breaks) {
            line_starts.push(pieces.len() as u32);
            while spans.get(first).is_some_and(|span| span.end <= line.start) {
                first += 1;
            }
            // A span that runs past this line is looked at again on the next one, so `first`
            // stays where it is; spans do not overlap, so each line only revisits one.
            for span in &spans[first.min(spans.len())..] {
                if span.start >= line.end {
                    break;
                }
                let start = span.start.max(line.start);
                let end = span.end.min(line.end);
                if start < end {
                    pieces.push((start - line.start..end - line.start, span.capture));
                }
            }
        }
        line_starts.push(pieces.len() as u32);
        LineHighlights {
            line_starts,
            spans: pieces,
        }
    }

    /// The spans on a line, left to right, as byte ranges within the line and the capture to
    /// draw each with. Bytes no span covers are plain text. A line past the end has none.
    pub fn spans_for_line(&self, line: usize) -> &[(Range<usize>, CaptureId)] {
        let next = line
            .checked_add(1)
            .and_then(|next| self.line_starts.get(next));
        match (self.line_starts.get(line), next) {
            (Some(&start), Some(&end)) => &self.spans[start as usize..end as usize],
            _ => &[],
        }
    }

    /// How many lines the text had: one more than its line breaks. Zero only for a value
    /// made by `Default`, which stands for "nothing highlighted yet".
    pub fn line_count(&self) -> usize {
        self.line_starts.len().saturating_sub(1)
    }

    /// Whether no line has any span.
    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::intern;

    fn lines_of(text: &str, breaks: LineBreaks) -> Vec<&str> {
        line_ranges(text, breaks)
            .map(|range| &text[range])
            .collect()
    }

    #[test]
    fn splits_lines_like_a_rope() {
        use LineBreaks::Unicode;
        assert_eq!(lines_of("", Unicode), [""]);
        assert_eq!(lines_of("a", Unicode), ["a"]);
        assert_eq!(lines_of("a\n", Unicode), ["a", ""]);
        assert_eq!(lines_of("a\nb", Unicode), ["a", "b"]);
        assert_eq!(lines_of("a\r\nb\r\n", Unicode), ["a", "b", ""]);
        assert_eq!(lines_of("a\rb", Unicode), ["a", "b"]);
        assert_eq!(lines_of("\n\n", Unicode), ["", "", ""]);
        assert_eq!(lines_of("a\u{b}b\u{c}c", Unicode), ["a", "b", "c"]);
        assert_eq!(
            lines_of("a\u{85}b\u{2028}c\u{2029}d", Unicode),
            ["a", "b", "c", "d"]
        );
        // Bytes that only look like the start of a break.
        assert_eq!(lines_of("é\u{2027}\u{a0}x", Unicode), ["é\u{2027}\u{a0}x"]);
    }

    #[test]
    fn counts_lines_exactly_as_a_rope_does() {
        // Every line break a rope knows, bytes that begin like one, and ordinary text.
        let pieces = [
            "\n", "\r", "\r\n", "\u{b}", "\u{c}", "\u{85}", "\u{2028}", "\u{2029}", "\u{86}",
            "\u{2027}", "\u{202a}", "a", " ", "é", "漢", "\t",
        ];
        let breaks = &pieces[..8];
        let mut state = 0x2545_F491_4F6C_DD1D_u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as usize
        };
        for _ in 0..2000 {
            let text: String = (0..next() % 24)
                .map(|_| pieces[next() % pieces.len()])
                .collect();
            let rope = ropey::Rope::from_str(&text);
            let ranges: Vec<Range<usize>> = line_ranges(&text, LineBreaks::Unicode).collect();
            assert_eq!(ranges.len(), rope.len_lines(), "{text:?}");
            for (line, range) in ranges.iter().enumerate() {
                assert_eq!(range.start, rope.line_to_byte(line), "{text:?} line {line}");
                // What lies between this line's content and the next line is one break.
                let next_start = ranges.get(line + 1).map_or(text.len(), |next| next.start);
                let between = &text[range.end..next_start];
                let is_last = line + 1 == ranges.len();
                assert_eq!(between.is_empty(), is_last, "{text:?} line {line}");
                assert!(is_last || breaks.contains(&between), "{text:?} line {line}");
            }
            // The first and last byte offsets of each line's content are on that line.
            let offsets: Vec<usize> = ranges
                .iter()
                .flat_map(|range| [range.start, range.end])
                .collect();
            let expected: Vec<usize> = offsets
                .iter()
                .map(|&offset| rope.byte_to_line(offset))
                .collect();
            assert_eq!(
                lines_of_offsets(&text, &offsets, LineBreaks::Unicode),
                expected,
                "{text:?}"
            );
            assert_eq!(
                LineHighlights::new(&text, &[]).line_count(),
                rope.len_lines()
            );
        }
    }

    #[test]
    fn lf_mode_breaks_only_at_line_feeds() {
        use LineBreaks::Lf;
        assert_eq!(lines_of("a\nb", Lf), ["a", "b"]);
        assert_eq!(lines_of("a\r\nb", Lf), ["a", "b"]);
        assert_eq!(lines_of("a\rb\u{c}c\u{2028}d", Lf), ["a\rb\u{c}c\u{2028}d"]);
        assert_eq!(lines_of("a\n", Lf), ["a", ""]);
    }

    #[test]
    fn finds_the_line_of_each_offset() {
        let text = "ab\ncd\r\n\nef";
        //          0123 4567 8 9
        let offsets: Vec<usize> = (0..=text.len()).collect();
        assert_eq!(
            lines_of_offsets(text, &offsets, LineBreaks::Unicode),
            [0, 0, 0, 1, 1, 1, 1, 2, 3, 3, 3]
        );
        assert_eq!(lines_of_offsets("", &[0], LineBreaks::Unicode), [0]);
        assert_eq!(
            lines_of_offsets("a\u{c}b", &[0, 2], LineBreaks::Unicode),
            [0, 1]
        );
        assert_eq!(lines_of_offsets("a\u{c}b", &[0, 2], LineBreaks::Lf), [0, 0]);
        // Offsets past the end stay on the last line; repeated offsets are fine.
        assert_eq!(
            lines_of_offsets("a\nb", &[2, 2, 9], LineBreaks::Unicode),
            [1, 1, 1]
        );
    }

    fn span(start: usize, end: usize, name: &str) -> HighlightSpan {
        HighlightSpan {
            start,
            end,
            capture: intern(name).unwrap(),
        }
    }

    #[test]
    fn files_spans_by_line_with_line_relative_columns() {
        let text = "let a = 1;\nlet bb = \"x\";\n";
        let keyword = intern("keyword").unwrap();
        let number = intern("number").unwrap();
        let string = intern("string").unwrap();
        let spans = [
            span(0, 3, "keyword"),
            span(8, 9, "number"),
            span(11, 14, "keyword"),
            span(20, 23, "string"),
        ];
        let lines = LineHighlights::new(text, &spans);
        assert_eq!(lines.line_count(), 3);
        assert_eq!(lines.spans_for_line(0), [(0..3, keyword), (8..9, number)]);
        assert_eq!(lines.spans_for_line(1), [(0..3, keyword), (9..12, string)]);
        assert_eq!(lines.spans_for_line(2), []);
        assert_eq!(lines.spans_for_line(3), []);
        assert_eq!(lines.spans_for_line(usize::MAX), []);
        assert!(!lines.is_empty());
    }

    #[test]
    fn a_span_across_lines_is_cut_at_each_line_end() {
        let text = "/* one\n   two\r\n   three */ x";
        let comment = intern("comment").unwrap();
        let end = text.find(" x").unwrap();
        let lines = LineHighlights::new(text, &[span(0, end, "comment")]);
        assert_eq!(lines.line_count(), 3);
        assert_eq!(lines.spans_for_line(0), [(0..6, comment)]);
        // The `\r\n` is not part of the middle line.
        assert_eq!(lines.spans_for_line(1), [(0..6, comment)]);
        assert_eq!(lines.spans_for_line(2), [(0..11, comment)]);
    }

    #[test]
    fn a_span_that_is_only_a_line_break_disappears() {
        let text = "a\n\nb";
        let lines = LineHighlights::new(text, &[span(1, 3, "comment"), span(3, 4, "keyword")]);
        assert_eq!(lines.spans_for_line(0), []);
        assert_eq!(lines.spans_for_line(1), []);
        assert_eq!(
            lines.spans_for_line(2),
            [(0..1, intern("keyword").unwrap())]
        );
    }

    #[test]
    fn line_break_mode_changes_the_filing() {
        let text = "a\u{c}bc";
        let spans = [span(3, 4, "keyword")];
        let keyword = intern("keyword").unwrap();
        let unicode = LineHighlights::with_line_breaks(text, &spans, LineBreaks::Unicode);
        assert_eq!(unicode.line_count(), 2);
        assert_eq!(unicode.spans_for_line(1), [(1..2, keyword)]);
        let lf = LineHighlights::with_line_breaks(text, &spans, LineBreaks::Lf);
        assert_eq!(lf.line_count(), 1);
        assert_eq!(lf.spans_for_line(0), [(3..4, keyword)]);
    }

    #[test]
    fn empty_text_and_no_spans() {
        let lines = LineHighlights::new("", &[]);
        assert_eq!(lines.line_count(), 1);
        assert!(lines.is_empty());
        assert_eq!(lines.spans_for_line(0), []);
        assert_eq!(LineHighlights::default().line_count(), 0);
        assert_eq!(LineHighlights::default().spans_for_line(0), []);
    }

    #[test]
    fn spans_past_the_end_of_the_text_do_not_panic() {
        let lines = LineHighlights::new("ab", &[span(1, 50, "comment"), span(60, 70, "keyword")]);
        assert_eq!(
            lines.spans_for_line(0),
            [(1..2, intern("comment").unwrap())]
        );
    }
}
