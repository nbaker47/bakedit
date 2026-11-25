//! The project diff (`git::Diff`): every change since the last commit, file after file, in
//! one tab.
//!
//! # What the app calls (this signature stays)
//!
//! * `ProjectDiff::new(cx: &mut Cx<'_>) -> ProjectDiff`. `kind()` is `"project_diff"`: the
//!   shell keeps one and brings it back.
//!
//! The shell sends `project_diff::ShowFile {"path"}` to scroll to a file (the git panel's
//! arrow keys do that). The rest is the keymap's `ProjectDiff` section: moving by line,
//! page, hunk and file, `project_diff::OpenFile` to open the file at the line under the
//! cursor, staging the file under the cursor (`git::ToggleStaged`, `git::StageFile`,
//! `git::UnstageFile`, `git::StageAll`, `git::UnstageAll`), discarding it
//! (`git::RestoreAndNext`, which asks first and then shows the next file) and the commit
//! keys, which take you to the git panel's message box.
//!
//! # How it is built
//!
//! The diff against HEAD (untracked files as new ones) is read on a worker thread together
//! with the syntax colours of every line (each hunk is coloured as a snippet of its file:
//! the new side for added and context lines, the old side for removed ones). It is read
//! again when git or the files change; the cursor stays on the same line and at the same
//! height on screen. A file that is binary, too large for git's reader or over
//! [`MAX_FILE_ROWS`] lines is summarised in one row instead of dumped.

use std::any::Any;
use std::ops::Range;
use std::path::{Path, PathBuf};

use bakedit_core::{Action, KeyEvent, MouseButton, MouseEvent, MouseKind, Rgb, Theme};
use bakedit_git::{DiffLineKind, DiffStatus, DiffTarget, FileDiff, StageState};
use bakedit_syntax::{CaptureId, Highlighter, LineBreaks, LineHighlights, StyleCache};
use ratatui::layout::Rect;
use unicode_segmentation::UnicodeSegmentation;

use crate::background::Task;
use crate::chrome::Tab;
use crate::icons::file_icon;
use crate::line_input::{InputOutcome, LineInput};
use crate::paint::{self, ScrollMarks};
use crate::panels::git::{request_discard, Discarding};
use crate::settings::Icons;
use crate::ui::{self, Canvas, Style};
use crate::view::{Context, Cursor, CursorShape, Cx, Effect, FsChange, Item, RenderCx, View};
use crate::workspace::{Focus, PanelId};

/// Lines a wheel step scrolls.
const WHEEL_ROWS: usize = 3;
/// Columns a sideways wheel step scrolls.
const WHEEL_COLUMNS: usize = 6;
/// A file with more changed lines than this is summarised, not shown.
const MAX_FILE_ROWS: usize = 4000;
/// After this many rows the remaining files are only listed.
const MAX_ROWS: usize = 120_000;
/// Files longer than this are not coloured.
const MAX_HIGHLIGHT_LINES: usize = 6000;
const TAB: usize = 4;

type Spans = Vec<(Range<usize>, CaptureId)>;
/// A running discard and the paths it touches.
type DiscardJob = (Task<Result<String, String>>, Vec<PathBuf>);

/// One row of the drawn diff.
#[derive(Debug, Clone, PartialEq)]
enum DiffRow {
    /// The name of a file and how much changed in it.
    File(usize),
    /// A hunk's `@@ ... @@` line.
    Hunk { file: usize, header: String },
    Line { file: usize, kind: DiffLineKind, old: Option<u32>, new: Option<u32>, text: String, spans: Spans },
    /// Why a file has no lines to show (binary, too large).
    Note { file: usize, text: String },
    /// The blank row after a file.
    Gap,
}

impl DiffRow {
    fn file(&self) -> Option<usize> {
        match self {
            DiffRow::File(file) | DiffRow::Hunk { file, .. } | DiffRow::Line { file, .. } | DiffRow::Note { file, .. } => Some(*file),
            DiffRow::Gap => None,
        }
    }
}

/// What the worker thread hands over.
struct Loaded {
    files: Vec<FileDiff>,
    rows: Vec<DiffRow>,
    /// Columns of a line number: as wide as the longest in the diff (at least 3).
    number_width: u16,
}

/// Where the cursor was when the diff was read again, to put it back.
struct Anchor {
    path: PathBuf,
    kind: Option<DiffLineKind>,
    old: Option<u32>,
    new: Option<u32>,
    /// Rows between the top of the view and the cursor.
    offset: usize,
}

/// In-diff find bar (`Ctrl+F` / `Cmd+F`).
struct DiffFindBar {
    query: LineInput,
    focused: bool,
    case_sensitive: bool,
    /// Row indices in `ProjectDiff::rows` matching the query.
    matches: Vec<usize>,
    /// Active index in `matches`.
    current: Option<usize>,
}

/// The project diff.
pub struct ProjectDiff {
    files: Vec<FileDiff>,
    rows: Vec<DiffRow>,
    number_width: u16,
    task: Option<Task<Result<Loaded, String>>>,
    /// The git generation the diff was last read at.
    generation: Option<u64>,
    diff_base: crate::project::DiffBase,
    /// Files changed on disk since the diff was read.
    stale: bool,
    error: Option<String>,
    cursor: usize,
    scroll: usize,
    left: usize,
    /// When true (NAV mode), Markdown files in the diff render styled Markdown;
    /// `bakedit::ToggleMode` (`Ctrl+Opt+I`) flips to raw Markdown (EDIT mode).
    render_markdown: bool,
    height: usize,
    area: Rect,
    /// A file to scroll to once the diff has arrived.
    wanted: Option<PathBuf>,
    /// True once we fell back from Uncommitted to Branch diff while looking for `wanted`.
    wanted_fallback: bool,
    /// A discard that was confirmed: the file index to land on when its file is gone.
    after_discard: Option<(usize, PathBuf)>,
    discards: Vec<DiscardJob>,
    styles: StyleCache,
    styles_theme: String,
    /// The top of the repository (git's paths are relative to it), as the project spells it.
    top: Option<PathBuf>,
    /// In-diff find bar (`Ctrl+F` / `Cmd+F`).
    find: Option<DiffFindBar>,
}

fn status_letter(status: DiffStatus) -> &'static str {
    match status {
        DiffStatus::Added => "A",
        DiffStatus::Deleted => "D",
        DiffStatus::Modified => "M",
        DiffStatus::Renamed => "R",
        DiffStatus::Copied => "C",
        DiffStatus::Untracked => "U",
        DiffStatus::Conflicted => "!",
    }
}

fn status_color(status: DiffStatus, theme: &Theme) -> Rgb {
    match status {
        DiffStatus::Added | DiffStatus::Untracked | DiffStatus::Copied => theme.created,
        DiffStatus::Modified | DiffStatus::Renamed => theme.modified,
        DiffStatus::Deleted => theme.deleted,
        DiffStatus::Conflicted => theme.conflict,
    }
}

fn checkbox_width(icons: Icons) -> u16 {
    match icons {
        Icons::Nerd => 1,
        Icons::Ascii => 3,
    }
}

/// The colours of each line of `text`, as spans of byte columns.
fn highlight_lines(highlighter: &mut Highlighter, language: &'static bakedit_syntax::Language, text: &str) -> Option<LineHighlights> {
    let spans = highlighter.highlight(language, text, None).ok()?;
    Some(LineHighlights::with_line_breaks(text, &spans, LineBreaks::Lf))
}

fn spans_of(lines: &Option<LineHighlights>, index: usize) -> Spans {
    lines.as_ref().map(|lines| lines.spans_for_line(index).to_vec()).unwrap_or_default()
}

