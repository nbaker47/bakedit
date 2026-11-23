//! Undo history: transactions, grouping of typed characters, and the saved-state marker.
//!
//! The buffer records every batch of edits here. A [`Transaction`] is one undo step: one or
//! more batches plus the selections before and after. The history never drops a step.

use std::ops::Range;

use crate::selection::Selections;

/// Typed characters merge into the previous undo step while they arrive within this many
/// milliseconds of each other.
pub const UNDO_GROUP_INTERVAL_MS: u64 = 300;

/// What kind of change an undo step holds. Consecutive steps of the same mergeable kind
/// fuse into one while they arrive quickly and continue where the last one ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UndoKind {
    /// Typed text. Mergeable.
    Typing,
    /// Deleting backwards one step at a time (Backspace). Mergeable.
    Backspace,
    /// Deleting forwards one step at a time (Delete). Mergeable.
    Delete,
    /// Anything else: always its own step.
    Other,
}

impl UndoKind {
    fn is_mergeable(self) -> bool {
        !matches!(self, UndoKind::Other)
    }
}

/// One applied replacement with the text needed to undo and redo it. `old` is in the text
/// before its batch, `new` in the text after it.
#[derive(Debug, Clone)]
pub(crate) struct StoredEdit {
    pub old: Range<usize>,
    pub new: Range<usize>,
    pub removed: String,
    pub inserted: String,
}

/// One undo step.
#[derive(Debug, Clone)]
pub(crate) struct Transaction {
    pub id: u64,
    pub kind: UndoKind,
    pub batches: Vec<Vec<StoredEdit>>,
    pub selections_before: Option<Selections>,
    pub selections_after: Option<Selections>,
    pub last_edit_ms: u64,
    /// No later step may merge into a sealed one.
    pub sealed: bool,
}

/// A transaction that has been begun and not yet ended.
#[derive(Debug)]
struct Open {
    /// Nesting depth of begin calls.
    depth: usize,
    /// When true, batches are appended to the top of the undo stack instead of `pending`.
    merging: bool,
    pending: Transaction,
    /// Whether any batch was recorded since the outermost begin.
    recorded: bool,
}

/// The undo and redo stacks of one buffer.
#[derive(Debug, Default)]
pub(crate) struct History {
    undo: Vec<Transaction>,
    redo: Vec<Transaction>,
    next_id: u64,
    open: Option<Open>,
}

