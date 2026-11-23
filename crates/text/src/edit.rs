//! What an edit did: the description a buffer hands back after changing its text.
//!
//! An [`EditSummary`] is precise enough to move any old position to where it now is
//! ([`EditSummary::map_offset`]) and to feed an incremental parser
//! ([`EditSummary::input_edits`]).

use std::ops::Range;

/// Which side a position sticks to when text is inserted exactly at it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Bias {
    /// Stay before the inserted text.
    Left,
    /// Move after the inserted text.
    Right,
}

/// A position as a 0-based line and a byte offset within that line. Lines are separated by
/// `\n` only. This is the row/column pair tree-sitter works with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct BytePoint {
    /// 0-based line number.
    pub line: usize,
    /// Bytes between the start of the line and the position.
    pub byte_col: usize,
}

/// One replaced range, as it was applied.
///
/// A buffer applies edits in batches. Within a batch every `old` value is in the text as it
/// was before the batch and every `new` value is in the text as it is after the batch.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Change {
    /// The replaced range, in chars, before the batch.
    pub old: Range<usize>,
    /// The range the replacement text occupies, in chars, after the batch.
    pub new: Range<usize>,
    /// The replaced range in bytes, before the batch.
    pub old_bytes: Range<usize>,
    /// The replacement's range in bytes, after the batch.
    pub new_bytes: Range<usize>,
    /// Line and byte column of `old.start`, before the batch.
    pub old_start_point: BytePoint,
    /// Line and byte column of `old.end`, before the batch.
    pub old_end_point: BytePoint,
    /// Line and byte column of `new.start`, after the batch.
    pub new_start_point: BytePoint,
    /// Line and byte column of `new.end`, after the batch.
    pub new_end_point: BytePoint,
}

impl Change {
    /// True when nothing was removed and nothing was inserted.
    pub fn is_noop(&self) -> bool {
        self.old.is_empty() && self.new.is_empty()
    }

    /// How much longer (positive) or shorter (negative) the text became, in chars.
    pub fn char_delta(&self) -> isize {
        self.new.len() as isize - self.old.len() as isize
    }
}

/// One edit in the form an incremental parser wants: positions as they are at the moment
/// this edit is applied, when the edits are applied in the order given. The fields mirror
/// tree-sitter's `InputEdit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InputEdit {
    /// Byte offset where the edit starts.
    pub start_byte: usize,
    /// Byte offset where the replaced text ended.
    pub old_end_byte: usize,
    /// Byte offset where the replacement ends.
    pub new_end_byte: usize,
    /// Line and byte column of `start_byte`.
    pub start_point: BytePoint,
    /// Line and byte column of `old_end_byte`.
    pub old_end_point: BytePoint,
    /// Line and byte column of `new_end_byte`.
    pub new_end_point: BytePoint,
}

/// Everything one operation changed in a buffer: a sequence of batches, applied in order,
/// each a sorted list of non-overlapping [`Change`]s.
///
/// Most operations produce one batch. Undoing a step that merged several keystrokes
/// produces one batch per keystroke.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EditSummary {
    batches: Vec<Vec<Change>>,
}

impl EditSummary {
    /// A summary of nothing.
    pub fn new() -> EditSummary {
        EditSummary::default()
    }

    /// A summary of a single batch. `changes` must be sorted by position.
    pub fn from_batch(changes: Vec<Change>) -> EditSummary {
        EditSummary {
            batches: vec![changes],
        }
    }

    /// True when the text did not change at all.
    pub fn is_empty(&self) -> bool {
        self.batches
            .iter()
            .all(|batch| batch.iter().all(Change::is_noop))
    }

    /// The batches in the order they were applied.
    pub fn batches(&self) -> &[Vec<Change>] {
        &self.batches
    }

    /// Every change, batch by batch. Remember that each batch has its own coordinates.
    pub fn changes(&self) -> impl Iterator<Item = &Change> {
        self.batches.iter().flatten()
    }

    /// Append the batches of a later operation.
    pub fn extend(&mut self, later: EditSummary) {
        self.batches.extend(later.batches);
    }

    /// Append one batch. `changes` must be sorted by position.
    pub fn push_batch(&mut self, changes: Vec<Change>) {
        self.batches.push(changes);
    }

