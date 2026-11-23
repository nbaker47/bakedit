//! The smallest set of edits that turns a buffer's text into a new text.
//!
//! Used when a file changes on disk: instead of replacing the whole buffer, only the lines
//! that differ are edited, so cursors, selections and the undo history survive.

use std::borrow::Cow;
use std::ops::Range;
use std::time::Duration;

use ropey::Rope;
use similar::{DiffTag, TextDiff};

/// How long the line diff may search for a small answer before it settles for a coarser
/// one. A coarser diff is still correct, it only replaces more than strictly needed.
const LINE_DIFF_TIMEOUT: Duration = Duration::from_millis(250);

/// Changed regions up to this many bytes on both sides are refined character by character.
const INLINE_DIFF_LIMIT: usize = 4096;

const INLINE_DIFF_TIMEOUT: Duration = Duration::from_millis(20);

/// A character diff is only used when the two sides are at least this similar; below that
/// it would match stray letters of unrelated text.
const INLINE_DIFF_MIN_RATIO: f32 = 0.5;

/// The edits that turn `rope`'s text into `new_text`: sorted, non-overlapping
/// `(old char range, replacement)` pairs ready for a buffer edit. Empty when the texts are
/// equal.
///
/// The texts are compared line by line; inside each changed region the common start and
/// end are kept and, for small regions, the middle is compared character by character.
pub(crate) fn diff_edits(rope: &Rope, new_text: &str) -> Vec<(Range<usize>, String)> {
    if rope_equals(rope, new_text) {
        return Vec::new();
    }
    let old_text: Cow<'_, str> = rope.slice(..).into();
    let old_text: &str = &old_text;

    let diff = TextDiff::configure()
        .timeout(LINE_DIFF_TIMEOUT)
        .diff_lines(old_text, new_text);
    let mut byte_edits: Vec<(Range<usize>, String)> = Vec::new();
    let mut old_byte = 0;
    let mut new_byte = 0;
    for op in diff.ops() {
        let (tag, old_lines, new_lines) = op.as_tag_tuple();
        let old_len: usize = old_lines
            .map(|line| diff.old_slice(line).map_or(0, str::len))
            .sum();
        let new_len: usize = new_lines
            .map(|line| diff.new_slice(line).map_or(0, str::len))
            .sum();
        let old_end = (old_byte + old_len).min(old_text.len());
        let new_end = (new_byte + new_len).min(new_text.len());
        if tag != DiffTag::Equal {
            refine(
                &old_text[old_byte..old_end],
                &new_text[new_byte..new_end],
                old_byte,
                &mut byte_edits,
            );
        }
        old_byte = old_end;
        new_byte = new_end;
    }

    byte_edits
        .into_iter()
        .map(|(bytes, text)| {
            (
                rope.byte_to_char(bytes.start)..rope.byte_to_char(bytes.end),
                text,
            )
        })
        .collect()
}

/// Whether the rope holds exactly `text`, compared chunk by chunk without copying.
fn rope_equals(rope: &Rope, text: &str) -> bool {
    if rope.len_bytes() != text.len() {
        return false;
    }
    let bytes = text.as_bytes();
    let mut offset = 0;
    for chunk in rope.chunks() {
        let end = offset + chunk.len();
        if bytes[offset..end] != *chunk.as_bytes() {
            return false;
        }
        offset = end;
    }
    true
}

/// Number of leading bytes `a` and `b` share, cut back to a character boundary.
fn common_prefix_len(a: &str, b: &str) -> usize {
    let mut len = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
    while !a.is_char_boundary(len) {
        len -= 1;
    }
    len
}

/// Number of trailing bytes `a` and `b` share, cut back to a character boundary.
fn common_suffix_len(a: &str, b: &str) -> usize {
    let mut len = a
        .bytes()
        .rev()
        .zip(b.bytes().rev())
        .take_while(|(x, y)| x == y)
        .count();
    while !a.is_char_boundary(a.len() - len) {
        len -= 1;
    }
    len
}

