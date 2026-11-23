//! Selections: one or more cursors, each possibly selecting a range.

use std::ops::Range;

use crate::edit::EditSummary;

/// One cursor. `head` is where the cursor is drawn and where typing happens; `anchor` is
/// the other end of the selected range. When they are equal nothing is selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Selection {
    /// The end that stays put when the selection is extended (char offset).
    pub anchor: usize,
    /// The end that moves: the cursor (char offset).
    pub head: usize,
    /// The display column vertical motion tries to return to, remembered across short
    /// lines. `None` means "wherever the head is".
    pub goal: Option<usize>,
}

impl Selection {
    /// A selection from `anchor` to `head` with no goal column.
    pub fn new(anchor: usize, head: usize) -> Selection {
        Selection {
            anchor,
            head,
            goal: None,
        }
    }

    /// A cursor at `pos` selecting nothing.
    pub fn cursor(pos: usize) -> Selection {
        Selection::new(pos, pos)
    }

    /// A selection covering `range`, with the head at the start when `reversed`.
    pub fn from_range(range: Range<usize>, reversed: bool) -> Selection {
        let (start, end) = (range.start.min(range.end), range.start.max(range.end));
        if reversed {
            Selection::new(end, start)
        } else {
            Selection::new(start, end)
        }
    }

    /// The smaller of the two ends.
    pub fn start(&self) -> usize {
        self.anchor.min(self.head)
    }

    /// The larger of the two ends.
    pub fn end(&self) -> usize {
        self.anchor.max(self.head)
    }

    /// True when nothing is selected.
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// The selected chars as `start..end`.
    pub fn range(&self) -> Range<usize> {
        self.start()..self.end()
    }

    /// Number of selected chars.
    pub fn len(&self) -> usize {
        self.end() - self.start()
    }

    /// True when the head is before the anchor.
    pub fn reversed(&self) -> bool {
        self.head < self.anchor
    }

    /// The same selection with a different goal column.
    pub fn with_goal(self, goal: Option<usize>) -> Selection {
        Selection { goal, ..self }
    }

    /// The same anchor with the head moved to `head` and the given goal column.
    pub fn with_head(self, head: usize, goal: Option<usize>) -> Selection {
        Selection {
            anchor: self.anchor,
            head,
            goal,
        }
    }

    /// A cursor at this selection's head.
    pub fn collapsed(self) -> Selection {
        Selection::cursor(self.head)
    }

    /// Where this selection is after `summary`'s edits: text inserted at either edge of a
    /// non-empty selection stays outside it, and text inserted exactly at a cursor pushes
    /// the cursor after it. The goal column is dropped when the selection moved.
    pub fn map_through(self, summary: &EditSummary) -> Selection {
        let range = summary.map_range(self.range());
        if range == self.range() {
            return self;
        }
        Selection::from_range(range, self.reversed())
    }

    /// The same selection with both ends no greater than `len`.
    pub fn clamped(self, len: usize) -> Selection {
        Selection {
            anchor: self.anchor.min(len),
            head: self.head.min(len),
            goal: self.goal,
        }
    }
}

/// When each selection was made: the stamps of the oldest and the newest selection merged
/// into it. Higher is newer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Age {
    oldest: u64,
    newest: u64,
}

/// A set of selections that is never empty, always sorted by position and never
/// overlapping.
///
/// Selections that overlap are merged into one. So are a cursor and a selection that touch,
/// and two cursors at the same place; two non-empty selections that merely touch end to
/// start stay separate. One selection is the *primary*: the newest one, which the view
/// follows. The set also remembers which is the oldest, the one `Cancel` falls back to.
#[derive(Debug, Clone)]
pub struct Selections {
    items: Vec<Selection>,
    ages: Vec<Age>,
    next_stamp: u64,
}

impl Default for Selections {
    /// A single cursor at the start of the text.
    fn default() -> Selections {
        Selections::single(Selection::cursor(0))
    }
}

impl PartialEq for Selections {
    /// Two sets are equal when they hold the same selections and agree on the primary.
    fn eq(&self, other: &Selections) -> bool {
        self.items == other.items && self.primary_index() == other.primary_index()
    }
}

impl Eq for Selections {}

