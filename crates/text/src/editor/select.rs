//! Commands that change what is selected without touching the text.

use std::borrow::Cow;
use std::ops::Range;

use crate::chars::{self, CharKind};
use crate::display;
use crate::indent;
use crate::pairs::{self, BetweenTarget, PairKind};
use crate::search::SearchQuery;
use crate::selection::{Selection, Selections};

use super::{AddSelectionsGroup, AddSelectionsState, Editor, SelectNextState};

/// How many rows `AddSelectionAbove`/`Below` will look past for one that is long enough.
const MAX_ROWS_SKIPPED: usize = 100_000;

/// One display row: a buffer line and a soft-wrap row within it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RowKey {
    line: usize,
    row: usize,
}

impl Editor {
    /// `SelectLine`: grow every selection to the whole lines it touches, line breaks
    /// included. A selection that already ends at the start of a line takes that line too,
    /// so repeating the command adds a line each time.
    pub(crate) fn select_line(&mut self) {
        let buffer = &self.buffer;
        let len = buffer.len_chars();
        let mut selections = self.selections.clone();
        selections.transform(|selection| {
            let first = buffer.char_to_line(selection.start());
            let last = buffer.char_to_line(selection.end());
            Selection::new(
                buffer.line_to_char(first),
                buffer.line_to_char(last + 1).min(len),
            )
        });
        self.selections = selections;
    }

    /// `Cancel`: with several selections keep the oldest; with one, collapse it.
    pub(crate) fn cancel(&mut self) {
        if self.selections.len() > 1 {
            self.selections.retain_oldest();
        } else {
            let head = self.selections.primary().head;
            self.selections = Selections::single(Selection::cursor(head));
        }
    }

    /// The text of a char range of the buffer.
    fn text_of(&self, range: Range<usize>) -> String {
        self.buffer.slice(range).to_string()
    }

    /// Whether `range` neither continues a word to its left nor to its right.
    fn is_whole_word(&self, range: &Range<usize>) -> bool {
        let rope = self.buffer.rope();
        let words = self.word_chars();
        let joins = |pos: usize| {
            chars::kind_before(rope, pos, words) == Some(CharKind::Word)
                && chars::kind_at(rope, pos, words) == Some(CharKind::Word)
        };
        !joins(range.start) && !joins(range.end)
    }

    /// Start a `SelectNext` run from the current selections. Returns true when the same
    /// keypress should go on to add an occurrence (something was already selected), false
    /// when it only selected the words under the cursors or found nothing to search for.
    fn begin_select_next(&mut self) -> bool {
        let all_cursors = self.selections.iter().all(Selection::is_empty);
        if all_cursors {
            let rope = self.buffer.rope();
            let words = self.word_chars();
            let mut selections = self.selections.clone();
            selections.transform(|selection| {
                let (run, kind) = chars::surrounding_run(rope, selection.head, words);
                if kind == CharKind::Whitespace || run.is_empty() {
                    selection
                } else {
                    Selection::new(run.start, run.end)
                }
            });
            self.selections = selections;
        }
        let primary = self.selections.primary();
        let source = if primary.is_empty() {
            self.selections
                .iter()
                .rev()
                .find(|s| !s.is_empty())
                .copied()
        } else {
            Some(primary)
        };
        let Some(source) = source else {
            return false;
        };
        self.select_next = Some(SelectNextState {
            query: self.text_of(source.range()),
            wordwise: all_cursors,
        });
        !all_cursors
    }

    /// The nearest occurrence of the query after (or, going backwards, before) the primary
    /// selection that is not selected yet, wrapping around the text.
    fn next_unselected_match(
        &self,
        state: &SelectNextState,
        backwards: bool,
    ) -> Option<Range<usize>> {
        let search = SearchQuery::text(state.query.as_str()).compile().ok()?;
        let rope = self.buffer.rope();
        let primary = self.selections.primary();
        let acceptable = |found: &Range<usize>| {
            let taken = self.selections.iter().any(|selection| {
                if selection.is_empty() {
                    found.start <= selection.head && selection.head <= found.end
                } else {
                    selection.start() < found.end && found.start < selection.end()
                }
            });
            !taken && (!state.wordwise || self.is_whole_word(found))
        };
        let mut wrapped = false;
        if backwards {
            let origin = primary.start();
            let mut from = origin;
            loop {
                match search.find_prev(rope, from, false) {
                    Some(found) => {
                        if wrapped && found.start < origin {
                            return None;
                        }
                        if acceptable(&found) {
                            return Some(found);
                        }
                        from = found.start;
                    }
                    None if wrapped => return None,
                    None => {
                        wrapped = true;
                        from = rope.len_chars();
                    }
                }
            }
        } else {
            let origin = primary.end();
            let mut from = origin;
            loop {
                match search.find_next(rope, from, false) {
                    Some(found) => {
                        if wrapped && found.start >= origin {
                            return None;
                        }
                        if acceptable(&found) {
                            return Some(found);
                        }
                        from = found.end.max(found.start + 1);
                    }
                    None if wrapped => return None,
                    None => {
                        wrapped = true;
                        from = 0;
                    }
                }
            }
        }
    }