/// Turn one changed region (`old` became `new`) into edits, as byte ranges of the old text
/// offset by `old_offset`.
fn refine(old: &str, new: &str, old_offset: usize, out: &mut Vec<(Range<usize>, String)>) {
    let prefix = common_prefix_len(old, new);
    let suffix = common_suffix_len(&old[prefix..], &new[prefix..]);
    let old_middle = &old[prefix..old.len() - suffix];
    let new_middle = &new[prefix..new.len() - suffix];
    if old_middle.is_empty() && new_middle.is_empty() {
        return;
    }
    let base = old_offset + prefix;
    let small = old_middle.len() <= INLINE_DIFF_LIMIT && new_middle.len() <= INLINE_DIFF_LIMIT;
    if small
        && !old_middle.is_empty()
        && !new_middle.is_empty()
        && refine_by_chars(old_middle, new_middle, base, out)
    {
        return;
    }
    out.push((base..base + old_middle.len(), new_middle.to_string()));
}

/// Compare a small changed region character by character. Returns false, adding nothing,
/// when the two sides have too little in common for that to be meaningful.
fn refine_by_chars(
    old: &str,
    new: &str,
    old_offset: usize,
    out: &mut Vec<(Range<usize>, String)>,
) -> bool {
    let diff = TextDiff::configure()
        .timeout(INLINE_DIFF_TIMEOUT)
        .diff_chars(old, new);
    if diff.ratio() < INLINE_DIFF_MIN_RATIO {
        return false;
    }
    let byte_starts = |text: &str| -> Vec<usize> {
        text.char_indices()
            .map(|(byte, _)| byte)
            .chain(std::iter::once(text.len()))
            .collect()
    };
    let old_starts = byte_starts(old);
    let new_starts = byte_starts(new);
    let at = |starts: &[usize], index: usize| {
        starts
            .get(index)
            .copied()
            .unwrap_or_else(|| starts[starts.len() - 1])
    };
    for op in diff.ops() {
        let (tag, old_chars, new_chars) = op.as_tag_tuple();
        if tag == DiffTag::Equal {
            continue;
        }
        let old_bytes = at(&old_starts, old_chars.start)..at(&old_starts, old_chars.end);
        let new_bytes = at(&new_starts, new_chars.start)..at(&new_starts, new_chars.end);
        out.push((
            old_offset + old_bytes.start..old_offset + old_bytes.end,
            new[new_bytes].to_string(),
        ));
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Apply `edits` (old char ranges) to `old` the slow, obvious way.
    fn apply(old: &str, edits: &[(Range<usize>, String)]) -> String {
        let chars: Vec<char> = old.chars().collect();
        let mut out = String::new();
        let mut position = 0;
        for (range, text) in edits {
            assert!(
                range.start >= position,
                "edits overlap or are unsorted: {edits:?}"
            );
            assert!(range.start <= range.end && range.end <= chars.len());
            out.extend(&chars[position..range.start]);
            out.push_str(text);
            position = range.end;
        }
        out.extend(&chars[position..]);
        out
    }

    fn check(old: &str, new: &str) -> Vec<(Range<usize>, String)> {
        let edits = diff_edits(&Rope::from_str(old), new);
        assert_eq!(
            apply(old, &edits),
            new,
            "edits {edits:?} do not turn {old:?} into {new:?}"
        );
        edits
    }

    #[test]
    fn equal_texts_need_no_edits() {
        assert!(check("", "").is_empty());
        assert!(check("a\nb\n", "a\nb\n").is_empty());
        let long = "line\n".repeat(5000);
        assert!(check(&long, &long).is_empty());
    }

    #[test]
    fn a_changed_word_is_a_small_edit() {
        let edits = check("one\ntwo three four\nfive\n", "one\ntwo THREE four\nfive\n");
        assert_eq!(edits, vec![(8..13, "THREE".to_string())]);
    }

    #[test]
    fn inserted_and_removed_lines() {
        let edits = check("a\nb\nc\n", "a\nb\nNEW\nc\n");
        assert_eq!(edits, vec![(4..4, "NEW\n".to_string())]);
        let edits = check("a\nb\nc\n", "a\nc\n");
        assert_eq!(edits, vec![(2..4, String::new())]);
        let edits = check("a\nb\n", "");
        assert_eq!(edits, vec![(0..4, String::new())]);
        let edits = check("", "hello\n");
        assert_eq!(edits, vec![(0..0, "hello\n".to_string())]);
    }

    #[test]
    fn several_separate_changes_stay_separate() {
        let old = "alpha\nbeta\ngamma\ndelta\nepsilon\n";
        let new = "alpha\nBETA\ngamma\ndelta\nepsilon!\n";
        let edits = check(old, new);
        assert_eq!(edits.len(), 2);
        assert_eq!(edits[0], (6..10, "BETA".to_string()));
        assert_eq!(edits[1], (30..30, "!".to_string()));
    }

    #[test]
    fn two_changes_in_one_line_are_refined() {
        let edits = check(
            "let total = alpha + beta;\n",
            "let total = alpha1 + beta2;\n",
        );
        assert_eq!(
            edits,
            vec![(17..17, "1".to_string()), (24..24, "2".to_string())]
        );
    }

    #[test]
    fn unrelated_text_is_one_replacement() {
        let edits = check("the quick brown fox\n", "0123456789\n");
        assert_eq!(edits, vec![(0..19, "0123456789".to_string())]);
    }

    #[test]
    fn final_newline_changes() {
        assert_eq!(check("a\nb", "a\nb\n"), vec![(3..3, "\n".to_string())]);
        assert_eq!(check("a\nb\n", "a\nb"), vec![(3..4, String::new())]);
        check("a", "a\n\n\n");
        check("\n\n\n", "\n");
    }

    #[test]
    fn multibyte_text_keeps_char_offsets_right() {
        let edits = check("héllo wörld\n世界 你好\n", "héllo wörld\n世界 再见\n");
        assert_eq!(edits, vec![(15..17, "再见".to_string())]);
        // Characters that share a UTF-8 prefix must not be split.
        check("é\n", "è\n");
        check("aé\n", "aè\n");
        check("世\n", "丗\n");
        check("x👍🏽y\n", "x👍🏿y\n");
    }

    #[test]
    fn big_rewrites_still_apply() {
        let old: String = (0..2000).map(|i| format!("line number {i}\n")).collect();
        let new: String = (0..2000)
            .map(|i| {
                if i % 97 == 0 {
                    format!("changed {i}\n")
                } else {
                    format!("line number {i}\n")
                }
            })
            .chain(std::iter::once("tail\n".to_string()))
            .collect();
        let edits = check(&old, &new);
        assert!(edits.len() >= 21);
        check(&new, &old);
        check(&old, "");
        check("", &old);
    }

    #[test]
    fn prefix_and_suffix_helpers() {
        assert_eq!(common_prefix_len("hello", "help"), 3);
        assert_eq!(
            common_prefix_len("é", "è"),
            0,
            "shared lead byte is not a shared char"
        );
        assert_eq!(common_prefix_len("", "abc"), 0);
        assert_eq!(common_prefix_len("abc", "abc"), 3);
        assert_eq!(common_suffix_len("running", "jumping"), 3);
        assert_eq!(common_suffix_len("世", "丗"), 0);
        assert_eq!(common_suffix_len("abc", ""), 0);
        assert!(rope_equals(&Rope::from_str("abc"), "abc"));
        assert!(!rope_equals(&Rope::from_str("abc"), "abd"));
        assert!(!rope_equals(&Rope::from_str("abc"), "ab"));
    }
}
