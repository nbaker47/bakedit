//! Project search (`pane::DeploySearch`, cmd-shift-f): find text in every file of the
//! project, results grouped by file, in one tab. Search and replace (cmd-shift-h) adds a
//! replace row.
//!
//! # What the app calls (these signatures stay)
//!
//! * `ProjectSearch::new(replace: bool, cx: &mut Cx<'_>) -> ProjectSearch`. `kind()` is
//!   `"project_search"`: the shell keeps one and brings it back. `replace` is whether it was
//!   opened for search and replace.
//! * `ProjectSearch::in_directory(dir, replace, cx)` and `ProjectSearch::set_include`: a
//!   search that starts limited to one folder (`project_panel::NewSearchInDirectory`).
//!   `search::FocusSearch` carries the same: `{"query": "...", "include": "src/app",
//!   "replace": true}`, every key optional. The shell sends it when the tab it was opened
//!   from had a selection or a word at the cursor, and when the project panel asks for a
//!   search in a folder (`pane::DeploySearch {"include": "<folder relative to the root>"}`).
//!
//! # What it does
//!
//! The query row has the case, whole word and regex toggles; the replace row (only while
//! replace is on) has the replacement and the replace buttons; the filter row has the
//! include and exclude globs (comma separated, as in Zed). The search runs as you type
//! (after a short pause) on the search worker threads and streams in; a new query cancels
//! the old one. Results are grouped by file, in path order, every line with its matches
//! highlighted; a search stops at [`bakedit_project::search::DEFAULT_MAX_RESULTS`] matches
//! and says so.
//!
//! Keys, by what has the keyboard:
//!
//! * a field (`ProjectSearchView > ProjectSearchBar{in_replace} > Editor{mode=single_line}`):
//!   typing and the usual line editing, tab and shift-tab to the next and previous field,
//!   up and down through the query history (`search::PreviousHistoryQuery` and
//!   `NextHistoryQuery`), enter to search now and move into the results, cmd-enter to open
//!   the first match, escape (`project_search::ToggleFocus`) into the results. In the
//!   replace field enter is `search::ReplaceNext` and cmd-enter `search::ReplaceAll`.
//! * the results (`ProjectSearchView`): `menu::SelectNext`/`SelectPrevious` by match with the
//!   file headers skipped (up from the first match returns to the query), enter opens the
//!   match as a preview tab and `menu::SecondaryConfirm` (cmd-enter) opens it and moves the
//!   keyboard to the editor. `project_search::CollapseEntry`/`ExpandEntry` (left, right)
//!   fold a file's results like the file tree; `CollapseAllEntries`/`ExpandAllEntries`.
//!
//! `search::ReplaceNext` replaces the first match on the selected result line, in its file,
//! and moves on. `search::ReplaceAll` asks first, then rewrites every match of the search
//! (the files are written through [`bakedit_project::replace_all`], off the UI thread) and
//! reports how many it replaced. Both report the files they wrote with
//! [`Effect::PathsChanged`], so open tabs reload.

use std::any::Any;
use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bakedit_core::{Action, KeyEvent, MouseButton, MouseEvent, MouseKind, Rgb};
use bakedit_project::{search_with_wake, LineMatch, ReplaceOutcome, ReplaceResult, SearchError, SearchEvent, SearchHandle, SearchOptions};
use ratatui::layout::{Position, Rect};
use unicode_width::UnicodeWidthChar;

use super::search_replace::{replace_one, Target};
use crate::background::Task;
use crate::chrome::Tab;
use crate::icons::file_icon;
use crate::line_input::{InputOutcome, LineInput};
use crate::paint::{self, ScrollMarks};
use crate::project::SearchKind;
use crate::settings::Icons;
use crate::ui::{self, Canvas, Style};
use crate::view::{Context, Cursor, CursorShape, Cx, Effect, Item, Question, RenderCx, View};

/// Rows a wheel step scrolls.
const WHEEL_ROWS: usize = 3;
/// How long typing must pause before the search runs.
const DEBOUNCE: Duration = Duration::from_millis(150);
/// Columns left of the fields: room for the replace chevron.
const LEFT: u16 = 3;
/// Context characters kept before a match when a long line is cut around it.
const CONTEXT_BEFORE: usize = 10;

/// What has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    Query,
    Replace,
    Include,
    Exclude,
    Results,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Opt {
    Case,
    Word,
    Regex,
}

/// Something in the header that a click does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    Field(Part),
    Toggle(Opt),
    ReplaceRow,
    ReplaceNext,
    ReplaceAll,
}

#[derive(Debug, Clone, PartialEq)]
enum State {
    /// Nothing searched for yet.
    Idle,
    Running,
    Done { truncated: bool },
    Failed(String),
}

/// One file's results.
struct FileResults {
    /// Relative to the project root.
    path: PathBuf,
    matches: Vec<LineMatch>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResultRow {
    File(usize),
    Match(usize, usize),
}

/// A row's identity, which survives re-sorting and re-folding: its file and match.
type RowKey = (PathBuf, Option<usize>);

/// Replacing, running on a worker.
enum Job {
    Next { task: Task<Result<String, String>>, path: PathBuf, line: u32, clamp: usize, start: usize, old_len: usize },
    All(Task<Result<Vec<ReplaceResult>, SearchError>>),
}

/// The project search tab.
pub struct ProjectSearch {
    query: LineInput,
    replacement: LineInput,
    include: LineInput,
    exclude: LineInput,
    replace_open: bool,
    focus: Part,
    case_sensitive: bool,
    whole_word: bool,
    regex: bool,
    handle: Option<SearchHandle>,
    /// The options the shown results are for.
    searched: Option<SearchOptions>,
    /// When the pending (typed-ahead) search is to run.
    due: Option<Instant>,
    /// Enter was pressed before there were results: move into them when they arrive.
    focus_results_when_ready: bool,
    files: Vec<FileResults>,
    collapsed: HashSet<PathBuf>,
    rows: Vec<ResultRow>,
    /// The row the user chose; `None` means the first match.
    selected_key: Option<RowKey>,
    selected: usize,
    /// Digits of the largest line number, for the number column.
    number_digits: usize,
    state: State,
    scroll: usize,
    /// Scroll the selected row into view on the next draw (after a key, not a wheel).
    reveal: bool,
    /// Which history entry the query shows, and what was typed before browsing.
    history_at: Option<usize>,
    draft: String,
    job: Option<Job>,
    /// Where the header was last drawn.
    hits: Vec<(Rect, Hit)>,
    list: Rect,
}

impl ProjectSearch {
    /// An empty search; `replace` when it was opened to search and replace.
    pub fn new(replace: bool, cx: &mut Cx<'_>) -> ProjectSearch {
        ProjectSearch {
            query: LineInput::new(),
            replacement: LineInput::new(),
            include: LineInput::new(),
            exclude: LineInput::new(),
            replace_open: replace,
            focus: Part::Query,
            case_sensitive: cx.settings.search_case_sensitive,
            whole_word: cx.settings.search_whole_word,
            regex: cx.settings.search_regex,
            handle: None,
            searched: None,
            due: None,
            focus_results_when_ready: false,
            files: Vec::new(),
            collapsed: HashSet::new(),
            rows: Vec::new(),
            selected_key: None,
            selected: 0,
            number_digits: 2,
            state: State::Idle,
            scroll: 0,
            reveal: false,
            history_at: None,
            draft: String::new(),
            job: None,
            hits: Vec::new(),
            list: Rect::default(),
        }
    }