fn note_for(file: &FileDiff) -> Option<String> {
    let changed: usize = file.hunks.iter().map(|hunk| hunk.lines.len()).sum();
    if file.binary {
        Some("Binary file: not shown".to_string())
    } else if file.too_large {
        Some("Too large to show here: open the file to read it".to_string())
    } else if changed > MAX_FILE_ROWS {
        Some(format!("{changed} lines: too long to show here, open the file to read it"))
    } else if file.hunks.is_empty() {
        Some(match (file.old_mode, file.new_mode, file.status) {
            (Some(old), Some(new), _) if old != new => format!("File mode changed from {old:o} to {new:o}"),
            (_, _, DiffStatus::Renamed) => "Renamed without changes".to_string(),
            (_, _, DiffStatus::Deleted) => "Empty file deleted".to_string(),
            (_, _, DiffStatus::Added | DiffStatus::Untracked) => "Empty file".to_string(),
            _ => "No changes to show".to_string(),
        })
    } else {
        None
    }
}

/// The rows of every file, coloured. Runs on a worker thread.
fn build(files: Vec<FileDiff>) -> Loaded {
    let mut rows = Vec::new();
    let mut highlighter = Highlighter::new();
    let mut widest = 0u32;
    for (index, file) in files.iter().enumerate() {
        rows.push(DiffRow::File(index));
        if rows.len() > MAX_ROWS {
            rows.push(DiffRow::Note { file: index, text: "Not shown: the diff is too long. Commit some of it first".to_string() });
            rows.push(DiffRow::Gap);
            continue;
        }
        if let Some(text) = note_for(file) {
            rows.push(DiffRow::Note { file: index, text });
            rows.push(DiffRow::Gap);
            continue;
        }
        let language = bakedit_syntax::language_for_path(&file.path, None).filter(|language| language.has_grammar());
        let total: usize = file.hunks.iter().map(|hunk| hunk.lines.len()).sum();
        let colour = language.filter(|_| total <= MAX_HIGHLIGHT_LINES);
        for hunk in &file.hunks {
            let header = format!("@@ -{},{} +{},{} @@ {}", hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines, hunk.header.trim());
            rows.push(DiffRow::Hunk { file: index, header: header.trim_end().to_string() });
            let text_of = |line: &bakedit_git::DiffLine| line.text.trim_end_matches(['\r', '\n']).to_string();
            // Each hunk is coloured as a snippet: the new side and the old side apart.
            let side = |keep: DiffLineKind| -> String {
                let lines: Vec<String> = hunk.lines.iter().filter(|l| l.kind == DiffLineKind::Context || l.kind == keep).map(text_of).collect();
                lines.join("\n")
            };
            let new_lines = colour.and_then(|language| highlight_lines(&mut highlighter, language, &side(DiffLineKind::Added)));
            let old_lines = colour.and_then(|language| highlight_lines(&mut highlighter, language, &side(DiffLineKind::Removed)));
            let (mut new_index, mut old_index) = (0, 0);
            for line in &hunk.lines {
                widest = widest.max(line.old_no.unwrap_or(0)).max(line.new_no.unwrap_or(0));
                let spans = match line.kind {
                    DiffLineKind::Context => {
                        new_index += 1;
                        old_index += 1;
                        spans_of(&new_lines, new_index - 1)
                    }
                    DiffLineKind::Added => {
                        new_index += 1;
                        spans_of(&new_lines, new_index - 1)
                    }
                    DiffLineKind::Removed => {
                        old_index += 1;
                        spans_of(&old_lines, old_index - 1)
                    }
                };
                rows.push(DiffRow::Line { file: index, kind: line.kind, old: line.old_no, new: line.new_no, text: text_of(line), spans });
            }
        }
        rows.push(DiffRow::Gap);
    }
    Loaded { files, rows, number_width: (widest.to_string().len() as u16).max(3) }
}

impl ProjectDiff {
    /// The diff of the project's repository against HEAD, read in the background.
    pub fn new(cx: &mut Cx<'_>) -> ProjectDiff {
        let mut diff = ProjectDiff {
            files: Vec::new(),
            rows: Vec::new(),
            number_width: 3,
            task: None,
            generation: None,
            diff_base: cx.project.diff_base().clone(),
            stale: true,
            error: None,
            cursor: 0,
            scroll: 0,
            left: 0,
            render_markdown: true,
            height: 0,
            area: Rect::default(),
            wanted: None,
            wanted_fallback: false,
            after_discard: None,
            discards: Vec::new(),
            styles: StyleCache::new(),
            styles_theme: String::new(),
            top: None,
            find: None,
        };
        diff.reload(cx);
        diff
    }

    /// Start reading the diff again if git or the files changed since it was read.
    fn reload(&mut self, cx: &Cx<'_>) {
        if let Some(top) = cx.project.abs_path(Path::new("")) {
            self.top = Some(top);
        }
        let generation = cx.project.git_generation();
        if self.task.is_some() || (!self.stale && self.generation == Some(generation)) {
            return;
        }
        let Some(repo) = cx.project.repo().cloned() else {
            self.generation = Some(generation);
            self.stale = false;
            return;
        };
        self.generation = Some(generation);
        self.stale = false;
        let base = cx.project.diff_base().clone();
        self.diff_base = base.clone();
        self.task = Some(cx.project.spawn(move || {
            let result = match &base {
                crate::project::DiffBase::Uncommitted => repo.diff_files(DiffTarget::HeadToWorktree),
                crate::project::DiffBase::Branch(ref_name) => repo.diff_against_ref(ref_name),
                crate::project::DiffBase::Commit { sha, .. } => repo.diff_commit(sha),
            };
            result.map(build).map_err(|error| error.to_string())
        }));
    }

    /// Open or re-focus the in-diff find bar.
    fn open_find(&mut self) {
        if let Some(bar) = &mut self.find {
            bar.focused = true;
            bar.query.select_all();
        } else {
            self.find = Some(DiffFindBar {
                query: LineInput::new(),
                focused: true,
                case_sensitive: false,
                matches: Vec::new(),
                current: None,
            });
        }
    }

    fn find_focused(&self) -> bool {
        self.find.as_ref().is_some_and(|bar| bar.focused)
    }

    fn row_matches(&self, row: &DiffRow, needle: &str, case_sensitive: bool) -> bool {
        let check = |haystack: &str| {
            if case_sensitive {
                haystack.contains(needle)
            } else {
                haystack.to_lowercase().contains(needle)
            }
        };
        match row {
            DiffRow::Line { text, .. } => check(text),
            DiffRow::File(index) => self.files.get(*index).is_some_and(|f| check(&f.path.to_string_lossy())),
            DiffRow::Hunk { header, .. } => check(header),
            DiffRow::Note { text, .. } => check(text),
            DiffRow::Gap => false,
        }
    }

