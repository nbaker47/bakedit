//! The text buffer: a rope with line and column conversions, atomic edits, undo history,
//! and the file it came from.
//!
//! # Lines
//!
//! A line ends at `\n` and nowhere else. The text in memory never contains `\r`: files are
//! normalised when loaded (the original ending is remembered and written back on save) and
//! so is every piece of text that is inserted. Other characters that Unicode calls line
//! breaks (form feed, U+2028, ...) are ordinary characters inside a line, which is how git,
//! compilers and Zed count lines.
//!
//! Ropey itself may be built to treat those characters as line breaks, so **always use the
//! buffer's own line methods** ([`Buffer::len_lines`], [`Buffer::line`],
//! [`Buffer::char_to_line`], [`Buffer::line_to_char`]) rather than the ones on
//! [`Buffer::rope`]. The rope's char and byte methods are fine.

use std::borrow::Cow;
use std::fmt;
use std::io::{self, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use ropey::{Rope, RopeSlice};

use crate::clock::{Clock, SystemClock};
use crate::diff;
use crate::display;
use crate::edit::{BytePoint, Change, EditSummary};
use crate::file::{self, BufferError, DiskStamp, DiskState, LineEnding, SaveOptions};
use crate::graphemes;
use crate::history::{History, StoredEdit, UndoKind};
use crate::selection::Selections;

/// A position as a 0-based line and a char column within that line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Point {
    /// 0-based line number.
    pub line: usize,
    /// Chars between the start of the line and the position.
    pub col: usize,
}

impl Point {
    /// The position `col` chars into line `line`.
    pub fn new(line: usize, col: usize) -> Point {
        Point { line, col }
    }
}

/// What an undo or a redo did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryStep {
    /// The text changes that were made to go back (or forward) one step.
    pub edits: EditSummary,
    /// The selections recorded with the step: the ones from before it for an undo, the ones
    /// from after it for a redo. `None` when the step was made without selections.
    pub selections: Option<Selections>,
}

/// One replacement ready to be applied: a clamped range and normalised text.
struct Prepared<'a> {
    range: Range<usize>,
    text: Cow<'a, str>,
}

/// The characters, other than `\n`, at which this build of ropey breaks lines. The buffer
/// tracks where they occur so it can count lines by `\n` alone.
fn extra_breaks() -> &'static [char] {
    static EXTRA: OnceLock<Vec<char>> = OnceLock::new();
    EXTRA.get_or_init(|| {
        ['\u{b}', '\u{c}', '\u{85}', '\u{2028}', '\u{2029}']
            .into_iter()
            .filter(|c| {
                let mut buf = [0u8; 4];
                Rope::from_str(c.encode_utf8(&mut buf)).len_lines() > 1
            })
            .collect()
    })
}

/// Byte offsets in `text` of every character from [`extra_breaks`], in order.
fn extra_break_bytes(text: &str) -> Vec<usize> {
    let extra = extra_breaks();
    if extra.is_empty() {
        return Vec::new();
    }
    let bytes = text.as_bytes();
    let mut out: Vec<usize> = memchr::memchr2_iter(0x0b, 0x0c, bytes)
        .filter(|&at| extra.contains(&char::from(bytes[at])))
        .collect();
    let single_byte_hits = out.len();
    for at in memchr::memchr2_iter(0xc2, 0xe2, bytes) {
        let c = match (bytes[at], bytes.get(at + 1), bytes.get(at + 2)) {
            (0xc2, Some(0x85), _) => '\u{85}',
            (0xe2, Some(0x80), Some(0xa8)) => '\u{2028}',
            (0xe2, Some(0x80), Some(0xa9)) => '\u{2029}',
            _ => continue,
        };
        if extra.contains(&c) {
            out.push(at);
        }
    }
    if single_byte_hits > 0 && out.len() > single_byte_hits {
        out.sort_unstable();
    }
    out
}

/// A text buffer.
///
/// Positions are char offsets unless a name says otherwise. No method panics on a position
/// or range that is out of bounds: it is clamped to the text.
pub struct Buffer {
    rope: Rope,
    /// Char offsets, sorted, of the characters ropey counts as line breaks that are not
    /// `\n` (see [`extra_breaks`]). Usually empty.
    soft_breaks: Vec<usize>,
    version: u64,
    path: Option<PathBuf>,
    line_ending: LineEnding,
    has_bom: bool,
    lossy: bool,
    /// The line ending or byte order mark was changed since the last load or save.
    format_changed: bool,
    disk: Option<DiskStamp>,
    history: History,
    /// [`History::state_id`] at the last load or save.
    saved_state: u64,
    clock: Arc<dyn Clock>,
}

impl fmt::Debug for Buffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Buffer")
            .field("path", &self.path)
            .field("len_chars", &self.len_chars())
            .field("len_lines", &self.len_lines())
            .field("version", &self.version)
            .field("dirty", &self.is_dirty())
            .field("line_ending", &self.line_ending)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for Buffer {
    /// Writes the whole text, with `\n` line endings.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for chunk in self.rope.chunks() {
            f.write_str(chunk)?;
        }
        Ok(())
    }
}

impl Default for Buffer {
    fn default() -> Buffer {
        Buffer::new()
    }
}

impl From<&str> for Buffer {
    fn from(text: &str) -> Buffer {
        Buffer::from_str(text)
    }
}

impl Buffer {
    /// An empty buffer with no file.
    pub fn new() -> Buffer {
        Buffer::from_normalized("")
    }

