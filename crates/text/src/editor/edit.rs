//! Commands that change text at the cursors: typing, deleting, clipboard.

use std::ops::Range;

use crate::chars::{self, CharKind};
use crate::command::{Clipboard, ClipboardPiece};
use crate::edit::{Bias, Change, EditSummary};
use crate::file;
use crate::indent;
use crate::pairs;
use crate::selection::{Selection, Selections};

use super::Editor;

/// Characters before which a bracket or quote is closed automatically (besides whitespace,
/// the end of the text, and any closing bracket). Zed's default `autoclose_before`.
const AUTOCLOSE_BEFORE: &str = ";:.,=}])>";

/// The brackets that always count as a pair for "Enter between brackets".
const BRACKETS: [(char, char); 3] = [('(', ')'), ('[', ']'), ('{', '}')];

/// What typing one character does at one selection.
enum InsertPlan {
    /// The character is already there: move past it.
    StepOver,
    /// Put an opener before the selection and a closer after it.
    Surround(char, char),
    /// Insert the character and its closer, leaving the cursor between them.
    Close(char),
    /// Replace the selection with the text.
    Plain,
}

/// The changes of a summary that holds a single batch.
fn changes_of(summary: &EditSummary) -> &[Change] {
    summary.batches().first().map_or(&[], Vec::as_slice)
}

impl Editor {
    /// Replace the selections one for one, keeping which is oldest and newest.
    pub(crate) fn replace_selections(&mut self, replacements: Vec<Selection>) {
        if replacements.len() != self.selections.len() {
            let primary = self
                .selections
                .primary_index()
                .min(replacements.len().saturating_sub(1));
            self.selections = Selections::new(replacements, primary);
            return;
        }
        let mut next = replacements.into_iter();
        let mut selections = self.selections.clone();
        selections.transform(|selection| next.next().unwrap_or(selection));
        self.selections = selections;
    }

    /// Apply edits given in any order. Returns the summary and, for each given edit, the
    /// index of its change in the summary's batch.
    pub(crate) fn apply_unsorted(
        &mut self,
        edits: Vec<(Range<usize>, String)>,
    ) -> (EditSummary, Vec<usize>) {
        let mut order: Vec<usize> = (0..edits.len()).collect();
        order.sort_by_key(|&index| edits[index].0.start);
        let sorted: Vec<(Range<usize>, &str)> = order
            .iter()
            .map(|&index| (edits[index].0.clone(), edits[index].1.as_str()))
            .collect();
        let summary = self.buffer.edit(&sorted);
        let mut index_of = vec![0; edits.len()];
        for (position, &index) in order.iter().enumerate() {
            index_of[index] = position;
        }
        (summary, index_of)
    }

    /// Delete `ranges` (in any order, overlapping or not) and let the selections collapse
    /// onto what is left.
    pub(crate) fn delete_ranges(&mut self, mut ranges: Vec<Range<usize>>) -> EditSummary {
        ranges.retain(|range| range.start < range.end);
        ranges.sort_by_key(|range| range.start);
        let mut merged: Vec<Range<usize>> = Vec::new();
        for range in ranges {
            match merged.last_mut() {
                Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
                _ => merged.push(range),
            }
        }
        let edits: Vec<(Range<usize>, &str)> =
            merged.into_iter().map(|range| (range, "")).collect();
        let summary = self.buffer.edit(&edits);
        let mut selections = self.selections.clone();
        selections.transform(|selection| selection.map_through(&summary).with_goal(None));
        self.selections = selections;
        summary
    }

    // ----- typing --------------------------------------------------------------------

    /// The closer of the auto-close pair opened by `open`.
    fn autoclose_partner(&self, open: char) -> Option<char> {
        self.settings
            .autoclose_pairs
            .iter()
            .find(|pair| pair.0 == open)
            .map(|pair| pair.1)
    }

    fn is_autoclose_closer(&self, c: char) -> bool {
        self.settings.autoclose_pairs.iter().any(|pair| pair.1 == c)
    }

    /// Whether `c` closes a bracket pair (a pair whose two characters differ).
    fn is_closing_bracket(&self, c: char) -> bool {
        self.settings
            .autoclose_pairs
            .iter()
            .any(|pair| pair.1 == c && pair.0 != c)
    }

