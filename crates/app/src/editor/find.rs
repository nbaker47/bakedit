//! Find in file: the bar above the text, the matches in it, and stepping through them.
//!
//! Contexts while the bar has the keys: `BufferSearchBar{in_replace} > Editor{mode=single_line}`.
//! The query and the replacement are `LineInput`s; matches are found with
//! `bakedit_text::Search`, on the spot for ordinary files and on a worker for huge ones.

use std::ops::Range;

use bakedit_core::Action;
use bakedit_text::{Search, SearchQuery, Selections};
use ratatui::layout::Rect;

use super::layout::{Geometry, Reveal};
use super::EditorItem;
use crate::background::Task;
use crate::line_input::{InputOutcome, LineInput};
use crate::project::{Project, SearchKind};
use crate::ui::{self, Canvas, Style};
use crate::view::{Cursor, CursorShape, Cx, Item, RenderCx};

/// Files with at most this many chars have their matches found on the UI thread.
const SYNC_CHARS: usize = 200_000;
/// More matches than this are not kept (the count says "N+").
const MAX_MATCHES: usize = 100_000;
/// Width of the match count.
const COUNT_WIDTH: u16 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FindFocus {
    Query,
    Replace,
    /// The bar is open but the keys go to the text.
    Editor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FindHit {
    Case,
    Word,
    Regex,
    Previous,
    Next,
    Close,
    Query,
    Replace,
    ReplaceNext,
    ReplaceAll,
}

/// A match search running on a worker: the buffer version and query serial it is for.
type PendingMatches = (u64, u64, Task<Vec<Range<usize>>>);

pub(super) struct FindBar {
    query: LineInput,
    replace: LineInput,
    show_replace: bool,
    pub focus: FindFocus,
    case_sensitive: bool,
    whole_word: bool,
    regex: bool,
    search: Option<Search>,
    error: Option<String>,
    matches: Vec<Range<usize>>,
    truncated: bool,
    /// The buffer version and query the matches are for.
    matches_for: Option<(u64, u64)>,
    /// Bumped whenever the query or a toggle changes.
    serial: u64,
    pending: Option<PendingMatches>,
    /// Where the search started: typing a query selects the first match from here.
    origin: usize,
    history: Option<usize>,
    draft: String,
    pub hits: Vec<(Rect, FindHit)>,
}

impl FindBar {
    fn new(settings: &crate::settings::Settings, origin: usize) -> FindBar {
        FindBar {
            query: LineInput::new(),
            replace: LineInput::new(),
            show_replace: false,
            focus: FindFocus::Query,
            case_sensitive: settings.search_case_sensitive,
            whole_word: settings.search_whole_word,
            regex: settings.search_regex,
            search: None,
            error: None,
            matches: Vec::new(),
            truncated: false,
            matches_for: None,
            serial: 0,
            pending: None,
            origin,
            history: None,
            draft: String::new(),
            hits: Vec::new(),
        }
    }

    /// Compile the query with the current toggles.
    fn compile(&mut self) {
        self.serial += 1;
        self.matches_for = None;
        self.pending = None;
        self.error = None;
        self.search = None;
        self.matches.clear();
        self.truncated = false;
        let pattern = self.query.text();
        if pattern.is_empty() {
            return;
        }
        let query = SearchQuery { pattern: pattern.to_string(), regex: self.regex, case_sensitive: self.case_sensitive, whole_word: self.whole_word };
        match query.compile() {
            Ok(search) => self.search = Some(search),
            Err(_) => self.error = Some("bad regex".to_string()),
        }
    }

    pub fn rows(&self) -> u16 {
        if self.show_replace {
            2
        } else {
            1
        }
    }

    fn input(&mut self) -> &mut LineInput {
        if self.focus == FindFocus::Replace {
            &mut self.replace
        } else {
            &mut self.query
        }
    }
}

impl EditorItem {
    pub(super) fn find_focused(&self) -> bool {
        self.find.as_ref().is_some_and(|bar| bar.focus != FindFocus::Editor)
    }

    pub(super) fn find_rows(&self) -> u16 {
        self.find.as_ref().map_or(0, |bar| bar.rows())
    }

    /// Open the bar (or focus it) seeded from the selection or the word at the cursor.
    fn find_deploy(&mut self, replace: bool, cx: &mut Cx<'_>) {
        let selection = self.editor.selections().primary();
        let seed = if cx.settings.seed_search_query_from_cursor { self.query_suggestion() } else { None };
        let reseed = self.find.is_some() && !selection.is_empty() && seed.is_some();
        let origin = if selection.is_empty() { self.editor.word_at(selection.head).map_or(selection.head, |word| word.start) } else { selection.start() };
        let fresh = self.find.is_none();
        let bar = self.find.get_or_insert_with(|| Box::new(FindBar::new(cx.settings, origin)));
        if fresh || reseed {
            if let Some(seed) = &seed {
                bar.query.set_text(seed);
            }
            bar.origin = origin;
        }
        if replace {
            bar.show_replace = true;
        }
        bar.focus = if replace { FindFocus::Replace } else { FindFocus::Query };
        bar.query.select_all();
        bar.compile();
        if fresh || reseed {
            self.select_first_from_origin(cx);
        }
    }

    /// Select the first match at or after where the search started.
    fn select_first_from_origin(&mut self, cx: &mut Cx<'_>) {
        let Some(bar) = &self.find else { return };
        let origin = bar.origin;
        let Some(search) = bar.search.clone() else { return };
        if let Some(found) = search.find_next(self.editor.buffer().rope(), origin, cx.settings.search_wrap) {
            self.select_match(found);
        }
    }

    fn select_match(&mut self, found: Range<usize>) {
        self.editor.break_undo_group();
        self.editor.select_range(found.start, found.end);
        self.reveal = Reveal::Visible;
    }

    /// The query or a toggle changed.
    fn find_query_edited(&mut self, cx: &mut Cx<'_>) {
        let Some(bar) = &mut self.find else { return };
        bar.history = None;
        bar.compile();
        if bar.search.is_none() {
            let origin = bar.origin;
            if bar.query.is_empty() {
                self.editor.select_range(origin, origin);
                self.reveal = Reveal::Visible;
            }
            return;
        }
        self.select_first_from_origin(cx);
    }

    fn remember_query(&mut self, cx: &mut Cx<'_>) {
        if let Some(bar) = &self.find {
            cx.project.remember_search(SearchKind::Buffer, bar.query.text());
        }
    }

    /// The search `n` and `shift-n` use: the bar's, the last one, or the newest in the history.
    fn current_search(&self, cx: &Cx<'_>) -> Option<Search> {
        if let Some(search) = self.find.as_ref().and_then(|bar| bar.search.clone()) {
            return Some(search);
        }
        if let Some(search) = &self.last_search {
            return Some(search.clone());
        }
        let pattern = cx.project.search_history(SearchKind::Buffer).into_iter().next()?;
        let settings = cx.settings;
        SearchQuery { pattern, regex: settings.search_regex, case_sensitive: settings.search_case_sensitive, whole_word: settings.search_whole_word }
            .compile()
            .ok()
    }

    /// Select the next or previous match from the selection.
    fn find_step(&mut self, forward: bool, cx: &mut Cx<'_>) -> bool {
        let Some(search) = self.current_search(cx) else {
            cx.status("Nothing to search for: press cmd-f (or / in nav mode)");
            return true;
        };
        self.last_search = Some(search.clone());
        let selection = self.editor.selections().primary();
        let rope = self.editor.buffer().rope();
        let wrap = cx.settings.search_wrap;
        let found = if forward {
            let from = if selection.is_empty() { selection.head } else { selection.end() };
            search.find_next(rope, from, wrap)
        } else {
            search.find_prev(rope, selection.start(), wrap)
        };
        match found {
            Some(found) => {
                self.select_match(found);
                self.remember_query(cx);
            }
            None => cx.status(if wrap { "No matches" } else { "No more matches" }),
        }
        true
    }

    fn find_select_all(&mut self, cx: &mut Cx<'_>) -> bool {
        let Some(search) = self.current_search(cx) else { return false };
        let len = self.editor.buffer().len_chars();
        let found = search.find_all(self.editor.buffer().rope(), 0..len);
        if found.is_empty() {
            cx.status("No matches");
            return true;
        }
        self.editor.break_undo_group();
        self.editor.set_selections(Selections::from_ranges(found));
        self.reveal = Reveal::Fit;
        self.remember_query(cx);
        if let Some(bar) = &mut self.find {
            bar.focus = FindFocus::Editor;
        }
        true
    }

    fn find_replace(&mut self, all: bool, cx: &mut Cx<'_>) -> bool {
        if self.read_only_blocks(cx) {
            return true;
        }
        let Some(bar) = &self.find else { return false };
        let Some(search) = bar.search.clone() else { return true };
        let template = bar.replace.text().to_string();
        let wrap = cx.settings.search_wrap;
        if all {
            let len = self.editor.buffer().len_chars();
            let count = search.find_all(self.editor.buffer().rope(), 0..len).len();
            let outcome = self.editor.replace_all(&search, &template);
            self.after_outcome(&outcome, cx);
            cx.status(match count {
                0 => "No matches".to_string(),
                1 => "Replaced 1 match".to_string(),
                n => format!("Replaced {n} matches"),
            });
            return true;
        }
        let selection = self.editor.selections().primary();
        let on_match = search.find_next(self.editor.buffer().rope(), selection.start(), false).is_some_and(|m| m == selection.range());
        if !on_match {
            return self.find_step(true, cx);
        }
        let (replaced, outcome) = self.editor.replace_next(&search, selection.start(), wrap, &template);
        self.after_outcome(&outcome, cx);
        match replaced.and_then(|range| search.find_next(self.editor.buffer().rope(), range.end, wrap)) {
            Some(next) => self.select_match(next),
            None => cx.status("No more matches"),
        }
        true
    }

    /// The `buffer_search::*` and `search::*` actions. `None` when the name is not one of them.
    pub(super) fn search_action(&mut self, action: &Action, cx: &mut Cx<'_>) -> Option<bool> {
        let name = action.name.as_str();
        let open = self.find.is_some();
        let handled = match name {
            "buffer_search::Deploy" => {
                let replace = action.arg_bool("replace_enabled").unwrap_or(false);
                self.find_deploy(replace, cx);
                true
            }
            "buffer_search::DeployReplace" => {
                self.find_deploy(true, cx);
                true
            }
            "buffer_search::Dismiss" => {
                if let Some(bar) = self.find.take() {
                    cx.project.remember_search(SearchKind::Buffer, bar.query.text());
                    self.last_search = bar.search;
                }
                open
            }
            "buffer_search::FocusEditor" => {
                self.remember_query(cx);
                match &mut self.find {
                    Some(bar) => {
                        bar.focus = FindFocus::Editor;
                        true
                    }
                    None => false,
                }
            }
            "search::FocusSearch" => {
                if !open {
                    self.find_deploy(false, cx);
                }
                if let Some(bar) = &mut self.find {
                    bar.focus = FindFocus::Query;
                    if let Some(query) = action.arg_str("query") {
                        bar.query.set_text(query);
                    }
                    bar.query.select_all();
                }
                if action.arg_str("query").is_some() {
                    self.find_query_edited(cx);
                }
                true
            }
            "search::SelectNextMatch" => self.find_step(true, cx),
            "search::SelectPreviousMatch" => self.find_step(false, cx),
            "search::SelectAllMatches" => self.find_select_all(cx),
            "search::ReplaceNext" => open && self.find_replace(false, cx),
            "search::ReplaceAll" => open && self.find_replace(true, cx),
            "search::ToggleReplace" => {
                if !open {
                    self.find_deploy(true, cx);
                } else if let Some(bar) = &mut self.find {
                    bar.show_replace = !bar.show_replace;
                    bar.focus = if bar.show_replace { FindFocus::Replace } else { FindFocus::Query };
                }
                true
            }
            "search::ToggleCaseSensitive" | "search::ToggleWholeWord" | "search::ToggleRegex" => {
                if !open {
                    return Some(false);
                }
                self.find_toggle(name, cx);
                true
            }
            "search::PreviousHistoryQuery" | "search::NextHistoryQuery" => {
                if !open {
                    return Some(false);
                }
                self.find_history(name == "search::PreviousHistoryQuery", cx)
            }
            _ => return None,
        };
        Some(handled)
    }

    fn find_toggle(&mut self, name: &str, cx: &mut Cx<'_>) {
        if let Some(bar) = &mut self.find {
            match name {
                "search::ToggleCaseSensitive" => bar.case_sensitive = !bar.case_sensitive,
                "search::ToggleWholeWord" => bar.whole_word = !bar.whole_word,
                _ => bar.regex = !bar.regex,
            }
        }
        self.find_query_edited(cx);
    }

    fn find_history(&mut self, older: bool, cx: &mut Cx<'_>) -> bool {
        let history = cx.project.search_history(SearchKind::Buffer);
        let Some(bar) = &mut self.find else { return false };
        if bar.focus != FindFocus::Query {
            return false;
        }
        let next = match (bar.history, older) {
            (None, true) => {
                bar.draft = bar.query.text().to_string();
                (!history.is_empty()).then_some(0)
            }
            (Some(i), true) => Some((i + 1).min(history.len().saturating_sub(1))),
            (Some(0), false) | (None, false) => None,
            (Some(i), false) => Some(i - 1),
        };
        if older && history.is_empty() {
            return true;
        }
        let text = match next {
            Some(i) => history.get(i).cloned().unwrap_or_default(),
            None => bar.draft.clone(),
        };
        bar.query.set_text(&text);
        let remembered = next;
        self.find_query_edited(cx);
        if let Some(bar) = &mut self.find {
            bar.history = remembered;
        }
        true
    }

    /// An `editor::*` action while the keys are in the bar.
    pub(super) fn find_input_action(&mut self, action: &Action, cx: &mut Cx<'_>) -> bool {
        let Some(bar) = &mut self.find else { return false };
        let in_query = bar.focus == FindFocus::Query;
        let outcome = bar.input().handle_action(action);
        match outcome {
            InputOutcome::Ignored => false,
            InputOutcome::Moved => true,
            InputOutcome::Edited => {
                if in_query {
                    self.find_query_edited(cx);
                }
                true
            }
            InputOutcome::Copied { text, edited } => {
                cx.clipboard.set(&text, false);
                if edited && in_query {
                    self.find_query_edited(cx);
                }
                true
            }
            InputOutcome::WantsPaste => {
                if let Some(clip) = cx.clipboard.get() {
                    self.find_insert(&clip.text, cx);
                }
                true
            }
        }
    }

    /// Typed or pasted text while the keys are in the bar.
    pub(super) fn find_insert(&mut self, text: &str, cx: &mut Cx<'_>) {
        let Some(bar) = &mut self.find else { return };
        let in_query = bar.focus == FindFocus::Query;
        bar.input().insert(text);
        if in_query {
            self.find_query_edited(cx);
        }
    }

    /// Collect finished match searches and (re)start one when the text or query moved on.
    /// Returns whether what is shown changed.
    pub(super) fn find_refresh_matches(&mut self, project: &Project) -> bool {
        let version = self.editor.buffer().version();
        let len = self.editor.buffer().len_chars();
        let Some(bar) = &mut self.find else { return false };
        let mut changed = false;
        if let Some((for_version, serial, task)) = &mut bar.pending {
            if let Some(found) = task.poll() {
                if *serial == bar.serial && *for_version == version {
                    bar.truncated = found.len() >= MAX_MATCHES;
                    bar.matches = found;
                    bar.matches_for = Some((version, bar.serial));
                    changed = true;
                }
                bar.pending = None;
            } else if task.is_finished() {
                bar.pending = None;
            }
        }
        let Some(search) = bar.search.clone() else { return changed };
        if bar.matches_for == Some((version, bar.serial)) || bar.pending.is_some() {
            return changed;
        }
        let rope = self.editor.buffer().rope().clone();
        let run = move || {
            let mut found = search.find_all(&rope, 0..len);
            found.truncate(MAX_MATCHES);
            found
        };
        if len <= SYNC_CHARS {
            bar.truncated = false;
            bar.matches = run();
            bar.matches_for = Some((version, bar.serial));
            return true;
        }
        bar.pending = Some((version, bar.serial, project.spawn(run)));
        changed
    }

    /// The matches that touch chars `range`, as sorted ranges.
    pub(super) fn matches_in(&self, range: Range<usize>) -> &[Range<usize>] {
        let Some(bar) = &self.find else { return &[] };
        if bar.matches_for.is_none() {
            return &[];
        }
        let first = bar.matches.partition_point(|m| m.end <= range.start);
        let last = bar.matches.partition_point(|m| m.start < range.end);
        &bar.matches[first..last.max(first)]
    }

    /// Lines of the matches, thinned out to at most `limit`, for the scrollbar.
    pub(super) fn match_lines(&self, limit: usize) -> Vec<usize> {
        let Some(bar) = &self.find else { return Vec::new() };
        if bar.matches_for.is_none() || bar.matches.is_empty() {
            return Vec::new();
        }
        let step = (bar.matches.len() / limit.max(1)).max(1);
        bar.matches.iter().step_by(step).map(|m| self.editor.buffer().char_to_line(m.start)).collect()
    }

    /// The match the selection is on.
    pub(super) fn active_match(&self) -> Option<Range<usize>> {
        let bar = self.find.as_ref()?;
        let selection = self.editor.selections().primary().range();
        let at = bar.matches.binary_search_by(|m| m.start.cmp(&selection.start)).ok()?;
        (bar.matches[at] == selection).then_some(selection)
    }

    // ---- drawing -----------------------------------------------------------------------

    /// Draw the bar in `area` (`find_rows` rows). Returns the cursor when the bar has the keys.
    pub(super) fn draw_find(&mut self, canvas: &mut Canvas<'_>, area: Rect, cx: &RenderCx<'_>) -> Option<Cursor> {
        let primary = self.editor.selections().primary().range();
        let current = self
            .find
            .as_ref()
            .and_then(|bar| bar.matches.binary_search_by(|m| m.start.cmp(&primary.start)).ok().filter(|&i| bar.matches[i] == primary));
        let theme = cx.theme;
        let bar = self.find.as_mut()?;
        bar.hits.clear();
        let bg = theme.toolbar_bg;
        canvas.fill(area, bg);
        let muted = Style::colors(theme.text_muted, bg);
        let field_bg = theme.element_hover.lerp(theme.toolbar_bg, 0.4);
        let label_w: u16 = if bar.show_replace { 7 } else { 4 };
        let field_x = area.x + 1 + label_w + 1;
        let right = area.right().saturating_sub(1);

        // Which groups on the right fit, most important first.
        let toggles = [("Aa", FindHit::Case, bar.case_sensitive), ("W", FindHit::Word, bar.whole_word), (".*", FindHit::Regex, bar.regex)];
        let toggles_w: u16 = toggles.iter().map(|(label, ..)| ui::width(label) as u16 + 2).sum::<u16>() + 2;
        let mut room = right.saturating_sub(field_x + 10);
        let mut take = |width: u16| {
            let fits = room >= width;
            if fits {
                room -= width;
            }
            fits
        };
        let show_toggles = take(toggles_w + 1);
        let show_count = take(COUNT_WIDTH + 2);
        let show_close = take(4);
        let show_arrows = take(8);

        let mut x = right;
        let button = |canvas: &mut Canvas<'_>, hits: &mut Vec<(Rect, FindHit)>, x: &mut u16, label: &str, hit: FindHit, style: Style| {
            let width = ui::width(label) as u16 + 2;
            *x = x.saturating_sub(width);
            let rect = Rect::new(*x, area.y, width, 1);
            canvas.fill(rect, style.bg.unwrap_or(bg));
            canvas.text(rect.x + 1, rect.y, label, style, width - 2);
            hits.push((rect, hit));
        };
        let button_style = Style::colors(theme.text, theme.element_hover);
        if show_close {
            button(canvas, &mut bar.hits, &mut x, "×", FindHit::Close, button_style);
            x = x.saturating_sub(1);
        }
        if show_arrows {
            button(canvas, &mut bar.hits, &mut x, "↓", FindHit::Next, button_style);
            x = x.saturating_sub(1);
            button(canvas, &mut bar.hits, &mut x, "↑", FindHit::Previous, button_style);
            x = x.saturating_sub(1);
        }
        if show_toggles {
            for (label, hit, on) in toggles.iter().rev() {
                let style = if *on { Style::colors(theme.text_accent, theme.element_selected) } else { Style::colors(theme.text_muted, bg) };
                button(canvas, &mut bar.hits, &mut x, label, *hit, style);
                x = x.saturating_sub(1);
            }
        }
        if show_count {
            x = x.saturating_sub(1);
            let (text, color) = match (&bar.error, bar.search.is_some()) {
                (Some(error), _) => (error.clone(), theme.error),
                (None, false) => (String::new(), theme.text_muted),
                (None, true) if bar.matches_for.is_none() => ("...".to_string(), theme.text_muted),
                (None, true) if bar.matches.is_empty() => ("No matches".to_string(), theme.warning),
                (None, true) => {
                    let total = format!("{}{}", bar.matches.len(), if bar.truncated { "+" } else { "" });
                    match current {
                        Some(index) => (format!("{} of {total}", index + 1), theme.text_muted),
                        None => (format!("{total} matches"), theme.text_muted),
                    }
                }
            };
            canvas.text_right(x, area.y, &ui::truncate_end(&text, COUNT_WIDTH as usize), Style::colors(color, bg));
            x = x.saturating_sub(COUNT_WIDTH + 1);
        }
        let field_end = x.max(field_x + 10).min(right);
        let field = Rect::new(field_x, area.y, field_end.saturating_sub(field_x), 1);

        canvas.text(area.x + 1, area.y, "Find", muted, 4);
        let input_style = Style::colors(theme.text, field_bg);
        let selection_style = Style::colors(theme.text, theme.selection_bg);
        let placeholder = Style::colors(theme.text_placeholder, field_bg);
        canvas.fill(field, field_bg);
        let inner = Rect::new(field.x + 1, field.y, field.width.saturating_sub(2), 1);
        let query_col = bar.query.render(canvas, inner, input_style, selection_style, "Find in file", placeholder);
        bar.hits.push((field, FindHit::Query));
        let mut cursor = (bar.focus == FindFocus::Query).then_some(Cursor { col: query_col, row: area.y, shape: CursorShape::Bar });

        if bar.show_replace && area.height >= 2 {
            let y = area.y + 1;
            canvas.text(area.x + 1, y, "Replace", muted, 7);
            let replace_field = Rect::new(field.x, y, field.width, 1);
            canvas.fill(replace_field, field_bg);
            let inner = Rect::new(replace_field.x + 1, y, replace_field.width.saturating_sub(2), 1);
            let col = bar.replace.render(canvas, inner, input_style, selection_style, "Replace with", placeholder);
            bar.hits.push((replace_field, FindHit::Replace));
            if bar.focus == FindFocus::Replace {
                cursor = Some(Cursor { col, row: y, shape: CursorShape::Bar });
            }
            let mut bx = replace_field.right() + 1;
            for (label, hit) in [("Replace", FindHit::ReplaceNext), ("All", FindHit::ReplaceAll)] {
                let width = ui::width(label) as u16 + 2;
                if bx + width <= right {
                    let rect = Rect::new(bx, y, width, 1);
                    canvas.fill(rect, theme.element_hover);
                    canvas.text(rect.x + 1, y, label, Style::colors(theme.text, theme.element_hover), width - 2);
                    bar.hits.push((rect, hit));
                    bx += width + 1;
                }
            }
        }
        cursor
    }

    /// A click on the bar. Returns whether it hit anything.
    pub(super) fn find_click(&mut self, col: u16, row: u16, cx: &mut Cx<'_>) -> bool {
        let Some(bar) = &self.find else { return false };
        let Some(hit) = bar.hits.iter().rev().find(|(rect, _)| Geometry::contains(*rect, col, row)).map(|(_, hit)| *hit) else {
            return false;
        };
        match hit {
            FindHit::Case => self.find_toggle("search::ToggleCaseSensitive", cx),
            FindHit::Word => self.find_toggle("search::ToggleWholeWord", cx),
            FindHit::Regex => self.find_toggle("search::ToggleRegex", cx),
            FindHit::Previous => {
                self.find_step(false, cx);
            }
            FindHit::Next => {
                self.find_step(true, cx);
            }
            FindHit::Close => {
                self.search_action(&Action::new("buffer_search::Dismiss"), cx);
            }
            FindHit::Query | FindHit::Replace => {
                if let Some(bar) = &mut self.find {
                    bar.focus = if hit == FindHit::Query { FindFocus::Query } else { FindFocus::Replace };
                }
            }
            FindHit::ReplaceNext => {
                self.find_replace(false, cx);
            }
            FindHit::ReplaceAll => {
                self.find_replace(true, cx);
            }
        }
        true
    }

    /// Put the keys back in the text, as when the editor is clicked.
    pub(super) fn find_focus_editor(&mut self) {
        if let Some(bar) = &mut self.find {
            bar.focus = FindFocus::Editor;
        }
    }
}
