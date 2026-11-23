//! Commands that work on whole lines: indenting, moving, duplicating, joining, commenting.

use std::ops::Range;

use crate::buffer::Point;
use crate::edit::{Bias, EditSummary};
use crate::indent;
use crate::selection::Selection;

use super::Editor;

/// A run of consecutive lines touched by one or more selections.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LineBlock {
    first: usize,
    last: usize,
    /// Indices of the selections that touch the block.
    selections: Vec<usize>,
}

impl Editor {
    /// The lines `selection` touches. A selection that ends at the very start of a line
    /// does not touch that line.
    fn spanned_lines(&self, selection: &Selection) -> (usize, usize) {
        let buffer = &self.buffer;
        let first = buffer.char_to_line(selection.start());
        let mut last = buffer.char_to_line(selection.end());
        if last > first && selection.end() == buffer.line_to_char(last) {
            last -= 1;
        }
        (first, last)
    }

    /// The lines touched by the selections, grouped into blocks. Selections that share a
    /// line always share a block; with `merge_adjacent`, so do selections on neighbouring
    /// lines.
    fn line_blocks(&self, merge_adjacent: bool) -> Vec<LineBlock> {
        let mut blocks: Vec<LineBlock> = Vec::new();
        for (index, selection) in self.selections.iter().enumerate() {
            let (first, last) = self.spanned_lines(selection);
            match blocks.last_mut() {
                Some(block)
                    if first <= block.last || (merge_adjacent && first == block.last + 1) =>
                {
                    block.last = block.last.max(last);
                    block.selections.push(index);
                }
                _ => blocks.push(LineBlock {
                    first,
                    last,
                    selections: vec![index],
                }),
            }
        }
        blocks
    }

    /// Clear every goal column after an edit moved the text.
    fn map_selections(&mut self, summary: &EditSummary) {
        let mut selections = self.selections.clone();
        selections.transform(|selection| selection.map_through(summary).with_goal(None));
        self.selections = selections;
    }

    /// `DeleteLine`: delete the lines the selections touch. Each cursor lands on the line
    /// that takes their place (or the line before, at the end of the text), in the column
    /// it was in.
    pub(crate) fn delete_line(&mut self) -> EditSummary {
        let buffer = &self.buffer;
        let last_line = buffer.len_lines() - 1;
        let blocks = self.line_blocks(true);
        let old: Vec<Selection> = self.selections.iter().copied().collect();
        let mut edits: Vec<(Range<usize>, &str)> = Vec::new();
        let mut landing: Vec<usize> = vec![0; old.len()];
        for block in &blocks {
            let (range, landing_line) = if block.last < last_line {
                (
                    buffer.line_to_char(block.first)..buffer.line_to_char(block.last + 1),
                    Some(block.last + 1),
                )
            } else if block.first > 0 {
                // The last lines go together with the line break before them.
                (
                    buffer.line_end(block.first - 1)..buffer.len_chars(),
                    Some(block.first - 1),
                )
            } else {
                (0..buffer.len_chars(), None)
            };
            for &index in &block.selections {
                landing[index] = match landing_line {
                    Some(line) => {
                        let col = buffer.char_to_point(old[index].head).col;
                        buffer.point_to_char(Point::new(line, col))
                    }
                    None => 0,
                };
            }
            edits.push((range, ""));
        }
        let summary = self.buffer.edit(&edits);
        let replacements = landing
            .iter()
            .map(|&pos| Selection::cursor(summary.map_offset(pos, Bias::Left)))
            .collect();
        self.replace_selections(replacements);
        summary
    }

    /// The whitespace that takes a line indented `width` columns to the next indent level.
    fn indent_step(&self, width: usize) -> String {
        if self.settings.hard_tabs {
            "\t".to_string()
        } else {
            let tab_size = self.tab_size();
            " ".repeat(tab_size - width % tab_size)
        }
    }

    /// Indent the lines of the blocks: each gets whitespace up to the next indent level at
    /// its start. Empty lines inside a block of several lines are left empty.
    fn indent_blocks(&self, blocks: &[LineBlock]) -> Vec<(Range<usize>, String)> {
        let buffer = &self.buffer;
        let mut edits = Vec::new();
        for block in blocks {
            for line in block.first..=block.last {
                if block.first != block.last && buffer.line_len(line) == 0 {
                    continue;
                }
                let at = buffer.line_to_char(line);
                let width = indent::indent_of_line(buffer, line, self.tab_size()).width;
                edits.push((at..at, self.indent_step(width)));
            }
        }
        edits
    }