    /// A search limited to one folder: its include field starts as `dir` (made relative to
    /// the project root). The folder being the root itself limits nothing.
    pub fn in_directory(dir: &Path, replace: bool, cx: &mut Cx<'_>) -> ProjectSearch {
        let mut search = ProjectSearch::new(replace, cx);
        search.set_include(&include_for(dir, cx.root), cx);
        search
    }

    /// Put `globs` (comma separated, as typed in the include field) in the include field
    /// and search again if there is a query.
    pub fn set_include(&mut self, globs: &str, cx: &mut Cx<'_>) {
        self.include.set_text(globs);
        self.run(cx);
    }

    // ---- searching ----------------------------------------------------------------------

    fn options(&self) -> SearchOptions {
        SearchOptions {
            regex: self.regex,
            case_sensitive: self.case_sensitive,
            whole_word: self.whole_word,
            include: split_globs(self.include.text()),
            exclude: split_globs(self.exclude.text()),
            ..SearchOptions::new(self.query.text())
        }
    }

    /// The results on screen are for exactly what the fields say now.
    fn is_current(&self) -> bool {
        self.due.is_none() && self.state != State::Running && self.searched.as_ref() == Some(&self.options())
    }

    /// Search again after the typing pause.
    fn schedule(&mut self, cx: &mut Cx<'_>) {
        let due = cx.now + DEBOUNCE;
        self.due = Some(due);
        cx.project.waker().wake_at(due);
    }

    /// Start the search for what the fields say now, dropping whatever was running.
    fn run(&mut self, cx: &mut Cx<'_>) {
        self.due = None;
        self.handle = None;
        self.searched = None;
        self.files.clear();
        self.collapsed.clear();
        self.rows.clear();
        self.selected_key = None;
        self.selected = 0;
        self.scroll = 0;
        self.number_digits = 2;
        if self.query.is_empty() {
            self.state = State::Idle;
            self.focus_results_when_ready = false;
            return;
        }
        let options = self.options();
        let waker = cx.project.waker().clone();
        match search_with_wake(cx.root, options.clone(), Box::new(move || waker.wake())) {
            Ok(handle) => {
                self.handle = Some(handle);
                self.state = State::Running;
                self.searched = Some(options);
            }
            Err(error) => {
                self.state = State::Failed(describe(&error));
                self.focus_results_when_ready = false;
            }
        }
    }

    /// Take in what the search threads found. Returns true if anything arrived.
    fn collect(&mut self) -> bool {
        let Some(handle) = &self.handle else { return false };
        let mut arrived = false;
        let mut done = None;
        while let Ok(event) = handle.events().try_recv() {
            arrived = true;
            match event {
                SearchEvent::File { path, matches } => self.files.push(FileResults { path, matches }),
                SearchEvent::Done { truncated, .. } => done = Some(State::Done { truncated }),
            }
        }
        if !arrived {
            return false;
        }
        self.rebuild_rows();
        if let Some(done) = done {
            self.state = done;
            self.handle = None;
        }
        if self.focus_results_when_ready && !self.files.is_empty() && self.focus != Part::Results {
            self.focus_results_when_ready = false;
            self.focus = Part::Results;
            self.reveal = true;
        } else if self.state != State::Running {
            self.focus_results_when_ready = false;
        }
        true
    }

    fn total_matches(&self) -> usize {
        self.files.iter().flat_map(|file| &file.matches).map(|line| line.ranges.len().max(1)).sum()
    }

    // ---- rows and selection -------------------------------------------------------------

    /// Re-sort the files (they arrive in any order) and lay out the rows, keeping the
    /// selection on the row it was on.
    fn rebuild_rows(&mut self) {
        self.files.sort_by(|a, b| a.path.cmp(&b.path));
        self.rows.clear();
        let mut widest = 0;
        for (file, results) in self.files.iter().enumerate() {
            self.rows.push(ResultRow::File(file));
            if self.collapsed.contains(&results.path) {
                continue;
            }
            for (index, found) in results.matches.iter().enumerate() {
                widest = widest.max(found.line_number);
                self.rows.push(ResultRow::Match(file, index));
            }
        }
        self.number_digits = self.number_digits.max(widest.to_string().len());
        self.selected = match &self.selected_key {
            Some(key) => self
                .position_of(key)
                .or_else(|| self.position_of(&(key.0.clone(), None)))
                .unwrap_or_else(|| self.first_selectable()),
            None => self.first_selectable(),
        };
    }

    fn key_of(&self, row: ResultRow) -> RowKey {
        match row {
            ResultRow::File(file) => (self.files[file].path.clone(), None),
            ResultRow::Match(file, index) => (self.files[file].path.clone(), Some(index)),
        }
    }

    fn position_of(&self, key: &RowKey) -> Option<usize> {
        self.rows.iter().position(|row| &self.key_of(*row) == key)
    }

    /// Whether arrow keys stop on this row: matches, and the headers of folded files.
    fn is_selectable(&self, row: ResultRow) -> bool {
        match row {
            ResultRow::Match(..) => true,
            ResultRow::File(file) => self.collapsed.contains(&self.files[file].path),
        }
    }

    fn first_selectable(&self) -> usize {
        self.rows.iter().position(|row| self.is_selectable(*row)).unwrap_or(0)
    }

    fn select(&mut self, index: usize) {
        if let Some(row) = self.rows.get(index).copied() {
            self.selected = index;
            self.selected_key = Some(self.key_of(row));
            self.reveal = true;
        }
    }

    /// Move to the next (`forward`) or previous row arrow keys stop on. Going up from the
    /// first one leaves the results for the query.
    fn step(&mut self, forward: bool) {
        let target = if forward {
            (self.selected + 1..self.rows.len()).find(|&i| self.is_selectable(self.rows[i]))
        } else {
            (0..self.selected).rev().find(|&i| self.is_selectable(self.rows[i]))
        };
        match target {
            Some(index) => self.select(index),
            None if !forward => self.focus = Part::Query,
            None => {}
        }
    }