    fn recompute_find_matches(&mut self, jump: bool) {
        let Some(bar) = &self.find else { return };
        let raw = bar.query.text();
        if raw.is_empty() {
            if let Some(bar) = &mut self.find {
                bar.matches.clear();
                bar.current = None;
            }
            return;
        }
        let case_sensitive = bar.case_sensitive;
        let needle = if case_sensitive { raw.to_string() } else { raw.to_lowercase() };
        let matches: Vec<usize> = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(i, row)| self.row_matches(row, &needle, case_sensitive).then_some(i))
            .collect();
        let current = if matches.is_empty() {
            None
        } else {
            let pos = matches.iter().position(|&row| row >= self.cursor).unwrap_or(0);
            if jump {
                self.cursor = matches[pos];
            }
            Some(pos)
        };
        if let Some(bar) = &mut self.find {
            bar.matches = matches;
            bar.current = current;
        }
    }

    fn step_find_match(&mut self, forward: bool) {
        let Some(bar) = &mut self.find else { return };
        if bar.matches.is_empty() {
            return;
        }
        let len = bar.matches.len();
        let next = match bar.current {
            Some(i) if forward => (i + 1) % len,
            Some(i) => (i + len - 1) % len,
            None => 0,
        };
        bar.current = Some(next);
        self.cursor = bar.matches[next];
    }

    fn find_input_action(&mut self, action: &Action, cx: &mut Cx<'_>) -> bool {
        let Some(bar) = &mut self.find else { return false };
        match bar.query.handle_action(action) {
            InputOutcome::Ignored => false,
            InputOutcome::Moved => true,
            InputOutcome::Edited => {
                self.recompute_find_matches(true);
                true
            }
            InputOutcome::Copied { text, edited } => {
                cx.clipboard.set(&text, false);
                if edited {
                    self.recompute_find_matches(true);
                }
                true
            }
            InputOutcome::WantsPaste => {
                if let Some(clip) = cx.clipboard.get() {
                    bar.query.insert(&clip.text);
                    self.recompute_find_matches(true);
                }
                true
            }
        }
    }

    fn render_find_bar(&mut self, canvas: &mut Canvas<'_>, area: Rect, cx: &RenderCx<'_>) -> Option<Cursor> {
        let bar = self.find.as_mut()?;
        let theme = cx.theme;
        let bar_bg = theme.surface;
        let field_bg = theme.editor_bg;
        canvas.fill(area, bar_bg);

        let count_text = if bar.query.is_empty() {
            String::new()
        } else if let Some(current) = bar.current {
            format!(" {}/{} ", current + 1, bar.matches.len())
        } else {
            " 0/0 ".to_string()
        };
        let case_badge = if bar.case_sensitive { "[Aa] " } else { "" };
        let right_w = (count_text.len() + case_badge.len()) as u16;
        let label = " Find ";
        let label_w = label.len() as u16;
        canvas.text(area.x, area.y, label, Style::colors(theme.text_muted, bar_bg), label_w);

        let field_x = area.x + label_w;
        let field_w = area.width.saturating_sub(label_w + right_w + 1);
        let mut cursor = None;
        if field_w >= 4 {
            let field = Rect::new(field_x, area.y, field_w, 1);
            canvas.fill(field, field_bg);
            let inner = Rect::new(field.x + 1, field.y, field.width.saturating_sub(2), 1);
            let input_style = Style::colors(theme.text, field_bg);
            let selection_style = Style::colors(theme.text, theme.selection_bg);
            let placeholder = Style::colors(theme.text_placeholder, field_bg);
            let col = bar.query.render(canvas, inner, input_style, selection_style, "Find in diff", placeholder);
            if bar.focused && cx.focused {
                cursor = Some(Cursor { col, row: area.y, shape: CursorShape::Bar });
            }
        }

        let mut rx = area.right().saturating_sub(right_w);
        if !count_text.is_empty() {
            let color = if bar.matches.is_empty() { theme.deleted } else { theme.text_muted };
            let w = count_text.len() as u16;
            canvas.text(rx, area.y, &count_text, Style::colors(color, bar_bg), w);
            rx += w;
        }
        if !case_badge.is_empty() {
            canvas.text(rx, area.y, case_badge, Style::colors(theme.text_accent, bar_bg), case_badge.len() as u16);
        }
        cursor
    }

    /// Copy the text of the highlighted diff row (code line without +/- prefix, file path,
    /// hunk header, or note) to the clipboard.
    fn copy_current_row(&self, cx: &mut Cx<'_>) {
        let Some(row) = self.rows.get(self.cursor) else { return };
        match row {
            DiffRow::Line { text, .. } => {
                cx.clipboard.set(text, false);
                cx.status("Copied line");
            }
            DiffRow::File(index) => {
                if let Some(file) = self.files.get(*index) {
                    let path = file.path.to_string_lossy();
                    cx.clipboard.set(&path, false);
                    cx.status(format!("Copied path: {path}"));
                }
            }
            DiffRow::Hunk { header, .. } => {
                cx.clipboard.set(header, false);
                cx.status("Copied hunk header");
            }
            DiffRow::Note { text, .. } => {
                cx.clipboard.set(text, false);
                cx.status("Copied note");
            }
            DiffRow::Gap => {}
        }
    }

    fn anchor(&self) -> Option<Anchor> {
        let row = self.rows.get(self.cursor)?;
        let file = self.files.get(row.file()?)?;
        let (kind, old, new) = match row {
            DiffRow::Line { kind, old, new, .. } => (Some(*kind), *old, *new),
            _ => (None, None, None),
        };
        Some(Anchor { path: file.path.clone(), kind, old, new, offset: self.cursor.saturating_sub(self.scroll) })
    }

    /// The row to put the cursor on after a re-read: the same line of the same file, else
    /// the same file's header.
    fn row_for(&self, anchor: &Anchor) -> Option<usize> {
        let file = self.files.iter().position(|file| file.path == anchor.path)?;
        let header = self.rows.iter().position(|row| matches!(row, DiffRow::File(f) if *f == file))?;
        if anchor.kind.is_none() {
            return Some(header);
        }
        let same = |row: &DiffRow| matches!(row, DiffRow::Line { file: f, kind, old, new, .. } if *f == file && Some(*kind) == anchor.kind && *old == anchor.old && *new == anchor.new);
        // Lines above a change keep their numbers; below it only one side may still match.
        let near = |row: &DiffRow| matches!(row, DiffRow::Line { file: f, kind, old, new, .. } if *f == file && Some(*kind) == anchor.kind && (anchor.new.is_some() && *new == anchor.new || anchor.new.is_none() && *old == anchor.old));
        Some(self.rows.iter().position(same).or_else(|| self.rows.iter().position(near)).unwrap_or(header))
    }

    fn take_result(&mut self, loaded: Result<Loaded, String>) {
        let anchor = self.anchor();
        match loaded {
            Ok(loaded) => {
                self.files = loaded.files;
                self.rows = loaded.rows;
                self.number_width = loaded.number_width;
                self.left = 0;
                self.error = None;
                self.recompute_find_matches(false);
            }
            Err(error) => {
                self.error = Some(error);
                return;
            }
        }
        if let Some(path) = self.wanted.take() {
            if let Some(row) = self.file_row(&path) {
                self.cursor = row;
                self.scroll = row;
                self.wanted_fallback = false;
                return;
            }
            if self.diff_base == crate::project::DiffBase::Uncommitted && !self.wanted_fallback {
                self.wanted = Some(path);
                self.wanted_fallback = true;
            } else {
                self.wanted_fallback = false;
            }
        }
        if let Some((index, path)) = self.after_discard.take() {
            if self.files.iter().all(|file| file.path != path) {
                let next = index.min(self.files.len().saturating_sub(1));
                self.cursor = self.rows.iter().position(|row| matches!(row, DiffRow::File(f) if *f == next)).unwrap_or(0);
                self.scroll = self.cursor;
                return;
            }
            // Read before the discard finished: wait for the next read.
            self.after_discard = Some((index, path));
        }
        match anchor.and_then(|anchor| self.row_for(&anchor).map(|row| (row, anchor.offset))) {
            Some((row, offset)) => {
                self.cursor = row;
                self.scroll = row.saturating_sub(offset);
            }
            None => {
                self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
                self.scroll = self.scroll.min(self.cursor);
            }
        }
    }

    /// The row of a file's header, by its path relative to the work tree.
    fn file_row(&self, relative: &Path) -> Option<usize> {
        self.rows.iter().position(|row| matches!(row, DiffRow::File(file) if self.files[*file].path == relative))
    }

    fn move_cursor(&mut self, delta: isize) {
        let last = self.rows.len().saturating_sub(1) as isize;
        self.cursor = (self.cursor as isize + delta).clamp(0, last.max(0)) as usize;
    }

    /// Move to the next (or previous) row that `is_stop` picks.
    fn jump(&mut self, forward: bool, is_stop: impl Fn(&DiffRow) -> bool) {
        let found = if forward {
            self.rows.iter().enumerate().skip(self.cursor + 1).find(|(_, row)| is_stop(row)).map(|(index, _)| index)
        } else {
            self.rows.iter().enumerate().take(self.cursor).rev().find(|(_, row)| is_stop(row)).map(|(index, _)| index)
        };
        if let Some(index) = found {
            self.cursor = index;
            // A file header goes to the top, so the file can be read.
            if matches!(self.rows[index], DiffRow::File(_)) {
                self.scroll = index;
            }
        }
    }

    fn file_index_under_cursor(&self) -> Option<usize> {
        self.rows.get(self.cursor).and_then(DiffRow::file)
    }

    fn file_under_cursor(&self) -> Option<&FileDiff> {
        self.files.get(self.file_index_under_cursor()?)
    }

    /// Where `project_diff::OpenFile` goes: the file and the line of the cursor row on the
    /// new side (a removed line opens where it was).
    fn open_target(&self, cx: &mut Cx<'_>) -> Option<Effect> {
        let file = self.file_under_cursor()?;
        if file.status == DiffStatus::Deleted {
            cx.error(format!("{} was deleted: there is nothing to open", file.path.display()));
            return None;
        }
        let path = cx.project.abs_path(&file.path)?;
        // The first line of the new side from the cursor on, within this file.
        let mut line = None;
        for (offset, row) in self.rows[self.cursor..].iter().enumerate() {
            match row {
                DiffRow::Line { new: Some(new), .. } => {
                    line = Some(*new);
                    break;
                }
                DiffRow::Gap => break,
                DiffRow::File(_) if offset > 0 => break,
                _ => {}
            }
        }
        Some(Effect::OpenFile { path, line, column: None, preview: false, focus: true })
    }

    fn stage_state(&self, file: &FileDiff, cx: &RenderCx<'_>) -> StageState {
        cx.project.status().and_then(|status| status.entry(&file.path)).map(|entry| entry.stage_state()).unwrap_or(StageState::Unstaged)
    }

    fn stage_file(&self, index: usize, stage: bool, cx: &mut Cx<'_>) -> bool {
        let Some(file) = self.files.get(index) else { return false };
        let paths = match &file.old_path {
            Some(old) => vec![file.path.clone(), old.clone()],
            None => vec![file.path.clone()],
        };
        let label = format!("{} {}", if stage { "Stage" } else { "Unstage" }, file.path.display());
        let _ = cx.project.git_job(label, move |repo| {
            if stage {
                repo.stage(&paths).map(|()| String::new())
            } else {
                repo.unstage(&paths).map(|()| String::new())
            }
        });
        true
    }

    fn toggle_staged(&self, index: usize, cx: &mut Cx<'_>) -> bool {
        let Some(file) = self.files.get(index) else { return false };
        let staged = cx.project.status().and_then(|status| status.entry(&file.path)).is_some_and(|entry| entry.is_fully_staged());
        self.stage_file(index, !staged, cx)
    }

    /// `git::RestoreAndNext`: discard the file under the cursor (asking first) and show the
    /// file after it.
    fn restore_and_next(&mut self, action: &Action, cx: &mut Cx<'_>) -> bool {
        let skip_prompt = action.arg_bool("skip_prompt").unwrap_or(false);
        let target = match action.arg_str("path") {
            Some(path) => self.files.iter().position(|file| file.path == Path::new(path)),
            None => self.file_index_under_cursor(),
        };
        let Some(index) = target else { return false };
        let path = self.files[index].path.clone();
        match request_discard(cx, &path, skip_prompt, "git::RestoreAndNext") {
            Discarding::Refused => false,
            Discarding::Asked => true,
            Discarding::Started { task, paths } => {
                self.after_discard = Some((index, path));
                self.discards.push((task, paths));
                true
            }
        }
    }

    /// The commit keys: the message is written in the git panel.
    fn commit_in_panel(&self, action: &Action, cx: &mut Cx<'_>) {
        cx.push(Effect::ShowPanel(PanelId::Git));
        cx.push(Effect::Focus(Focus::Panel(PanelId::Git)));
        cx.push(Effect::Dispatch(Action::new(&action.name)));
    }

    // ---- drawing -----------------------------------------------------------------------

    /// Draw `text` at `(x, y)` in `width` columns: tabs expanded, syntax colours from
    /// `spans` (byte ranges), scrolled `self.left` columns to the right.
    #[allow(clippy::too_many_arguments)] // a drawing call: position, extent, text, colours
    fn draw_code(&mut self, canvas: &mut Canvas<'_>, x: u16, y: u16, width: u16, text: &str, spans: &Spans, bg: Rgb, theme: &Theme) {
        let right = self.left + width as usize;
        let mut col = 0usize;
        let mut span = 0usize;
        for (byte, grapheme) in text.grapheme_indices(true) {
            if col >= right {
                break;
            }
            while span < spans.len() && spans[span].0.end <= byte {
                span += 1;
            }
            let mut style = Style::colors(theme.editor_fg, bg);
            if let Some((range, capture)) = spans.get(span) {
                if range.start <= byte {
                    if let Some(syntax) = self.styles.style(theme, *capture) {
                        style.fg = syntax.color.or(style.fg);
                        style.italic = syntax.italic;
                        style.bold = syntax.bold;
                    }
                }
            }
            let (cells, shown) = if grapheme == "\t" {
                (TAB - col % TAB, " ")
            } else if grapheme.chars().all(char::is_control) {
                (1, "·")
            } else {
                (ui::width(grapheme).max(1), grapheme)
            };
            for cell in 0..cells {
                let cell_col = col + cell;
                if cell_col < self.left || cell_col >= right {
                    continue;
                }
                // A wide character cut by either edge is left out.
                if cell == 0 && col >= self.left && col + cells <= right {
                    canvas.put(x + (cell_col - self.left) as u16, y, shown, style);
                } else if cell > 0 || col < self.left {
                    canvas.put(x + (cell_col - self.left) as u16, y, " ", Style::new().on(bg));
                }
            }
            col += cells;
        }
    }

    fn draw_file_header(&self, canvas: &mut Canvas<'_>, area: Rect, y: u16, index: usize, selected: bool, cx: &RenderCx<'_>) {
        let theme = cx.theme;
        let file = &self.files[index];
        let bg = if selected { theme.element_selected } else { theme.elevated_surface };
        canvas.fill(Rect::new(area.x, y, area.width, 1), bg);
        let mut x = area.x + 1;
        let (box_glyph, box_color) = match (cx.settings.icons, self.stage_state(file, cx)) {
            (Icons::Nerd, StageState::Staged) => ("\u{f14a}", theme.text_accent),
            (Icons::Nerd, StageState::PartiallyStaged) => ("\u{f147}", theme.text_accent),
            (Icons::Nerd, StageState::Unstaged) => ("\u{f096}", theme.text_muted),
            (Icons::Ascii, StageState::Staged) => ("[x]", theme.text_accent),
            (Icons::Ascii, StageState::PartiallyStaged) => ("[-]", theme.text_accent),
            (Icons::Ascii, StageState::Unstaged) => ("[ ]", theme.text_muted),
        };
        canvas.text(x, y, box_glyph, Style::colors(box_color, bg), checkbox_width(cx.settings.icons));
        x += checkbox_width(cx.settings.icons) + 1;
        let name = file.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let (glyph, color) = file_icon(cx.settings, &name, false, false);
        x += canvas.text(x, y, &glyph, Style::colors(color, bg), 2) + 1;
        x += canvas.text(x, y, status_letter(file.status), Style::colors(status_color(file.status, theme), bg).bold(), 1) + 1;
        // The counts sit at the right edge, the path gets what is left.
        let (added, removed) = (file.additions(), file.deletions());
        let removed_text = format!("−{removed}");
        let added_text = format!("+{added}");
        let mut right = area.right() - 1;
        if area.width > 24 && (added > 0 || removed > 0) {
            if removed > 0 {
                right -= canvas.text_right(right, y, &removed_text, Style::colors(theme.deleted, bg)) + 1;
            }
            if added > 0 {
                right -= canvas.text_right(right, y, &added_text, Style::colors(theme.created, bg)) + 1;
            }
        }
        let room = right.saturating_sub(x);
        let used = canvas.text_truncated(x, y, &name, Style::colors(theme.text, bg).bold(), room);
        let mut shown = match &file.old_path {
            Some(old) => format!("← {}", old.display()),
            None => file.path.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
        };
        if file.old_path.is_none() && shown.is_empty() {
            return;
        }
        let left = room.saturating_sub(used + 2) as usize;
        if left >= 4 {
            shown = ui::truncate_start(&shown, left);
            canvas.text(x + used + 2, y, &shown, Style::colors(theme.text_muted, bg), left as u16);
        }
    }

    fn draw_row(&mut self, canvas: &mut Canvas<'_>, area: Rect, y: u16, index: usize, cx: &RenderCx<'_>) {
        let theme = cx.theme;
        // A focused diff shows the cursor row strongly, an unfocused one faintly.
        let is_cursor = index == self.cursor;
        let cursor_bg = if cx.focused { theme.element_selected } else { theme.ghost_element_selected };
        let nw = self.number_width;
        let text_x = area.x + 1 + nw * 2 + 4;
        let text_w = area.right().saturating_sub(text_x + 1);
        match self.rows[index].clone() {
            DiffRow::File(file) => self.draw_file_header(canvas, area, y, file, is_cursor && cx.focused, cx),
            DiffRow::Hunk { header, .. } => {
                let bg = if is_cursor { cursor_bg } else { theme.editor_bg };
                canvas.fill(Rect::new(area.x, y, area.width, 1), bg);
                let numbers_end = area.x + 1 + nw * 2 + 2;
                canvas.text_truncated(numbers_end, y, &header, Style::colors(theme.text_muted, bg).italic(), area.width.saturating_sub(numbers_end - area.x + 1));
            }
            DiffRow::Line { file, kind, old, new, text, spans } => {
                let tint = match kind {
                    DiffLineKind::Added => Some(theme.created),
                    DiffLineKind::Removed => Some(theme.deleted),
                    DiffLineKind::Context => None,
                };
                let line_bg = tint.map(|t| t.lerp(theme.editor_bg, 0.86)).unwrap_or(theme.editor_bg);
                let gutter_bg = tint.map(|t| t.lerp(theme.editor_bg, 0.72)).unwrap_or(theme.editor_bg);
                let (line_bg, gutter_bg) = if is_cursor { (line_bg.lerp(cursor_bg, 0.75), gutter_bg.lerp(cursor_bg, 0.75)) } else { (line_bg, gutter_bg) };
                canvas.fill(Rect::new(area.x, y, area.width, 1), line_bg);
                canvas.fill(Rect::new(area.x, y, text_x - area.x - 1, 1), gutter_bg);
                let number_color = if is_cursor { theme.active_line_number } else { theme.line_number };
                let numbers = Style::colors(number_color, gutter_bg);
                if let Some(old) = old {
                    canvas.text_right(area.x + 1 + nw, y, &old.to_string(), numbers);
                }
                if let Some(new) = new {
                    canvas.text_right(area.x + 2 + nw * 2, y, &new.to_string(), numbers);
                }
                let marker = match kind {
                    DiffLineKind::Added => "+",
                    DiffLineKind::Removed => "−",
                    DiffLineKind::Context => " ",
                };
                canvas.text(area.x + 3 + nw * 2, y, marker, Style::colors(tint.unwrap_or(theme.text_muted), gutter_bg).bold(), 1);
                let is_md = self.render_markdown
                    && self
                        .files
                        .get(file)
                        .and_then(|f| f.path.extension())
                        .and_then(|e| e.to_str())
                        .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "md" | "markdown" | "mdx"));
                if is_md {
                    let mut in_fence = false;
                    for r in &self.rows[..index] {
                        match r {
                            DiffRow::File(f) if *f == file => in_fence = false,
                            DiffRow::Line { file: f, text: t, .. } if *f == file && crate::markdown::is_fence(t) => {
                                in_fence = !in_fence;
                            }
                            _ => {}
                        }
                    }
                    let fence_line = crate::markdown::is_fence(&text);
                    if !in_fence || fence_line {
                        crate::markdown::draw_line(canvas, text_x, y, text_w, self.left, &text, line_bg, theme, in_fence);
                    } else {
                        self.draw_code(canvas, text_x, y, text_w, &text, &spans, line_bg, theme);
                    }
                } else {
                    self.draw_code(canvas, text_x, y, text_w, &text, &spans, line_bg, theme);
                }
            }
            DiffRow::Note { text, .. } => {
                let bg = if is_cursor { cursor_bg } else { theme.editor_bg };
                canvas.fill(Rect::new(area.x, y, area.width, 1), bg);
                canvas.text_truncated(area.x + 3, y, &text, Style::colors(theme.text_muted, bg).italic(), area.width.saturating_sub(4));
            }
            DiffRow::Gap => {}
        }
    }
}