    /// `Indent`: add one level of indentation to every line the selections touch.
    pub(crate) fn indent_lines(&mut self) -> EditSummary {
        let blocks = self.line_blocks(true);
        let edits = self.indent_blocks(&blocks);
        let summary = self.buffer.edit(&edits);
        self.map_selections(&summary);
        summary
    }

    /// `Tab`: a selection indents the lines it touches; a bare cursor inserts a tab, or
    /// spaces up to the next tab stop, where it is.
    pub(crate) fn tab(&mut self) -> EditSummary {
        let buffer = &self.buffer;
        let tab_size = self.tab_size();
        // Lines indented because a selection touches them.
        let mut blocks: Vec<LineBlock> = Vec::new();
        for (index, selection) in self
            .selections
            .iter()
            .enumerate()
            .filter(|(_, selection)| !selection.is_empty())
        {
            let (first, last) = self.spanned_lines(selection);
            match blocks.last_mut() {
                Some(block) if first <= block.last + 1 => {
                    block.last = block.last.max(last);
                    block.selections.push(index);
                }
                _ => blocks.push(LineBlock {
                    first,
                    last,
                    selections: vec![index],
                }),
            }
        }
        let indented = |line: usize| {
            blocks
                .iter()
                .any(|block| block.first <= line && line <= block.last)
        };

        // Cursors on other lines insert at their own position. Several cursors on one line
        // each count the columns the ones before them added.
        let mut cursor_edits: Vec<(Range<usize>, String)> = Vec::new();
        let mut current_line = usize::MAX;
        let mut added = 0;
        for selection in self
            .selections
            .iter()
            .filter(|selection| selection.is_empty())
        {
            let line = buffer.char_to_line(selection.head);
            if indented(line) {
                continue;
            }
            if line != current_line {
                current_line = line;
                added = 0;
            }
            let text = if self.settings.hard_tabs {
                "\t".to_string()
            } else {
                let col = buffer.display_col(selection.head, tab_size) + added;
                " ".repeat(tab_size - col % tab_size)
            };
            added += if self.settings.hard_tabs {
                0
            } else {
                text.len()
            };
            cursor_edits.push((selection.head..selection.head, text));
        }

        let mut edits = self.indent_blocks(&blocks);
        edits.extend(cursor_edits);
        let (summary, _) = self.apply_unsorted(edits);
        self.map_selections(&summary);
        summary
    }

    /// `Outdent` and `Backtab`: remove one level of indentation from every line the
    /// selections touch: one leading tab, or the leading spaces back to the previous
    /// indent level.
    pub(crate) fn outdent_lines(&mut self) -> EditSummary {
        let buffer = &self.buffer;
        let tab_size = self.tab_size();
        let mut edits: Vec<(Range<usize>, &str)> = Vec::new();
        for block in self.line_blocks(true) {
            for line in block.first..=block.last {
                let start = buffer.line_to_char(line);
                let remove = match buffer.char_at(start) {
                    Some('\t') => 1,
                    Some(' ') => {
                        let spaces = buffer
                            .rope()
                            .chars_at(start)
                            .take_while(|&c| c == ' ')
                            .count();
                        let extra = spaces % tab_size;
                        (if extra == 0 { tab_size } else { extra }).min(spaces)
                    }
                    _ => 0,
                };
                if remove > 0 {
                    edits.push((start..start + remove, ""));
                }
            }
        }
        let summary = self.buffer.edit(&edits);
        self.map_selections(&summary);
        summary
    }