impl Selections {
    /// A set holding just `selection`.
    pub fn single(selection: Selection) -> Selections {
        Selections {
            items: vec![selection],
            ages: vec![Age {
                oldest: 0,
                newest: 0,
            }],
            next_stamp: 1,
        }
    }

    /// A set built from `selections`, in any order, with `selections[primary]` as the newest
    /// and the others aged by their order in the list. Overlapping selections are merged.
    /// An empty list gives a single cursor at 0; a `primary` out of range means the last.
    pub fn new(selections: Vec<Selection>, primary: usize) -> Selections {
        if selections.is_empty() {
            return Selections::default();
        }
        let count = selections.len() as u64;
        let primary = primary.min(selections.len() - 1);
        let ages = (0..selections.len())
            .map(|index| {
                let stamp = if index == primary {
                    count
                } else {
                    index as u64
                };
                Age {
                    oldest: stamp,
                    newest: stamp,
                }
            })
            .collect();
        let mut set = Selections {
            items: selections,
            ages,
            next_stamp: count + 1,
        };
        set.normalize();
        set
    }

    /// A set of cursors and ranges from plain char ranges; the last one is the primary.
    pub fn from_ranges(ranges: impl IntoIterator<Item = Range<usize>>) -> Selections {
        let selections: Vec<Selection> = ranges
            .into_iter()
            .map(|range| Selection::from_range(range, false))
            .collect();
        let primary = selections.len().saturating_sub(1);
        Selections::new(selections, primary)
    }

    /// Number of selections; at least 1.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// The selections in position order.
    pub fn as_slice(&self) -> &[Selection] {
        &self.items
    }

    /// The selections in position order.
    pub fn iter(&self) -> std::slice::Iter<'_, Selection> {
        self.items.iter()
    }

    /// The selection at `index` in position order.
    pub fn get(&self, index: usize) -> Option<Selection> {
        self.items.get(index).copied()
    }

    /// The selected ranges in position order.
    pub fn ranges(&self) -> Vec<Range<usize>> {
        self.items.iter().map(Selection::range).collect()
    }

    /// Index of the primary (newest) selection.
    pub fn primary_index(&self) -> usize {
        let mut best = 0;
        for (index, age) in self.ages.iter().enumerate() {
            if age.newest > self.ages[best].newest {
                best = index;
            }
        }
        best
    }

    /// The primary (newest) selection.
    pub fn primary(&self) -> Selection {
        self.items[self.primary_index()]
    }

    /// Index of the oldest selection.
    pub fn oldest_index(&self) -> usize {
        let mut best = 0;
        for (index, age) in self.ages.iter().enumerate() {
            if age.oldest < self.ages[best].oldest {
                best = index;
            }
        }
        best
    }

    /// The oldest selection: the one the others were added to.
    pub fn oldest(&self) -> Selection {
        self.items[self.oldest_index()]
    }

    /// The selection that starts first.
    pub fn first(&self) -> Selection {
        self.items[0]
    }

    /// The selection that starts last.
    pub fn last(&self) -> Selection {
        self.items[self.items.len() - 1]
    }

    /// True when the set is one cursor with nothing selected.
    pub fn is_single_cursor(&self) -> bool {
        self.items.len() == 1 && self.items[0].is_empty()
    }

    /// Replace every selection by `f(selection)`, keeping their ages, then merge whatever
    /// now overlaps.
    pub fn transform(&mut self, mut f: impl FnMut(Selection) -> Selection) {
        for selection in &mut self.items {
            *selection = f(*selection);
        }
        self.normalize();
    }

    /// Add `selection` as the newest. It merges with any selection it overlaps.
    pub fn push(&mut self, selection: Selection) {
        let stamp = self.take_stamp();
        self.items.push(selection);
        self.ages.push(Age {
            oldest: stamp,
            newest: stamp,
        });
        self.normalize();
    }

    /// Replace the primary selection by `selection`, which becomes the newest.
    pub fn replace_primary(&mut self, selection: Selection) {
        let index = self.primary_index();
        let stamp = self.take_stamp();
        self.items[index] = selection;
        self.ages[index].newest = stamp;
        self.normalize();
    }

    /// Remove the primary selection; the next newest becomes the primary. Returns false and
    /// does nothing when it is the only one.
    pub fn remove_primary(&mut self) -> bool {
        if self.items.len() < 2 {
            return false;
        }
        let index = self.primary_index();
        self.items.remove(index);
        self.ages.remove(index);
        true
    }

    /// Drop every selection except the oldest.
    pub fn retain_oldest(&mut self) {
        let keep = self.oldest();
        *self = Selections::single(keep);
    }

    /// Drop every selection except the primary.
    pub fn retain_primary(&mut self) {
        let keep = self.primary();
        *self = Selections::single(keep);
    }

    /// Clamp every end to `len` (the length of the text), merging what collapses together.
    pub fn clamp(&mut self, len: usize) {
        if self.items.iter().all(|selection| selection.end() <= len) {
            return;
        }
        self.transform(|selection| selection.clamped(len));
    }

    /// Move every selection through `summary`'s edits (see [`Selection::map_through`]).
    pub fn map_through(&mut self, summary: &EditSummary) {
        if summary.batches().is_empty() {
            return;
        }
        self.transform(|selection| selection.map_through(summary));
    }

    fn take_stamp(&mut self) -> u64 {
        let stamp = self.next_stamp;
        self.next_stamp += 1;
        stamp
    }

    /// Sort by position and merge overlapping and touching-duplicate selections.
    fn normalize(&mut self) {
        let already_fine = self
            .items
            .windows(2)
            .all(|pair| pair[0].start() <= pair[1].start() && !should_merge(&pair[0], &pair[1]));
        if already_fine {
            return;
        }
        let mut entries: Vec<(Selection, Age)> =
            self.items.drain(..).zip(self.ages.drain(..)).collect();
        entries.sort_by_key(|(selection, _)| (selection.start(), selection.end()));
        let mut merged: Vec<(Selection, Age)> = Vec::with_capacity(entries.len());
        for (selection, age) in entries {
            match merged.last_mut() {
                Some((previous, previous_age)) if should_merge(previous, &selection) => {
                    let range = previous.start()..previous.end().max(selection.end());
                    let newer = if age.newest > previous_age.newest {
                        selection
                    } else {
                        *previous
                    };
                    *previous =
                        Selection::from_range(range, newer.reversed()).with_goal(newer.goal);
                    previous_age.oldest = previous_age.oldest.min(age.oldest);
                    previous_age.newest = previous_age.newest.max(age.newest);
                }
                _ => merged.push((selection, age)),
            }
        }
        for (selection, age) in merged {
            self.items.push(selection);
            self.ages.push(age);
        }
    }
}

