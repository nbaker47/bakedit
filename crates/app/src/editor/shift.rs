//! Keeping the colours of the old text on screen while the new ones are worked out.
//!
//! Highlighting runs on its own thread and answers for the text as it was when it was asked.
//! Typing in the meantime would leave every colour one character off for a few frames, and
//! on a long file for a good part of a second. A [`ShiftLog`] remembers the edits made since
//! the text the highlights are for, and answers two questions about them: which old line (and
//! columns) a line of the new text came from, and where an old position is now. The painter
//! uses that to draw the old spans and brackets shifted into place.

use std::ops::Range;

use bakedit_syntax::{BracketMark, CaptureId, LineHighlights};
use bakedit_text::EditSummary;

/// More edits than this are not remembered; the oldest go first and the colours may then be
/// a little off until the highlights catch up.
const MAX_CHANGES: usize = 512;

/// A `(line, byte column)` position.
type Point = (usize, usize);

/// One replaced range, in the coordinates of the text before and after its batch.
#[derive(Debug, Clone)]
struct Change {
    old_start: Point,
    old_end: Point,
    new_start: Point,
    new_end: Point,
    old_bytes: Range<usize>,
    new_bytes: Range<usize>,
}

/// The changes of one batch (what one command did), sorted by position.
#[derive(Debug, Clone)]
struct Batch {
    /// The buffer version after this batch.
    version: u64,
    changes: Vec<Change>,
}

impl Batch {
    fn from_summary(version: u64, summary: &EditSummary) -> Vec<Batch> {
        summary
            .batches()
            .iter()
            .map(|changes| Batch {
                version,
                changes: changes
                    .iter()
                    .map(|change| Change {
                        old_start: (change.old_start_point.line, change.old_start_point.byte_col),
                        old_end: (change.old_end_point.line, change.old_end_point.byte_col),
                        new_start: (change.new_start_point.line, change.new_start_point.byte_col),
                        new_end: (change.new_end_point.line, change.new_end_point.byte_col),
                        old_bytes: change.old_bytes.clone(),
                        new_bytes: change.new_bytes.clone(),
                    })
                    .collect(),
            })
            .collect()
    }

    /// Where a position of the text before this batch is after it. `is_end` says the
    /// position ends a span: at an insertion point a span start stays in front of the new
    /// text and a span end moves behind it, so a word being typed stays one span.
    fn forward(&self, point: Point, is_end: bool) -> Point {
        let at = self.changes.partition_point(|change| change.old_start <= point);
        let Some(change) = at.checked_sub(1).map(|index| &self.changes[index]) else { return point };
        if point == change.old_start && change.old_start == change.old_end {
            return if is_end { change.new_end } else { change.new_start };
        }
        if point == change.old_start {
            return change.new_start;
        }
        if point < change.old_end {
            return if is_end { change.new_end } else { change.new_start };
        }
        shifted(point, change.old_end, change.new_end)
    }

    /// Where a position of the text after this batch was before it.
    fn backward(&self, point: Point) -> Point {
        let at = self.changes.partition_point(|change| change.new_start <= point);
        let Some(change) = at.checked_sub(1).map(|index| &self.changes[index]) else { return point };
        if point < change.new_end {
            return change.old_start;
        }
        shifted(point, change.new_end, change.old_end)
    }

    fn forward_byte(&self, byte: usize, is_end: bool) -> usize {
        let at = self.changes.partition_point(|change| change.old_bytes.start <= byte);
        let Some(change) = at.checked_sub(1).map(|index| &self.changes[index]) else { return byte };
        let insertion = change.old_bytes.is_empty();
        if byte == change.old_bytes.start && (insertion || !is_end) {
            return if insertion && is_end { change.new_bytes.end } else { change.new_bytes.start };
        }
        if byte < change.old_bytes.end {
            return if is_end { change.new_bytes.end } else { change.new_bytes.start };
        }
        byte + change.new_bytes.end - change.old_bytes.end
    }

