//! Bracket nesting, for colouring each pair by its depth.

use crate::capture::CaptureId;
use crate::highlight::HighlightSpan;

/// One bracket in the text: where it is, how deeply nested, and whether it has a partner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BracketMark {
    /// Byte offset of the bracket, which is one byte long.
    pub byte: usize,
    /// How many pairs enclose it, from 0. An opener and its closer have the same depth, so
    /// `depth % colours` gives both the same colour.
    pub depth: u16,
    /// Whether it is an opener (`(`, `[`, `{`) rather than a closer.
    pub open: bool,
    /// Whether its partner was found. An unmatched bracket still has the depth it sits at.
    pub matched: bool,
}

/// Find every `(`, `)`, `[`, `]`, `{` and `}` that is a bracket and work out its nesting.
///
/// `spans` are the highlights of the same `text`, sorted and non-overlapping. A bracket
/// character is not a bracket when the innermost span over it says it is text or a
/// separator: a string, a comment, a character or number literal, a URL, or punctuation the
/// grammar calls a delimiter (the `)` after a shell `case` pattern, the brackets of a
/// Markdown link). With no spans every bracket character counts.
///
/// Pairing is the usual stack. A closer that does not match the innermost open bracket
/// closes the nearest one of its own kind, and the openers skipped over are left unmatched;
/// a closer with nothing to close is unmatched and changes nothing. The marks come back in
/// text order. One pass over the text, linear in its length.
pub fn bracket_marks(text: &str, spans: &[HighlightSpan]) -> Vec<BracketMark> {
    let mut marks: Vec<BracketMark> = Vec::new();
    // Indexes into `marks` of the brackets still open, and how many of each kind.
    let mut open: Vec<usize> = Vec::new();
    let mut open_of_kind = [0usize; 3];
    let mut opaque = OpaqueCaptures::default();
    let mut span_index = 0;
    for (byte, &character) in text.as_bytes().iter().enumerate() {
        let Some((kind, is_open)) = classify(character) else {
            continue;
        };
        while spans.get(span_index).is_some_and(|span| span.end <= byte) {
            span_index += 1;
        }
        let covering = spans.get(span_index).filter(|span| span.start <= byte);
        if covering.is_some_and(|span| opaque.contains(span.capture)) {
            continue;
        }
        if is_open {
            open.push(marks.len());
            open_of_kind[kind] += 1;
            marks.push(BracketMark {
                byte,
                depth: depth_of(open.len() - 1),
                open: true,
                matched: false,
            });
            continue;
        }
        if open_of_kind[kind] == 0 {
            marks.push(BracketMark {
                byte,
                depth: depth_of(open.len()),
                open: false,
                matched: false,
            });
            continue;
        }
        // Close the nearest opener of this kind; the ones above it stay unmatched.
        while let Some(opener) = open.pop() {
            let opener_kind =
                classify(text.as_bytes()[marks[opener].byte]).map_or(kind, |(kind, _)| kind);
            open_of_kind[opener_kind] -= 1;
            if opener_kind == kind {
                marks[opener].matched = true;
                break;
            }
        }
        marks.push(BracketMark {
            byte,
            depth: depth_of(open.len()),
            open: false,
            matched: true,
        });
    }
    marks
}

/// The kind of bracket a byte is (0 round, 1 square, 2 curly) and whether it opens.
fn classify(byte: u8) -> Option<(usize, bool)> {
    match byte {
        b'(' => Some((0, true)),
        b')' => Some((0, false)),
        b'[' => Some((1, true)),
        b']' => Some((1, false)),
        b'{' => Some((2, true)),
        b'}' => Some((2, false)),
        _ => None,
    }
}

fn depth_of(open: usize) -> u16 {
    u16::try_from(open).unwrap_or(u16::MAX)
}

/// Remembers, per capture, whether brackets under it are text rather than brackets.
#[derive(Default)]
struct OpaqueCaptures {
    known: Vec<Option<bool>>,
}

impl OpaqueCaptures {
    fn contains(&mut self, capture: CaptureId) -> bool {
        let index = capture.index();
        if index >= self.known.len() {
            self.known.resize(index + 1, None);
        }
        *self.known[index].get_or_insert_with(|| is_opaque(capture.name()))
    }
}