    /// Decide what typing `typed` (the single typed character, if the text is one) does at
    /// `selection`.
    fn plan_insert(&self, selection: &Selection, typed: Option<char>) -> InsertPlan {
        let (true, Some(c)) = (self.settings.autoclose, typed) else {
            return InsertPlan::Plain;
        };
        if !selection.is_empty() {
            return match self.settings.surround_pairs.iter().find(|pair| pair.0 == c) {
                Some(&(open, close)) => InsertPlan::Surround(open, close),
                None => InsertPlan::Plain,
            };
        }
        let next = self.buffer.char_at(selection.head);
        if next == Some(c) && self.is_autoclose_closer(c) {
            return InsertPlan::StepOver;
        }
        let Some(close) = self.autoclose_partner(c) else {
            return InsertPlan::Plain;
        };
        let room_after = next.is_none_or(|n| {
            n.is_whitespace() || AUTOCLOSE_BEFORE.contains(n) || self.is_closing_bracket(n)
        });
        let previous = selection
            .head
            .checked_sub(1)
            .and_then(|before| self.buffer.char_at(before));
        // A quote next to a word is an apostrophe or the end of a string, and a quote right
        // after the same quote is the start of a longer delimiter: leave both alone.
        let quote_in_the_way = c == close
            && previous.is_some_and(|p| {
                p == c || chars::char_kind(p, self.word_chars()) == CharKind::Word
            });
        if room_after && !quote_in_the_way {
            InsertPlan::Close(close)
        } else {
            InsertPlan::Plain
        }
    }

    /// `Insert`: type `text` at every selection.
    ///
    /// The text replaces what is selected and the cursor ends after it. With `autoclose`
    /// on and a single typed character: an opening bracket or quote is inserted together
    /// with its closer when what follows allows it (a quote is not closed next to a word
    /// character); typing a closer that is already the next character steps over it; and
    /// typing an opener with something selected wraps the selection instead of replacing
    /// it. With `auto_indent` on, a closing bracket typed as the first thing on its line
    /// moves back to the indentation of the line that opened it. Line endings in `text`
    /// become `\n`.
    pub(crate) fn insert(&mut self, text: &str) -> EditSummary {
        let text = file::normalize_newlines(text);
        let typed = {
            let mut chars = text.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Some(c),
                _ => None,
            }
        };
        let old: Vec<Selection> = self.selections.iter().copied().collect();
        let plans: Vec<InsertPlan> = old
            .iter()
            .map(|selection| self.plan_insert(selection, typed))
            .collect();