    /// Where the position `old` (a char offset before the operation) is after it.
    ///
    /// Text inserted before the position shifts it. A position inside a replaced range keeps
    /// its distance from the start of the range, clamped to the replacement's length, so a
    /// deletion collapses it to the deletion point. For text inserted exactly at the
    /// position, `bias` decides whether it stays before the insertion or moves after it.
    pub fn map_offset(&self, old: usize, bias: Bias) -> usize {
        self.batches
            .iter()
            .fold(old, |pos, batch| map_in_batch(batch, pos, bias))
    }

    /// Where a char range is after the operation: the start is mapped with `Bias::Right` and
    /// the end with `Bias::Left`, so text inserted at either edge stays outside. An empty
    /// range maps as a single position with `Bias::Right`.
    pub fn map_range(&self, old: Range<usize>) -> Range<usize> {
        if old.start >= old.end {
            let pos = self.map_offset(old.start, Bias::Right);
            return pos..pos;
        }
        let start = self.map_offset(old.start, Bias::Right);
        let end = self.map_offset(old.end, Bias::Left);
        start..end.max(start)
    }

    /// The same changes as a flat list of edits for an incremental parser: apply them to the
    /// syntax tree in the order returned, then reparse.
    pub fn input_edits(&self) -> Vec<InputEdit> {
        let mut out = Vec::new();
        for batch in &self.batches {
            for change in batch.iter().filter(|change| !change.is_noop()) {
                let start_point = change.new_start_point;
                let old_lines = change
                    .old_end_point
                    .line
                    .saturating_sub(change.old_start_point.line);
                let old_end_point = if old_lines == 0 {
                    let width = change
                        .old_end_point
                        .byte_col
                        .saturating_sub(change.old_start_point.byte_col);
                    BytePoint {
                        line: start_point.line,
                        byte_col: start_point.byte_col + width,
                    }
                } else {
                    BytePoint {
                        line: start_point.line + old_lines,
                        byte_col: change.old_end_point.byte_col,
                    }
                };
                out.push(InputEdit {
                    start_byte: change.new_bytes.start,
                    old_end_byte: change.new_bytes.start + change.old_bytes.len(),
                    new_end_byte: change.new_bytes.end,
                    start_point,
                    old_end_point,
                    new_end_point: change.new_end_point,
                });
            }
        }
        out
    }
}

/// Add a signed distance to a position without wrapping.
fn shift(pos: usize, delta: isize) -> usize {
    if delta >= 0 {
        pos.saturating_add(delta as usize)
    } else {
        pos.saturating_sub(delta.unsigned_abs())
    }
}