    fn backward_byte(&self, byte: usize) -> usize {
        let at = self.changes.partition_point(|change| change.new_bytes.start <= byte);
        let Some(change) = at.checked_sub(1).map(|index| &self.changes[index]) else { return byte };
        if byte < change.new_bytes.end {
            return change.old_bytes.start;
        }
        byte + change.old_bytes.end - change.new_bytes.end
    }
}

/// `point`, which is at or after `from`, moved by the change that turned `from` into `to`.
fn shifted(point: Point, from: Point, to: Point) -> Point {
    if point.0 == from.0 {
        (to.0, (to.1 + point.1).saturating_sub(from.1))
    } else {
        ((point.0 + to.0).saturating_sub(from.0), point.1)
    }
}

/// The edits made since the text the colours on screen are for.
#[derive(Debug, Clone, Default)]
pub struct ShiftLog {
    batches: Vec<Batch>,
    changes: usize,
}

impl ShiftLog {
    /// Remember what an edit did; `version` is the buffer's version after it.
    pub fn push(&mut self, version: u64, summary: &EditSummary) {
        for batch in Batch::from_summary(version, summary) {
            self.changes += batch.changes.len();
            self.batches.push(batch);
        }
        while self.changes > MAX_CHANGES && self.batches.len() > 1 {
            self.changes -= self.batches.remove(0).changes.len();
        }
    }

    /// The highlights for `version` have arrived: the edits up to it are in them.
    pub fn prune(&mut self, version: u64) {
        let keep = self.batches.partition_point(|batch| batch.version <= version);
        if keep > 0 {
            self.batches.drain(..keep);
            self.changes = self.batches.iter().map(|batch| batch.changes.len()).sum();
        }
    }

    pub fn is_empty(&self) -> bool {
        self.batches.is_empty()
    }

    fn forward(&self, mut point: Point, is_end: bool) -> Point {
        for batch in &self.batches {
            point = batch.forward(point, is_end);
        }
        point
    }

    fn backward(&self, mut point: Point) -> Point {
        for batch in self.batches.iter().rev() {
            point = batch.backward(point);
        }
        point
    }

    /// The old byte offset the new one came from.
    pub fn old_byte(&self, byte: usize) -> usize {
        self.batches.iter().rev().fold(byte, |byte, batch| batch.backward_byte(byte))
    }

    /// Where an old byte offset is now.
    pub fn new_byte(&self, byte: usize, is_end: bool) -> usize {
        self.batches.iter().fold(byte, |byte, batch| batch.forward_byte(byte, is_end))
    }

    /// The old highlights for `line` of the current text (`len` bytes long), shifted to fit
    /// it, as byte ranges within the line.
    pub fn spans_for_line(&self, line: usize, len: usize, old: &LineHighlights) -> Vec<(Range<usize>, CaptureId)> {
        let first = self.backward((line, 0)).0;
        let last = self.backward((line, len)).0.max(first).min(first + 3);
        let mut spans: Vec<(Range<usize>, CaptureId)> = Vec::new();
        for old_line in first..=last {
            for (range, capture) in old.spans_for_line(old_line) {
                let start = self.forward((old_line, range.start), false);
                let end = self.forward((old_line, range.end), true);
                if start.0 > line || end.0 < line {
                    continue;
                }
                let start = if start.0 < line { 0 } else { start.1.min(len) };
                let end = if end.0 > line { len } else { end.1.min(len) };
                if start < end {
                    spans.push((start..end, *capture));
                }
            }
        }
        spans.sort_by_key(|(range, _)| range.start);
        let mut clean: Vec<(Range<usize>, CaptureId)> = Vec::with_capacity(spans.len());
        for (range, capture) in spans {
            match clean.last() {
                Some((previous, _)) if range.start < previous.end => {}
                _ => clean.push((range, capture)),
            }
        }
        clean
    }

    /// The old bracket marks that fall in the line whose current bytes are `bytes`, as
    /// `(current byte, depth)`.
    pub fn brackets_in(&self, bytes: Range<usize>, old: &[BracketMark], out: &mut Vec<(usize, u16)>) {
        let start = self.old_byte(bytes.start);
        let end = self.old_byte(bytes.end).max(start);
        let first = old.partition_point(|mark| mark.byte < start);
        for mark in old[first..].iter().take_while(|mark| mark.byte < end.saturating_add(1)) {
            let now = self.new_byte(mark.byte, false);
            if now >= bytes.start && now < bytes.end {
                out.push((now, mark.depth));
            }
        }
    }