    /// The row the user is on, as a file and match (a folded file's header counts as its
    /// first match).
    fn current(&self) -> Option<(usize, Option<usize>)> {
        match *self.rows.get(self.selected)? {
            ResultRow::Match(file, index) => Some((file, Some(index))),
            ResultRow::File(file) => Some((file, if self.files[file].matches.is_empty() { None } else { Some(0) })),
        }
    }

    fn fold(&mut self, action: &str) {
        let Some(row) = self.rows.get(self.selected).copied() else { return };
        match (action, row) {
            ("CollapseEntry", ResultRow::Match(file, _)) => {
                let header = self.position_of(&(self.files[file].path.clone(), None)).unwrap_or(self.selected);
                self.select(header);
            }
            ("CollapseEntry", ResultRow::File(file)) => {
                self.collapsed.insert(self.files[file].path.clone());
                self.selected_key = Some((self.files[file].path.clone(), None));
                self.rebuild_rows();
                self.reveal = true;
            }
            ("ExpandEntry", ResultRow::File(file)) => {
                if self.collapsed.remove(&self.files[file].path) {
                    self.selected_key = Some((self.files[file].path.clone(), None));
                    self.rebuild_rows();
                } else if !self.files[file].matches.is_empty() {
                    self.select(self.selected + 1);
                }
            }
            ("CollapseAllEntries", ResultRow::File(file) | ResultRow::Match(file, _)) => {
                self.collapsed = self.files.iter().map(|f| f.path.clone()).collect();
                self.selected_key = Some((self.files[file].path.clone(), None));
                self.rebuild_rows();
                self.reveal = true;
            }
            ("ExpandAllEntries", _) => {
                self.collapsed.clear();
                self.rebuild_rows();
                self.reveal = true;
            }
            _ => {}
        }
    }

    fn toggle_file_at(&mut self, row_index: usize) {
        if let Some(ResultRow::File(file)) = self.rows.get(row_index).copied() {
            let path = self.files[file].path.clone();
            if !self.collapsed.remove(&path) {
                self.collapsed.insert(path.clone());
            }
            self.selected_key = Some((path, None));
            self.rebuild_rows();
        }
    }

    // ---- opening ------------------------------------------------------------------------

    /// Open the selected result in the editor. `focus_editor` moves the keyboard there and
    /// keeps the tab; otherwise the file opens as a preview.
    fn open_selected(&mut self, focus_editor: bool, cx: &mut Cx<'_>) -> bool {
        let Some((file, index)) = self.current() else { return false };
        let line_match = index.and_then(|i| self.files[file].matches.get(i));
        let path = cx.root.join(&self.files[file].path);
        let line = line_match.map(|m| m.line_number);
        let column = line_match.and_then(|m| m.ranges.first().map(|r| (m.clamp_offset + r.start) as u32 + 1));
        self.remember_query(cx);
        cx.push(Effect::OpenFile { path, line, column, preview: !focus_editor, focus: focus_editor });
        true
    }

    /// Keep the query in the history, once it has found something worth coming back to.
    fn remember_query(&self, cx: &Cx<'_>) {
        if !self.files.is_empty() && self.searched.as_ref().is_some_and(|s| s.pattern == self.query.text()) {
            cx.project.remember_search(SearchKind::Project, self.query.text());
        }
    }

    // ---- replacing ----------------------------------------------------------------------

    fn replace_next(&mut self, cx: &mut Cx<'_>) -> bool {
        if !self.replace_open {
            self.replace_open = true;
            self.focus = Part::Replace;
            return true;
        }
        if self.job.is_some() {
            return true;
        }
        let Some(options) = self.searched.clone().filter(|_| self.is_current()) else {
            self.run(cx);
            cx.status("Searching again: check the results, then replace");
            return true;
        };
        let Some((file, Some(index))) = self.current() else {
            cx.status("No match to replace");
            return true;
        };
        let found = &self.files[file].matches[index];
        let Some(range) = found.ranges.first().cloned() else { return true };
        let expected: String = found.line.chars().skip(range.start).take(range.len()).collect();
        let target = Target { line: found.line_number, start: found.clamp_offset + range.start, end: found.clamp_offset + range.end, expected };
        let path = self.files[file].path.clone();
        let (line, clamp, start, old_len) = (found.line_number, found.clamp_offset, range.start, range.len());
        let absolute = cx.root.join(&path);
        let replacement = self.replacement.text().to_string();
        let task = cx.project.spawn(move || replace_one(&absolute, &options, &target, &replacement));
        self.job = Some(Job::Next { task, path, line, clamp, start, old_len });
        true
    }

    fn replace_all(&mut self, cx: &mut Cx<'_>) -> bool {
        if !self.replace_open {
            self.replace_open = true;
            self.focus = Part::Replace;
            return true;
        }
        if self.job.is_some() {
            return true;
        }
        if self.query.is_empty() {
            cx.status("Type what to replace first");
            return true;
        }
        if !self.is_current() {
            self.run(cx);
            cx.status("Searching again: check the results, then replace all");
            return true;
        }
        let results = self.total_matches();
        if results == 0 {
            cx.status("No matches to replace");
            return true;
        }
        let at_least = if matches!(self.state, State::Done { truncated: true }) { "at least " } else { "" };
        let replacement = self.replacement.text();
        let into = if replacement.is_empty() { "nothing".to_string() } else { format!("\"{}\"", ui::truncate_end(replacement, 40)) };
        cx.push(Effect::Ask(Question {
            message: format!("Replace {at_least}{} in {} with {into}?", plural(results, "match", "matches"), plural(self.files.len(), "file", "files")),
            detail: Some("This rewrites the files on disk.".to_string()),
            answers: vec![
                ("Replace all".to_string(), vec![Effect::Dispatch(Action::new("project_search::ConfirmReplaceAll"))]),
                ("Cancel".to_string(), Vec::new()),
            ],
        }));
        true
    }

    /// The user said yes to [`ProjectSearch::replace_all`].
    fn confirm_replace_all(&mut self, cx: &mut Cx<'_>) -> bool {
        let Some(options) = self.searched.clone().filter(|_| self.is_current()) else {
            cx.status("The search changed: replace again");
            return true;
        };
        let root = cx.root.to_path_buf();
        let replacement = self.replacement.text().to_string();
        self.job = Some(Job::All(cx.project.spawn(move || bakedit_project::replace_all(&root, &options, &replacement))));
        true
    }