    /// `MoveLineUp` and `MoveLineDown`: swap each block of touched lines with the line
    /// above or below it. The selections travel with their lines. A block at the edge of
    /// the text stays where it is.
    pub(crate) fn move_lines(&mut self, up: bool) -> EditSummary {
        let buffer = &self.buffer;
        let last_line = buffer.len_lines() - 1;
        let len = buffer.len_chars();
        let mut replacements: Vec<Selection> = self.selections.iter().copied().collect();
        let mut edits: Vec<(Range<usize>, String)> = Vec::new();
        for block in self.line_blocks(true) {
            if (up && block.first == 0) || (!up && block.last == last_line) {
                continue;
            }
            let block_range = buffer.line_to_char(block.first)..buffer.line_end(block.last);
            let block_text = buffer.slice(block_range.clone()).to_string();
            let (range, text, shift) = if up {
                let other = block.first - 1;
                let other_text = buffer.line_text(other);
                let shift = -(buffer.line_len(other) as isize + 1);
                (
                    buffer.line_to_char(other)..block_range.end,
                    format!("{block_text}\n{other_text}"),
                    shift,
                )
            } else {
                let other = block.last + 1;
                let other_text = buffer.line_text(other);
                let shift = buffer.line_len(other) as isize + 1;
                (
                    block_range.start..buffer.line_end(other),
                    format!("{other_text}\n{block_text}"),
                    shift,
                )
            };
            edits.push((range, text));
            for &index in &block.selections {
                let moved = |pos: usize| ((pos as isize + shift).max(0) as usize).min(len);
                let selection = replacements[index];
                replacements[index] =
                    Selection::new(moved(selection.anchor), moved(selection.head));
            }
        }
        let summary = self.buffer.edit(&edits);
        self.replace_selections(replacements);
        summary
    }

    /// `DuplicateLineUp` and `DuplicateLineDown`: copy each block of touched lines. The
    /// selections stay on the upper copy (`up`) or move to the lower one.
    pub(crate) fn duplicate_lines(&mut self, up: bool) -> EditSummary {
        let buffer = &self.buffer;
        let last_line = buffer.len_lines() - 1;
        let mut edits: Vec<(Range<usize>, String)> = Vec::new();
        for block in self.line_blocks(false) {
            let start = buffer.line_to_char(block.first);
            let end = buffer.line_end(block.last);
            let text = buffer.slice(start..end).to_string();
            if !up {
                edits.push((start..start, format!("{text}\n")));
            } else if block.last < last_line {
                // At the start of the following line, so a selection that ends there (a
                // whole-line selection) stays on the upper copy.
                let after = buffer.line_to_char(block.last + 1);
                edits.push((after..after, format!("{text}\n")));
            } else {
                edits.push((end..end, format!("\n{text}")));
            }
        }
        let summary = self.buffer.edit(&edits);
        // Upwards the selections stay before the copy added after their block; downwards
        // they move past the copy added before it.
        let bias = if up { Bias::Left } else { Bias::Right };
        let mut selections = self.selections.clone();
        selections.transform(|selection| Selection {
            anchor: summary.map_offset(selection.anchor, bias),
            head: summary.map_offset(selection.head, bias),
            goal: None,
        });
        self.selections = selections;
        summary
    }

    /// `JoinLines`: join the line of each cursor with the line after it, or all the lines
    /// a selection touches into one. The line break and the next line's indentation are
    /// replaced by a single space, or by nothing when either side is empty or the first
    /// already ends in whitespace. A bare cursor moves to the join.
    pub(crate) fn join_lines(&mut self) -> EditSummary {
        let buffer = &self.buffer;
        let last_line = buffer.len_lines() - 1;
        // The lines to join with their successor, grouped into runs of consecutive lines.
        let mut joins: Vec<usize> = Vec::new();
        for selection in self.selections.iter() {
            let (first, last) = self.spanned_lines(selection);
            let until = if first == last { first + 1 } else { last };
            joins.extend((first..until).filter(|&line| line < last_line));
        }
        joins.sort_unstable();
        joins.dedup();

        let ends_in_whitespace = |line: usize| {
            buffer.line_len(line) == 0
                || buffer
                    .char_at(buffer.line_end(line) - 1)
                    .is_some_and(char::is_whitespace)
        };
        let mut edits: Vec<(Range<usize>, &str)> = Vec::new();
        // Whether the text joined so far gives no reason for a separating space.
        let mut no_space_needed = false;
        for (position, &line) in joins.iter().enumerate() {
            let continues_run = position > 0 && joins[position - 1] + 1 == line;
            if !continues_run {
                no_space_needed = ends_in_whitespace(line);
            }
            let next = line + 1;
            let next_start = buffer.line_to_char(next);
            let next_indent = indent::indent_of_line(buffer, next, self.tab_size()).chars;
            let next_has_text = buffer.line_len(next) > next_indent;
            let separator = if next_has_text && !no_space_needed {
                " "
            } else {
                ""
            };
            edits.push((buffer.line_end(line)..next_start + next_indent, separator));
            if next_has_text {
                no_space_needed = ends_in_whitespace(next);
            }
        }

        let old: Vec<Selection> = self.selections.iter().copied().collect();
        let join_points: Vec<Option<usize>> = old
            .iter()
            .map(|selection| {
                let line = buffer.char_to_line(selection.head);
                (selection.is_empty() && joins.binary_search(&line).is_ok())
                    .then(|| buffer.line_end(line))
            })
            .collect();
        let summary = self.buffer.edit(&edits);
        let replacements = old
            .iter()
            .zip(&join_points)
            .map(|(selection, join)| match join {
                Some(pos) => Selection::cursor(summary.map_offset(*pos, Bias::Left)),
                None => selection.map_through(&summary).with_goal(None),
            })
            .collect();
        self.replace_selections(replacements);
        summary
    }