    /// `line` (0-based) of the current text as `(old line, old start column)`.
    #[cfg(test)]
    fn source_of(&self, line: usize) -> Point {
        self.backward((line, 0))
    }
}

/// Move the line numbers of git hunks along with an edit, so the gutter marks stay on
/// their lines until the hunks are worked out again.
pub fn shift_hunks(hunks: &mut [bakedit_git::LineHunk], version: u64, summary: &EditSummary) {
    for batch in Batch::from_summary(version, summary) {
        for hunk in hunks.iter_mut() {
            hunk.new_start = batch.forward((hunk.new_start, 0), false).0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bakedit_text::Editor;

    fn edited(text: &str, edits: &[(Range<usize>, &str)]) -> (Editor, EditSummary) {
        let mut editor = Editor::from_str(text);
        let outcome = editor.edit(edits);
        (editor, outcome.edits)
    }

    #[test]
    fn a_typed_character_shifts_the_columns_after_it() {
        let (_, summary) = edited("let answer = 42;\nnext\n", &[(7..7, "x")]);
        let mut log = ShiftLog::default();
        log.push(2, &summary);
        // The old text had a span over `answer` (bytes 4..10); the new one has `xanswer`.
        let start = log.forward((0, 4), false);
        let end = log.forward((0, 10), true);
        assert_eq!((start, end), ((0, 4), (0, 11)), "typing inside a word grows its span");
        assert_eq!(log.forward((0, 13), false), (0, 14), "what follows moves right");
        assert_eq!(log.backward((0, 14)), (0, 13));
        assert_eq!(log.backward((1, 0)), (1, 0), "other lines stay put");
    }

    #[test]
    fn a_new_line_moves_the_lines_below_and_splits_the_old_one() {
        let (_, summary) = edited("one two\nthree\n", &[(3..3, "\n")]);
        let mut log = ShiftLog::default();
        log.push(2, &summary);
        assert_eq!(log.source_of(0), (0, 0));
        assert_eq!(log.source_of(1), (0, 3), "the second half of the split line");
        assert_eq!(log.source_of(2), (1, 0), "`three` was line 1");
        assert_eq!(log.forward((1, 2), false), (2, 2));
    }

    #[test]
    fn removing_a_line_pulls_the_lines_below_up() {
        let (_, summary) = edited("a\nb\nc\nd\n", &[(2..4, "")]);
        let mut log = ShiftLog::default();
        log.push(2, &summary);
        assert_eq!(log.source_of(1), (2, 0));
        assert_eq!(log.forward((3, 0), false), (2, 0));
    }

    #[test]
    fn bytes_follow_multibyte_text_and_prune_forgets_old_edits() {
        let (_, summary) = edited("é = 1\n", &[(0..0, "ab")]);
        let mut log = ShiftLog::default();
        log.push(5, &summary);
        assert_eq!(log.new_byte(0, false), 0);
        assert_eq!(log.new_byte(2, false), 4, "`é` is two bytes, and 'ab' two more in front of what follows");
        assert_eq!(log.old_byte(4), 2);
        assert!(!log.is_empty());
        log.prune(4);
        assert!(!log.is_empty(), "the highlights are older than the edit");
        log.prune(5);
        assert!(log.is_empty());
        assert_eq!(log.new_byte(7, false), 7);
    }

    #[test]
    fn hunks_move_with_the_lines() {
        let (_, summary) = edited("a\nb\nc\n", &[(0..0, "new\n")]);
        let mut hunks = vec![bakedit_git::LineHunk { kind: bakedit_git::LineHunkKind::Added, new_start: 2, new_lines: 1, old_start: 0, old_lines: 0 }];
        shift_hunks(&mut hunks, 2, &summary);
        assert_eq!(hunks[0].new_start, 3);
    }
}