    /// A buffer holding `text`, with no file. `\r\n` and lone `\r` become `\n`, and the
    /// buffer remembers CRLF as its line ending when most lines used it.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(text: &str) -> Buffer {
        let line_ending = file::detect_line_ending(text);
        let mut buffer = Buffer::from_normalized(&file::normalize_newlines(text));
        buffer.line_ending = line_ending;
        buffer
    }

    /// Load the file at `path`.
    ///
    /// The file is read in one go and never scanned character by character, so large files
    /// load quickly. CRLF line endings are remembered and written back on save; a UTF-8
    /// byte order mark likewise. Invalid UTF-8 is replaced by U+FFFD and flagged
    /// ([`Buffer::had_invalid_utf8`]); saving such a buffer would overwrite the original
    /// bytes, so the caller should warn first. A file with a NUL byte in its first 8 KiB is
    /// refused with [`BufferError::Binary`].
    pub fn from_file(path: impl AsRef<Path>) -> Result<Buffer, BufferError> {
        let path = path.as_ref();
        let decoded = file::read(path)?;
        let mut buffer = Buffer::from_normalized(&decoded.text);
        buffer.path = Some(path.to_path_buf());
        buffer.line_ending = decoded.line_ending;
        buffer.has_bom = decoded.has_bom;
        buffer.lossy = decoded.lossy;
        buffer.disk = decoded.stamp;
        Ok(buffer)
    }

    /// Build a buffer from text that has no `\r` in it.
    fn from_normalized(text: &str) -> Buffer {
        let rope = Rope::from_str(text);
        let soft_breaks = extra_break_bytes(text)
            .into_iter()
            .map(|byte| rope.byte_to_char(byte))
            .collect();
        Buffer {
            rope,
            soft_breaks,
            version: 0,
            path: None,
            line_ending: LineEnding::Lf,
            has_bom: false,
            lossy: false,
            format_changed: false,
            disk: None,
            history: History::default(),
            saved_state: 0,
            clock: Arc::new(SystemClock::new()),
        }
    }

    // ----- text access ---------------------------------------------------------------

    /// The underlying rope. Use it for char and byte access; for anything about lines use
    /// the buffer's methods (see the module documentation).
    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    /// Length of the text in chars.
    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    /// Length of the text in bytes (UTF-8, `\n` line endings).
    pub fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }

    /// Number of lines: one more than the number of `\n`. An empty buffer has one (empty)
    /// line, and text ending in `\n` has an empty last line.
    pub fn len_lines(&self) -> usize {
        self.rope
            .len_lines()
            .saturating_sub(self.soft_breaks.len())
            .max(1)
    }

    /// True when the buffer holds no text.
    pub fn is_empty(&self) -> bool {
        self.rope.len_chars() == 0
    }

    /// The text of line `line` without its line break. A line past the end is empty.
    pub fn line(&self, line: usize) -> RopeSlice<'_> {
        self.rope
            .slice(self.line_to_char(line)..self.line_end(line))
    }

    /// The text of line `line` without its line break, as a string. Borrowed when the line
    /// lies in one piece of the rope, copied otherwise.
    pub fn line_text(&self, line: usize) -> Cow<'_, str> {
        self.line(line).into()
    }

    /// Number of chars in line `line`, not counting its line break.
    pub fn line_len(&self, line: usize) -> usize {
        self.line_end(line) - self.line_to_char(line)
    }

    /// The chars in `range`, which is clamped to the text.
    pub fn slice(&self, range: Range<usize>) -> RopeSlice<'_> {
        let end = range.end.min(self.rope.len_chars());
        let start = range.start.min(end);
        self.rope.slice(start..end)
    }

    /// The char at `char_idx`, or `None` at or past the end.
    pub fn char_at(&self, char_idx: usize) -> Option<char> {
        self.rope.get_char(char_idx)
    }

    // ----- conversions ---------------------------------------------------------------

    /// How many tracked non-`\n` line break characters lie before `char_idx`.
    fn soft_breaks_before(&self, char_idx: usize) -> usize {
        self.soft_breaks.partition_point(|&at| at < char_idx)
    }

    /// How many tracked non-`\n` line break characters lie in lines before `line`.
    fn soft_breaks_before_line(&self, line: usize) -> usize {
        let (mut low, mut high) = (0, self.soft_breaks.len());
        while low < high {
            let middle = low + (high - low) / 2;
            // The break at index `middle` has `middle` such breaks before it, so its line
            // is its ropey line minus `middle`.
            let line_of_break = self
                .rope
                .char_to_line(self.soft_breaks[middle])
                .saturating_sub(middle);
            if line_of_break < line {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        low
    }

    /// The line containing `char_idx`. The position just after a `\n` belongs to the next
    /// line; a position past the end is on the last line.
    pub fn char_to_line(&self, char_idx: usize) -> usize {
        let char_idx = char_idx.min(self.rope.len_chars());
        self.rope
            .char_to_line(char_idx)
            .saturating_sub(self.soft_breaks_before(char_idx))
    }

    /// The char offset where line `line` starts. A line past the end starts at the end of
    /// the text.
    pub fn line_to_char(&self, line: usize) -> usize {
        if line == 0 {
            return 0;
        }
        if line >= self.len_lines() {
            return self.rope.len_chars();
        }
        let raw_line = if self.soft_breaks.is_empty() {
            line
        } else {
            line + self.soft_breaks_before_line(line)
        };
        self.rope.line_to_char(raw_line.min(self.rope.len_lines()))
    }

    /// The char offset where the text of line `line` ends: before its `\n`, or the end of
    /// the text for the last line.
    pub fn line_end(&self, line: usize) -> usize {
        if line + 1 >= self.len_lines() {
            self.rope.len_chars()
        } else {
            self.line_to_char(line + 1).saturating_sub(1)
        }
    }

    /// The line and char column of `char_idx` (clamped to the text).
    pub fn char_to_point(&self, char_idx: usize) -> Point {
        let char_idx = char_idx.min(self.rope.len_chars());
        let line = self.char_to_line(char_idx);
        Point {
            line,
            col: char_idx - self.line_to_char(line),
        }
    }

    /// The char offset of a line and column. A line past the end means the last line, and a
    /// column past the end of the line means the end of the line.
    pub fn point_to_char(&self, point: Point) -> usize {
        let line = point.line.min(self.len_lines() - 1);
        let start = self.line_to_char(line);
        start.saturating_add(point.col).min(self.line_end(line))
    }

    /// The byte offset of `char_idx` (clamped to the text).
    pub fn char_to_byte(&self, char_idx: usize) -> usize {
        self.rope.char_to_byte(char_idx.min(self.rope.len_chars()))
    }

    /// The char at byte offset `byte_idx` (clamped to the text). A byte in the middle of a
    /// multi-byte char maps to that char.
    pub fn byte_to_char(&self, byte_idx: usize) -> usize {
        self.rope.byte_to_char(byte_idx.min(self.rope.len_bytes()))
    }

    /// The byte offset where line `line` starts.
    pub fn line_to_byte(&self, line: usize) -> usize {
        self.rope.char_to_byte(self.line_to_char(line))
    }

    /// The line and byte column of `char_idx`: the row/column pair tree-sitter uses.
    pub fn char_to_byte_point(&self, char_idx: usize) -> BytePoint {
        let char_idx = char_idx.min(self.rope.len_chars());
        let line = self.char_to_line(char_idx);
        let line_start_byte = self.rope.char_to_byte(self.line_to_char(line));
        BytePoint {
            line,
            byte_col: self.rope.char_to_byte(char_idx) - line_start_byte,
        }
    }

    /// The display column of `char_idx` in its line when the line is not wrapped: tabs run
    /// to the next multiple of `tab_size` and wide characters take two cells (see
    /// [`crate::display`]).
    pub fn display_col(&self, char_idx: usize, tab_size: usize) -> usize {
        let point = self.char_to_point(char_idx);
        display::char_col_to_display_col(&self.line_text(point.line), point.col, tab_size)
    }

    /// The char offset in line `line` drawn at `display_col` when the line is not wrapped.
    /// A column inside a tab or a wide character maps to the start of that character; a
    /// column past the end of the line maps to the end of the line.
    pub fn char_at_display_col(&self, line: usize, display_col: usize, tab_size: usize) -> usize {
        let line = line.min(self.len_lines() - 1);
        self.line_to_char(line)
            + display::display_col_to_char_col(&self.line_text(line), display_col, tab_size)
    }

    /// The closest grapheme cluster boundary before `char_idx`, or 0.
    pub fn prev_grapheme(&self, char_idx: usize) -> usize {
        graphemes::prev_boundary(&self.rope, char_idx)
    }

    /// The closest grapheme cluster boundary after `char_idx`, or the end of the text.
    pub fn next_grapheme(&self, char_idx: usize) -> usize {
        graphemes::next_boundary(&self.rope, char_idx)
    }

    /// Whether a cursor may stand at `char_idx`: it is not inside a grapheme cluster.
    pub fn is_grapheme_boundary(&self, char_idx: usize) -> bool {
        graphemes::is_boundary(&self.rope, char_idx)
    }

    /// `char_idx` moved back to the start of its grapheme cluster if it is inside one, and
    /// clamped to the text.
    pub fn snap_to_grapheme(&self, char_idx: usize) -> usize {
        graphemes::floor_boundary(&self.rope, char_idx)
    }

    // ----- state ---------------------------------------------------------------------

    /// A counter that goes up every time the text changes, including by undo and redo.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Whether the text (or its line ending or byte order mark) differs from what was last
    /// loaded or saved. Undoing back to the saved state makes the buffer clean again.
    pub fn is_dirty(&self) -> bool {
        self.format_changed || self.saved_state != self.history.state_id()
    }

    /// The file this buffer is loaded from or saved to.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Point the buffer at a different file without reading or writing anything. The
    /// buffer forgets what it knew about the old file's state on disk.
    pub fn set_path(&mut self, path: Option<PathBuf>) {
        if self.path != path {
            self.path = path;
            self.disk = None;
        }
    }

    /// The line ending written on save.
    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    /// Change the line ending written on save. The buffer becomes dirty when it changes.
    pub fn set_line_ending(&mut self, line_ending: LineEnding) {
        if self.line_ending != line_ending {
            self.line_ending = line_ending;
            self.format_changed = true;
        }
    }

    /// Whether the file starts with a UTF-8 byte order mark (kept out of the text).
    pub fn has_bom(&self) -> bool {
        self.has_bom
    }

    /// Choose whether a UTF-8 byte order mark is written on save. The buffer becomes dirty
    /// when this changes.
    pub fn set_bom(&mut self, has_bom: bool) {
        if self.has_bom != has_bom {
            self.has_bom = has_bom;
            self.format_changed = true;
        }
    }

    /// True when the file was not valid UTF-8 and the bad bytes were replaced by U+FFFD.
    /// Saving then loses the original bytes. Cleared by a save or a clean reload.
    pub fn had_invalid_utf8(&self) -> bool {
        self.lossy
    }

    /// Replace the time source used to group typed characters into undo steps.
    pub fn set_clock(&mut self, clock: Arc<dyn Clock>) {
        self.clock = clock;
    }

    // ----- editing -------------------------------------------------------------------

    /// Clamp, normalise and sort a list of edits so it can be applied safely.
    fn prepare<'a, S: AsRef<str>>(&self, edits: &'a [(Range<usize>, S)]) -> Vec<Prepared<'a>> {
        let len = self.rope.len_chars();
        let mut prepared: Vec<Prepared<'a>> = edits
            .iter()
            .map(|(range, text)| {
                let (low, high) = (range.start.min(range.end), range.start.max(range.end));
                let end = high.min(len);
                Prepared {
                    range: low.min(end)..end,
                    text: file::normalize_newlines(text.as_ref()),
                }
            })
            .collect();
        prepared.sort_by_key(|edit| edit.range.start);
        let mut previous_end = 0;
        for edit in &mut prepared {
            if edit.range.start < previous_end {
                edit.range.start = previous_end;
                edit.range.end = edit.range.end.max(previous_end);
            }
            previous_end = edit.range.end;
        }
        prepared
    }

    /// Apply sorted, non-overlapping, in-bounds edits to the rope. This is the one place
    /// the text changes. Returns what changed, and the removed text of each edit when
    /// `keep_removed` is set.
    fn apply_batch(
        &mut self,
        edits: &[Prepared<'_>],
        keep_removed: bool,
    ) -> (Vec<Change>, Vec<String>) {
        let mut changes: Vec<Change> = Vec::with_capacity(edits.len());
        let mut removed: Vec<String> = Vec::new();
        let mut inserted_lens: Vec<usize> = Vec::with_capacity(edits.len());
        let mut any_change = false;
        for edit in edits {
            let inserted_len = edit.text.chars().count();
            any_change |= !edit.range.is_empty() || inserted_len > 0;
            inserted_lens.push(inserted_len);
            changes.push(Change {
                old: edit.range.clone(),
                old_bytes: self.rope.char_to_byte(edit.range.start)
                    ..self.rope.char_to_byte(edit.range.end),
                old_start_point: self.char_to_byte_point(edit.range.start),
                old_end_point: self.char_to_byte_point(edit.range.end),
                ..Change::default()
            });
            if keep_removed {
                removed.push(self.rope.slice(edit.range.clone()).to_string());
            }
        }

        if any_change {
            for edit in edits.iter().rev() {
                if !edit.range.is_empty() {
                    self.rope.remove(edit.range.clone());
                }
                if !edit.text.is_empty() {
                    self.rope.insert(edit.range.start, &edit.text);
                }
            }
            self.update_soft_breaks(edits, &inserted_lens);
            self.version += 1;
        }

        let mut delta: isize = 0;
        for (change, inserted_len) in changes.iter_mut().zip(&inserted_lens) {
            let new_start = (change.old.start as isize + delta) as usize;
            change.new = new_start..new_start + inserted_len;
            delta += *inserted_len as isize - change.old.len() as isize;
        }
        for change in &mut changes {
            change.new_bytes =
                self.rope.char_to_byte(change.new.start)..self.rope.char_to_byte(change.new.end);
            change.new_start_point = self.char_to_byte_point(change.new.start);
            change.new_end_point = self.char_to_byte_point(change.new.end);
        }
        (changes, removed)
    }

    /// Bring `soft_breaks` up to date after `edits` were applied to the rope.
    fn update_soft_breaks(&mut self, edits: &[Prepared<'_>], inserted_lens: &[usize]) {
        if extra_breaks().is_empty() {
            return;
        }
        let old = std::mem::take(&mut self.soft_breaks);
        let mut updated = Vec::with_capacity(old.len());
        let mut next = 0;
        let mut delta: isize = 0;
        let shifted = |at: usize, delta: isize| (at as isize + delta) as usize;
        for (edit, inserted_len) in edits.iter().zip(inserted_lens) {
            while next < old.len() && old[next] < edit.range.start {
                updated.push(shifted(old[next], delta));
                next += 1;
            }
            while next < old.len() && old[next] < edit.range.end {
                next += 1;
            }
            let new_start = shifted(edit.range.start, delta);
            for byte in extra_break_bytes(&edit.text) {
                updated.push(new_start + edit.text[..byte].chars().count());
            }
            delta += *inserted_len as isize - edit.range.len() as isize;
        }
        updated.extend(old[next..].iter().map(|&at| shifted(at, delta)));
        self.soft_breaks = updated;
    }

    /// Replace several ranges at once.
    ///
    /// Each pair is a char range of the current text and its replacement; the ranges may
    /// come in any order and must not overlap. They all refer to the text as it is before
    /// the call, and they are applied together as one batch.
    ///
    /// Bad input never panics: ranges are clamped to the text, a reversed range is turned
    /// around, and a range that overlaps the one before it (by start position) is cut back
    /// to start where that one ends. `\r\n` and lone `\r` in the replacement text become
    /// `\n`.
    ///
    /// The returned summary has one batch with one [`Change`] per given edit, sorted by
    /// position (edits that start at the same place keep their given order), or no batch at
    /// all when `edits` is empty. Outside an open transaction the call is one undo step of
    /// its own; when nothing actually changes, nothing is recorded.
    pub fn edit<S: AsRef<str>>(&mut self, edits: &[(Range<usize>, S)]) -> EditSummary {
        if edits.is_empty() {
            return EditSummary::new();
        }
        let prepared = self.prepare(edits);
        let (changes, removed) = self.apply_batch(&prepared, true);
        let stored: Vec<StoredEdit> = changes
            .iter()
            .zip(removed)
            .zip(&prepared)
            .filter(|((change, _), _)| !change.is_noop())
            .map(|((change, removed), edit)| StoredEdit {
                old: change.old.clone(),
                new: change.new.clone(),
                removed,
                inserted: edit.text.to_string(),
            })
            .collect();
        if !stored.is_empty() {
            let now = self.clock.now_ms();
            let implicit = !self.history.is_open();
            if implicit {
                self.history.begin(UndoKind::Other, None, now);
            }
            self.history.record(stored);
            if implicit {
                self.history.end(None, now);
            }
        }
        EditSummary::from_batch(changes)
    }

    /// Start an undo step that lasts until the matching [`Buffer::end_transaction`]; every
    /// edit in between belongs to it. Calls nest: only the outermost pair decides the kind
    /// and the selections.
    ///
    /// `selections` are the selections before the step; undoing the step hands them back.
    /// A step of a mergeable `kind` (typing, backspacing, deleting forward) fuses with the
    /// previous step when that one has the same kind, was last edited at most 300 ms ago,
    /// has not been cut off by [`Buffer::break_undo_group`], and left the selections
    /// exactly where `selections` are now.
    pub fn begin_transaction(&mut self, kind: UndoKind, selections: Option<&Selections>) {
        let now = self.clock.now_ms();
        self.history.begin(kind, selections, now);
    }

    /// End the innermost open undo step. `selections` are the selections after the step;
    /// redoing it hands them back. Returns true when this closed the outermost step and it
    /// changed the text. Without an open step this does nothing and returns false.
    pub fn end_transaction(&mut self, selections: Option<&Selections>) -> bool {
        let now = self.clock.now_ms();
        self.history.end(selections, now)
    }

    /// Make the next edit start a new undo step even if it would otherwise merge with the
    /// previous one.
    pub fn break_undo_group(&mut self) {
        self.history.seal();
    }

    /// Whether there is a step to undo.
    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    /// Whether there is an undone step to redo.
    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    /// Undo the newest step. Returns what changed and the selections from before the step,
    /// or `None` when there is nothing to undo (or a transaction is open).
    pub fn undo(&mut self) -> Option<HistoryStep> {
        let transaction = self.history.pop_undo()?;
        let mut edits = EditSummary::new();
        for batch in transaction.batches.iter().rev() {
            let prepared: Vec<Prepared<'_>> = batch
                .iter()
                .map(|edit| Prepared {
                    range: edit.new.clone(),
                    text: Cow::Borrowed(edit.removed.as_str()),
                })
                .collect();
            edits.push_batch(self.apply_batch(&prepared, false).0);
        }
        let selections = transaction.selections_before.clone();
        self.history.push_redo(transaction);
        Some(HistoryStep { edits, selections })
    }

    /// Redo the step that was undone last. Returns what changed and the selections from
    /// after the step, or `None` when there is nothing to redo (or a transaction is open).
    pub fn redo(&mut self) -> Option<HistoryStep> {
        let transaction = self.history.pop_redo()?;
        let mut edits = EditSummary::new();
        for batch in &transaction.batches {
            let prepared: Vec<Prepared<'_>> = batch
                .iter()
                .map(|edit| Prepared {
                    range: edit.old.clone(),
                    text: Cow::Borrowed(edit.inserted.as_str()),
                })
                .collect();
            edits.push_batch(self.apply_batch(&prepared, false).0);
        }
        let selections = transaction.selections_after.clone();
        self.history.push_undo(transaction);
        Some(HistoryStep { edits, selections })
    }

    // ----- files ---------------------------------------------------------------------

    /// How the file on disk compares with what this buffer last loaded or saved, judged by
    /// modification time and size.
    ///
    /// A buffer without a path is `Unchanged`. A buffer that has a path but never read or
    /// wrote it is `Unchanged` while the file does not exist and `Modified` once it does.
    /// When the file cannot be examined for another reason the answer is `Unchanged`.
    pub fn disk_state(&self) -> DiskState {
        let Some(path) = &self.path else {
            return DiskState::Unchanged;
        };
        match (file::stamp(path), self.disk) {
            (Ok(Some(current)), Some(known)) if current == known => DiskState::Unchanged,
            (Ok(Some(_)), _) => DiskState::Modified,
            (Ok(None), Some(_)) => DiskState::Deleted,
            (Ok(None), None) | (Err(_), _) => DiskState::Unchanged,
        }
    }

    /// The edit that makes the text end in exactly one line break, if one is needed.
    fn final_newline_edit(&self) -> Option<(Range<usize>, &'static str)> {
        let len = self.rope.len_chars();
        if len == 0 {
            return None;
        }
        let mut keep = len;
        let mut chars = self.rope.chars_at(len);
        while let Some(c) = chars.prev() {
            if !c.is_ascii_whitespace() {
                break;
            }
            keep -= 1;
        }
        if keep + 1 == len && self.rope.get_char(keep) == Some('\n') {
            return None;
        }
        Some((keep..len, "\n"))
    }

    /// Write the text as file bytes: byte order mark, then the text with the buffer's line
    /// ending.
    fn write_contents(&self, writer: &mut dyn Write) -> io::Result<()> {
        file::write_text(writer, self.rope.chunks(), self.line_ending, self.has_bom)
    }

    /// Remember that the text now matches the file on disk.
    fn mark_saved(&mut self, stamp: Option<DiskStamp>) {
        self.disk = stamp;
        self.history.seal();
        self.saved_state = self.history.state_id();
        self.format_changed = false;
    }

    /// Save to the buffer's path. See [`Buffer::save`]; this variant also carries
    /// selections through the final-newline edit so undo can restore them.
    pub(crate) fn save_with_selections(
        &mut self,
        options: SaveOptions,
        selections: Option<&Selections>,
    ) -> Result<(EditSummary, Option<Selections>), BufferError> {
        let path = self.path.clone().ok_or(BufferError::NoPath)?;
        let mut summary = EditSummary::new();
        let mut after = selections.cloned();
        if options.ensure_final_newline {
            if let Some(edit) = self.final_newline_edit() {
                self.begin_transaction(UndoKind::Other, selections);
                summary = self.edit(&[edit]);
                if let Some(after) = &mut after {
                    after.map_through(&summary);
                }
                self.end_transaction(after.as_ref());
            }
        }
        file::write_atomic(&path, &mut |writer| self.write_contents(writer)).map_err(|source| {
            BufferError::Io {
                path: path.clone(),
                source,
            }
        })?;
        let stamp = file::stamp(&path).ok().flatten();
        self.mark_saved(stamp);
        self.lossy = false;
        Ok((summary, after))
    }

    /// Write the buffer to its path.
    ///
    /// The file is written atomically: the bytes go to a temporary file in the same
    /// directory, which then replaces the target by a rename, so a crash never leaves a
    /// half-written file. The target's permissions are kept, and a symlink is followed (the
    /// file it points to is replaced, the link stays). CRLF line endings and a byte order
    /// mark are restored if the file had them. A file the user may not write is refused.
    /// If the directory does not allow the temporary file, or a rename would change the
    /// file's owner or break hard links, the file is overwritten in place instead.
    ///
    /// With `ensure_final_newline` the text is first edited to end in exactly one `\n`
    /// (see [`SaveOptions`]); that edit is an undo step and is described by the returned
    /// summary, which is empty otherwise. Afterwards the buffer is clean.
    ///
    /// Fails with [`BufferError::NoPath`] when the buffer has no path. If writing fails,
    /// the final-newline edit (if any) stays in the buffer and the buffer stays dirty.
    pub fn save(&mut self, options: SaveOptions) -> Result<EditSummary, BufferError> {
        self.save_with_selections(options, None)
            .map(|(summary, _)| summary)
    }

    /// Write the buffer to `path` and make that its path. See [`Buffer::save`]. When the
    /// write fails the buffer keeps its old path.
    pub fn save_as(
        &mut self,
        path: impl Into<PathBuf>,
        options: SaveOptions,
    ) -> Result<EditSummary, BufferError> {
        self.save_as_with_selections(path.into(), options, None)
            .map(|(summary, _)| summary)
    }

    /// See [`Buffer::save_as`] and [`Buffer::save_with_selections`].
    pub(crate) fn save_as_with_selections(
        &mut self,
        path: PathBuf,
        options: SaveOptions,
        selections: Option<&Selections>,
    ) -> Result<(EditSummary, Option<Selections>), BufferError> {
        let old_path = self.path.replace(path);
        let old_disk = self.disk.take();
        let result = self.save_with_selections(options, selections);
        if result.is_err() {
            self.path = old_path;
            self.disk = old_disk;
        }
        result
    }

    /// Reload from disk. See [`Buffer::reload`]; this variant also moves `selections`
    /// through the edits and records them with the undo step.
    pub(crate) fn reload_with_selections(
        &mut self,
        selections: Option<&Selections>,
    ) -> Result<(EditSummary, Option<Selections>), BufferError> {
        let path = self.path.clone().ok_or(BufferError::NoPath)?;
        let decoded = file::read(&path)?;
        let edits = diff::diff_edits(&self.rope, &decoded.text);
        let mut after = selections.cloned();
        self.begin_transaction(UndoKind::Other, selections);
        let summary = self.edit(&edits);
        if let Some(after) = &mut after {
            after.map_through(&summary);
        }
        self.end_transaction(after.as_ref());
        self.line_ending = decoded.line_ending;
        self.has_bom = decoded.has_bom;
        self.lossy = decoded.lossy;
        self.mark_saved(decoded.stamp);
        Ok((summary, after))
    }

    /// Replace the text with what is on disk, changing as little as possible.
    ///
    /// The file is read and compared with the buffer line by line, and only the parts that
    /// differ are edited, so positions in unchanged text stay meaningful: map cursors and
    /// other positions through the returned summary. The change is one undo step (undoing
    /// it brings back the text from before the reload, and the buffer is then dirty), and
    /// the buffer is clean afterwards. The line ending and byte order mark are taken from
    /// the file again.
    ///
    /// On an error (no path, unreadable file, binary file) the buffer is left untouched.
    pub fn reload(&mut self) -> Result<EditSummary, BufferError> {
        self.reload_with_selections(None)
            .map(|(summary, _)| summary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ManualClock;
    use crate::edit::Bias;
    use crate::selection::Selection;

    fn text(buffer: &Buffer) -> String {
        buffer.to_string()
    }

    #[test]
    fn buffers_are_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Buffer>();
    }

    #[test]
    fn empty_buffer() {
        let buffer = Buffer::new();
        assert!(buffer.is_empty());
        assert_eq!(buffer.len_chars(), 0);
        assert_eq!(buffer.len_bytes(), 0);
        assert_eq!(buffer.len_lines(), 1);
        assert_eq!(buffer.line(0), "");
        assert_eq!(buffer.line(5), "");
        assert_eq!(buffer.line_to_char(3), 0);
        assert_eq!(buffer.line_end(0), 0);
        assert_eq!(buffer.char_to_line(10), 0);
        assert_eq!(buffer.char_to_point(10), Point::new(0, 0));
        assert_eq!(buffer.point_to_char(Point::new(4, 4)), 0);
        assert_eq!(buffer.char_at(0), None);
        assert_eq!(buffer.slice(3..9), "");
        assert_eq!(text(&buffer), "");
        assert!(!buffer.is_dirty());
        assert_eq!(buffer.version(), 0);
        assert_eq!(buffer.path(), None);
        assert_eq!(buffer.line_ending(), LineEnding::Lf);
        assert_eq!(Buffer::default().len_lines(), 1);
    }

    #[test]
    fn lines_of_plain_text() {
        let buffer = Buffer::from_str("one\ntwo\n\nfour");
        assert_eq!(buffer.len_lines(), 4);
        assert_eq!(buffer.line(0), "one");
        assert_eq!(buffer.line(1), "two");
        assert_eq!(buffer.line(2), "");
        assert_eq!(buffer.line(3), "four");
        assert_eq!(buffer.line_text(1), "two");
        assert_eq!(buffer.line_len(3), 4);
        assert_eq!(buffer.line_to_char(1), 4);
        assert_eq!(buffer.line_to_char(2), 8);
        assert_eq!(buffer.line_to_char(3), 9);
        assert_eq!(buffer.line_to_char(4), 13);
        assert_eq!(buffer.line_end(0), 3);
        assert_eq!(buffer.line_end(2), 8);
        assert_eq!(buffer.line_end(3), 13);
        assert_eq!(buffer.line_end(9), 13);
        assert_eq!(buffer.char_to_line(3), 0);
        assert_eq!(buffer.char_to_line(4), 1);
        assert_eq!(buffer.char_to_line(8), 2);
        assert_eq!(buffer.char_to_line(13), 3);
    }

    #[test]
    fn a_trailing_newline_makes_an_empty_last_line() {
        let buffer = Buffer::from_str("a\nb\n");
        assert_eq!(buffer.len_lines(), 3);
        assert_eq!(buffer.line(2), "");
        assert_eq!(buffer.line_to_char(2), 4);
        assert_eq!(buffer.char_to_line(4), 2);
        assert_eq!(buffer.line_end(1), 3);
    }

    #[test]
    fn points_and_bytes() {
        let buffer = Buffer::from_str("héllo\n世界 ok\nlast");
        assert_eq!(buffer.char_to_point(0), Point::new(0, 0));
        assert_eq!(buffer.char_to_point(5), Point::new(0, 5));
        assert_eq!(buffer.char_to_point(6), Point::new(1, 0));
        assert_eq!(buffer.char_to_point(8), Point::new(1, 2));
        assert_eq!(buffer.char_to_point(999), Point::new(2, 4));
        assert_eq!(buffer.point_to_char(Point::new(1, 2)), 8);
        assert_eq!(buffer.point_to_char(Point::new(1, 99)), 11);
        assert_eq!(buffer.point_to_char(Point::new(99, 1)), 13);
        assert_eq!(buffer.point_to_char(Point::new(99, 99)), 16);

        assert_eq!(buffer.char_to_byte(2), 3);
        assert_eq!(buffer.char_to_byte(6), 7);
        assert_eq!(buffer.char_to_byte(7), 10);
        assert_eq!(buffer.char_to_byte(999), buffer.len_bytes());
        assert_eq!(buffer.byte_to_char(3), 2);
        assert_eq!(buffer.byte_to_char(2), 1, "a byte inside é maps to é");
        assert_eq!(buffer.byte_to_char(999), buffer.len_chars());
        assert_eq!(buffer.line_to_byte(1), 7);
        assert_eq!(
            buffer.char_to_byte_point(8),
            BytePoint {
                line: 1,
                byte_col: 6
            }
        );
        assert_eq!(
            buffer.char_to_byte_point(0),
            BytePoint {
                line: 0,
                byte_col: 0
            }
        );
    }

    #[test]
    fn display_columns() {
        let buffer = Buffer::from_str("a\tb\n世界x\ne\u{301}!");
        assert_eq!(buffer.display_col(0, 4), 0);
        assert_eq!(buffer.display_col(2, 4), 4);
        assert_eq!(buffer.display_col(2, 8), 8);
        assert_eq!(buffer.display_col(3, 4), 5);
        assert_eq!(buffer.display_col(6, 4), 4, "after two wide chars");
        assert_eq!(buffer.char_at_display_col(0, 3, 4), 1, "inside the tab");
        assert_eq!(buffer.char_at_display_col(0, 4, 4), 2);
        assert_eq!(buffer.char_at_display_col(1, 3, 4), 5, "second half of 界");
        assert_eq!(buffer.char_at_display_col(1, 99, 4), 7);
        assert_eq!(buffer.char_at_display_col(2, 1, 4), 10);
        assert_eq!(buffer.char_at_display_col(99, 0, 4), 8);
    }

    #[test]
    fn grapheme_helpers() {
        let buffer = Buffer::from_str("ae\u{301}🇨🇦z");
        assert_eq!(buffer.next_grapheme(0), 1);
        assert_eq!(buffer.next_grapheme(1), 3);
        assert_eq!(buffer.next_grapheme(3), 5);
        assert_eq!(buffer.prev_grapheme(5), 3);
        assert_eq!(buffer.prev_grapheme(3), 1);
        assert!(buffer.is_grapheme_boundary(3));
        assert!(!buffer.is_grapheme_boundary(2));
        assert!(!buffer.is_grapheme_boundary(4));
        assert_eq!(buffer.snap_to_grapheme(4), 3);
        assert_eq!(buffer.snap_to_grapheme(2), 1);
        assert_eq!(buffer.snap_to_grapheme(99), 6);
    }

    #[test]
    fn carriage_returns_never_enter_the_text() {
        let buffer = Buffer::from_str("a\r\nb\r\nc");
        assert_eq!(text(&buffer), "a\nb\nc");
        assert_eq!(buffer.line_ending(), LineEnding::CrLf);
        assert_eq!(buffer.len_lines(), 3);
        assert!(!buffer.is_dirty());

        let mut buffer = Buffer::from_str("x");
        let summary = buffer.edit(&[(1..1, "\r\nline\rtwo\r\n")]);
        assert_eq!(text(&buffer), "x\nline\ntwo\n");
        assert_eq!(
            summary.batches()[0][0].new,
            1..11,
            "lengths are of the normalised text"
        );
        assert_eq!(
            buffer.line_ending(),
            LineEnding::Lf,
            "edits do not change the line ending"
        );
        assert_eq!(Buffer::from("p\rq").to_string(), "p\nq");
    }

    #[test]
    fn single_edits() {
        let mut buffer = Buffer::from_str("hello world");
        let summary = buffer.edit(&[(5..5, ",")]);
        assert_eq!(text(&buffer), "hello, world");
        assert_eq!(summary.batches().len(), 1);
        assert_eq!(summary.batches()[0][0].old, 5..5);
        assert_eq!(summary.batches()[0][0].new, 5..6);
        assert_eq!(buffer.version(), 1);
        assert!(buffer.is_dirty());

        buffer.edit(&[(7..12, "there")]);
        assert_eq!(text(&buffer), "hello, there");
        buffer.edit(&[(0..7, "")]);
        assert_eq!(text(&buffer), "there");
        assert_eq!(buffer.version(), 3);
    }

    #[test]
    fn several_edits_use_pre_edit_coordinates_in_any_order() {
        let mut buffer = Buffer::from_str("0123456789");
        let summary = buffer.edit(&[(8..10, "X"), (0..1, "abc"), (4..4, "--"), (5..7, "")]);
        assert_eq!(text(&buffer), "abc123--47X");
        let changes = &summary.batches()[0];
        assert_eq!(changes.len(), 4);
        assert_eq!(
            (changes[0].old.clone(), changes[0].new.clone()),
            (0..1, 0..3)
        );
        assert_eq!(
            (changes[1].old.clone(), changes[1].new.clone()),
            (4..4, 6..8)
        );
        assert_eq!(
            (changes[2].old.clone(), changes[2].new.clone()),
            (5..7, 9..9)
        );
        assert_eq!(
            (changes[3].old.clone(), changes[3].new.clone()),
            (8..10, 10..11)
        );
        assert_eq!(buffer.version(), 1, "one batch is one version step");

        assert_eq!(summary.map_offset(0, Bias::Left), 0);
        assert_eq!(summary.map_offset(1, Bias::Left), 3);
        assert_eq!(summary.map_offset(4, Bias::Left), 6);
        assert_eq!(summary.map_offset(4, Bias::Right), 8);
        assert_eq!(summary.map_offset(6, Bias::Left), 9);
        assert_eq!(summary.map_offset(7, Bias::Left), 9);
        assert_eq!(summary.map_offset(10, Bias::Left), 11);

        assert!(buffer.undo().is_some());
        assert_eq!(text(&buffer), "0123456789");
    }

    #[test]
    fn edits_at_the_same_place_keep_their_order() {
        let mut buffer = Buffer::from_str("ab");
        buffer.edit(&[(1..1, "1"), (1..1, "2"), (1..1, "3")]);
        assert_eq!(text(&buffer), "a123b");
    }

    #[test]
    fn bad_ranges_are_clamped_not_fatal() {
        let mut buffer = Buffer::from_str("abcdef");
        buffer.edit(&[(4..99, "!")]);
        assert_eq!(text(&buffer), "abcd!");
        buffer.edit(&[(50..60, "?")]);
        assert_eq!(text(&buffer), "abcd!?");
        // A reversed range is turned around.
        buffer.edit(&[(Range { start: 3, end: 1 }, "X")]);
        assert_eq!(text(&buffer), "aXd!?");
        // Overlapping edits: the later one is cut back to start where the earlier one ends.
        let mut buffer = Buffer::from_str("0123456789");
        buffer.edit(&[(2..6, "A"), (4..8, "B")]);
        assert_eq!(text(&buffer), "01AB89");
        let mut buffer = Buffer::from_str("0123456789");
        buffer.edit(&[(2..8, "A"), (4..6, "B")]);
        assert_eq!(text(&buffer), "01AB89");
    }

    #[test]
    fn empty_and_no_op_edits_record_nothing() {
        let mut buffer = Buffer::from_str("abc");
        let none: [(Range<usize>, &str); 0] = [];
        assert!(buffer.edit(&none).batches().is_empty());
        let summary = buffer.edit(&[(1..1, ""), (2..2, "")]);
        assert!(summary.is_empty());
        assert_eq!(summary.batches()[0].len(), 2, "still one change per edit");
        assert_eq!(buffer.version(), 0);
        assert!(!buffer.is_dirty());
        assert!(!buffer.can_undo());
    }

    #[test]
    fn change_records_bytes_and_points() {
        let mut buffer = Buffer::from_str("héllo\nwörld\nend");
        // Replace "llo\nwö" (chars 2..8) with "L\nL\nL".
        let summary = buffer.edit(&[(2..8, "L\nL\nL")]);
        assert_eq!(text(&buffer), "héL\nL\nLrld\nend");
        let change = &summary.batches()[0][0];
        assert_eq!(change.old, 2..8);
        assert_eq!(change.new, 2..7);
        assert_eq!(change.old_bytes, 3..10);
        assert_eq!(change.new_bytes, 3..8);
        assert_eq!(
            change.old_start_point,
            BytePoint {
                line: 0,
                byte_col: 3
            }
        );
        assert_eq!(
            change.old_end_point,
            BytePoint {
                line: 1,
                byte_col: 3
            }
        );
        assert_eq!(
            change.new_start_point,
            BytePoint {
                line: 0,
                byte_col: 3
            }
        );
        assert_eq!(
            change.new_end_point,
            BytePoint {
                line: 2,
                byte_col: 1
            }
        );
        let edits = summary.input_edits();
        assert_eq!(edits.len(), 1);
        assert_eq!(
            (
                edits[0].start_byte,
                edits[0].old_end_byte,
                edits[0].new_end_byte
            ),
            (3, 10, 8)
        );
        assert_eq!(
            edits[0].old_end_point,
            BytePoint {
                line: 1,
                byte_col: 3
            }
        );
    }

    #[test]
    fn undo_and_redo_round_trip() {
        let mut buffer = Buffer::from_str("one two");
        assert!(!buffer.can_undo() && !buffer.can_redo());
        assert!(buffer.undo().is_none() && buffer.redo().is_none());

        buffer.edit(&[(3..3, " and")]);
        buffer.edit(&[(0..3, "1")]);
        assert_eq!(text(&buffer), "1 and two");
        assert_eq!(buffer.version(), 2);

        let step = buffer.undo().unwrap();
        assert_eq!(text(&buffer), "one and two");
        assert_eq!(step.selections, None);
        assert_eq!(step.edits.map_offset(1, Bias::Left), 3);
        assert_eq!(
            buffer.version(),
            3,
            "undo changes the text, so the version moves on"
        );
        buffer.undo().unwrap();
        assert_eq!(text(&buffer), "one two");
        assert!(buffer.undo().is_none());
        assert!(!buffer.is_dirty());

        buffer.redo().unwrap();
        assert_eq!(text(&buffer), "one and two");
        buffer.redo().unwrap();
        assert_eq!(text(&buffer), "1 and two");
        assert!(buffer.redo().is_none());
        assert!(buffer.is_dirty());

        // A new edit after an undo drops the redo stack.
        buffer.undo().unwrap();
        buffer.edit(&[(0..0, ">")]);
        assert!(!buffer.can_redo());
        assert_eq!(text(&buffer), ">one and two");
    }

    #[test]
    fn transactions_group_edits_and_carry_selections() {
        let mut buffer = Buffer::from_str("abc");
        let before = Selections::single(Selection::cursor(1));
        let after = Selections::single(Selection::cursor(5));
        buffer.begin_transaction(UndoKind::Other, Some(&before));
        buffer.edit(&[(1..1, "X")]);
        buffer.edit(&[(0..0, "Y")]);
        assert!(buffer.undo().is_none(), "no undo while the step is open");
        assert!(buffer.end_transaction(Some(&after)));
        assert_eq!(text(&buffer), "YaXbc");

        let step = buffer.undo().unwrap();
        assert_eq!(text(&buffer), "abc");
        assert_eq!(step.selections, Some(before));
        assert_eq!(step.edits.batches().len(), 2);
        let step = buffer.redo().unwrap();
        assert_eq!(text(&buffer), "YaXbc");
        assert_eq!(step.selections, Some(after));

        // A transaction without edits leaves no step behind.
        buffer.begin_transaction(UndoKind::Other, None);
        assert!(!buffer.end_transaction(None));
        buffer.undo().unwrap();
        assert_eq!(text(&buffer), "abc");
        assert!(
            !buffer.end_transaction(None),
            "ending without a begin is harmless"
        );
    }

    /// Type one character at the end as a mergeable step.
    fn type_at_end(buffer: &mut Buffer, c: &str) {
        let end = buffer.len_chars();
        buffer.begin_transaction(
            UndoKind::Typing,
            Some(&Selections::single(Selection::cursor(end))),
        );
        buffer.edit(&[(end..end, c)]);
        buffer.end_transaction(Some(&Selections::single(Selection::cursor(end + 1))));
    }

    #[test]
    fn typed_characters_merge_within_300_ms() {
        let clock = Arc::new(ManualClock::new(0));
        let mut buffer = Buffer::new();
        buffer.set_clock(clock.clone());

        for c in ["h", "e", "y"] {
            type_at_end(&mut buffer, c);
            clock.advance(300);
        }
        clock.advance(1);
        for c in ["!", "?"] {
            type_at_end(&mut buffer, c);
            clock.advance(50);
        }
        assert_eq!(text(&buffer), "hey!?");

        let step = buffer.undo().unwrap();
        assert_eq!(text(&buffer), "hey");
        assert_eq!(
            step.selections,
            Some(Selections::single(Selection::cursor(3)))
        );
        let step = buffer.undo().unwrap();
        assert_eq!(text(&buffer), "");
        assert_eq!(
            step.selections,
            Some(Selections::single(Selection::cursor(0)))
        );
        assert_eq!(step.edits.batches().len(), 3);
        assert!(buffer.undo().is_none());

        let step = buffer.redo().unwrap();
        assert_eq!(text(&buffer), "hey");
        assert_eq!(
            step.selections,
            Some(Selections::single(Selection::cursor(3)))
        );
    }

    #[test]
    fn break_undo_group_separates_fast_typing() {
        let clock = Arc::new(ManualClock::new(0));
        let mut buffer = Buffer::new();
        buffer.set_clock(clock);
        type_at_end(&mut buffer, "a");
        buffer.break_undo_group();
        type_at_end(&mut buffer, "b");
        buffer.undo().unwrap();
        assert_eq!(text(&buffer), "a");
    }

    #[test]
    fn dirty_tracks_the_saved_state_not_just_edits() {
        let mut buffer = Buffer::from_str("x");
        assert!(!buffer.is_dirty());
        buffer.edit(&[(1..1, "y")]);
        assert!(buffer.is_dirty());
        buffer.undo();
        assert!(!buffer.is_dirty(), "undone back to the loaded text");
        buffer.redo();
        assert!(buffer.is_dirty());
        buffer.undo();
        buffer.edit(&[(1..1, "z")]);
        buffer.undo();
        assert!(!buffer.is_dirty());

        buffer.set_line_ending(LineEnding::CrLf);
        assert!(buffer.is_dirty(), "a different line ending needs saving");
        let mut buffer = Buffer::from_str("x");
        buffer.set_line_ending(LineEnding::Lf);
        buffer.set_bom(false);
        assert!(
            !buffer.is_dirty(),
            "setting what is already set changes nothing"
        );
        buffer.set_bom(true);
        assert!(buffer.is_dirty() && buffer.has_bom());
    }

    #[test]
    fn saving_and_reloading_need_a_path() {
        let mut buffer = Buffer::from_str("x");
        assert!(matches!(
            buffer.save(SaveOptions::default()),
            Err(BufferError::NoPath)
        ));
        assert!(matches!(buffer.reload(), Err(BufferError::NoPath)));
        assert_eq!(buffer.disk_state(), DiskState::Unchanged);
        assert_eq!(
            BufferError::NoPath.to_string(),
            "the buffer has no file path"
        );
    }

    #[test]
    fn final_newline_edit_matches_zed() {
        let edit = |text: &str| Buffer::from_str(text).final_newline_edit();
        assert_eq!(edit(""), None);
        assert_eq!(edit("a\n"), None);
        assert_eq!(edit("a"), Some((1..1, "\n")));
        assert_eq!(edit("a\n\n\n"), Some((1..4, "\n")));
        assert_eq!(edit("a  \n\t\n"), Some((1..6, "\n")));
        assert_eq!(edit("a \n"), Some((1..3, "\n")));
        assert_eq!(edit("\n\n"), Some((0..2, "\n")));
        assert_eq!(edit("\n"), None);
        assert_eq!(edit("  "), Some((0..2, "\n")));
        assert_eq!(edit("a\nb"), Some((3..3, "\n")));
    }

    /// The chars other than `\n` that some builds of ropey break lines at.
    const ODD_BREAKS: [char; 5] = ['\u{b}', '\u{c}', '\u{85}', '\u{2028}', '\u{2029}'];

    /// Check every line conversion against a plain string.
    fn check_lines(buffer: &Buffer, model: &str) {
        assert_eq!(text(buffer), model);
        let lines: Vec<&str> = model.split('\n').collect();
        assert_eq!(buffer.len_lines(), lines.len(), "len_lines of {model:?}");
        let mut start = 0;
        for (index, line) in lines.iter().enumerate() {
            let len = line.chars().count();
            assert_eq!(
                buffer.line_to_char(index),
                start,
                "line_to_char({index}) of {model:?}"
            );
            assert_eq!(
                buffer.line_end(index),
                start + len,
                "line_end({index}) of {model:?}"
            );
            assert_eq!(buffer.line(index), *line, "line({index}) of {model:?}");
            for offset in 0..=len {
                assert_eq!(
                    buffer.char_to_line(start + offset),
                    index,
                    "char_to_line of {model:?}"
                );
                assert_eq!(
                    buffer.char_to_point(start + offset),
                    Point::new(index, offset)
                );
                assert_eq!(
                    buffer.point_to_char(Point::new(index, offset)),
                    start + offset
                );
            }
            let byte_start: usize = lines[..index].iter().map(|l| l.len() + 1).sum();
            assert_eq!(buffer.line_to_byte(index), byte_start);
            start += len + 1;
        }
        assert_eq!(buffer.line_to_char(lines.len()), model.chars().count());
    }

    #[test]
    fn only_line_feeds_break_lines() {
        for odd in ODD_BREAKS {
            let model = format!("a{odd}b\nc{odd}{odd}\n{odd}");
            let buffer = Buffer::from_str(&model);
            check_lines(&buffer, &model);
            assert_eq!(buffer.len_lines(), 3);
            assert_eq!(buffer.line(0).len_chars(), 3);
        }
        let model = "\u{c}\n\u{2028}x\u{85}\n\n\u{b}\u{2029}";
        check_lines(&Buffer::from_str(model), model);
    }

    /// A small deterministic random number generator for the model tests.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, bound: usize) -> usize {
            (self.next() % bound.max(1) as u64) as usize
        }
    }

    fn random_text(rng: &mut Rng, max_len: usize) -> String {
        const ALPHABET: [char; 14] = [
            'a', 'b', ' ', '\n', '\n', 'é', '世', '\u{c}', '\u{2028}', '\u{85}', '\u{b}',
            '\u{2029}', '\t', '🙂',
        ];
        let len = rng.below(max_len + 1);
        (0..len)
            .map(|_| ALPHABET[rng.below(ALPHABET.len())])
            .collect()
    }

    #[test]
    fn random_edits_match_a_string_model() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for _ in 0..60 {
            let mut model = random_text(&mut rng, 40);
            let mut buffer = Buffer::from_str(&model);
            check_lines(&buffer, &model);
            let original = model.clone();
            let mut steps = 0;
            for _ in 0..25 {
                // Build a few non-overlapping edits in pre-edit coordinates.
                let len = model.chars().count();
                let mut cuts: Vec<usize> =
                    (0..rng.below(4) * 2).map(|_| rng.below(len + 1)).collect();
                cuts.sort_unstable();
                let edits: Vec<(Range<usize>, String)> = cuts
                    .chunks(2)
                    .map(|pair| (pair[0]..pair[1], random_text(&mut rng, 6)))
                    .collect();
                let chars: Vec<char> = model.chars().collect();
                let mut next = String::new();
                let mut position = 0;
                for (range, replacement) in &edits {
                    next.extend(&chars[position..range.start]);
                    next.push_str(replacement);
                    position = range.end;
                }
                next.extend(&chars[position..]);

                let version = buffer.version();
                let summary = buffer.edit(&edits);
                if !summary.is_empty() {
                    steps += 1;
                    assert_eq!(buffer.version(), version + 1);
                }
                model = next;
                check_lines(&buffer, &model);
                for change in summary.changes() {
                    assert_eq!(
                        buffer.char_to_byte(change.new.start),
                        change.new_bytes.start
                    );
                    assert_eq!(buffer.char_to_byte(change.new.end), change.new_bytes.end);
                    assert_eq!(
                        buffer.char_to_byte_point(change.new.end),
                        change.new_end_point
                    );
                }
            }
            let end = model.clone();
            for _ in 0..steps {
                assert!(buffer.undo().is_some());
                check_lines(&buffer, &buffer.to_string());
            }
            assert!(buffer.undo().is_none());
            assert_eq!(text(&buffer), original);
            assert!(!buffer.is_dirty());
            for _ in 0..steps {
                assert!(buffer.redo().is_some());
            }
            check_lines(&buffer, &end);
        }
    }
}