    /// `ToggleComments`: comment or uncomment the lines the selections touch.
    ///
    /// A block of lines is uncommented when every non-blank line in it starts with the
    /// prefix, and commented otherwise. Commenting puts the prefix and a space at the
    /// smallest indentation of the block's non-blank lines, so the markers line up. Blank
    /// lines are skipped unless `comment_empty_lines` is set or the block is a single
    /// line. Uncommenting removes the prefix and one space after it. With
    /// `advance_downwards` a bare cursor then moves down a line.
    pub(crate) fn toggle_comments(
        &mut self,
        prefix: &str,
        comment_empty_lines: bool,
        advance_downwards: bool,
    ) -> EditSummary {
        let token = prefix.trim();
        if token.is_empty() {
            return EditSummary::new();
        }
        let token_len = token.chars().count();
        let buffer = &self.buffer;
        let tab_size = self.tab_size();
        let starts_with_token = |line: usize| {
            let start =
                buffer.line_to_char(line) + indent::indent_of_line(buffer, line, tab_size).chars;
            buffer.slice(start..start + token_len) == token
        };
        let mut edits: Vec<(Range<usize>, String)> = Vec::new();
        for block in self.line_blocks(false) {
            let lines = block.first..=block.last;
            let single = block.first == block.last;
            let with_text: Vec<usize> = lines
                .clone()
                .filter(|&line| !indent::is_blank_line(buffer, line))
                .collect();
            if !with_text.is_empty() && with_text.iter().all(|&line| starts_with_token(line)) {
                for &line in &with_text {
                    let start = buffer.line_to_char(line)
                        + indent::indent_of_line(buffer, line, tab_size).chars;
                    let mut end = start + token_len;
                    if buffer.char_at(end) == Some(' ') {
                        end += 1;
                    }
                    edits.push((start..end, String::new()));
                }
                continue;
            }
            let column = with_text
                .iter()
                .map(|&line| indent::indent_of_line(buffer, line, tab_size).chars)
                .min()
                .unwrap_or(usize::MAX);
            for line in lines {
                let start = buffer.line_to_char(line);
                if !indent::is_blank_line(buffer, line) {
                    edits.push((start + column..start + column, format!("{token} ")));
                } else if single {
                    let at = buffer.line_end(line);
                    edits.push((at..at, format!("{token} ")));
                } else if comment_empty_lines {
                    let at = start + column.min(buffer.line_len(line));
                    edits.push((at..at, token.to_string()));
                }
            }
        }
        let was_cursor: Vec<bool> = self.selections.iter().map(Selection::is_empty).collect();
        let summary = self.buffer.edit(&edits);
        self.map_selections(&summary);
        if advance_downwards {
            let buffer = &self.buffer;
            let last_line = buffer.len_lines() - 1;
            let mut cursors = was_cursor.into_iter();
            let mut selections = self.selections.clone();
            selections.transform(|selection| {
                let point = buffer.char_to_point(selection.head);
                if cursors.next() == Some(true) && selection.is_empty() && point.line < last_line {
                    Selection::cursor(buffer.point_to_char(Point::new(point.line + 1, point.col)))
                } else {
                    selection
                }
            });
            self.selections = selections;
        }
        summary
    }