impl History {
    /// Identifies the state the text is in: the id of the newest applied step, or 0 when
    /// nothing is applied. Two equal ids mean equal text, as long as steps are sealed when
    /// the id is remembered.
    pub fn state_id(&self) -> u64 {
        self.undo.last().map_or(0, |transaction| transaction.id)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Stop the newest step from absorbing later ones.
    pub fn seal(&mut self) {
        if let Some(top) = self.undo.last_mut() {
            top.sealed = true;
        }
    }

    /// Start a step, or go one level deeper when one is already open. The outermost call
    /// decides the kind and the selections; it merges into the previous step when that one
    /// has the same mergeable kind, is not sealed, was last edited at most
    /// [`UNDO_GROUP_INTERVAL_MS`] ago, and left the selections exactly where they are now.
    pub fn begin(&mut self, kind: UndoKind, selections: Option<&Selections>, now_ms: u64) {
        if let Some(open) = &mut self.open {
            open.depth += 1;
            return;
        }
        let merging = kind.is_mergeable()
            && self.redo.is_empty()
            && self.undo.last().is_some_and(|top| {
                top.kind == kind
                    && !top.sealed
                    && now_ms.saturating_sub(top.last_edit_ms) <= UNDO_GROUP_INTERVAL_MS
                    && top.selections_after.is_some()
                    && top.selections_after.as_ref() == selections
            });
        let pending = Transaction {
            id: 0,
            kind,
            batches: Vec::new(),
            selections_before: selections.cloned(),
            selections_after: None,
            last_edit_ms: now_ms,
            sealed: false,
        };
        self.open = Some(Open {
            depth: 1,
            merging,
            pending,
            recorded: false,
        });
    }

    /// Record one applied batch into the open step. Does nothing when no step is open.
    pub fn record(&mut self, batch: Vec<StoredEdit>) {
        if batch.is_empty() {
            return;
        }
        let Some(open) = &mut self.open else {
            return;
        };
        self.redo.clear();
        open.recorded = true;
        match self.undo.last_mut() {
            Some(top) if open.merging => top.batches.push(batch),
            _ => open.pending.batches.push(batch),
        }
    }

    /// End the innermost open step. When the outermost one ends and it recorded something,
    /// the step is committed with `selections` as its "after" state. Returns true when a
    /// step was committed or extended.
    pub fn end(&mut self, selections: Option<&Selections>, now_ms: u64) -> bool {
        let Some(open) = &mut self.open else {
            return false;
        };
        open.depth -= 1;
        if open.depth > 0 {
            return false;
        }
        let Some(open) = self.open.take() else {
            return false;
        };
        if !open.recorded {
            // A mergeable step that changed no text (typing over a closing bracket, say)
            // still continues the run it would have merged into.
            if let (true, Some(top)) = (open.merging, self.undo.last_mut()) {
                top.selections_after = selections.cloned();
                top.last_edit_ms = now_ms;
            }
            return false;
        }
        match self.undo.last_mut() {
            Some(top) if open.merging => {
                top.selections_after = selections.cloned();
                top.last_edit_ms = now_ms;
            }
            _ => {
                let mut transaction = open.pending;
                self.next_id += 1;
                transaction.id = self.next_id;
                transaction.selections_after = selections.cloned();
                transaction.last_edit_ms = now_ms;
                self.undo.push(transaction);
            }
        }
        true
    }

    /// Take the newest applied step off the undo stack. The caller reverts its batches and
    /// hands it back with [`History::push_redo`].
    pub fn pop_undo(&mut self) -> Option<Transaction> {
        if self.open.is_some() {
            return None;
        }
        self.undo.pop()
    }

    /// Put a reverted step on the redo stack.
    pub fn push_redo(&mut self, mut transaction: Transaction) {
        transaction.sealed = true;
        self.redo.push(transaction);
        self.seal();
    }

    /// Take the newest reverted step off the redo stack. The caller reapplies its batches
    /// and hands it back with [`History::push_undo`].
    pub fn pop_redo(&mut self) -> Option<Transaction> {
        if self.open.is_some() {
            return None;
        }
        self.redo.pop()
    }

    /// Put a reapplied step back on the undo stack.
    pub fn push_undo(&mut self, mut transaction: Transaction) {
        transaction.sealed = true;
        self.undo.push(transaction);
    }

    /// Number of applied steps.
    #[cfg(test)]
    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selection::Selection;

    fn edit(at: usize, inserted: &str) -> StoredEdit {
        StoredEdit {
            old: at..at,
            new: at..at + inserted.chars().count(),
            removed: String::new(),
            inserted: inserted.to_string(),
        }
    }

    fn cursor(pos: usize) -> Selections {
        Selections::single(Selection::cursor(pos))
    }

    /// Record one typed character at `pos` at time `now`.
    fn type_char(history: &mut History, pos: usize, now: u64) {
        history.begin(UndoKind::Typing, Some(&cursor(pos)), now);
        history.record(vec![edit(pos, "x")]);
        history.end(Some(&cursor(pos + 1)), now);
    }

    #[test]
    fn an_empty_step_is_discarded() {
        let mut history = History::default();
        history.begin(UndoKind::Other, None, 0);
        assert!(history.is_open());
        assert!(!history.end(None, 0));
        assert!(!history.is_open());
        assert!(!history.can_undo());
        assert_eq!(history.state_id(), 0);
    }

    #[test]
    fn typing_merges_while_fast_and_contiguous() {
        let mut history = History::default();
        type_char(&mut history, 0, 0);
        type_char(&mut history, 1, 100);
        type_char(&mut history, 2, 400);
        assert_eq!(
            history.undo_depth(),
            1,
            "each key within 300 ms of the previous one"
        );
        let id = history.state_id();

        type_char(&mut history, 3, 701);
        assert_eq!(history.undo_depth(), 2, "301 ms later starts a new step");
        assert_ne!(history.state_id(), id);

        type_char(&mut history, 9, 702);
        assert_eq!(
            history.undo_depth(),
            3,
            "typing somewhere else starts a new step"
        );

        let top = history.pop_undo().unwrap();
        assert_eq!(top.batches.len(), 1);
        let merged = history.undo.first().unwrap();
        assert_eq!(merged.batches.len(), 3);
        assert_eq!(merged.selections_before, Some(cursor(0)));
        assert_eq!(merged.selections_after, Some(cursor(3)));
    }

    #[test]
    fn sealing_and_other_kinds_break_the_group() {
        let mut history = History::default();
        type_char(&mut history, 0, 0);
        history.seal();
        type_char(&mut history, 1, 10);
        assert_eq!(history.undo_depth(), 2);

        // A different kind never merges, even right away at the same place.
        history.begin(UndoKind::Backspace, Some(&cursor(2)), 20);
        history.record(vec![edit(1, "")]);
        history.end(Some(&cursor(1)), 20);
        assert_eq!(history.undo_depth(), 3);

        // The same mergeable kind does.
        history.begin(UndoKind::Backspace, Some(&cursor(1)), 30);
        history.record(vec![edit(0, "")]);
        history.end(Some(&cursor(0)), 30);
        assert_eq!(history.undo_depth(), 3);

        // Steps without selections never merge.
        history.begin(UndoKind::Typing, None, 40);
        history.record(vec![edit(0, "a")]);
        history.end(None, 40);
        history.begin(UndoKind::Typing, None, 41);
        history.record(vec![edit(1, "b")]);
        history.end(None, 41);
        assert_eq!(history.undo_depth(), 5);

        // Other is never mergeable.
        for now in [50, 51] {
            history.begin(UndoKind::Other, Some(&cursor(0)), now);
            history.record(vec![edit(0, "z")]);
            history.end(Some(&cursor(0)), now);
        }
        assert_eq!(history.undo_depth(), 7);
    }

    #[test]
    fn a_textless_step_continues_a_typing_run() {
        let mut history = History::default();
        type_char(&mut history, 0, 0);
        // The cursor steps over a character without editing: same kind, same start.
        history.begin(UndoKind::Typing, Some(&cursor(1)), 100);
        assert!(!history.end(Some(&cursor(2)), 100));
        // Typing goes on from the new position and still merges.
        type_char(&mut history, 2, 150);
        assert_eq!(history.undo_depth(), 1);
        assert_eq!(
            history.undo.first().unwrap().selections_after,
            Some(cursor(3))
        );

        // A textless step of another kind changes nothing.
        history.begin(UndoKind::Other, Some(&cursor(3)), 160);
        assert!(!history.end(Some(&cursor(9)), 160));
        assert_eq!(
            history.undo.first().unwrap().selections_after,
            Some(cursor(3))
        );
    }

    #[test]
    fn nested_begins_form_one_step() {
        let mut history = History::default();
        history.begin(UndoKind::Other, Some(&cursor(0)), 0);
        history.begin(UndoKind::Typing, Some(&cursor(5)), 0);
        history.record(vec![edit(0, "a")]);
        assert!(
            !history.end(Some(&cursor(9)), 0),
            "the inner end commits nothing"
        );
        history.record(vec![edit(1, "b")]);
        assert!(history.end(Some(&cursor(2)), 0));
        assert_eq!(history.undo_depth(), 1);
        let step = history.pop_undo().unwrap();
        assert_eq!(step.kind, UndoKind::Other);
        assert_eq!(step.batches.len(), 2);
        assert_eq!(step.selections_before, Some(cursor(0)));
        assert_eq!(step.selections_after, Some(cursor(2)));
    }

    #[test]
    fn redo_is_cleared_by_a_new_edit_and_undo_seals() {
        let mut history = History::default();
        type_char(&mut history, 0, 0);
        type_char(&mut history, 5, 1000);
        let second = history.pop_undo().unwrap();
        history.push_redo(second);
        assert!(history.can_redo());

        // Typing right after an undo must not merge into the step below it.
        type_char(&mut history, 1, 1001);
        assert!(!history.can_redo());
        assert_eq!(history.undo_depth(), 2);

        let step = history.pop_undo().unwrap();
        history.push_redo(step);
        let step = history.pop_redo().unwrap();
        history.push_undo(step);
        type_char(&mut history, 2, 1002);
        assert_eq!(history.undo_depth(), 3, "a redone step is sealed");
    }

    #[test]
    fn recording_without_an_open_step_is_ignored() {
        let mut history = History::default();
        history.record(vec![edit(0, "a")]);
        assert!(!history.can_undo());
        assert!(!history.end(None, 0));
        history.begin(UndoKind::Other, None, 0);
        assert!(history.pop_undo().is_none(), "no undo while a step is open");
        assert!(history.pop_redo().is_none());
        history.record(Vec::new());
        assert!(!history.end(None, 0), "an empty batch records nothing");
    }
}