impl View for ProjectDiff {
    fn render(&mut self, canvas: &mut Canvas<'_>, area: Rect, cx: &RenderCx<'_>) -> Option<Cursor> {
        let theme = cx.theme;
        canvas.fill(area, theme.editor_bg);
        if area.width < 8 || area.height == 0 {
            self.area = area;
            self.height = area.height as usize;
            return None;
        }
        let (find_cursor, content_area) = if self.find.is_some() && area.height >= 2 {
            let bar_area = Rect::new(area.x, area.y, area.width, 1);
            let cursor = self.render_find_bar(canvas, bar_area, cx);
            (cursor, Rect::new(area.x, area.y + 1, area.width, area.height - 1))
        } else {
            (None, area)
        };
        self.area = content_area;
        self.height = content_area.height as usize;
        if self.styles_theme != theme.name {
            self.styles = StyleCache::new();
            self.styles_theme = theme.name.clone();
        }
        let muted = Style::colors(theme.text_muted, theme.editor_bg);
        let text = Style::colors(theme.text, theme.editor_bg);
        let message: Option<(&str, String)> = if cx.project.repo().is_none() {
            Some(("Not a git repository", "There is nothing to compare with. Run git init in the terminal to track changes".to_string()))
        } else if let Some(error) = &self.error {
            Some(("Could not read the changes", error.clone()))
        } else if self.rows.is_empty() && (self.task.is_some() || self.generation.is_none()) {
            Some(("Reading the changes…", String::new()))
        } else if self.rows.is_empty() {
            Some(("No changes", "The working tree matches the last commit. Edit a file and it appears here".to_string()))
        } else {
            None
        };
        if let Some((title, detail)) = message {
            let top = content_area.y + 1.min(content_area.height - 1);
            let width = content_area.width.saturating_sub(4);
            canvas.text_truncated(content_area.x + 2, top, title, text, width);
            if !detail.is_empty() && top + 1 < content_area.bottom() {
                canvas.text_truncated(content_area.x + 2, top + 1, &detail, muted, width);
            }
            return find_cursor;
        }
        let last = self.rows.len().saturating_sub(1);
        self.cursor = self.cursor.min(last);
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + self.height {
            self.scroll = self.cursor + 1 - self.height;
        }
        self.scroll = self.scroll.min(last);
        let scrollbar = self.rows.len() > self.height;
        let body = Rect::new(content_area.x, content_area.y, content_area.width - u16::from(scrollbar), content_area.height);
        for (offset, index) in (self.scroll..self.rows.len()).take(self.height).enumerate() {
            self.draw_row(canvas, body, content_area.y + offset as u16, index, cx);
        }
        if scrollbar {
            let mut marks: Vec<(usize, Rgb)> = self
                .rows
                .iter()
                .enumerate()
                .filter_map(|(i, row)| match row {
                    DiffRow::File(f) => Some((i, status_color(self.files[*f].status, theme))),
                    _ => None,
                })
                .collect();
            if let Some(bar) = &self.find {
                for &row in &bar.matches {
                    marks.push((row, theme.text_accent));
                }
            }
            let scroll_marks = ScrollMarks { marks, cursor_line: Some(self.cursor) };
            paint::scrollbar(canvas, content_area.right() - 1, content_area.y, content_area.height, self.rows.len(), self.scroll, self.height, &scroll_marks, theme);
        }
        find_cursor
    }