    /// `SelectNext` and `SelectPrevious`.
    ///
    /// The first use with nothing selected selects the word (or run of punctuation) under
    /// each cursor and remembers it; occurrences found later must then be whole words. Each
    /// further use selects the next occurrence that is not selected yet, wrapping around
    /// the text; with `replace_newest` the newest selection moves there instead of staying.
    /// The comparison is case-sensitive.
    pub(crate) fn select_next_match(&mut self, replace_newest: bool, backwards: bool) {
        if self.select_next.is_none() && !self.begin_select_next() {
            return;
        }
        let Some(state) = self.select_next.clone() else {
            return;
        };
        let Some(found) = self.next_unselected_match(&state, backwards) else {
            return;
        };
        let selection = Selection::new(found.start, found.end);
        if replace_newest && self.selections.len() > 1 {
            self.selections.remove_primary();
            self.selections.push(selection);
        } else if replace_newest {
            self.selections = Selections::single(selection);
        } else {
            self.selections.push(selection);
        }
    }

    /// `SelectAllMatches`: select every occurrence of the selected text, or of the word
    /// under the cursor (whole words only in that case). The selection the command started
    /// from stays the primary one.
    pub(crate) fn select_all_matches(&mut self) {
        if self.select_next.is_none() {
            self.begin_select_next();
        }
        let Some(state) = self.select_next.clone() else {
            return;
        };
        let Ok(search) = SearchQuery::text(state.query.as_str()).compile() else {
            return;
        };
        let rope = self.buffer.rope();
        let matches: Vec<Range<usize>> = search
            .find_all(rope, 0..rope.len_chars())
            .into_iter()
            .filter(|found| !state.wordwise || self.is_whole_word(found))
            .collect();
        if matches.is_empty() {
            return;
        }
        let primary = self.selections.primary().range();
        let primary_index = matches
            .iter()
            .position(|found| *found == primary)
            .unwrap_or(matches.len() - 1);
        let selections = matches
            .into_iter()
            .map(|found| Selection::new(found.start, found.end))
            .collect();
        self.selections = Selections::new(selections, primary_index);
    }

    // ----- column cursors ------------------------------------------------------------

    /// The display row holding `pos` and the display column of `pos` in it. With `by_line`
    /// a row is a whole buffer line, whatever the wrap width.
    fn row_of(&self, pos: usize, by_line: bool) -> (RowKey, usize) {
        let point = self.buffer.char_to_point(pos);
        if by_line {
            let col = display::char_col_to_display_col(
                &self.buffer.line_text(point.line),
                point.col,
                self.tab_size(),
            );
            return (
                RowKey {
                    line: point.line,
                    row: 0,
                },
                col,
            );
        }
        let layout = self.layout(point.line);
        let (row, col) =
            display::char_col_to_row_col(&layout.text, &layout.rows, point.col, self.tab_size());
        (
            RowKey {
                line: point.line,
                row,
            },
            col,
        )
    }

    /// The row above or below `key`, if there is one.
    fn neighbor_row(&self, key: RowKey, above: bool, by_line: bool) -> Option<RowKey> {
        let last_line = self.buffer.len_lines() - 1;
        if by_line {
            return match above {
                true => key.line.checked_sub(1).map(|line| RowKey { line, row: 0 }),
                false => (key.line < last_line).then(|| RowKey {
                    line: key.line + 1,
                    row: 0,
                }),
            };
        }
        if above {
            if key.row > 0 {
                return Some(RowKey {
                    line: key.line,
                    row: key.row - 1,
                });
            }
            let line = key.line.checked_sub(1)?;
            Some(RowKey {
                line,
                row: self.layout(line).rows.len().saturating_sub(1),
            })
        } else {
            if key.row + 1 < self.layout(key.line).rows.len() {
                return Some(RowKey {
                    line: key.line,
                    row: key.row + 1,
                });
            }
            (key.line < last_line).then(|| RowKey {
                line: key.line + 1,
                row: 0,
            })
        }
    }