impl<'a> IntoIterator for &'a Selections {
    type Item = &'a Selection;
    type IntoIter = std::slice::Iter<'a, Selection>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.iter()
    }
}

/// Whether `later` (which starts at or after `earlier`) must be merged into it: they
/// overlap, or they touch and at least one of them is a cursor.
fn should_merge(earlier: &Selection, later: &Selection) -> bool {
    later.start() < earlier.end()
        || (later.start() == earlier.end() && (earlier.is_empty() || later.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::Change;

    fn sel(anchor: usize, head: usize) -> Selection {
        Selection::new(anchor, head)
    }

    #[test]
    fn selection_basics() {
        let forward = sel(2, 7);
        assert_eq!((forward.start(), forward.end(), forward.len()), (2, 7, 5));
        assert_eq!(forward.range(), 2..7);
        assert!(!forward.reversed() && !forward.is_empty());

        let backward = sel(7, 2);
        assert_eq!(backward.range(), 2..7);
        assert!(backward.reversed());

        let cursor = Selection::cursor(4);
        assert!(cursor.is_empty() && !cursor.reversed());
        assert_eq!(cursor.len(), 0);

        assert_eq!(Selection::from_range(3..9, false), sel(3, 9));
        assert_eq!(Selection::from_range(3..9, true), sel(9, 3));
        assert_eq!(
            sel(1, 5).with_head(3, Some(8)),
            Selection {
                anchor: 1,
                head: 3,
                goal: Some(8)
            }
        );
        assert_eq!(
            sel(1, 5).with_goal(Some(2)).collapsed(),
            Selection::cursor(5)
        );
        assert_eq!(sel(3, 90).clamped(10), sel(3, 10));
    }

    #[test]
    fn a_set_is_never_empty() {
        let set = Selections::new(Vec::new(), 0);
        assert_eq!(set.len(), 1);
        assert_eq!(set.primary(), Selection::cursor(0));
        assert!(set.is_single_cursor());
        assert_eq!(Selections::default(), set);
    }

    #[test]
    fn sorted_with_the_requested_primary() {
        let set = Selections::new(vec![sel(20, 25), sel(1, 3), sel(10, 10)], 1);
        assert_eq!(set.ranges(), vec![1..3, 10..10, 20..25]);
        assert_eq!(set.primary(), sel(1, 3));
        assert_eq!(set.primary_index(), 0);
        assert_eq!(set.oldest(), sel(20, 25));
        assert_eq!(set.first(), sel(1, 3));
        assert_eq!(set.last(), sel(20, 25));
        assert_eq!(set.get(1), Some(Selection::cursor(10)));
        assert_eq!(set.get(3), None);
        // A primary out of range means the last one given.
        assert_eq!(
            Selections::new(vec![sel(5, 6), sel(1, 2)], 9).primary(),
            sel(1, 2)
        );
    }

    #[test]
    fn overlapping_selections_merge() {
        let set = Selections::new(vec![sel(1, 5), sel(3, 9), sel(20, 22)], 2);
        assert_eq!(set.ranges(), vec![1..9, 20..22]);
        let set = Selections::new(vec![sel(1, 9), sel(3, 4)], 0);
        assert_eq!(set.ranges(), vec![1..9]);
    }

    #[test]
    fn touching_selections_merge_only_when_one_is_a_cursor() {
        // Two non-empty selections that touch stay separate.
        let set = Selections::new(vec![sel(1, 5), sel(5, 9)], 1);
        assert_eq!(set.ranges(), vec![1..5, 5..9]);
        // A cursor at either edge of a selection is absorbed.
        assert_eq!(
            Selections::new(vec![sel(1, 5), sel(5, 5)], 1).ranges(),
            vec![1..5]
        );
        assert_eq!(
            Selections::new(vec![sel(1, 1), sel(1, 5)], 1).ranges(),
            vec![1..5]
        );
        // Two cursors in one place are one cursor.
        assert_eq!(
            Selections::new(vec![sel(4, 4), sel(4, 4), sel(4, 4)], 0).ranges(),
            vec![4..4]
        );
    }

    #[test]
    fn a_merge_takes_the_direction_of_the_newer_selection() {
        let set = Selections::new(vec![sel(1, 5), sel(9, 3)], 1);
        assert_eq!(set.as_slice(), &[sel(9, 1)]);
        let set = Selections::new(vec![sel(1, 5), sel(9, 3)], 0);
        assert_eq!(set.as_slice(), &[sel(1, 9)]);
    }

    #[test]
    fn push_makes_the_new_selection_primary() {
        let mut set = Selections::single(sel(10, 12));
        set.push(sel(2, 4));
        assert_eq!(set.ranges(), vec![2..4, 10..12]);
        assert_eq!(set.primary(), sel(2, 4));
        assert_eq!(set.oldest(), sel(10, 12));
        set.push(sel(30, 31));
        assert_eq!(set.primary(), sel(30, 31));
        assert_eq!(set.oldest(), sel(10, 12));

        // Pushing onto an existing selection merges and keeps both ages.
        set.push(sel(11, 13));
        assert_eq!(set.ranges(), vec![2..4, 10..13, 30..31]);
        assert_eq!(set.primary(), sel(10, 13));
        assert_eq!(set.oldest(), sel(10, 13));
    }

    #[test]
    fn removing_the_primary_falls_back_to_the_next_newest() {
        let mut set = Selections::single(sel(1, 2));
        set.push(sel(5, 6));
        set.push(sel(9, 10));
        assert!(set.remove_primary());
        assert_eq!(set.ranges(), vec![1..2, 5..6]);
        assert_eq!(set.primary(), sel(5, 6));
        assert!(set.remove_primary());
        assert_eq!(set.primary(), sel(1, 2));
        assert!(
            !set.remove_primary(),
            "the last selection cannot be removed"
        );
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn replacing_the_primary_keeps_the_others() {
        let mut set = Selections::single(sel(1, 2));
        set.push(sel(5, 6));
        set.replace_primary(sel(7, 9));
        assert_eq!(set.ranges(), vec![1..2, 7..9]);
        assert_eq!(set.primary(), sel(7, 9));
        assert_eq!(set.oldest(), sel(1, 2));
        // Replacing it with something that overlaps another selection merges the two.
        set.replace_primary(sel(0, 3));
        assert_eq!(set.ranges(), vec![0..3]);
    }

    #[test]
    fn retain_oldest_and_primary() {
        let mut set = Selections::single(sel(10, 12));
        set.push(sel(2, 4));
        set.push(sel(20, 24));
        let mut oldest = set.clone();
        oldest.retain_oldest();
        assert_eq!(oldest.as_slice(), &[sel(10, 12)]);
        set.retain_primary();
        assert_eq!(set.as_slice(), &[sel(20, 24)]);
    }

    #[test]
    fn transform_keeps_ages_and_merges() {
        let mut set = Selections::single(sel(1, 2));
        set.push(sel(5, 6));
        set.push(sel(9, 10));
        set.transform(|selection| Selection::cursor(selection.head + 1));
        assert_eq!(set.ranges(), vec![3..3, 7..7, 11..11]);
        assert_eq!(set.primary(), Selection::cursor(11));
        assert_eq!(set.oldest(), Selection::cursor(3));
        set.transform(|_| Selection::cursor(0));
        assert_eq!(set.as_slice(), &[Selection::cursor(0)]);
    }

    #[test]
    fn clamp_pulls_selections_into_the_text() {
        let mut set = Selections::new(vec![sel(1, 2), sel(8, 12), sel(20, 30)], 0);
        set.clamp(10);
        assert_eq!(set.ranges(), vec![1..2, 8..10]);
        set.clamp(100);
        assert_eq!(set.ranges(), vec![1..2, 8..10]);
    }

    #[test]
    fn equality_ignores_how_the_set_was_built() {
        let mut built = Selections::single(sel(1, 2));
        built.push(sel(5, 6));
        assert_eq!(built, Selections::new(vec![sel(1, 2), sel(5, 6)], 1));
        assert_ne!(built, Selections::new(vec![sel(1, 2), sel(5, 6)], 0));
        assert_ne!(built, Selections::single(sel(1, 2)));
        assert_eq!(
            Selections::from_ranges([3..4, 0..1]).ranges(),
            vec![0..1, 3..4]
        );
        assert_eq!(Selections::from_ranges([3..4, 0..1]).primary(), sel(0, 1));
    }

    #[test]
    fn selections_move_through_edits() {
        let change = |old: Range<usize>, new: Range<usize>| Change {
            old,
            new,
            ..Change::default()
        };
        // Insert 2 chars at 5.
        let insert = EditSummary::from_batch(vec![change(5..5, 5..7)]);
        assert_eq!(
            Selection::cursor(3).map_through(&insert),
            Selection::cursor(3)
        );
        assert_eq!(
            Selection::cursor(5).map_through(&insert),
            Selection::cursor(7)
        );
        assert_eq!(
            Selection::cursor(9).map_through(&insert),
            Selection::cursor(11)
        );
        assert_eq!(
            sel(5, 8).map_through(&insert),
            sel(7, 10),
            "inserted at the start edge: stays outside"
        );
        assert_eq!(
            sel(2, 5).map_through(&insert),
            sel(2, 5),
            "inserted at the end edge: stays outside"
        );
        assert_eq!(
            sel(8, 3).map_through(&insert),
            sel(10, 3),
            "direction is kept"
        );
        let with_goal = Selection::cursor(3).with_goal(Some(9));
        assert_eq!(
            with_goal.map_through(&insert).goal,
            Some(9),
            "untouched selections keep their goal"
        );
        assert_eq!(
            Selection::cursor(9)
                .with_goal(Some(9))
                .map_through(&insert)
                .goal,
            None
        );

        // Delete 4..10.
        let delete = EditSummary::from_batch(vec![change(4..10, 4..4)]);
        assert_eq!(sel(6, 8).map_through(&delete), Selection::cursor(4));
        assert_eq!(sel(2, 8).map_through(&delete), sel(2, 4));
        assert_eq!(sel(8, 12).map_through(&delete), sel(4, 6));

        let mut set = Selections::new(vec![sel(1, 2), sel(5, 6), sel(7, 9), sel(20, 22)], 3);
        set.map_through(&delete);
        assert_eq!(set.ranges(), vec![1..2, 4..4, 14..16]);
        assert_eq!(set.primary(), sel(14, 16));
    }
}