    fn contexts(&self, out: &mut Vec<Context>) {
        if self.find_focused() {
            // Omit ProjectDiff while typing in the find bar so single-letter diff bindings
            // (j, k, n, g, y, space, [, ], {, }) type into the query instead of firing.
            out.push(Context::new("BufferSearchBar"));
            out.push(Context::new("Editor").value("mode", "single_line"));
        } else {
            out.push(Context::new("ProjectDiff"));
        }
    }

    fn handle_action(&mut self, action: &Action, cx: &mut Cx<'_>) -> bool {
        match action.name.as_str() {
            "buffer_search::Deploy" | "search::FocusSearch" => {
                self.open_find();
                return true;
            }
            "buffer_search::Dismiss" => {
                self.find = None;
                return true;
            }
            "buffer_search::FocusEditor" if self.find.is_some() => {
                if let Some(bar) = &mut self.find {
                    bar.focused = false;
                }
                return true;
            }
            "search::SelectNextMatch" if self.find.is_some() => {
                self.step_find_match(true);
                return true;
            }
            "search::SelectPreviousMatch" if self.find.is_some() => {
                self.step_find_match(false);
                return true;
            }
            "search::ToggleCaseSensitive" if self.find.is_some() => {
                if let Some(bar) = &mut self.find {
                    bar.case_sensitive = !bar.case_sensitive;
                }
                self.recompute_find_matches(true);
                return true;
            }
            "menu::Cancel" | "editor::Cancel" if self.find.is_some() => {
                self.find = None;
                return true;
            }
            _ => {}
        }
        if self.find_focused() && self.find_input_action(action, cx) {
            return true;
        }
        let page = self.height.max(1) as isize;
        match action.name.as_str() {
            "project_diff::LineDown" => self.move_cursor(1),
            "project_diff::LineUp" => self.move_cursor(-1),
            "project_diff::PageDown" => self.move_cursor(page),
            "project_diff::PageUp" => self.move_cursor(-page),
            "project_diff::HalfPageDown" => self.move_cursor(page / 2),
            "project_diff::HalfPageUp" => self.move_cursor(-page / 2),
            "project_diff::Top" => {
                self.cursor = 0;
                self.left = 0;
            }
            "project_diff::Bottom" => self.cursor = self.rows.len().saturating_sub(1),
            "project_diff::NextHunk" => self.jump(true, |row| matches!(row, DiffRow::Hunk { .. })),
            "project_diff::PreviousHunk" => self.jump(false, |row| matches!(row, DiffRow::Hunk { .. })),
            "project_diff::NextFile" => {
                self.left = 0;
                self.jump(true, |row| matches!(row, DiffRow::File(_)));
            }
            "project_diff::PreviousFile" => {
                self.left = 0;
                self.jump(false, |row| matches!(row, DiffRow::File(_)));
            }
            "project_diff::ShowFile" => {
                let Some(path) = action.arg_str("path").map(PathBuf::from) else { return false };
                let Some(relative) = cx.project.git_path(&path) else { return false };
                self.left = 0;
                self.wanted_fallback = false;
                match self.file_row(&relative) {
                    Some(row) => {
                        self.cursor = row;
                        self.scroll = row;
                    }
                    None => {
                        self.wanted = Some(relative);
                        if self.task.is_none()
                            && self.generation.is_some()
                            && self.diff_base == crate::project::DiffBase::Uncommitted
                        {
                            self.wanted_fallback = true;
                            let base = cx
                                .project
                                .repo()
                                .and_then(bakedit_git::Repo::default_base_ref)
                                .unwrap_or_else(|| "main".to_string());
                            cx.push(Effect::SetDiffBase(Some(base)));
                        }
                    }
                }
            }
            "bakedit::ToggleMode" => {
                self.render_markdown = !self.render_markdown;
                cx.status(if self.render_markdown { "Diff: rendered Markdown (NAV)" } else { "Diff: raw Markdown (EDIT)" });
            }
            "menu::Cancel" | "editor::Cancel" if !self.render_markdown => {
                self.render_markdown = true;
                cx.status("Diff: rendered Markdown (NAV)");
            }
            "project_diff::OpenFile" => match self.open_target(cx) {
                Some(effect) => cx.push(effect),
                None => return self.file_under_cursor().is_some(),
            },
            "project_diff::CopyLine" | "editor::Copy" => {
                self.copy_current_row(cx);
            }
            "git::ToggleStaged" => {
                let Some(index) = self.file_index_under_cursor() else { return false };
                return self.toggle_staged(index, cx);
            }
            "git::StageFile" | "git::UnstageFile" => {
                let Some(index) = self.file_index_under_cursor() else { return false };
                return self.stage_file(index, action.name == "git::StageFile", cx);
            }
            "git::StageAll" => {
                let _ = cx.project.git_job("Stage all", |repo| repo.stage_all().map(|()| String::new()));
            }
            "git::UnstageAll" => {
                let _ = cx.project.git_job("Unstage all", |repo| repo.unstage_all().map(|()| String::new()));
            }
            "git::RestoreAndNext" => return self.restore_and_next(action, cx),
            "git::Commit" | "git::Amend" => self.commit_in_panel(action, cx),
            _ => return false,
        }
        true
    }

