//! Grapheme cluster boundaries on a rope.
//!
//! A cursor must never sit inside a grapheme cluster (an emoji with its modifiers, a letter
//! with its combining marks). These helpers answer "where is the next/previous place a
//! cursor may stand" without copying the text: they walk the rope's chunks with
//! `unicode-segmentation`'s incremental cursor.

use ropey::str_utils::byte_to_char_idx;
use ropey::Rope;
use unicode_segmentation::{GraphemeCursor, GraphemeIncomplete};

/// The closest cluster boundary strictly after `char_idx`, or the end of the rope.
/// Indexes past the end are treated as the end.
pub fn next_boundary(rope: &Rope, char_idx: usize) -> usize {
    let len_chars = rope.len_chars();
    if char_idx >= len_chars {
        return len_chars;
    }
    let byte_idx = rope.char_to_byte(char_idx);
    let (mut chunk, mut chunk_byte_idx, mut chunk_char_idx, _) = rope.chunk_at_byte(byte_idx);

    // Two ASCII bytes in a row always have a boundary between them (CR LF aside).
    let local = byte_idx - chunk_byte_idx;
    let bytes = chunk.as_bytes();
    if local + 1 < bytes.len()
        && bytes[local] < 0x80
        && bytes[local + 1] < 0x80
        && bytes[local] != b'\r'
    {
        return char_idx + 1;
    }

    let mut cursor = GraphemeCursor::new(byte_idx, rope.len_bytes(), true);
    loop {
        match cursor.next_boundary(chunk, chunk_byte_idx) {
            Ok(None) => return len_chars,
            Ok(Some(n)) => {
                return chunk_char_idx + byte_to_char_idx(chunk, n.saturating_sub(chunk_byte_idx));
            }
            Err(GraphemeIncomplete::NextChunk) => {
                chunk_byte_idx += chunk.len();
                if chunk_byte_idx >= rope.len_bytes() {
                    return len_chars;
                }
                let (next, _, next_char_idx, _) = rope.chunk_at_byte(chunk_byte_idx);
                chunk = next;
                chunk_char_idx = next_char_idx;
            }
            Err(GraphemeIncomplete::PreContext(n)) => {
                let (context, context_byte_idx, _, _) = rope.chunk_at_byte(n.saturating_sub(1));
                cursor.provide_context(context, context_byte_idx);
            }
            Err(_) => return (char_idx + 1).min(len_chars),
        }
    }
}

/// The closest cluster boundary strictly before `char_idx`, or 0.
/// Indexes past the end are treated as the end.
pub fn prev_boundary(rope: &Rope, char_idx: usize) -> usize {
    let char_idx = char_idx.min(rope.len_chars());
    if char_idx == 0 {
        return 0;
    }
    let byte_idx = rope.char_to_byte(char_idx);
    let (mut chunk, mut chunk_byte_idx, mut chunk_char_idx, _) = rope.chunk_at_byte(byte_idx);

    let local = byte_idx - chunk_byte_idx;
    let bytes = chunk.as_bytes();
    if local >= 2 && bytes[local - 1] < 0x80 && bytes[local - 2] < 0x80 && bytes[local - 2] != b'\r'
    {
        return char_idx - 1;
    }

    let mut cursor = GraphemeCursor::new(byte_idx, rope.len_bytes(), true);
    loop {
        match cursor.prev_boundary(chunk, chunk_byte_idx) {
            Ok(None) => return 0,
            Ok(Some(n)) => {
                return chunk_char_idx + byte_to_char_idx(chunk, n.saturating_sub(chunk_byte_idx));
            }
            Err(GraphemeIncomplete::PrevChunk) => {
                if chunk_byte_idx == 0 {
                    return 0;
                }
                let (prev, prev_byte_idx, prev_char_idx, _) =
                    rope.chunk_at_byte(chunk_byte_idx - 1);
                chunk = prev;
                chunk_byte_idx = prev_byte_idx;
                chunk_char_idx = prev_char_idx;
            }
            Err(GraphemeIncomplete::PreContext(n)) => {
                let (context, context_byte_idx, _, _) = rope.chunk_at_byte(n.saturating_sub(1));
                cursor.provide_context(context, context_byte_idx);
            }
            Err(_) => return char_idx - 1,
        }
    }
}

/// Whether a cursor may stand at `char_idx`. The start and the end of the rope are
/// boundaries; indexes past the end are not.
pub fn is_boundary(rope: &Rope, char_idx: usize) -> bool {
    let len_chars = rope.len_chars();
    if char_idx == 0 || char_idx == len_chars {
        return true;
    }
    if char_idx > len_chars {
        return false;
    }
    let byte_idx = rope.char_to_byte(char_idx);
    let (chunk, chunk_byte_idx, _, _) = rope.chunk_at_byte(byte_idx);

    let local = byte_idx - chunk_byte_idx;
    let bytes = chunk.as_bytes();
    if local >= 1 && local < bytes.len() && bytes[local - 1] < 0x80 && bytes[local] < 0x80 {
        return !(bytes[local - 1] == b'\r' && bytes[local] == b'\n');
    }

    let mut cursor = GraphemeCursor::new(byte_idx, rope.len_bytes(), true);
    loop {
        match cursor.is_boundary(chunk, chunk_byte_idx) {
            Ok(answer) => return answer,
            Err(GraphemeIncomplete::PreContext(n)) => {
                let (context, context_byte_idx, _, _) = rope.chunk_at_byte(n.saturating_sub(1));
                cursor.provide_context(context, context_byte_idx);
            }
            Err(_) => return true,
        }
    }
}