        let mut edits: Vec<(Range<usize>, String)> = Vec::new();
        let mut first_edit: Vec<usize> = Vec::with_capacity(old.len());
        for (selection, plan) in old.iter().zip(&plans) {
            first_edit.push(edits.len());
            match plan {
                InsertPlan::StepOver => {}
                InsertPlan::Surround(open, close) => {
                    edits.push((selection.start()..selection.start(), open.to_string()));
                    edits.push((selection.end()..selection.end(), close.to_string()));
                }
                InsertPlan::Close(close) => {
                    let mut pair = text.to_string();
                    pair.push(*close);
                    edits.push((selection.range(), pair));
                }
                InsertPlan::Plain => edits.push((selection.range(), text.to_string())),
            }
        }
        let mut summary = self.buffer.edit(&edits);
        let changes = changes_of(&summary);
        let replacements: Vec<Selection> = old
            .iter()
            .zip(&plans)
            .zip(&first_edit)
            .map(
                |((selection, plan), &index)| match (plan, changes.get(index)) {
                    (InsertPlan::StepOver, _) => {
                        Selection::cursor(summary.map_offset(selection.head + 1, Bias::Left))
                    }
                    (InsertPlan::Surround(..), Some(open)) => {
                        let close_start = changes
                            .get(index + 1)
                            .map_or(open.new.end, |close| close.new.start);
                        Selection::from_range(open.new.end..close_start, selection.reversed())
                    }
                    (InsertPlan::Close(_), Some(change)) => {
                        Selection::cursor(change.new.end.saturating_sub(1))
                    }
                    (_, Some(change)) => Selection::cursor(change.new.end),
                    (_, None) => selection.map_through(&summary),
                },
            )
            .collect();
        self.replace_selections(replacements);
        let typed_closer = typed.filter(|c| BRACKETS.iter().any(|pair| pair.1 == *c));
        if let (true, Some(closer)) = (self.settings.auto_indent, typed_closer) {
            let stepped_over = plans
                .iter()
                .any(|plan| matches!(plan, InsertPlan::StepOver));
            if !stepped_over {
                summary.extend(self.outdent_typed_closers(closer));
            }
        }
        summary
    }

    /// After a closing bracket was typed as the first thing on its line, give the line the
    /// indentation of the line its opening bracket is on, when that is less. Typing `}`
    /// under an indented block closes the block where it started.
    fn outdent_typed_closers(&mut self, closer: char) -> EditSummary {
        let buffer = &self.buffer;
        let tab_size = self.tab_size();
        let mut edits: Vec<(Range<usize>, String)> = Vec::new();
        for selection in self
            .selections
            .iter()
            .filter(|selection| selection.is_empty())
        {
            let Some(at) = selection.head.checked_sub(1) else {
                continue;
            };
            let line = buffer.char_to_line(at);
            let line_start = buffer.line_to_char(line);
            let indent = indent::indent_of_line(buffer, line, tab_size);
            if buffer.char_at(at) != Some(closer) || line_start + indent.chars != at {
                continue;
            }
            let Some((open, _)) = pairs::bracket_pair_at(buffer, at) else {
                continue;
            };
            let open_line = buffer.char_to_line(open);
            let open_indent = indent::indent_of_line(buffer, open_line, tab_size);
            if open_line == line || open_indent.width >= indent.width {
                continue;
            }
            let open_start = buffer.line_to_char(open_line);
            let text = buffer
                .slice(open_start..open_start + open_indent.chars)
                .to_string();
            if edits
                .last()
                .is_none_or(|previous| previous.0.end <= line_start)
            {
                edits.push((line_start..at, text));
            }
        }
        let summary = self.buffer.edit(&edits);
        let mut selections = self.selections.clone();
        selections.transform(|selection| selection.map_through(&summary));
        self.selections = selections;
        summary
    }

    /// The whitespace a new line gets when it follows line `line`: that line's indentation,
    /// one level deeper when the line ends in an `indent_after` character.
    fn indent_below(&self, line: usize, unit: &str) -> String {
        let buffer = &self.buffer;
        let start = buffer.line_to_char(line);
        let chars = indent::indent_of_line(buffer, line, self.tab_size()).chars;
        let mut text = buffer.slice(start..start + chars).to_string();
        if indent::last_non_blank(buffer, line)
            .is_some_and(|c| self.settings.indent_after.contains(c))
        {
            text.push_str(unit);
        }
        text
    }

    /// The closing bracket that pairs with `open` for "Enter between brackets".
    fn bracket_closer(&self, open: char) -> Option<char> {
        BRACKETS
            .iter()
            .chain(
                self.settings
                    .autoclose_pairs
                    .iter()
                    .filter(|pair| pair.0 != pair.1),
            )
            .find(|pair| pair.0 == open)
            .map(|pair| pair.1)
    }

    /// The edit `Newline` makes at one selection: the range to replace, the text, and how
    /// many chars into the text the cursor ends up.
    fn newline_edit(&self, selection: &Selection, unit: &str) -> (Range<usize>, String, usize) {
        let (start, end) = (selection.start(), selection.end());
        if !self.settings.auto_indent {
            return (start..end, "\n".to_string(), 1);
        }
        let buffer = &self.buffer;
        let is_blank = |pos: usize| matches!(buffer.char_at(pos), Some(' ' | '\t'));
        let line = buffer.char_to_line(start);
        let line_start = buffer.line_to_char(line);
        let indent_chars = indent::indent_of_line(buffer, line, self.tab_size())
            .chars
            .min(start - line_start);
        let indent = buffer
            .slice(line_start..line_start + indent_chars)
            .to_string();

        // Look past the blanks on both sides for what the cursor sits between.
        let mut before = start;
        while before > line_start && is_blank(before - 1) {
            before -= 1;
        }
        let previous = if before > line_start {
            buffer.char_at(before - 1)
        } else {
            None
        };
        let end_of_line = buffer.line_end(buffer.char_to_line(end));
        let mut after = end;
        while after < end_of_line && is_blank(after) {
            after += 1;
        }
        let next = if after < end_of_line {
            buffer.char_at(after)
        } else {
            None
        };

        let closer = previous.and_then(|open| self.bracket_closer(open));
        if closer.is_some() && closer == next {
            // Between a pair: the closer goes on its own line and the cursor in between.
            let text = format!("\n{indent}{unit}\n{indent}");
            let cursor = 1 + indent.chars().count() + unit.chars().count();
            return (before..after, text, cursor);
        }
        let mut text = format!("\n{indent}");
        if previous.is_some_and(|c| self.settings.indent_after.contains(c)) {
            text.push_str(unit);
        }
        let cursor = text.chars().count();
        (start..after, text, cursor)
    }

    /// `Newline`: break the line at every selection.
    ///
    /// With `auto_indent` the new line starts with the indentation of the line being
    /// broken (up to the cursor's column), one level deeper when the text before the
    /// cursor ends in an `indent_after` character, and blanks right after the cursor are
    /// dropped so the rest of the line starts at that indentation. Between a bracket pair
    /// (`{|}`) the closing bracket goes on a line of its own and the cursor on an indented
    /// line between. Without `auto_indent` only a line break is inserted.
    pub(crate) fn newline(&mut self) -> EditSummary {
        let unit = self.indent_unit();
        let mut edits = Vec::new();
        let mut cursor_offsets = Vec::new();
        for selection in self.selections.iter() {
            let (range, text, cursor) = self.newline_edit(selection, &unit);
            edits.push((range, text));
            cursor_offsets.push(cursor);
        }
        let summary = self.buffer.edit(&edits);
        let replacements = changes_of(&summary)
            .iter()
            .zip(&cursor_offsets)
            .map(|(change, offset)| {
                Selection::cursor((change.new.start + offset).min(change.new.end))
            })
            .collect();
        self.replace_selections(replacements);
        summary
    }

    /// `NewlineAbove` and `NewlineBelow`: open an empty line next to the line of every
    /// cursor and move there. Several cursors on one line open one line between them.
    ///
    /// With `auto_indent`, a line opened below is indented like `Newline` at the end of
    /// the cursor's line would; a line opened above gets the cursor line's indentation, or
    /// one level more than the line before it when that line ends in an `indent_after`
    /// character.
    pub(crate) fn newline_beside(&mut self, above: bool) -> EditSummary {
        let unit = self.indent_unit();
        let buffer = &self.buffer;
        let primary_line = buffer.char_to_line(self.selections.primary().head);
        let mut lines: Vec<usize> = self
            .selections
            .iter()
            .map(|selection| buffer.char_to_line(selection.head))
            .collect();
        lines.dedup();
        let mut edits = Vec::new();
        let mut cursor_offsets = Vec::new();
        for &line in &lines {
            let indent = if !self.settings.auto_indent {
                String::new()
            } else if !above {
                self.indent_below(line, &unit)
            } else if line > 0
                && indent::last_non_blank(buffer, line - 1)
                    .is_some_and(|c| self.settings.indent_after.contains(c))
            {
                self.indent_below(line - 1, &unit)
            } else {
                let start = buffer.line_to_char(line);
                let chars = indent::indent_of_line(buffer, line, self.tab_size()).chars;
                buffer.slice(start..start + chars).to_string()
            };
            let indent_len = indent.chars().count();
            if above {
                let at = buffer.line_to_char(line);
                edits.push((at..at, format!("{indent}\n")));
                cursor_offsets.push(indent_len);
            } else {
                let at = buffer.line_end(line);
                edits.push((at..at, format!("\n{indent}")));
                cursor_offsets.push(1 + indent_len);
            }
        }
        let summary = self.buffer.edit(&edits);
        let cursors: Vec<Selection> = changes_of(&summary)
            .iter()
            .zip(&cursor_offsets)
            .map(|(change, offset)| Selection::cursor(change.new.start + offset))
            .collect();
        let primary = lines
            .iter()
            .position(|&line| line == primary_line)
            .unwrap_or(0);
        self.selections = Selections::new(cursors, primary);
        summary
    }

    // ----- deleting ------------------------------------------------------------------

    /// What `Backspace` deletes at one selection.
    fn backspace_range(&self, selection: &Selection) -> Option<Range<usize>> {
        if !selection.is_empty() {
            return Some(selection.range());
        }
        let buffer = &self.buffer;
        let head = selection.head;
        let previous = buffer.char_at(head.checked_sub(1)?)?;
        if self.settings.autoclose {
            let next = buffer.char_at(head);
            if self
                .settings
                .autoclose_pairs
                .iter()
                .any(|&(open, close)| open == previous && Some(close) == next)
            {
                return Some(head - 1..head + 1);
            }
        }
        let line = buffer.char_to_line(head);
        let line_start = buffer.line_to_char(line);
        let tab_size = self.tab_size();
        let in_indent = head - line_start <= indent::indent_of_line(buffer, line, tab_size).chars;
        if in_indent && previous == ' ' && !self.settings.hard_tabs {
            // Soft tabs: delete back to the previous tab stop, spaces only. Everything
            // before the cursor on this line is a space or a tab.
            let col = buffer.slice(line_start..head).chars().fold(0, |col, c| {
                if c == '\t' {
                    col + tab_size - col % tab_size
                } else {
                    col + 1
                }
            });
            let stop = (col.saturating_sub(1) / tab_size) * tab_size;
            let mut start = head;
            let mut at = col;
            while start > line_start && at > stop && buffer.char_at(start - 1) == Some(' ') {
                start -= 1;
                at -= 1;
            }
            return Some(start..head);
        }
        Some(buffer.prev_grapheme(head)..head)
    }

    /// `Backspace`: delete the selection, or at a bare cursor the grapheme cluster before
    /// it. Two special cases at a cursor: an empty auto-close pair (`(|)`) is deleted
    /// whole, and in the leading whitespace of a line indented with spaces (with
    /// `hard_tabs` off) a whole indent step is deleted.
    pub(crate) fn backspace(&mut self) -> EditSummary {
        let ranges = self
            .selections
            .iter()
            .filter_map(|selection| self.backspace_range(selection))
            .collect();
        self.delete_ranges(ranges)
    }

    /// `Delete`: delete the selection, or at a bare cursor the grapheme cluster after it.
    pub(crate) fn delete_forward(&mut self) -> EditSummary {
        let buffer = &self.buffer;
        let ranges = self
            .selections
            .iter()
            .map(|selection| {
                if selection.is_empty() {
                    selection.head..buffer.next_grapheme(selection.head)
                } else {
                    selection.range()
                }
            })
            .collect();
        self.delete_ranges(ranges)
    }

    /// Zed's `adjust_greedy_deletion`: cut a word deletion from `from` towards `until`
    /// short so that it does not swallow too much at once. Unless `ignore_brackets`, the
    /// deletion stops at the bracket nearest to the cursor inside the range; and a run of
    /// two or more whitespace characters is deleted on its own before the word beyond it.
    fn adjust_greedy_deletion(&self, from: usize, until: usize, ignore_brackets: bool) -> usize {
        if from == until {
            return until;
        }
        let backward = from > until;
        let (mut start, mut end) = (from.min(until), from.max(until));
        let buffer = &self.buffer;
        if !ignore_brackets {
            // Both edges of every bracket strictly inside the range are candidate stops.
            let stops = buffer
                .slice(start..end)
                .chars()
                .enumerate()
                .filter(|(_, c)| "()[]{}".contains(*c))
                .flat_map(|(offset, _)| {
                    [start + offset, start + offset + 1]
                        .into_iter()
                        .filter(|&stop| start < stop && stop < end)
                });
            if backward {
                start = stops.max().unwrap_or(start);
            } else {
                end = stops.min().unwrap_or(end);
            }
        }
        // Runs of two or more whitespace characters, as (start, end).
        let mut runs: Vec<(usize, usize)> = Vec::new();
        let mut run_start: Option<usize> = None;
        let mut position = start;
        for c in buffer.slice(start..end).chars() {
            if c.is_whitespace() {
                run_start.get_or_insert(position);
            } else if let Some(began) = run_start.take() {
                if position - began >= 2 {
                    runs.push((began, position));
                }
            }
            position += 1;
        }
        if let Some(began) = run_start {
            if position - began >= 2 {
                runs.push((began, position));
            }
        }
        if backward {
            runs.last().map_or(start, |run| run.0)
        } else {
            runs.first().map_or(end, |run| run.1)
        }
    }

    /// The word-deletion commands. At a bare cursor they delete to the start of the word
    /// before it (or, with `forward`, the end of the word after it), in sub-word steps with
    /// `subword`. Unless `ignore_newlines`, a deletion never takes more than one line
    /// break; see [`Editor::adjust_greedy_deletion`] for `ignore_brackets`. A selection is
    /// simply deleted.
    pub(crate) fn delete_word(
        &mut self,
        forward: bool,
        subword: bool,
        ignore_newlines: bool,
        ignore_brackets: bool,
    ) -> EditSummary {
        let rope = self.buffer.rope();
        let words = self.word_chars();
        let boundary = match (forward, subword, ignore_newlines) {
            (false, false, false) => chars::previous_word_start_or_newline,
            (false, false, true) => chars::previous_word_start,
            (false, true, false) => chars::previous_subword_start_or_newline,
            (false, true, true) => chars::previous_subword_start,
            (true, false, false) => chars::next_word_end_or_newline,
            (true, false, true) => chars::next_word_end,
            (true, true, false) => chars::next_subword_end_or_newline,
            (true, true, true) => chars::next_subword_end,
        };
        let ranges = self
            .selections
            .iter()
            .map(|selection| {
                if !selection.is_empty() {
                    return selection.range();
                }
                let head = selection.head;
                let target =
                    self.adjust_greedy_deletion(head, boundary(rope, head, words), ignore_brackets);
                head.min(target)..head.max(target)
            })
            .collect();
        self.delete_ranges(ranges)
    }

    /// `DeleteToBeginningOfLine`: delete from the start of the line (or, with
    /// `stop_at_indent`, the first non-blank character) to the end of each selection. A
    /// bare cursor with nothing to its left on the line deletes what `Backspace` would.
    pub(crate) fn delete_to_beginning_of_line(&mut self, stop_at_indent: bool) -> EditSummary {
        let ranges = self
            .selections
            .iter()
            .filter_map(|selection| {
                let target = self.beginning_of_line(selection.start(), false, stop_at_indent);
                if target < selection.start() {
                    Some(target..selection.end())
                } else {
                    self.backspace_range(selection)
                }
            })
            .collect();
        self.delete_ranges(ranges)
    }

    /// `DeleteToEndOfLine`: delete from the start of each selection to the end of the line
    /// it ends on. A bare cursor at the end of a line deletes the line break.
    pub(crate) fn delete_to_end_of_line(&mut self) -> EditSummary {
        let buffer = &self.buffer;
        let ranges = self
            .selections
            .iter()
            .map(|selection| {
                let line_end = buffer.line_end(buffer.char_to_line(selection.end()));
                if selection.start() < line_end {
                    selection.start()..line_end
                } else {
                    selection.head..buffer.next_grapheme(selection.head)
                }
            })
            .collect();
        self.delete_ranges(ranges)
    }

    /// `Transpose`: at every bare cursor, swap the grapheme clusters before and after it
    /// and move past them. At the end of a line the two clusters before the cursor are
    /// swapped instead. Selections and a cursor at the very start are left alone.
    pub(crate) fn transpose(&mut self) -> EditSummary {
        let buffer = &self.buffer;
        let mut edits: Vec<(Range<usize>, String)> = Vec::new();
        let mut cursors: Vec<Option<usize>> = Vec::new();
        for selection in self.selections.iter() {
            cursors.push(None);
            if !selection.is_empty() {
                continue;
            }
            let head = selection.head;
            let at_line_end = head == buffer.line_end(buffer.char_to_line(head));
            let middle = if at_line_end {
                buffer.prev_grapheme(head)
            } else {
                head
            };
            if middle == 0 {
                continue;
            }
            let start = buffer.prev_grapheme(middle);
            let end = buffer.next_grapheme(middle);
            if end <= middle || edits.last().is_some_and(|previous| previous.0.end > start) {
                continue;
            }
            let swapped = format!(
                "{}{}",
                buffer.slice(middle..end),
                buffer.slice(start..middle)
            );
            edits.push((start..end, swapped));
            if let Some(last) = cursors.last_mut() {
                *last = Some(edits.len() - 1);
            }
        }
        let summary = self.buffer.edit(&edits);
        let changes = changes_of(&summary);
        let replacements = self
            .selections
            .iter()
            .zip(&cursors)
            .map(
                |(selection, edit)| match edit.and_then(|index| changes.get(index)) {
                    Some(change) => Selection::cursor(change.new.end),
                    None => selection.map_through(&summary),
                },
            )
            .collect();
        self.replace_selections(replacements);
        summary
    }

    // ----- clipboard -----------------------------------------------------------------

    /// What `Copy` and `Cut` take: the clipboard contents and the ranges they came from.
    ///
    /// A selection gives its text. A bare cursor gives its whole line including the line
    /// break (one is added for a last line without one), marked line-wise; several cursors
    /// on one line give that line once.
    pub(crate) fn copy_selections(&self) -> (Clipboard, Vec<Range<usize>>) {
        let buffer = &self.buffer;
        let mut pieces = Vec::new();
        let mut ranges = Vec::new();
        let mut last_whole_line: Option<usize> = None;
        for selection in self.selections.iter() {
            if !selection.is_empty() {
                last_whole_line = None;
                pieces.push(ClipboardPiece {
                    text: buffer.slice(selection.range()).to_string(),
                    linewise: false,
                });
                ranges.push(selection.range());
                continue;
            }
            let line = buffer.char_to_line(selection.head);
            if last_whole_line == Some(line) {
                continue;
            }
            last_whole_line = Some(line);
            let range = buffer.line_to_char(line)..buffer.line_to_char(line + 1);
            let mut text = buffer.slice(range.clone()).to_string();
            if !text.ends_with('\n') {
                text.push('\n');
            }
            pieces.push(ClipboardPiece {
                text,
                linewise: true,
            });
            ranges.push(range);
        }
        (Clipboard::from_pieces(pieces), ranges)
    }

    /// `Paste`: insert `clipboard` at every selection.
    ///
    /// When the clipboard has exactly one piece per selection, each selection gets its own
    /// piece; otherwise every selection gets the whole text. Text replaces a selection and
    /// the cursor ends after it. A line-wise piece at a bare cursor is inserted as a whole
    /// line above the cursor's line, and the cursor stays where it was in its own line.
    /// Nothing is re-indented.
    pub(crate) fn paste(&mut self, clipboard: &Clipboard) -> EditSummary {
        if clipboard.is_empty() && clipboard.pieces.iter().all(|piece| piece.text.is_empty()) {
            return EditSummary::new();
        }
        let buffer = &self.buffer;
        let per_selection =
            !clipboard.pieces.is_empty() && clipboard.pieces.len() == self.selections.len();
        let old: Vec<Selection> = self.selections.iter().copied().collect();
        let mut edits: Vec<(Range<usize>, String)> = Vec::with_capacity(old.len());
        let mut above_line: Vec<bool> = Vec::with_capacity(old.len());
        for (index, selection) in old.iter().enumerate() {
            let (text, linewise) = match clipboard.pieces.get(index) {
                Some(piece) if per_selection => (piece.text.as_str(), piece.linewise),
                _ => (clipboard.text.as_str(), clipboard.linewise),
            };
            let mut text = file::normalize_newlines(text).into_owned();
            if linewise && selection.is_empty() {
                if !text.ends_with('\n') {
                    text.push('\n');
                }
                let line_start = buffer.line_to_char(buffer.char_to_line(selection.head));
                edits.push((line_start..line_start, text));
                above_line.push(true);
            } else {
                edits.push((selection.range(), text));
                above_line.push(false);
            }
        }
        let (summary, index_of) = self.apply_unsorted(edits);
        let changes = changes_of(&summary);
        let replacements = old
            .iter()
            .enumerate()
            .map(|(index, selection)| match changes.get(index_of[index]) {
                Some(change) if !above_line[index] => Selection::cursor(change.new.end),
                _ => Selection::cursor(summary.map_offset(selection.head, Bias::Right)),
            })
            .collect();
        self.replace_selections(replacements);
        summary
    }

    /// `KillRingCut`: cut each selection, or at a bare cursor the rest of its line (the
    /// line break itself when the cursor is already at the end), into the kill ring.
    pub(crate) fn kill_ring_cut(&mut self) -> EditSummary {
        let buffer = &self.buffer;
        let ranges: Vec<Range<usize>> = self
            .selections
            .iter()
            .map(|selection| {
                if !selection.is_empty() {
                    return selection.range();
                }
                let line_end = buffer.line_end(buffer.char_to_line(selection.head));
                if selection.head < line_end {
                    selection.head..line_end
                } else {
                    selection.head..buffer.next_grapheme(selection.head)
                }
            })
            .filter(|range| range.start < range.end)
            .collect();
        if ranges.is_empty() {
            return EditSummary::new();
        }
        let pieces = ranges
            .iter()
            .map(|range| ClipboardPiece {
                text: buffer.slice(range.clone()).to_string(),
                linewise: false,
            })
            .collect();
        self.kill_ring = Some(Clipboard::from_pieces(pieces));
        self.delete_ranges(ranges)
    }
}