    fn handle_key(&mut self, key: &KeyEvent, _cx: &mut Cx<'_>) -> bool {
        if !self.find_focused() {
            return false;
        }
        let Some(text) = key.text.as_deref().filter(|_| key.is_down()) else { return false };
        if text.chars().all(char::is_control) {
            return false;
        }
        if let Some(bar) = &mut self.find {
            bar.query.insert(text);
        }
        self.recompute_find_matches(true);
        true
    }

    fn handle_paste(&mut self, text: &str, _cx: &mut Cx<'_>) -> bool {
        if !self.find_focused() {
            return false;
        }
        if let Some(bar) = &mut self.find {
            bar.query.insert(text);
        }
        self.recompute_find_matches(true);
        true
    }

    fn handle_mouse(&mut self, event: &MouseEvent, _area: Rect, cx: &mut Cx<'_>) -> bool {
        let keep_cursor_visible = |diff: &mut ProjectDiff| {
            diff.cursor = diff.cursor.clamp(diff.scroll, (diff.scroll + diff.height).saturating_sub(1).max(diff.scroll));
        };
        match event.kind {
            MouseKind::ScrollDown => {
                let last = self.rows.len().saturating_sub(self.height.max(1));
                self.scroll = (self.scroll + WHEEL_ROWS).min(last);
                keep_cursor_visible(self);
            }
            MouseKind::ScrollUp => {
                self.scroll = self.scroll.saturating_sub(WHEEL_ROWS);
                keep_cursor_visible(self);
            }
            MouseKind::ScrollRight => self.left += WHEEL_COLUMNS,
            MouseKind::ScrollLeft => self.left = self.left.saturating_sub(WHEEL_COLUMNS),
            MouseKind::Press(MouseButton::Left) => {
                if self.find.is_some() && event.row < self.area.y {
                    if let Some(bar) = &mut self.find {
                        bar.focused = true;
                    }
                    return true;
                }
                if let Some(bar) = &mut self.find {
                    bar.focused = false;
                }
                let Some(row) = event.row.checked_sub(self.area.y) else { return true };
                let index = self.scroll + row as usize;
                if index >= self.rows.len() {
                    return true;
                }
                self.cursor = index;
                // The checkbox on a file's header stages or unstages the file.
                if let DiffRow::File(file) = self.rows[index] {
                    let first = self.area.x + 1;
                    if event.col >= first && event.col < first + checkbox_width(cx.settings.icons) {
                        self.toggle_staged(file, cx);
                    }
                }
            }
            _ => return false,
        }
        true
    }

    fn tick(&mut self, cx: &mut Cx<'_>) -> bool {
        let mut dirty = false;
        let (result, finished) = match &mut self.task {
            Some(task) => (task.poll(), task.is_finished()),
            None => (None, false),
        };
        if let Some(result) = result {
            self.take_result(result);
            dirty = true;
            if self.wanted.is_some()
                && self.wanted_fallback
                && self.diff_base == crate::project::DiffBase::Uncommitted
            {
                let base = cx
                    .project
                    .repo()
                    .and_then(bakedit_git::Repo::default_base_ref)
                    .unwrap_or_else(|| "main".to_string());
                cx.push(Effect::SetDiffBase(Some(base)));
            }
        }
        if finished {
            self.task = None;
        }
        let mut index = 0;
        while index < self.discards.len() {
            let result = self.discards[index].0.poll();
            if result.is_none() && !self.discards[index].0.is_finished() {
                index += 1;
                continue;
            }
            let (_, paths) = self.discards.remove(index);
            if result.is_some_and(|result| result.is_ok()) {
                cx.push(Effect::PathsChanged(paths));
            } else {
                self.after_discard = None;
            }
        }
        self.reload(cx);
        dirty
    }