/// `char_idx` if a cursor may stand there, otherwise the boundary just before it.
/// Indexes past the end become the end.
pub fn floor_boundary(rope: &Rope, char_idx: usize) -> usize {
    let char_idx = char_idx.min(rope.len_chars());
    if is_boundary(rope, char_idx) {
        char_idx
    } else {
        prev_boundary(rope, char_idx)
    }
}

/// `char_idx` if a cursor may stand there, otherwise the boundary just after it.
/// Indexes past the end become the end.
pub fn ceil_boundary(rope: &Rope, char_idx: usize) -> usize {
    let char_idx = char_idx.min(rope.len_chars());
    if is_boundary(rope, char_idx) {
        char_idx
    } else {
        next_boundary(rope, char_idx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_segmentation::UnicodeSegmentation;

    /// Every boundary of `text` as char offsets, computed the simple way.
    fn expected_boundaries(text: &str) -> Vec<usize> {
        let mut out = vec![0];
        let mut chars = 0;
        for cluster in text.graphemes(true) {
            chars += cluster.chars().count();
            out.push(chars);
        }
        out.dedup();
        out
    }

    fn check(text: &str) {
        let rope = Rope::from_str(text);
        let boundaries = expected_boundaries(text);
        let len = rope.len_chars();
        for idx in 0..=len {
            let is = boundaries.binary_search(&idx).is_ok();
            assert_eq!(
                is_boundary(&rope, idx),
                is,
                "is_boundary({idx}) in {text:?}"
            );
            let next = boundaries.iter().copied().find(|&b| b > idx).unwrap_or(len);
            assert_eq!(
                next_boundary(&rope, idx),
                next,
                "next_boundary({idx}) in {text:?}"
            );
            let prev = boundaries
                .iter()
                .copied()
                .rev()
                .find(|&b| b < idx)
                .unwrap_or(0);
            assert_eq!(
                prev_boundary(&rope, idx),
                prev,
                "prev_boundary({idx}) in {text:?}"
            );
            let floor = boundaries
                .iter()
                .copied()
                .rev()
                .find(|&b| b <= idx)
                .unwrap_or(0);
            assert_eq!(
                floor_boundary(&rope, idx),
                floor,
                "floor_boundary({idx}) in {text:?}"
            );
            let ceil = boundaries
                .iter()
                .copied()
                .find(|&b| b >= idx)
                .unwrap_or(len);
            assert_eq!(
                ceil_boundary(&rope, idx),
                ceil,
                "ceil_boundary({idx}) in {text:?}"
            );
        }
    }

    #[test]
    fn empty_rope() {
        let rope = Rope::new();
        assert_eq!(next_boundary(&rope, 0), 0);
        assert_eq!(prev_boundary(&rope, 0), 0);
        assert!(is_boundary(&rope, 0));
        assert!(!is_boundary(&rope, 1));
        assert_eq!(next_boundary(&rope, 9), 0);
        assert_eq!(prev_boundary(&rope, 9), 0);
        assert_eq!(floor_boundary(&rope, 9), 0);
    }

    #[test]
    fn ascii_and_newlines() {
        check("hello\nworld\n");
        check("a");
        check("\n\n\n");
    }

    #[test]
    fn combining_marks_emoji_and_flags() {
        check("e\u{301}a\u{308}\u{301}z");
        check("👨‍👩‍👧‍👦 family");
        check("🇨🇦🇹🇼x🇨");
        check("👍🏽 ok ❤️");
        check("한국어 क्षि नमस्ते");
        check("a\u{200d}b\u{fe0f}");
    }

    #[test]
    fn clusters_that_straddle_rope_chunks() {
        // Ropey chunks are about a kilobyte, so these force clusters across chunk borders.
        let mut text = String::new();
        for i in 0..400 {
            text.push_str("ab");
            text.push('e');
            for _ in 0..(i % 7) {
                text.push('\u{301}');
            }
            text.push_str("🇨🇦👨‍👩‍👧");
        }
        check(&text);

        let long_cluster = format!("x{}y", "\u{301}".repeat(3000));
        let rope = Rope::from_str(&long_cluster);
        assert_eq!(next_boundary(&rope, 0), 3001);
        assert_eq!(prev_boundary(&rope, 3001), 0);
        assert!(!is_boundary(&rope, 1500));
        assert_eq!(floor_boundary(&rope, 1500), 0);
        assert_eq!(ceil_boundary(&rope, 1500), 3001);
        assert_eq!(next_boundary(&rope, 3001), 3002);
    }

    #[test]
    fn past_the_end_is_clamped() {
        let rope = Rope::from_str("abc");
        assert_eq!(next_boundary(&rope, 50), 3);
        assert_eq!(prev_boundary(&rope, 50), 2);
        assert!(!is_boundary(&rope, 50));
        assert_eq!(ceil_boundary(&rope, 50), 3);
    }
}