    /// The selection covering display columns `columns` of row `key`, or `None` when the
    /// row is too short to reach them. A cursor needs the row to be at least as wide as its
    /// column; a range needs the row to extend past its first column, and is cut off at the
    /// end of the row.
    fn columnar_selection(
        &self,
        key: RowKey,
        columns: &Range<usize>,
        reversed: bool,
        by_line: bool,
    ) -> Option<Selection> {
        let tab_size = self.tab_size();
        let line_start = self.buffer.line_to_char(key.line);
        let (text, row_start): (Cow<'_, str>, usize) = if by_line {
            (self.buffer.line_text(key.line), line_start)
        } else {
            let layout = self.layout(key.line);
            let range = layout.rows.get(key.row)?.clone();
            (
                Cow::Owned(display::slice_chars(&layout.text, range.clone()).to_string()),
                line_start + range.start,
            )
        };
        let width = display::str_width(&text, tab_size);
        if columns.is_empty() {
            if width < columns.start {
                return None;
            }
            let col = display::display_col_to_char_col(&text, columns.start, tab_size);
            return Some(Selection::cursor(row_start + col).with_goal(Some(columns.start)));
        }
        if width <= columns.start {
            return None;
        }
        let start = row_start + display::display_col_to_char_col(&text, columns.start, tab_size);
        let end = row_start + display::display_col_to_char_col(&text, columns.end, tab_size);
        Some(Selection::from_range(start..end, reversed))
    }

    /// The nearest row above or below `last` that can hold `columns`, as a selection.
    fn columnar_neighbor(
        &self,
        last: Selection,
        columns: &Range<usize>,
        above: bool,
        by_line: bool,
    ) -> Option<Selection> {
        let (mut key, _) = self.row_of(last.head, by_line);
        for _ in 0..MAX_ROWS_SKIPPED {
            key = self.neighbor_row(key, above, by_line)?;
            if let Some(selection) = self.columnar_selection(key, columns, last.reversed(), by_line)
            {
                return Some(selection);
            }
        }
        None
    }

    /// Start a column-cursor run: every selection becomes a group. A selection that spans
    /// several rows is first split into one selection per row over its columns.
    fn new_add_selections_state(&self, above: bool, by_line: bool) -> AddSelectionsState {
        let mut groups = Vec::new();
        for selection in self.selections.iter() {
            let (start_row, start_col) = self.row_of(selection.start(), by_line);
            let (end_row, end_col) = self.row_of(selection.end(), by_line);
            if start_row == end_row {
                let columns = if selection.is_empty() {
                    let col = selection.goal.unwrap_or(start_col);
                    col..col
                } else {
                    start_col..end_col
                };
                groups.push(AddSelectionsGroup {
                    stack: vec![*selection],
                    columns,
                });
                continue;
            }
            let columns = start_col.min(end_col)..start_col.max(end_col);
            let mut stack = Vec::new();
            let mut key = Some(start_row);
            while let Some(row) = key {
                stack.extend(self.columnar_selection(row, &columns, selection.reversed(), by_line));
                key = if row == end_row {
                    None
                } else {
                    self.neighbor_row(row, false, by_line)
                };
            }
            if above {
                stack.reverse();
            }
            if stack.is_empty() {
                stack.push(*selection);
            }
            groups.push(AddSelectionsGroup { stack, columns });
        }
        AddSelectionsState {
            above,
            groups,
            result: self.selections.clone(),
        }
    }

    /// `AddSelectionAbove` and `AddSelectionBelow`.
    ///
    /// Each selection gets a twin in the same display columns on the next row in the given
    /// direction, skipping rows that are too short. Repeating the command keeps adding from
    /// the last twin. The opposite command takes the last twin away again; once only the
    /// original selections are left it starts adding in its own direction. With
    /// `skip_soft_wrap` (or when wrapping is off) rows are buffer lines.
    pub(crate) fn add_selection_in_direction(&mut self, above: bool, skip_soft_wrap: bool) {
        let by_line = skip_soft_wrap || self.wrap_width().is_none();
        let mut state = match self.add_selections.take() {
            Some(state) if state.result == self.selections => state,
            _ => self.new_add_selections_state(above, by_line),
        };
        if state.above == above {
            for group in &mut state.groups {
                if let Some(&last) = group.stack.last() {
                    group.stack.extend(self.columnar_neighbor(
                        last,
                        &group.columns,
                        above,
                        by_line,
                    ));
                }
            }
        } else {
            for group in &mut state.groups {
                if group.stack.len() > 1 {
                    group.stack.pop();
                }
            }
        }
        let all: Vec<Selection> = state
            .groups
            .iter()
            .flat_map(|group| group.stack.iter().copied())
            .collect();
        if all.is_empty() {
            return;
        }
        let newest = all.len() - 1;
        self.selections = Selections::new(all, newest);
        if state.groups.iter().any(|group| group.stack.len() > 1) {
            state.result = self.selections.clone();
            self.add_selections = Some(state);
        }
    }