    fn paths_changed(&mut self, change: &FsChange, cx: &mut Cx<'_>) {
        // An already modified file edited again changes no status, so git alone would not
        // say the diff is out of date.
        if change.rescan || change.paths.iter().any(|path| cx.project.git_path(path).is_some()) {
            self.stale = true;
            self.reload(cx);
        }
    }
}

impl Item for ProjectDiff {
    fn kind(&self) -> &'static str {
        "project_diff"
    }

    /// The file under the cursor, so that the git panel's arrow keys (and cmd-shift-e) show
    /// it in the tree as they show its diff. A deleted file is not in the tree to show.
    fn reveal_path(&self) -> Option<PathBuf> {
        let file = self.file_under_cursor().filter(|file| file.status != DiffStatus::Deleted)?;
        Some(self.top.as_ref()?.join(&file.path))
    }

    fn tab(&self, cx: &RenderCx<'_>) -> Tab {
        let icon = match cx.settings.icons {
            Icons::Nerd => Some(("\u{f1d3}".to_string(), cx.theme.modified)),
            Icons::Ascii => None,
        };
        let title = match &self.diff_base {
            crate::project::DiffBase::Uncommitted => "Project Diff".to_string(),
            crate::project::DiffBase::Branch(base) => format!("Diff vs {base}"),
            crate::project::DiffBase::Commit { label, .. } => format!("Commit {label}"),
        };
        Tab { title, icon, ..Default::default() }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use serde_json::json;

    use super::*;
    use crate::project::test_support::repo;
    use crate::project::Project;
    use crate::view::test_support::Harness;

    fn loaded(dir: &Path) -> (Harness, ProjectDiff) {
        let mut project = Project::open(dir, crate::background::Waker::detached());
        project.settle(Duration::from_secs(20));
        let mut harness = Harness::with_project(project);
        let mut diff = ProjectDiff::new(&mut harness.cx());
        let started = Instant::now();
        while diff.task.is_some() || diff.generation.is_none() {
            diff.tick(&mut harness.cx());
            assert!(started.elapsed() < Duration::from_secs(20), "the diff never arrived");
            std::thread::sleep(Duration::from_millis(5));
        }
        (harness, diff)
    }

    fn act(harness: &mut Harness, diff: &mut ProjectDiff, name: &str) -> bool {
        diff.handle_action(&Action::new(name), &mut harness.cx())
    }

    /// Tick the diff and the project until `done`.
    fn until(harness: &mut Harness, diff: &mut ProjectDiff, what: &str, done: impl Fn(&ProjectDiff) -> bool) {
        let started = Instant::now();
        loop {
            harness.project.poll();
            diff.tick(&mut harness.cx());
            if done(diff) {
                return;
            }
            assert!(started.elapsed() < Duration::from_secs(20), "timed out waiting for: {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn shows_each_file_with_its_hunks_and_counts() {
        let dir = repo();
        std::fs::write(dir.path().join("a.txt"), "one\nTWO\nthree\n").unwrap();
        std::fs::write(dir.path().join("fresh.md"), "new\n").unwrap();
        let (harness, mut diff) = loaded(dir.path());
        let (text, _, _) = harness.draw(&mut diff, 70, 14);
        let rows: Vec<&str> = text.lines().collect();
        assert!(rows[0].contains("M a.txt") && rows[0].ends_with("+2 −1"), "{:?}", rows[0]);
        assert!(rows[1].contains("@@ -1,2 +1,3 @@"), "{:?}", rows[1]);
        assert!(rows[2].trim_start().starts_with("1   1") && rows[2].ends_with("one"), "{:?}", rows[2]);
        assert!(rows[3].contains("−") && rows[3].ends_with("two"), "{:?}", rows[3]);
        assert!(rows[4].contains('+') && rows[4].ends_with("TWO"), "{:?}", rows[4]);
        assert!(rows.iter().any(|r| r.contains("U fresh.md")), "untracked files are listed: {text}");
    }

    #[test]
    fn lines_are_tinted_and_coloured_by_syntax() {
        let dir = repo();
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn answer() -> u32 {\n    43\n}\n").unwrap();
        let (harness, mut diff) = loaded(dir.path());
        let (text, _, buf) = harness.draw(&mut diff, 70, 12);
        let theme = &harness.theme;
        let row_of = |needle: &str| text.lines().position(|l| l.contains(needle)).unwrap() as u16;
        let added = row_of("43");
        let removed = row_of("42");
        let bg_at = |y: u16, x: u16| buf[(x, y)].bg;
        let tinted = |color: Rgb| ui::color(color.lerp(theme.editor_bg, 0.86));
        assert_eq!(bg_at(added, 40), tinted(theme.created), "added lines have the created tint");
        assert_eq!(bg_at(removed, 40), tinted(theme.deleted));
        // `pub` is a keyword: its colour is not the plain foreground.
        let pub_row = row_of("pub fn");
        let x = text.lines().nth(pub_row as usize).unwrap().find("pub").unwrap() as u16;
        assert_ne!(buf[(x, pub_row)].fg, ui::color(theme.editor_fg), "syntax colour on the keyword");
    }

    #[test]
    fn moves_by_file_shows_a_file_and_opens_at_the_line() {
        let dir = repo();
        std::fs::write(dir.path().join("a.txt"), "one\nTWO\n").unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn answer() -> u32 {\n    43\n}\n").unwrap();
        let (mut harness, mut diff) = loaded(dir.path());
        assert!(act(&mut harness, &mut diff, "project_diff::NextFile"));
        assert!(matches!(diff.rows[diff.cursor], DiffRow::File(1)));
        let show = Action::with_args("project_diff::ShowFile", json!({ "path": dir.path().join("a.txt").to_string_lossy() }));
        assert!(diff.handle_action(&show, &mut harness.cx()));
        assert_eq!(diff.cursor, 0);
        act(&mut harness, &mut diff, "project_diff::NextHunk");
        act(&mut harness, &mut diff, "project_diff::LineDown");
        assert!(act(&mut harness, &mut diff, "project_diff::OpenFile"));
        assert_eq!(
            harness.take_effects(),
            vec![Effect::OpenFile { path: dir.path().join("a.txt"), line: Some(1), column: None, preview: false, focus: true }]
        );
        // The file under the cursor is the one the tree reveals (the git panel's arrows).
        assert_eq!(diff.reveal_path(), Some(dir.path().join("a.txt")));
        assert!(act(&mut harness, &mut diff, "project_diff::NextFile"));
        assert_eq!(diff.reveal_path(), Some(dir.path().join("src/lib.rs")));
    }

    #[test]
    fn show_file_before_the_diff_arrives_scrolls_when_it_does() {
        let dir = repo();
        let mut body = String::new();
        for i in 0..40 {
            body.push_str(&format!("line {i}\n"));
        }
        std::fs::write(dir.path().join("a.txt"), "changed\n").unwrap();
        std::fs::write(dir.path().join("big.txt"), body).unwrap();
        let mut project = Project::open(dir.path(), crate::background::Waker::detached());
        project.settle(Duration::from_secs(20));
        let mut harness = Harness::with_project(project);
        let mut diff = ProjectDiff::new(&mut harness.cx());
        let show = Action::with_args("project_diff::ShowFile", json!({ "path": dir.path().join("big.txt").to_string_lossy() }));
        assert!(diff.handle_action(&show, &mut harness.cx()));
        until(&mut harness, &mut diff, "the diff", |d| !d.rows.is_empty());
        let text = harness.draw(&mut diff, 60, 8).0;
        assert!(text.lines().next().unwrap().contains("U big.txt"), "{text}");
    }

    #[test]
    fn a_clean_tree_and_no_repository_say_so() {
        let dir = repo();
        let (harness, mut diff) = loaded(dir.path());
        let text = harness.draw(&mut diff, 80, 4).0;
        assert!(text.contains("No changes") && text.contains("Edit a file"), "{text}");
        let plain = tempfile::tempdir().unwrap();
        let (harness, mut diff) = loaded(plain.path());
        assert!(harness.draw(&mut diff, 80, 4).0.contains("Not a git repository"));
        assert_eq!(diff.kind(), "project_diff");
        assert_eq!(diff.tab(&harness.render_cx(false)).title, "Project Diff");
    }

    #[test]
    fn edits_on_disk_are_read_again_and_the_cursor_stays_on_its_line() {
        let dir = repo();
        std::fs::write(dir.path().join("a.txt"), "one\nTWO\n").unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn answer() -> u32 {\n    43\n}\n").unwrap();
        let (mut harness, mut diff) = loaded(dir.path());
        act(&mut harness, &mut diff, "project_diff::NextFile");
        act(&mut harness, &mut diff, "project_diff::NextHunk");
        act(&mut harness, &mut diff, "project_diff::LineDown");
        act(&mut harness, &mut diff, "project_diff::LineDown");
        let before = diff.rows[diff.cursor].clone();
        // A line added to the first file moves everything below it down one row.
        std::fs::write(dir.path().join("a.txt"), "one\nTWO\nmore\n").unwrap();
        diff.paths_changed(&FsChange::of(vec![dir.path().join("a.txt")]), &mut harness.cx());
        assert!(diff.task.is_some(), "a fresh read started");
        until(&mut harness, &mut diff, "the new read", |d| d.rows.iter().any(|r| matches!(r, DiffRow::Line { text, .. } if text == "more")));
        assert_eq!(diff.rows[diff.cursor], before, "the cursor is on the same line of the same file");
    }

    #[test]
    fn binary_and_huge_files_are_summarised() {
        let dir = repo();
        std::fs::write(dir.path().join("pic.bin"), [0u8, 159, 146, 150, 0, 1, 2]).unwrap();
        let mut huge = String::new();
        for i in 0..(MAX_FILE_ROWS + 50) {
            huge.push_str(&format!("{i}\n"));
        }
        std::fs::write(dir.path().join("huge.txt"), huge).unwrap();
        std::fs::write(dir.path().join("empty.txt"), "").unwrap();
        let (harness, mut diff) = loaded(dir.path());
        let text = harness.draw(&mut diff, 90, 20).0;
        assert!(text.contains("Binary file"), "{text}");
        assert!(text.contains("too long to show here"), "{text}");
        assert!(text.contains("Empty file"), "{text}");
        assert!(diff.rows.len() < 40, "no lines of the huge file were kept: {}", diff.rows.len());
    }

    #[test]
    fn staging_and_the_commit_keys() {
        let dir = repo();
        std::fs::write(dir.path().join("a.txt"), "one\nTWO\n").unwrap();
        let (mut harness, mut diff) = loaded(dir.path());
        assert!(act(&mut harness, &mut diff, "git::ToggleStaged"));
        let started = Instant::now();
        while !harness.project.status_of(&dir.path().join("a.txt")).is_some_and(|e| e.is_fully_staged()) {
            harness.project.poll();
            assert!(started.elapsed() < Duration::from_secs(20), "never staged");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(harness.draw(&mut diff, 70, 4).0.lines().next().unwrap().contains("M a.txt"));
        harness.take_effects();
        assert!(act(&mut harness, &mut diff, "git::Commit"));
        assert_eq!(
            harness.take_effects(),
            vec![
                Effect::ShowPanel(PanelId::Git),
                Effect::Focus(Focus::Panel(PanelId::Git)),
                Effect::Dispatch(Action::new("git::Commit"))
            ]
        );
    }

    #[test]
    fn restore_and_next_asks_then_discards_and_lands_on_the_next_file() {
        let dir = repo();
        std::fs::write(dir.path().join("a.txt"), "one\nTWO\n").unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn answer() -> u32 {\n    43\n}\n").unwrap();
        let (mut harness, mut diff) = loaded(dir.path());
        assert!(act(&mut harness, &mut diff, "git::RestoreAndNext"));
        let effects = harness.take_effects();
        let Effect::Ask(question) = &effects[0] else { panic!("asked: {effects:?}") };
        let Effect::Dispatch(confirm) = &question.answers[0].1[0] else { panic!("dispatches") };
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "one\nTWO\n");
        diff.handle_action(confirm, &mut harness.cx());
        until(&mut harness, &mut diff, "a.txt gone from the diff", |d| d.files.len() == 1 && d.after_discard.is_none());
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "one\ntwo\n");
        assert_eq!(diff.files[0].path, Path::new("src/lib.rs"));
        assert!(matches!(diff.rows[diff.cursor], DiffRow::File(0)), "the next file is under the cursor");
        assert!(harness.take_effects().iter().any(|e| matches!(e, Effect::PathsChanged(_))));
    }

    #[test]
    fn narrow_and_short_views_do_not_panic() {
        let dir = repo();
        std::fs::write(dir.path().join("a-long-file-name-for-the-header.txt"), "x\n".repeat(30)).unwrap();
        let (harness, mut diff) = loaded(dir.path());
        for (w, h) in [(8, 1), (10, 3), (24, 6), (120, 2), (50, 40)] {
            harness.draw(&mut diff, w, h);
        }
    }

    #[test]
    fn copy_line_copies_code_or_file_path_to_clipboard() {
        let dir = repo();
        std::fs::write(dir.path().join("a.txt"), "one\nTWO\n").unwrap();
        let (mut harness, mut diff) = loaded(dir.path());

        // On the file header row, CopyLine copies the relative path.
        diff.cursor = 0;
        assert!(act(&mut harness, &mut diff, "project_diff::CopyLine"));
        assert_eq!(harness.clipboard.get().map(|c| c.text), Some("a.txt".to_string()));

        // On a diff line row, CopyLine copies the raw line without +/- prefix.
        let added_row = diff
            .rows
            .iter()
            .position(|r| matches!(r, DiffRow::Line { text, .. } if text == "TWO"))
            .expect("added line in diff");
        diff.cursor = added_row;
        assert!(act(&mut harness, &mut diff, "project_diff::CopyLine"));
        assert_eq!(harness.clipboard.get().map(|c| c.text), Some("TWO".to_string()));
    }

    #[test]
    fn find_bar_searches_and_steps_through_diff_matches() {
        let dir = repo();
        std::fs::write(dir.path().join("a.txt"), "alpha\nbeta\nALPHA\n").unwrap();
        let (mut harness, mut diff) = loaded(dir.path());

        assert!(act(&mut harness, &mut diff, "buffer_search::Deploy"));
        assert!(diff.find_focused());
        let mut ctxs = Vec::new();
        diff.contexts(&mut ctxs);
        assert!(ctxs.iter().all(|c| c.name != "ProjectDiff"), "find bar must suppress ProjectDiff single-letter bindings: {ctxs:?}");
        assert!(ctxs.iter().any(|c| c.name == "BufferSearchBar"));
        for ch in "alpha".chars() {
            assert!(diff.handle_key(&KeyEvent::typed(ch), &mut harness.cx()));
        }
        // Should match both "alpha" and "ALPHA" (case-insensitive by default).
        let bar = diff.find.as_ref().unwrap();
        assert_eq!(bar.matches.len(), 2);
        let first = diff.cursor;
        assert!(matches!(&diff.rows[first], DiffRow::Line { text, .. } if text == "alpha"));

        assert!(act(&mut harness, &mut diff, "search::SelectNextMatch"));
        let second = diff.cursor;
        assert_ne!(first, second);
        assert!(matches!(&diff.rows[second], DiffRow::Line { text, .. } if text == "ALPHA"));

        assert!(act(&mut harness, &mut diff, "search::SelectPreviousMatch"));
        assert_eq!(diff.cursor, first);

        // Renders cleanly with the find bar at the top.
        let text = harness.draw(&mut diff, 60, 10).0;
        assert!(text.lines().next().unwrap().contains("alpha"), "{text}");

        assert!(act(&mut harness, &mut diff, "buffer_search::Dismiss"));
        assert!(diff.find.is_none());
    }
}