    /// Collect a finished replacement. Returns true if there was one.
    fn poll_job(&mut self, cx: &mut Cx<'_>) -> bool {
        match &mut self.job {
            Some(Job::Next { task, .. }) => {
                let Some(result) = task.poll() else { return false };
                let Some(Job::Next { path, line, clamp, start, old_len, .. }) = self.job.take() else { return false };
                match result {
                    Ok(new_text) => {
                        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        cx.push(Effect::PathsChanged(vec![cx.root.join(&path)]));
                        cx.status(format!("Replaced in {name}:{line}"));
                        self.apply_replacement(&path, (line, clamp, start, old_len), &new_text, cx);
                    }
                    Err(message) => cx.error(message),
                }
                true
            }
            Some(Job::All(task)) => {
                let Some(result) = task.poll() else { return false };
                self.job = None;
                self.finish_replace_all(result, cx);
                true
            }
            None => false,
        }
    }

    /// Show the effect of replacing one match: its text changes, it leaves the list, and the
    /// selection stays where it was so the next match is the one under it. `at` is the
    /// match's line, clamp offset, start and length.
    fn apply_replacement(&mut self, path: &Path, at: (u32, usize, usize, usize), new_text: &str, cx: &mut Cx<'_>) {
        let (line, clamp, start, old_len) = at;
        let Some(file) = self.files.iter().position(|f| f.path == path) else { return };
        let same_line = self.files[file].matches.iter().filter(|m| m.line_number == line).count();
        let found = self.files[file].matches.iter().position(|m| m.line_number == line && m.clamp_offset == clamp && m.ranges.first().is_some_and(|r| r.start == start));
        let Some(index) = found.filter(|_| same_line == 1) else {
            // A very long line is reported in windows whose offsets all moved: start over.
            self.run(cx);
            return;
        };
        let row = self.selected;
        let entry = &mut self.files[file].matches[index];
        let delta = new_text.chars().count() as isize - old_len as isize;
        let chars: Vec<char> = entry.line.chars().collect();
        let from = start.min(chars.len());
        let to = (start + old_len).min(chars.len());
        entry.line = chars[..from].iter().copied().chain(new_text.chars()).chain(chars[to..].iter().copied()).collect();
        entry.ranges.remove(0);
        for range in &mut entry.ranges {
            range.start = (range.start as isize + delta).max(0) as usize;
            range.end = (range.end as isize + delta).max(0) as usize;
        }
        if entry.ranges.is_empty() {
            self.files[file].matches.remove(index);
        }
        if self.files[file].matches.is_empty() {
            self.collapsed.remove(path);
            self.files.remove(file);
        }
        self.selected_key = None;
        self.rebuild_rows();
        // Stay on the row the user was on, or the nearest one that can be chosen.
        let after = row..self.rows.len();
        let before = (0..row.min(self.rows.len())).rev();
        if let Some(target) = after.chain(before).find(|&i| self.is_selectable(self.rows[i])) {
            self.select(target);
        }
    }

    fn finish_replace_all(&mut self, result: Result<Vec<ReplaceResult>, SearchError>, cx: &mut Cx<'_>) {
        let results = match result {
            Ok(results) => results,
            Err(error) => {
                cx.error(describe(&error));
                return;
            }
        };
        let (mut replaced, mut files, mut skipped, mut failed) = (0, 0, 0, 0);
        let mut changed = Vec::new();
        for result in &results {
            match &result.outcome {
                ReplaceOutcome::Replaced { replacements } => {
                    replaced += replacements;
                    files += 1;
                    changed.push(cx.root.join(&result.path));
                }
                ReplaceOutcome::Skipped(_) => skipped += 1,
                ReplaceOutcome::Failed(_) => failed += 1,
            }
        }
        if !changed.is_empty() {
            cx.push(Effect::PathsChanged(changed));
        }
        let mut message = if files == 0 { "Nothing was replaced".to_string() } else { format!("Replaced {} in {}", plural(replaced, "match", "matches"), plural(files, "file", "files")) };
        if skipped > 0 {
            message.push_str(&format!("; skipped {} (changed on disk, not UTF-8 or too large)", plural(skipped, "file", "files")));
        }
        if failed > 0 {
            message.push_str(&format!("; {} could not be written", plural(failed, "file", "files")));
        }
        if failed > 0 || (skipped > 0 && files == 0) {
            cx.error(message);
        } else {
            cx.status(message);
        }
        self.run(cx);
    }

    // ---- fields -------------------------------------------------------------------------

    fn field_mut(&mut self, part: Part) -> Option<&mut LineInput> {
        match part {
            Part::Query => Some(&mut self.query),
            Part::Replace => Some(&mut self.replacement),
            Part::Include => Some(&mut self.include),
            Part::Exclude => Some(&mut self.exclude),
            Part::Results => None,
        }
    }

    /// The parts tab visits, in order.
    fn cycle(&self) -> Vec<Part> {
        let mut parts = vec![Part::Query];
        if self.replace_open {
            parts.push(Part::Replace);
        }
        parts.extend([Part::Include, Part::Exclude]);
        if !self.rows.is_empty() {
            parts.push(Part::Results);
        }
        parts
    }

    fn move_focus(&mut self, forward: bool) {
        let parts = self.cycle();
        let at = parts.iter().position(|p| *p == self.focus).unwrap_or(0);
        let next = if forward { (at + 1) % parts.len() } else { (at + parts.len() - 1) % parts.len() };
        self.focus = parts[next];
        self.reveal = true;
    }

    /// An edit to the focused field: the replacement changes nothing about the results; the
    /// others change what is searched.
    fn edited(&mut self, cx: &mut Cx<'_>) {
        self.history_at = None;
        if self.focus != Part::Replace {
            self.schedule(cx);
        }
    }

    fn history_step(&mut self, older: bool, cx: &mut Cx<'_>) -> bool {
        if self.focus != Part::Query {
            // The other fields keep no history; down from them goes into the results.
            if !older && !self.rows.is_empty() && self.focus != Part::Replace {
                self.focus = Part::Results;
                self.reveal = true;
            }
            return true;
        }
        let history = cx.project.search_history(SearchKind::Project);
        let next = match (self.history_at, older) {
            (None, true) => {
                self.draft = self.query.text().to_string();
                usize::from(history.first().is_some_and(|q| q == &self.draft))
            }
            (Some(at), true) => at + 1,
            (Some(0), false) => {
                self.history_at = None;
                let draft = std::mem::take(&mut self.draft);
                self.query.set_text(&draft);
                self.schedule(cx);
                return true;
            }
            (Some(at), false) => at - 1,
            (None, false) => return true,
        };
        if let Some(entry) = history.get(next) {
            self.history_at = Some(next);
            self.query.set_text(entry);
            self.schedule(cx);
        }
        true
    }

    fn toggle(&mut self, opt: Opt, cx: &mut Cx<'_>) {
        match opt {
            Opt::Case => self.case_sensitive = !self.case_sensitive,
            Opt::Word => self.whole_word = !self.whole_word,
            Opt::Regex => self.regex = !self.regex,
        }
        self.run(cx);
    }