    // ----- growing and shrinking -----------------------------------------------------

    /// The char offset just after the last non-blank character of `line`, or the start of
    /// the line when it is blank.
    fn trimmed_line_end(&self, line: usize) -> usize {
        let buffer = &self.buffer;
        let start = buffer.line_to_char(line);
        let mut end = buffer.line_end(line);
        let mut chars = buffer.rope().chars_at(end);
        while end > start {
            match chars.prev() {
                Some(c) if c.is_whitespace() => end -= 1,
                _ => break,
            }
        }
        end
    }

    /// The next larger unit around `selection`: the smallest of the word, the inside and
    /// the outside of the nearest brackets and quotes, the line's text, the whole line and
    /// the whole text that strictly contains it.
    fn grown(&self, selection: Selection) -> Selection {
        let buffer = &self.buffer;
        let range = selection.range();
        let mut candidates: Vec<Range<usize>> = Vec::new();

        let (run, kind) = chars::surrounding_run(buffer.rope(), range.start, self.word_chars());
        if kind == CharKind::Word {
            candidates.push(run);
        }
        let delimiters = [
            pairs::enclosing_bracket(buffer, range.clone()),
            pairs::enclosing_quote(buffer, range.clone()),
        ];
        for (open, close) in delimiters.into_iter().flatten() {
            candidates.push(open + 1..close);
            candidates.push(open..close + 1);
        }
        let first_line = buffer.char_to_line(range.start);
        let last_line = buffer.char_to_line(range.end);
        let line_start = buffer.line_to_char(first_line);
        let text_start =
            line_start + indent::indent_of_line(buffer, first_line, self.tab_size()).chars;
        let text_end = self.trimmed_line_end(last_line);
        if text_start <= text_end {
            candidates.push(text_start..text_end);
        }
        candidates.push(line_start..buffer.line_end(last_line));
        candidates.push(0..buffer.len_chars());

        candidates
            .into_iter()
            .filter(|candidate| {
                candidate.start <= range.start
                    && range.end <= candidate.end
                    && candidate.len() > range.len()
            })
            .min_by_key(|candidate| candidate.len())
            .map_or(selection, |candidate| {
                Selection::new(candidate.start, candidate.end)
            })
    }

    /// `SelectLargerSyntaxNode`: grow every selection one step (see [`Editor::grown`]) and
    /// remember the selections so `SelectSmallerSyntaxNode` can step back.
    pub(crate) fn select_larger(&mut self) {
        let before = self.selections.clone();
        let mut grown = before.clone();
        grown.transform(|selection| self.grown(selection));
        if grown == before {
            return;
        }
        if self.expand_result.as_ref() != Some(&before) {
            self.expand_stack.clear();
        }
        self.expand_stack.push(before);
        self.expand_result = Some(grown.clone());
        self.selections = grown;
    }

    /// `SelectSmallerSyntaxNode`: go back to the selections from before the last
    /// `SelectLargerSyntaxNode`. Does nothing when the selections were changed in any other
    /// way since.
    pub(crate) fn select_smaller(&mut self) {
        if self.expand_result.as_ref() != Some(&self.selections) {
            self.expand_stack.clear();
            self.expand_result = None;
            return;
        }
        if let Some(previous) = self.expand_stack.pop() {
            self.selections = previous;
            self.expand_result = Some(self.selections.clone());
        }
    }

    /// `SelectBetween`: the native port of the `zed-select-between` script (see
    /// [`pairs::select_between`]), applied to the primary selection. The result is always a
    /// single selection.
    pub(crate) fn select_between_primary(&mut self, kind: PairKind) {
        let primary = self.selections.primary();
        match pairs::select_between(self.buffer.rope(), kind, primary.head, primary.range()) {
            BetweenTarget::Select(range) => {
                self.selections = Selections::single(Selection::new(range.start, range.end));
            }
            BetweenTarget::Cursor(pos) => {
                self.selections = Selections::single(Selection::cursor(pos))
            }
            BetweenTarget::Nothing => {}
        }
    }
}