/// Whether a capture marks text in which a bracket character is not a bracket.
fn is_opaque(name: &str) -> bool {
    let root = name.split('.').next().unwrap_or(name);
    matches!(root, "string" | "comment" | "number" | "link_uri") || name == "punctuation.delimiter"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::intern;

    /// `(byte, depth, open, matched)` for every mark.
    fn marks(text: &str, spans: &[HighlightSpan]) -> Vec<(usize, u16, bool, bool)> {
        bracket_marks(text, spans)
            .into_iter()
            .map(|m| (m.byte, m.depth, m.open, m.matched))
            .collect()
    }

    fn span(text: &str, part: &str, capture: &str) -> HighlightSpan {
        let start = text
            .find(part)
            .unwrap_or_else(|| panic!("{part:?} is not in {text:?}"));
        HighlightSpan {
            start,
            end: start + part.len(),
            capture: intern(capture).unwrap(),
        }
    }

    #[test]
    fn nesting_counts_from_zero() {
        assert_eq!(
            marks("f(a[0], {b})", &[]),
            [
                (1, 0, true, true),
                (3, 1, true, true),
                (5, 1, false, true),
                (8, 1, true, true),
                (10, 1, false, true),
                (11, 0, false, true),
            ]
        );
        assert_eq!(
            marks("((()))", &[])
                .iter()
                .map(|mark| mark.1)
                .collect::<Vec<_>>(),
            [0, 1, 2, 2, 1, 0]
        );
        assert_eq!(marks("no brackets here", &[]), []);
        assert_eq!(marks("", &[]), []);
    }

    #[test]
    fn brackets_in_strings_and_comments_do_not_count() {
        let text = r#"call("(", x) // [note}"#;
        let spans = [
            span(text, r#""(""#, "string"),
            span(text, "// [note}", "comment"),
        ];
        assert_eq!(
            marks(text, &spans),
            [(4, 0, true, true), (11, 0, false, true)]
        );
        // Without the spans they would.
        assert_eq!(marks(text, &[]).len(), 5);
    }

    #[test]
    fn dotted_captures_follow_their_root() {
        let text = "a('\\(') b(/[)]/) c('}')";
        let spans = [
            span(text, "'\\('", "string"),
            span(text, "\\(", "string.escape"),
            span(text, "/[)]/", "string.regex"),
            span(text, "'}'", "number"),
        ];
        // Spans must not overlap: keep the escape inside by splitting the string by hand.
        let spans = [
            HighlightSpan {
                start: spans[0].start,
                end: spans[1].start,
                capture: spans[0].capture,
            },
            spans[1],
            HighlightSpan {
                start: spans[1].end,
                end: spans[0].end,
                capture: spans[0].capture,
            },
            spans[2],
            spans[3],
        ];
        let found = marks(text, &spans);
        assert_eq!(found.len(), 6, "{found:?}");
        assert!(found.iter().all(|mark| mark.3 && mark.1 == 0));
    }

    #[test]
    fn interpolation_inside_a_string_counts_again() {
        let text = r#"f"{a[0]}""#;
        let string = intern("string").unwrap();
        let spans = [
            HighlightSpan {
                start: 0,
                end: 2,
                capture: string,
            },
            span(text, "{", "punctuation.special"),
            span(text, "a[0]", "embedded"),
            span(text, "}", "punctuation.special"),
            HighlightSpan {
                start: 8,
                end: 9,
                capture: string,
            },
        ];
        assert_eq!(
            marks(text, &spans),
            [
                (2, 0, true, true),
                (4, 1, true, true),
                (6, 1, false, true),
                (7, 0, false, true)
            ]
        );
    }

    #[test]
    fn delimiters_and_urls_are_not_brackets() {
        let text = "case x in a) echo (b) ;; esac";
        let spans = [span(text, ")", "punctuation.delimiter")];
        assert_eq!(
            marks(text, &spans),
            [(18, 0, true, true), (20, 0, false, true)]
        );

        let text = "see (https://example.com/a_(b)) now";
        let spans = [span(text, "https://example.com/a_(b)", "link_uri")];
        assert_eq!(
            marks(text, &spans),
            [(4, 0, true, true), (30, 0, false, true)]
        );

        // Punctuation that is a bracket stays one.
        let text = "(x)";
        let spans = [
            span(text, "(", "punctuation.bracket"),
            span(text, ")", "punctuation.bracket"),
        ];
        assert_eq!(marks(text, &spans).len(), 2);
    }

    #[test]
    fn a_closer_with_nothing_to_close_is_unmatched() {
        assert_eq!(marks(")", &[]), [(0, 0, false, false)]);
        assert_eq!(
            marks("a) (b)", &[]),
            [
                (1, 0, false, false),
                (3, 0, true, true),
                (5, 0, false, true)
            ]
        );
        assert_eq!(
            marks("(]", &[]),
            [(0, 0, true, false), (1, 1, false, false)]
        );
        assert_eq!(
            marks("[)]", &[]),
            [
                (0, 0, true, true),
                (1, 1, false, false),
                (2, 0, false, true)
            ]
        );
    }

    #[test]
    fn an_opener_never_closed_is_unmatched() {
        assert_eq!(marks("(", &[]), [(0, 0, true, false)]);
        assert_eq!(
            marks("{ ( }", &[]),
            [(0, 0, true, true), (2, 1, true, false), (4, 0, false, true)]
        );
        assert_eq!(
            marks("fn f() { g( }", &[]),
            [
                (4, 0, true, true),
                (5, 0, false, true),
                (7, 0, true, true),
                (10, 1, true, false),
                (12, 0, false, true),
            ]
        );
        assert_eq!(
            marks("(((", &[])
                .iter()
                .map(|mark| (mark.1, mark.3))
                .collect::<Vec<_>>(),
            [(0, false), (1, false), (2, false)]
        );
    }

    #[test]
    fn multibyte_text_keeps_byte_offsets() {
        let text = "é(日本[語])";
        let found = bracket_marks(text, &[]);
        assert_eq!(found.len(), 4);
        for mark in &found {
            assert!(text.is_char_boundary(mark.byte));
            assert!(matches!(
                text.as_bytes()[mark.byte],
                b'(' | b')' | b'[' | b']'
            ));
        }
        assert_eq!(
            found.iter().map(|mark| mark.depth).collect::<Vec<_>>(),
            [0, 1, 1, 0]
        );
    }

    #[test]
    fn very_deep_nesting_saturates_instead_of_wrapping() {
        let text = "(".repeat(70_000);
        let found = bracket_marks(&text, &[]);
        assert_eq!(found.len(), 70_000);
        assert_eq!(found[65_535].depth, u16::MAX);
        assert_eq!(found[69_999].depth, u16::MAX);
    }

    #[test]
    fn long_unbalanced_input_stays_linear() {
        // Every closer has to skip a pile of openers of another kind exactly once.
        let text = format!("{}{}", "([".repeat(50_000), ")".repeat(50_000));
        let found = bracket_marks(&text, &[]);
        assert_eq!(found.len(), 150_000);
        assert_eq!(found.iter().filter(|mark| mark.matched).count(), 100_000);
    }
}