    /// The range of line `line` without its leading and trailing whitespace. Empty (at the
    /// end of the line) for a blank line.
    fn line_text_range(&self, line: usize) -> Range<usize> {
        let buffer = &self.buffer;
        let line_start = buffer.line_to_char(line);
        let start = line_start + indent::indent_of_line(buffer, line, self.tab_size()).chars;
        let mut end = buffer.line_end(line);
        let mut chars = buffer.rope().chars_at(end);
        while end > start {
            match chars.prev() {
                Some(c) if c.is_whitespace() => end -= 1,
                _ => break,
            }
        }
        start.min(end)..end
    }

    /// The edits that toggle a block comment around `range`: remove the delimiters when
    /// the range starts and ends with them or sits right between them, add them otherwise.
    fn block_comment_edits(
        &self,
        range: Range<usize>,
        open: &str,
        close: &str,
    ) -> Vec<(Range<usize>, String)> {
        let buffer = &self.buffer;
        let (open_len, close_len) = (open.chars().count(), close.chars().count());
        let is = |range: Range<usize>, text: &str| {
            range.end <= buffer.len_chars() && buffer.slice(range) == text
        };
        let space_at = |pos: usize| buffer.char_at(pos) == Some(' ');

        // The range holds the delimiters: /* text */
        if range.len() >= open_len + close_len
            && is(range.start..range.start + open_len, open)
            && is(range.end - close_len..range.end, close)
        {
            let mut open_end = range.start + open_len;
            let mut close_start = range.end - close_len;
            if open_end < close_start && space_at(open_end) {
                open_end += 1;
            }
            if open_end < close_start && space_at(close_start - 1) {
                close_start -= 1;
            }
            return vec![
                (range.start..open_end, String::new()),
                (close_start..range.end, String::new()),
            ];
        }

        // The delimiters are just outside the range: /* [text] */
        let before = range
            .start
            .saturating_sub(usize::from(range.start > 0 && space_at(range.start - 1)));
        let after = range.end + usize::from(space_at(range.end));
        if before >= open_len
            && is(before - open_len..before, open)
            && is(after..after + close_len, close)
        {
            return vec![
                (before - open_len..range.start, String::new()),
                (range.end..after + close_len, String::new()),
            ];
        }

        vec![
            (range.start..range.start, format!("{open} ")),
            (range.end..range.end, format!(" {close}")),
        ]
    }

    /// `ToggleBlockComments`: wrap each selection in the delimiters with a space of padding
    /// inside, or remove them when they are already there (inside the selection's edges or
    /// right around it). A bare cursor toggles a comment around the text of its line, and
    /// on a blank line inserts an empty comment with the cursor inside it.
    pub(crate) fn toggle_block_comments(&mut self, start: &str, end: &str) -> EditSummary {
        let (open, close) = (start.trim(), end.trim());
        if open.is_empty() || close.is_empty() {
            return EditSummary::new();
        }
        let buffer = &self.buffer;
        let old: Vec<Selection> = self.selections.iter().copied().collect();
        let mut edits: Vec<(Range<usize>, String)> = Vec::new();
        // For a cursor on a blank line: the index of its edit, to put the cursor inside.
        let mut inside_new_comment: Vec<Option<usize>> = Vec::with_capacity(old.len());
        let mut covered_until = 0;
        for selection in &old {
            inside_new_comment.push(None);
            let range = if selection.is_empty() {
                self.line_text_range(buffer.char_to_line(selection.head))
            } else {
                selection.range()
            };
            if range.start < covered_until {
                // Two cursors on one line toggle it once.
                continue;
            }
            if range.is_empty() && selection.is_empty() {
                edits.push((selection.head..selection.head, format!("{open}  {close}")));
                if let Some(last) = inside_new_comment.last_mut() {
                    *last = Some(edits.len() - 1);
                }
                covered_until = selection.head;
                continue;
            }
            covered_until = range.end;
            edits.extend(self.block_comment_edits(range, open, close));
        }
        let open_len = open.chars().count();
        let (summary, index_of) = self.apply_unsorted(edits);
        let changes = summary.batches().first().map_or(&[][..], Vec::as_slice);
        let replacements = old
            .iter()
            .zip(&inside_new_comment)
            .map(
                |(selection, edit)| match edit.and_then(|index| changes.get(index_of[index])) {
                    Some(change) => Selection::cursor(change.new.start + open_len + 1),
                    None => selection.map_through(&summary).with_goal(None),
                },
            )
            .collect();
        self.replace_selections(replacements);
        summary
    }
}