    fn toggle_replace(&mut self) {
        self.replace_open = !self.replace_open;
        if self.replace_open {
            self.focus = Part::Replace;
        } else if self.focus == Part::Replace {
            self.focus = Part::Query;
        }
    }

    fn confirm(&mut self, cx: &mut Cx<'_>) -> bool {
        if self.focus == Part::Results {
            return self.open_selected(false, cx);
        }
        if self.query.is_empty() {
            return true;
        }
        if self.is_current() {
            if self.rows.is_empty() {
                return true;
            }
            self.remember_query(cx);
            self.focus = Part::Results;
            self.reveal = true;
        } else {
            self.run(cx);
            self.focus_results_when_ready = true;
        }
        true
    }

    // ---- what the status row and the empty list say -------------------------------------

    fn status_line(&self) -> (String, bool) {
        if let Some(job) = &self.job {
            return (if matches!(job, Job::All(_)) { "Replacing in all files…" } else { "Replacing…" }.to_string(), false);
        }
        let counts = || format!("{} in {}", plural(self.total_matches(), "result", "results"), plural(self.files.len(), "file", "files"));
        match &self.state {
            State::Idle => (String::new(), false),
            State::Running if self.files.is_empty() => ("Searching…".to_string(), false),
            State::Running => (format!("Searching…  {}", counts()), false),
            State::Done { .. } if self.files.is_empty() => {
                let pattern = self.searched.as_ref().map(|s| s.pattern.as_str()).unwrap_or_default();
                (format!("No results for \"{}\"", ui::truncate_end(pattern, 60)), false)
            }
            State::Done { truncated: true } => (format!("Showing the first {}; narrow the search to see the rest", counts()), false),
            State::Done { truncated: false } => (counts(), false),
            State::Failed(message) => (message.clone(), true),
        }
    }