/// Map a position through one batch of sorted, non-overlapping changes.
fn map_in_batch(changes: &[Change], pos: usize, bias: Bias) -> usize {
    match bias {
        Bias::Left => {
            // The first change that is not entirely before the position.
            let index = changes.partition_point(|change| change.old.end < pos);
            let Some(change) = changes.get(index) else {
                return match changes.last() {
                    Some(last) => shift(pos, last.new.end as isize - last.old.end as isize),
                    None => pos,
                };
            };
            if change.old.start > pos {
                shift(pos, change.new.start as isize - change.old.start as isize)
            } else if pos == change.old.start {
                change.new.start
            } else if pos < change.old.end {
                change.new.start + (pos - change.old.start).min(change.new.len())
            } else {
                change.new.end
            }
        }
        Bias::Right => {
            // The last change that is not entirely after the position.
            let index = changes.partition_point(|change| change.old.start <= pos);
            let Some(change) = index.checked_sub(1).and_then(|i| changes.get(i)) else {
                return pos;
            };
            if change.old.end < pos {
                shift(pos, change.new.end as isize - change.old.end as isize)
            } else if pos == change.old.end {
                change.new.end
            } else if pos > change.old.start {
                change.new.start + (pos - change.old.start).min(change.new.len())
            } else {
                change.new.start
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A change with only its char ranges filled in.
    fn change(old: Range<usize>, new: Range<usize>) -> Change {
        Change {
            old,
            new,
            ..Change::default()
        }
    }

    #[test]
    fn empty_summary_maps_to_itself() {
        let summary = EditSummary::new();
        assert!(summary.is_empty());
        assert_eq!(summary.map_offset(7, Bias::Left), 7);
        assert_eq!(summary.map_offset(7, Bias::Right), 7);
        assert_eq!(summary.map_range(3..9), 3..9);
        assert!(summary.input_edits().is_empty());
        assert!(EditSummary::from_batch(vec![change(4..4, 4..4)]).is_empty());
    }

    #[test]
    fn insertion_shifts_what_follows() {
        // Insert 3 chars at 5.
        let summary = EditSummary::from_batch(vec![change(5..5, 5..8)]);
        assert!(!summary.is_empty());
        assert_eq!(summary.map_offset(4, Bias::Left), 4);
        assert_eq!(summary.map_offset(4, Bias::Right), 4);
        assert_eq!(summary.map_offset(5, Bias::Left), 5);
        assert_eq!(summary.map_offset(5, Bias::Right), 8);
        assert_eq!(summary.map_offset(6, Bias::Left), 9);
        assert_eq!(summary.map_offset(6, Bias::Right), 9);
    }

    #[test]
    fn deletion_clamps_what_was_inside() {
        // Delete 5..9.
        let summary = EditSummary::from_batch(vec![change(5..9, 5..5)]);
        for bias in [Bias::Left, Bias::Right] {
            assert_eq!(summary.map_offset(4, bias), 4);
            assert_eq!(summary.map_offset(5, bias), 5);
            assert_eq!(summary.map_offset(7, bias), 5);
            assert_eq!(summary.map_offset(9, bias), 5);
            assert_eq!(summary.map_offset(10, bias), 6);
        }
    }

    #[test]
    fn replacement_keeps_the_distance_from_its_start() {
        // Replace 10..16 (6 chars) with 4 chars.
        let summary = EditSummary::from_batch(vec![change(10..16, 10..14)]);
        for bias in [Bias::Left, Bias::Right] {
            assert_eq!(summary.map_offset(10, bias), 10);
            assert_eq!(summary.map_offset(12, bias), 12);
            assert_eq!(summary.map_offset(14, bias), 14);
            assert_eq!(summary.map_offset(15, bias), 14);
            assert_eq!(summary.map_offset(16, bias), 14);
            assert_eq!(summary.map_offset(20, bias), 18);
        }
    }

    #[test]
    fn several_changes_in_one_batch() {
        // Old text positions: delete 2..4, insert 2 chars at 6, replace 8..9 with 3 chars.
        let summary = EditSummary::from_batch(vec![
            change(2..4, 2..2),
            change(6..6, 4..6),
            change(8..9, 8..11),
        ]);
        assert_eq!(summary.map_offset(0, Bias::Left), 0);
        assert_eq!(summary.map_offset(3, Bias::Left), 2);
        assert_eq!(summary.map_offset(5, Bias::Left), 3);
        assert_eq!(summary.map_offset(6, Bias::Left), 4);
        assert_eq!(summary.map_offset(6, Bias::Right), 6);
        assert_eq!(summary.map_offset(7, Bias::Right), 7);
        assert_eq!(summary.map_offset(8, Bias::Left), 8);
        assert_eq!(summary.map_offset(9, Bias::Left), 11);
        assert_eq!(summary.map_offset(12, Bias::Right), 14);
    }

    #[test]
    fn insertions_at_one_point_and_touching_changes() {
        // Two insertions at 3 (2 chars, then 1 char), then a replacement of 3..5 by 1 char.
        let summary = EditSummary::from_batch(vec![
            change(3..3, 3..5),
            change(3..3, 5..6),
            change(3..5, 6..7),
        ]);
        assert_eq!(
            summary.map_offset(3, Bias::Left),
            3,
            "before everything inserted here"
        );
        assert_eq!(
            summary.map_offset(3, Bias::Right),
            6,
            "after the insertions, at the replacement"
        );
        assert_eq!(summary.map_offset(4, Bias::Left), 7);
        assert_eq!(summary.map_offset(5, Bias::Left), 7);
        assert_eq!(summary.map_offset(5, Bias::Right), 7);
        assert_eq!(summary.map_offset(6, Bias::Right), 8);

        // A replacement that ends where an insertion happens.
        let summary = EditSummary::from_batch(vec![change(1..3, 1..2), change(3..3, 2..5)]);
        assert_eq!(summary.map_offset(3, Bias::Left), 2);
        assert_eq!(summary.map_offset(3, Bias::Right), 5);
    }

    #[test]
    fn batches_compose_in_order() {
        let mut summary = EditSummary::from_batch(vec![change(0..0, 0..2)]);
        summary.extend(EditSummary::from_batch(vec![change(1..3, 1..1)]));
        // "ab" inserted at 0, then chars 1..3 of the result removed.
        assert_eq!(summary.map_offset(0, Bias::Left), 0);
        assert_eq!(summary.map_offset(0, Bias::Right), 1);
        assert_eq!(summary.map_offset(1, Bias::Left), 1);
        assert_eq!(summary.map_offset(5, Bias::Left), 5);
        assert_eq!(summary.batches().len(), 2);
        assert_eq!(summary.changes().count(), 2);
    }

    #[test]
    fn ranges_exclude_text_inserted_at_their_edges() {
        let summary = EditSummary::from_batch(vec![change(2..2, 2..4), change(6..6, 8..11)]);
        assert_eq!(summary.map_range(2..6), 4..8);
        assert_eq!(summary.map_range(2..2), 4..4);
        assert_eq!(summary.map_range(6..6), 11..11);
        let deletion = EditSummary::from_batch(vec![change(1..9, 1..1)]);
        assert_eq!(deletion.map_range(3..5), 1..1);
    }

    #[test]
    fn input_edits_are_in_application_order() {
        // Before: "ab\ncd\nef". Replace "b\nc" (1..4) with "X" and insert "YZ\n" at 6.
        // After:  "aXd\nYZ\nef".
        let first = Change {
            old: 1..4,
            new: 1..2,
            old_bytes: 1..4,
            new_bytes: 1..2,
            old_start_point: BytePoint {
                line: 0,
                byte_col: 1,
            },
            old_end_point: BytePoint {
                line: 1,
                byte_col: 1,
            },
            new_start_point: BytePoint {
                line: 0,
                byte_col: 1,
            },
            new_end_point: BytePoint {
                line: 0,
                byte_col: 2,
            },
        };
        let second = Change {
            old: 6..6,
            new: 4..7,
            old_bytes: 6..6,
            new_bytes: 4..7,
            old_start_point: BytePoint {
                line: 2,
                byte_col: 0,
            },
            old_end_point: BytePoint {
                line: 2,
                byte_col: 0,
            },
            new_start_point: BytePoint {
                line: 1,
                byte_col: 0,
            },
            new_end_point: BytePoint {
                line: 2,
                byte_col: 0,
            },
        };
        let summary = EditSummary::from_batch(vec![first, change(5..5, 3..3), second]);
        let edits = summary.input_edits();
        assert_eq!(edits.len(), 2, "no-op changes are left out");
        assert_eq!(
            edits[0],
            InputEdit {
                start_byte: 1,
                old_end_byte: 4,
                new_end_byte: 2,
                start_point: BytePoint {
                    line: 0,
                    byte_col: 1
                },
                old_end_point: BytePoint {
                    line: 1,
                    byte_col: 1
                },
                new_end_point: BytePoint {
                    line: 0,
                    byte_col: 2
                },
            }
        );
        assert_eq!(
            edits[1],
            InputEdit {
                start_byte: 4,
                old_end_byte: 4,
                new_end_byte: 7,
                start_point: BytePoint {
                    line: 1,
                    byte_col: 0
                },
                old_end_point: BytePoint {
                    line: 1,
                    byte_col: 0
                },
                new_end_point: BytePoint {
                    line: 2,
                    byte_col: 0
                },
            }
        );
    }

    #[test]
    fn change_helpers() {
        assert!(change(3..3, 3..3).is_noop());
        assert!(!change(3..4, 3..3).is_noop());
        assert_eq!(change(3..4, 3..3).char_delta(), -1);
        assert_eq!(change(3..3, 3..9).char_delta(), 6);
    }
}