    /// What the empty list says: why it is empty and what to do about it.
    fn empty_message(&self) -> Vec<String> {
        match &self.state {
            State::Idle => vec![
                "Nothing searched yet.".to_string(),
                "Type above to search every file in the project; files ignored by git are skipped.".to_string(),
                "The include and exclude fields take globs such as src/**, *.rs".to_string(),
            ],
            State::Done { .. } if self.files.is_empty() => {
                let mut lines = vec!["Nothing in the project matches.".to_string()];
                let mut limits = Vec::new();
                if self.case_sensitive {
                    limits.push("case is matched (Aa)");
                }
                if self.whole_word {
                    limits.push("whole words only (ab)");
                }
                if self.regex {
                    limits.push("the query is a regular expression (.*)");
                }
                if !self.include.is_empty() {
                    limits.push("only files matching the include globs are searched");
                }
                if !self.exclude.is_empty() {
                    limits.push("files matching the exclude globs are skipped");
                }
                if !limits.is_empty() {
                    lines.push(format!("Narrowed by: {}.", limits.join("; ")));
                }
                lines
            }
            State::Failed(_) if self.regex => vec!["The query is not a valid regular expression.".to_string(), "Fix it above, or turn off .* to search for it as typed.".to_string()],
            State::Failed(_) => vec!["The search could not start; the message above says why.".to_string()],
            _ => Vec::new(),
        }
    }
}

/// The include glob that limits a search to `dir`: its path under `root`, or nothing for
/// the root itself and for a folder outside the project.
fn include_for(dir: &Path, root: &Path) -> String {
    match dir.strip_prefix(root) {
        Ok(relative) => relative.to_string_lossy().trim_matches('/').to_string(),
        Err(_) if dir.is_relative() => dir.to_string_lossy().trim_matches('/').to_string(),
        Err(_) => String::new(),
    }
}

/// `"src/**, *.rs"` as the list of globs it names.
fn split_globs(text: &str) -> Vec<String> {
    text.split(',').map(str::trim).filter(|glob| !glob.is_empty()).map(str::to_string).collect()
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{} {}", thousands(count), if count == 1 { one } else { many })
}

fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A search error as a line for the status row.
fn describe(error: &SearchError) -> String {
    match error {
        SearchError::InvalidPattern(message) => {
            let detail = message.lines().rev().find_map(|line| line.trim().strip_prefix("error:")).map(str::trim).or_else(|| message.lines().next()).unwrap_or(message);
            format!("Invalid pattern: {detail}")
        }
        SearchError::InvalidGlob { glob, message } => format!("Invalid glob \"{glob}\": {}", message.lines().next().unwrap_or_default()),
        SearchError::MultiLine => "A pattern cannot match across lines: search looks at one line at a time".to_string(),
        other => other.to_string(),
    }
}

/// How one result line is shown in `room` columns: the cells to draw, each flagged when it
/// is part of a match, with the leading blanks dropped and the line cut with an ellipsis on
/// either side so the first match stays in view.
fn clip_line(line: &str, clamp_offset: usize, ranges: &[Range<usize>], room: usize) -> Vec<(char, bool)> {
    let chars: Vec<char> = line.chars().collect();
    let first = ranges.first().map(|r| r.start);
    let lead = chars.iter().take_while(|c| c.is_whitespace()).count();
    let base = if clamp_offset > 0 || first.is_some_and(|f| f < lead) { 0 } else { lead };
    let cell_width = |c: char| if c.is_control() { 1 } else { UnicodeWidthChar::width(c).unwrap_or(1).max(1) };
    let mut start = base;
    if let Some(first) = first {
        let end = ranges[0].end.max(first + 1).min(chars.len());
        let columns: usize = chars[start.min(end)..end].iter().map(|c| cell_width(*c)).sum();
        if columns + 1 > room {
            start = first.saturating_sub(CONTEXT_BEFORE).max(base);
        }
    }
    let cut_left = start > base || (clamp_offset > 0 && start == 0);
    let hit = |index: usize| ranges.iter().any(|r| r.start <= index && index < r.end);
    let mut cells: Vec<(char, bool)> = Vec::new();
    let mut used = 0;
    if cut_left {
        cells.push(('…', false));
        used = 1;
    }
    let mut index = start;
    while index < chars.len() {
        let w = cell_width(chars[index]);
        if used + w > room {
            break;
        }
        cells.push((if chars[index].is_control() { ' ' } else { chars[index] }, hit(index)));
        used += w;
        index += 1;
    }
    if index < chars.len() {
        // Something is cut on the right: make room for the ellipsis.
        while used + 1 > room && cells.len() > usize::from(cut_left) {
            if let Some((c, _)) = cells.pop() {
                used -= cell_width(c);
            }
        }
        if used < room {
            cells.push(('…', false));
        }
    }
    cells
}

// ---- drawing the header and the rows -------------------------------------------------------

/// A replace button: where it is, what a click does, its label.
type ReplaceButton = (Rect, Hit, &'static str);

/// Where the parts of the header go, for a width.
struct HeaderLayout {
    chevron: Option<Rect>,
    query: Rect,
    toggles: Vec<(Rect, Opt)>,
    /// The replacement field and the replace buttons.
    replace: Option<(Rect, Vec<ReplaceButton>)>,
    /// Each filter: the label's rectangle and the field's.
    include: (Rect, Rect),
    exclude: (Rect, Rect),
    rows: u16,
}

impl HeaderLayout {
    /// Lay out the header in `area`: query row, replace row if open, filter row(s).
    fn new(area: Rect, replace_open: bool) -> HeaderLayout {
        let padded = area.width >= 48;
        let right = area.right().saturating_sub(1);
        let button = if padded { 4 } else { 2 };
        let mut y = area.y;
        let chevron = (area.width >= 36).then(|| Rect::new(area.x, y, LEFT, 1));
        let left = if chevron.is_some() { area.x + LEFT } else { area.x + 1 };

        let toggles_x = right.saturating_sub(3 * button + 2);
        let toggles = [Opt::Case, Opt::Word, Opt::Regex].iter().enumerate().map(|(i, opt)| (Rect::new(toggles_x + i as u16 * (button + 1), y, button, 1), *opt)).collect();
        let query = Rect::new(left, y, toggles_x.saturating_sub(left + 1), 1);

        let replace = replace_open.then(|| {
            y += 1;
            let all_width = if padded { 5 } else { 3 };
            let next_width = if padded { 9 } else { 7 };
            let all = Rect::new(right.saturating_sub(all_width), y, all_width, 1);
            let next = Rect::new(all.x.saturating_sub(next_width + 1), y, next_width, 1);
            let buttons = vec![(next, Hit::ReplaceNext, "Replace"), (all, Hit::ReplaceAll, "All")];
            (Rect::new(left, y, next.x.saturating_sub(left + 1), 1), buttons)
        });

        y += 1;
        let total = right.saturating_sub(left);
        let label = 8u16;
        let (include, exclude);
        if area.width >= 60 {
            let first = total.saturating_sub(2) / 2;
            let second = total.saturating_sub(2) - first;
            include = (Rect::new(left, y, label, 1), Rect::new(left + label, y, first.saturating_sub(label), 1));
            let at = left + first + 2;
            exclude = (Rect::new(at, y, label, 1), Rect::new(at + label, y, second.saturating_sub(label), 1));
            y += 1;
        } else {
            include = (Rect::new(left, y, label, 1), Rect::new(left + label, y, total.saturating_sub(label), 1));
            y += 1;
            exclude = (Rect::new(left, y, label, 1), Rect::new(left + label, y, total.saturating_sub(label), 1));
            y += 1;
        }
        HeaderLayout { chevron, query, toggles, replace, include, exclude, rows: y - area.y }
    }
}

impl ProjectSearch {
    /// Draw the fields, toggles and buttons and record where a click lands. Returns the
    /// screen column of the focused field's text cursor.
    fn draw_header(&mut self, canvas: &mut Canvas<'_>, area: Rect, layout: &HeaderLayout, cx: &RenderCx<'_>) -> Option<u16> {
        let theme = cx.theme;
        let bar_bg = theme.toolbar_bg;
        let raised = bar_bg.lerp(theme.text, 0.10);
        let raised_focused = bar_bg.lerp(theme.text, 0.18);
        canvas.fill(Rect::new(area.x, area.y, area.width, layout.rows), bar_bg);
        let muted = Style::colors(theme.text_muted, bar_bg);
        let mut cursor = None;

        if let Some(chevron) = layout.chevron {
            let glyph = if self.replace_open { "▾" } else { "▸" };
            canvas.put(chevron.x + 1, chevron.y, glyph, Style::colors(if self.replace_open { theme.text } else { theme.text_muted }, bar_bg));
            self.hits.push((chevron, Hit::ReplaceRow));
        }

        let mut fields: Vec<(Part, Rect, &str)> = vec![(Part::Query, layout.query, "Find in files")];
        if let Some((rect, _)) = &layout.replace {
            fields.push((Part::Replace, *rect, "Replace with"));
        }
        fields.push((Part::Include, layout.include.1, "src/**, *.rs"));
        fields.push((Part::Exclude, layout.exclude.1, "target, *.min.js"));
        for (part, rect, placeholder) in fields {
            if rect.width < 3 {
                continue;
            }
            let focused = self.focus == part && cx.focused;
            let bg = if focused { raised_focused } else { raised };
            canvas.fill(rect, bg);
            let inner = Rect::new(rect.x + 1, rect.y, rect.width - 2, 1);
            let style = Style::colors(theme.text, bg);
            let selection = Style::colors(theme.text, theme.selection_bg);
            let placeholder_style = Style::colors(theme.text_placeholder, bg);
            let column = self.field_mut(part).map(|input| input.render(canvas, inner, style, selection, placeholder, placeholder_style));
            if self.focus == part {
                cursor = column;
            }
            self.hits.push((rect, Hit::Field(part)));
        }
        for (rect, name) in [(layout.include.0, "include"), (layout.exclude.0, "exclude")] {
            canvas.text(rect.x, rect.y, name, muted, rect.width);
        }

        for (rect, opt) in &layout.toggles {
            let (label, on) = match opt {
                Opt::Case => ("Aa", self.case_sensitive),
                Opt::Word => ("ab", self.whole_word),
                Opt::Regex => (".*", self.regex),
            };
            let bg = if on { theme.element_selected } else { bar_bg };
            let mut style = if on { Style::colors(theme.text_accent, bg).bold() } else { Style::colors(theme.text_muted, bg) };
            if *opt == Opt::Word {
                style = style.underline();
            }
            canvas.fill(*rect, bg);
            canvas.text(rect.x + rect.width.saturating_sub(2) / 2, rect.y, label, style, 2);
            self.hits.push((*rect, Hit::Toggle(*opt)));
        }
        if let Some((_, buttons)) = &layout.replace {
            let ready = !self.rows.is_empty() && self.job.is_none();
            for (rect, hit, label) in buttons {
                let fg = if ready { theme.text } else { theme.text_muted };
                canvas.fill(*rect, raised);
                let x = rect.x + rect.width.saturating_sub(ui::width(label) as u16) / 2;
                canvas.text(x, rect.y, label, Style::colors(fg, raised), rect.width);
                self.hits.push((*rect, *hit));
            }
        }
        cursor
    }

    fn draw_results(&mut self, canvas: &mut Canvas<'_>, list: Rect, cx: &RenderCx<'_>) {
        let theme = cx.theme;
        self.list = list;
        let height = list.height as usize;
        if height == 0 {
            return;
        }
        if self.rows.is_empty() {
            for (i, line) in self.empty_message().iter().take(height).enumerate() {
                let color = if i == 0 { theme.text } else { theme.text_muted };
                canvas.text_truncated(list.x + 2, list.y + i as u16, line, Style::colors(color, theme.editor_bg), list.width.saturating_sub(4));
            }
            return;
        }
        if self.reveal {
            self.reveal = false;
            if self.selected < self.scroll {
                self.scroll = self.selected;
            } else if self.selected >= self.scroll + height {
                self.scroll = self.selected + 1 - height;
            }
        }
        self.scroll = self.scroll.min(self.rows.len().saturating_sub(height));
        let scrollbar = self.rows.len() > height;
        let width = list.width.saturating_sub(u16::from(scrollbar));
        for (offset, row) in self.rows.iter().enumerate().skip(self.scroll).take(height) {
            let y = list.y + (offset - self.scroll) as u16;
            let marked = offset == self.selected;
            let bg = if marked && self.focus == Part::Results && cx.focused {
                theme.element_selected
            } else if marked && (self.focus == Part::Results || self.replace_open) {
                theme.element_hover
            } else {
                theme.editor_bg
            };
            canvas.fill(Rect::new(list.x, y, width, 1), bg);
            match *row {
                ResultRow::File(file) => self.draw_file_row(canvas, list.x, y, width, file, bg, cx),
                ResultRow::Match(file, index) => self.draw_match_row(canvas, list.x, y, width, (file, index), bg, cx.theme),
            }
        }
        if scrollbar {
            paint::scrollbar(canvas, list.right() - 1, list.y, list.height, self.rows.len(), self.scroll, height, &ScrollMarks::default(), theme);
        }
    }

    /// A file header: fold chevron, icon, name, folder (muted) and the match count at the
    /// right edge.
    #[allow(clippy::too_many_arguments)]
    fn draw_file_row(&self, canvas: &mut Canvas<'_>, x: u16, y: u16, width: u16, file: usize, bg: Rgb, cx: &RenderCx<'_>) {
        let theme = cx.theme;
        let results = &self.files[file];
        let folded = self.collapsed.contains(&results.path);
        let right = x + width;
        let name = results.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let dir = results.path.parent().map(|p| p.to_string_lossy().into_owned()).filter(|d| !d.is_empty());
        let count: usize = results.matches.iter().map(|m| m.ranges.len().max(1)).sum();
        let count = count.to_string();
        let mut at = x + 1;
        at += canvas.put(at, y, if folded { "▸" } else { "▾" }, Style::colors(theme.text_muted, bg)) + 1;
        let (glyph, color) = file_icon(cx.settings, &name, false, false);
        at += canvas.text(at, y, &glyph, Style::colors(color, bg), 2) + 1;
        let room = right.saturating_sub(at + ui::width(&count) as u16 + 3) as usize;
        let used = canvas.text_truncated(at, y, &name, Style::colors(theme.text, bg).bold(), room as u16);
        let left_over = room.saturating_sub(used as usize + 1);
        if let (Some(dir), true) = (dir, left_over > 1) {
            let shown = ui::truncate_start(&dir, left_over);
            canvas.text(at + used + 1, y, &shown, Style::colors(theme.text_muted, bg), left_over as u16);
        }
        canvas.text_right(right.saturating_sub(1), y, &count, Style::colors(theme.text_muted, bg));
    }

    /// One result line: right-aligned line number, then the text clipped around its first
    /// match with every match highlighted.
    #[allow(clippy::too_many_arguments)]
    fn draw_match_row(&self, canvas: &mut Canvas<'_>, x: u16, y: u16, width: u16, at: (usize, usize), bg: Rgb, theme: &bakedit_core::Theme) {
        let found = &self.files[at.0].matches[at.1];
        let number_right = x + 3 + self.number_digits as u16;
        canvas.text_right(number_right, y, &found.line_number.to_string(), Style::colors(theme.line_number, bg));
        let text_x = number_right + 2;
        let room = (x + width).saturating_sub(text_x + 1) as usize;
        let mut col = text_x;
        for (ch, hit) in clip_line(&found.line, found.clamp_offset, &found.ranges, room) {
            let style = if hit {
                Style::colors(theme.text, theme.search_match_bg)
            } else if ch == '…' {
                Style::colors(theme.text_muted, bg)
            } else {
                Style::colors(theme.editor_fg, bg)
            };
            let mut buf = [0u8; 4];
            col += canvas.put(col, y, ch.encode_utf8(&mut buf), style).max(1);
        }
    }
}

impl View for ProjectSearch {
    fn render(&mut self, canvas: &mut Canvas<'_>, area: Rect, cx: &RenderCx<'_>) -> Option<Cursor> {
        let theme = cx.theme;
        canvas.fill(area, theme.editor_bg);
        self.hits.clear();
        self.list = Rect::default();
        if area.width < 8 || area.height < 4 {
            return None;
        }
        let layout = HeaderLayout::new(area, self.replace_open);
        let column = self.draw_header(canvas, area, &layout, cx);
        let status_y = area.y + layout.rows;
        if status_y >= area.bottom() {
            return None;
        }
        let (status, is_error) = self.status_line();
        let status_color = if is_error { theme.error } else { theme.text_muted };
        canvas.text_truncated(area.x + 2, status_y, &status, Style::colors(status_color, theme.editor_bg), area.width.saturating_sub(4));
        let list = Rect::new(area.x, status_y + 1, area.width, area.bottom().saturating_sub(status_y + 1));
        self.draw_results(canvas, list, cx);
        if !cx.focused {
            return None;
        }
        let row = match self.focus {
            Part::Query => layout.query.y,
            Part::Replace => layout.replace.as_ref().map(|(rect, _)| rect.y)?,
            Part::Include => layout.include.1.y,
            Part::Exclude => layout.exclude.1.y,
            Part::Results => return None,
        };
        Some(Cursor { col: column?, row, shape: CursorShape::Bar })
    }

    fn contexts(&self, out: &mut Vec<Context>) {
        out.push(Context::new("ProjectSearchView"));
        if self.focus != Part::Results {
            out.push(Context::new("ProjectSearchBar").flag_if(self.focus == Part::Replace, "in_replace"));
            out.push(Context::new("Editor").value("mode", "single_line"));
        }
    }

    fn handle_action(&mut self, action: &Action, cx: &mut Cx<'_>) -> bool {
        let in_field = self.focus != Part::Results;
        match action.name.as_str() {
            "search::FocusSearch" => self.focus_search(action, cx),
            "project_search::ToggleFocus" => {
                if !in_field {
                    self.focus = Part::Query;
                } else if !self.rows.is_empty() {
                    self.remember_query(cx);
                    self.focus = Part::Results;
                    self.reveal = true;
                }
            }
            "search::ToggleCaseSensitive" => self.toggle(Opt::Case, cx),
            "search::ToggleWholeWord" => self.toggle(Opt::Word, cx),
            "search::ToggleRegex" => self.toggle(Opt::Regex, cx),
            "search::ToggleReplace" => self.toggle_replace(),
            "search::ReplaceNext" => return self.replace_next(cx),
            "search::ReplaceAll" => return self.replace_all(cx),
            "project_search::ConfirmReplaceAll" => return self.confirm_replace_all(cx),
            "search::PreviousHistoryQuery" => return self.history_step(true, cx),
            "search::NextHistoryQuery" => return self.history_step(false, cx),
            "menu::Confirm" => return self.confirm(cx),
            "menu::SecondaryConfirm" => return self.open_selected(true, cx),
            "menu::SelectNext" if in_field => self.move_focus(true),
            "menu::SelectPrevious" if in_field => self.move_focus(false),
            "menu::SelectNext" => self.step(true),
            "menu::SelectPrevious" => self.step(false),
            "menu::SelectFirst" if !in_field => {
                if let Some(first) = self.rows.iter().position(|r| self.is_selectable(*r)) {
                    self.select(first);
                }
            }
            "menu::SelectLast" if !in_field => {
                if let Some(last) = self.rows.iter().rposition(|r| self.is_selectable(*r)) {
                    self.select(last);
                }
            }
            name @ ("project_search::CollapseEntry" | "project_search::ExpandEntry" | "project_search::CollapseAllEntries" | "project_search::ExpandAllEntries") if !in_field => {
                self.fold(name.trim_start_matches("project_search::"));
            }
            _ if in_field => return self.edit_field(action, cx),
            _ => return false,
        }
        true
    }

    fn handle_key(&mut self, key: &KeyEvent, cx: &mut Cx<'_>) -> bool {
        if !key.is_down() {
            return false;
        }
        let Some(text) = key.text.as_deref().filter(|t| !t.chars().any(char::is_control)) else { return false };
        self.type_text(text, cx);
        true
    }

    fn handle_paste(&mut self, text: &str, cx: &mut Cx<'_>) -> bool {
        self.type_text(text, cx);
        true
    }

    fn handle_mouse(&mut self, event: &MouseEvent, _area: Rect, cx: &mut Cx<'_>) -> bool {
        match event.kind {
            MouseKind::ScrollDown => self.scroll = (self.scroll + WHEEL_ROWS).min(self.rows.len().saturating_sub(self.list.height as usize)),
            MouseKind::ScrollUp => self.scroll = self.scroll.saturating_sub(WHEEL_ROWS),
            MouseKind::Press(MouseButton::Left) => return self.click(Position::new(event.col, event.row), cx),
            _ => return false,
        }
        true
    }

    fn tick(&mut self, cx: &mut Cx<'_>) -> bool {
        let mut dirty = false;
        if let Some(due) = self.due {
            if cx.now >= due {
                self.run(cx);
                dirty = true;
            } else {
                cx.project.waker().wake_at(due);
            }
        }
        dirty |= self.collect();
        dirty |= self.poll_job(cx);
        dirty
    }
}

impl ProjectSearch {
    /// `search::FocusSearch`: go to the query, with what the action carries filled in.
    fn focus_search(&mut self, action: &Action, cx: &mut Cx<'_>) {
        if let Some(include) = action.arg_str("include") {
            self.include.set_text(include);
        }
        if action.arg_bool("replace") == Some(true) {
            self.replace_open = true;
        }
        self.focus = Part::Query;
        match action.arg_str("query") {
            Some(query) => {
                self.query.set_text(query);
                self.query.select_all();
                self.history_at = None;
                self.run(cx);
            }
            None => {
                self.query.select_all();
                if action.arg_str("include").is_some() {
                    self.run(cx);
                }
            }
        }
    }

    /// An `editor::` action for the focused field.
    fn edit_field(&mut self, action: &Action, cx: &mut Cx<'_>) -> bool {
        let focus = self.focus;
        let Some(input) = self.field_mut(focus) else { return false };
        match input.handle_action(action) {
            InputOutcome::Ignored => return false,
            InputOutcome::Copied { text, edited } => {
                cx.clipboard.set(&text, false);
                if edited {
                    self.edited(cx);
                }
            }
            InputOutcome::WantsPaste => {
                if let Some(clip) = cx.clipboard.get() {
                    input.insert(&clip.text.replace('\n', " "));
                    self.edited(cx);
                }
            }
            InputOutcome::Edited => self.edited(cx),
            InputOutcome::Moved => {}
        }
        true
    }

    /// Typed or pasted text goes to the focused field; with the results focused, to the
    /// query, so no key is ever dead.
    fn type_text(&mut self, text: &str, cx: &mut Cx<'_>) {
        if self.focus == Part::Results {
            self.focus = Part::Query;
        }
        let focus = self.focus;
        if let Some(input) = self.field_mut(focus) {
            input.insert(text);
        }
        self.edited(cx);
    }

    fn click(&mut self, at: Position, cx: &mut Cx<'_>) -> bool {
        if let Some(hit) = self.hits.iter().find(|(rect, _)| rect.contains(at)).map(|(_, hit)| *hit) {
            match hit {
                Hit::Field(part) => self.focus = part,
                Hit::Toggle(opt) => self.toggle(opt, cx),
                Hit::ReplaceRow => self.toggle_replace(),
                Hit::ReplaceNext => {
                    self.replace_next(cx);
                }
                Hit::ReplaceAll => {
                    self.replace_all(cx);
                }
            }
            return true;
        }
        if !self.list.contains(at) {
            return false;
        }
        let index = self.scroll + (at.y - self.list.y) as usize;
        if index >= self.rows.len() {
            return true;
        }
        self.focus = Part::Results;
        self.select(index);
        self.reveal = false;
        // The chevron folds a file's results; the rest of a row opens it.
        if matches!(self.rows[index], ResultRow::File(_)) && at.x < self.list.x + 3 {
            self.toggle_file_at(index);
            true
        } else {
            self.open_selected(true, cx)
        }
    }
}

impl Item for ProjectSearch {
    fn kind(&self) -> &'static str {
        "project_search"
    }

    fn tab(&self, cx: &RenderCx<'_>) -> Tab {
        let pattern = self.searched.as_ref().map(|s| s.pattern.as_str()).unwrap_or_default();
        let title = if pattern.is_empty() { "Search".to_string() } else { format!("Search: {}", ui::truncate_end(pattern, 24)) };
        let icon = match cx.settings.icons {
            Icons::Nerd => Some(("\u{f002}".to_string(), cx.theme.text_muted)),
            Icons::Ascii => None,
        };
        Tab { title, icon, ..Default::default() }
    }

    fn query_suggestion(&self) -> Option<String> {
        Some(self.query.text().to_string()).filter(|q| !q.is_empty())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)]
#[path = "search_tests.rs"]
mod tests;
