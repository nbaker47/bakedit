//! Project search (`pane::DeploySearch`, cmd-shift-f): find text in every file of the
//! project, results grouped by file, in one tab.
//!
//! # What the app calls (this signature stays)
//!
//! * `ProjectSearch::new(replace: bool, cx: &mut Cx<'_>) -> ProjectSearch`. `kind()` is
//!   `"project_search"`: the shell keeps one and brings it back. `replace` is whether it was
//!   opened for search and replace (cmd-shift-h).
//!
//! The shell sends `search::FocusSearch`, with `{"query": "..."}` when the tab it was opened
//! from had a selection or a word at the cursor; the query is then filled in and selected.
//! Enter runs the search; escape (`project_search::ToggleFocus`) moves between the query and
//! the results; enter on a result opens it at its line. `search::ToggleCaseSensitive`,
//! `search::ToggleWholeWord` and `search::ToggleRegex` change how the query is read and run
//! it again. The search runs on worker threads; results come in as they are found.
//!
//! Key contexts: `ProjectSearchView`, and while the query has the keyboard
//! `ProjectSearchView > ProjectSearchBar{in_replace} > Editor{mode=single_line}`.
//! Replacing is for the view's next owner.

use std::any::Any;
use std::path::PathBuf;

use bakedit_core::{Action, KeyEvent, MouseButton, MouseEvent, MouseKind};
use bakedit_project::{LineMatch, SearchEvent, SearchHandle, SearchOptions};
use ratatui::layout::Rect;

use crate::chrome::Tab;
use crate::icons::file_icon;
use crate::line_input::{InputOutcome, LineInput};
use crate::project::SearchKind;
use crate::settings::Icons;
use crate::ui::{self, Canvas, Style};
use crate::view::{Context, Cursor, CursorShape, Cx, Effect, Item, RenderCx, View};

/// Rows a wheel step scrolls.
const WHEEL_ROWS: usize = 3;
/// Columns of the line-number column.
const NUMBER_WIDTH: u16 = 6;

#[derive(Debug, Clone, PartialEq)]
enum State {
    /// Nothing searched for yet.
    Idle,
    Running,
    Done { files: usize, matches: usize, truncated: bool },
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

/// The project search tab.
pub struct ProjectSearch {
    query: LineInput,
    replace: bool,
    /// The query box has the keyboard (else the results do).
    in_query: bool,
    case_sensitive: bool,
    whole_word: bool,
    regex: bool,
    handle: Option<SearchHandle>,
    /// What the results are for.
    searched: String,
    files: Vec<FileResults>,
    rows: Vec<ResultRow>,
    state: State,
    selected: usize,
    scroll: usize,
    /// Where the result rows were last drawn.
    list: Rect,
}

impl ProjectSearch {
    /// An empty search; `replace` when it was opened to search and replace.
    pub fn new(replace: bool, cx: &mut Cx<'_>) -> ProjectSearch {
        ProjectSearch {
            query: LineInput::new(),
            replace,
            in_query: true,
            case_sensitive: cx.settings.search_case_sensitive,
            whole_word: cx.settings.search_whole_word,
            regex: cx.settings.search_regex,
            handle: None,
            searched: String::new(),
            files: Vec::new(),
            rows: Vec::new(),
            state: State::Idle,
            selected: 0,
            scroll: 0,
            list: Rect::default(),
        }
    }

    fn run(&mut self, cx: &mut Cx<'_>) {
        let pattern = self.query.text().to_string();
        self.handle = None;
        self.files.clear();
        self.rows.clear();
        self.selected = 0;
        self.scroll = 0;
        if pattern.is_empty() {
            self.state = State::Idle;
            self.searched.clear();
            return;
        }
        cx.project.remember_search(SearchKind::Project, &pattern);
        let options = SearchOptions {
            regex: self.regex,
            case_sensitive: self.case_sensitive,
            whole_word: self.whole_word,
            ..SearchOptions::new(pattern.clone())
        };
        let waker = cx.project.waker().clone();
        match bakedit_project::search_with_wake(cx.root, options, Box::new(move || waker.wake())) {
            Ok(handle) => {
                self.handle = Some(handle);
                self.state = State::Running;
            }
            Err(error) => self.state = State::Failed(error.to_string()),
        }
        self.searched = pattern;
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
                SearchEvent::Done { files, matches, truncated, .. } => done = Some(State::Done { files, matches, truncated }),
            }
        }
        if arrived {
            // Files arrive in any order: keep them sorted so the list does not jump about.
            let selected = self.rows.get(self.selected).copied().map(|row| self.row_key(row));
            self.files.sort_by(|a, b| a.path.cmp(&b.path));
            self.rows = self
                .files
                .iter()
                .enumerate()
                .flat_map(|(file, results)| std::iter::once(ResultRow::File(file)).chain((0..results.matches.len()).map(move |m| ResultRow::Match(file, m))))
                .collect();
            if let Some(key) = selected {
                if let Some(index) = self.rows.iter().position(|row| self.row_key(*row) == key) {
                    self.selected = index;
                }
            }
        }
        if let Some(done) = done {
            self.state = done;
            self.handle = None;
        }
        arrived
    }

    /// A row's identity that survives re-sorting: its file and match.
    fn row_key(&self, row: ResultRow) -> (PathBuf, Option<usize>) {
        match row {
            ResultRow::File(file) => (self.files[file].path.clone(), None),
            ResultRow::Match(file, m) => (self.files[file].path.clone(), Some(m)),
        }
    }

    fn open_selected(&self, cx: &mut Cx<'_>) -> bool {
        let Some(row) = self.rows.get(self.selected) else { return false };
        let (file, line_match) = match *row {
            ResultRow::File(file) => (file, self.files[file].matches.first()),
            ResultRow::Match(file, m) => (file, self.files[file].matches.get(m)),
        };
        let path = cx.root.join(&self.files[file].path);
        let line = line_match.map(|m| m.line_number);
        let column = line_match.and_then(|m| m.ranges.first().map(|r| (m.clamp_offset + r.start) as u32 + 1));
        cx.push(Effect::OpenFile { path, line, column, preview: false, focus: true });
        true
    }

    fn summary(&self) -> String {
        let flags: Vec<&str> = [(self.case_sensitive, "case"), (self.whole_word, "word"), (self.regex, "regex")]
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, name)| *name)
            .collect();
        let flags = if flags.is_empty() { String::new() } else { format!("  ·  {}", flags.join(", ")) };
        let text = match &self.state {
            State::Idle => "Type what to find and press enter".to_string(),
            State::Running => format!("Searching…  {} files so far", self.files.len()),
            State::Done { matches: 0, .. } => format!("No results for \"{}\"", self.searched),
            State::Done { files, matches, truncated } => {
                let more = if *truncated { " (stopped at the limit)" } else { "" };
                let plural = |n: usize, one: &str| if n == 1 { format!("1 {one}") } else { format!("{n} {one}s") };
                format!("{} in {}{more}", plural(*matches, "result"), plural(*files, "file"))
            }
            State::Failed(error) => error.clone(),
        };
        format!("{text}{flags}")
    }
}

impl View for ProjectSearch {
    fn render(&mut self, canvas: &mut Canvas<'_>, area: Rect, cx: &RenderCx<'_>) -> Option<Cursor> {
        let theme = cx.theme;
        canvas.fill(area, theme.editor_bg);
        if area.width < 8 || area.height < 3 {
            return None;
        }
        // The query bar: a row of its own background, the field inset by two cells.
        let bar_bg = theme.toolbar_bg.lerp(theme.text, 0.04);
        let bar = Rect::new(area.x, area.y, area.width, 1);
        canvas.fill(bar, bar_bg);
        let label = if self.replace { "Find in files (replace is not yet available)" } else { "Find in files" };
        let field = Rect::new(area.x + 2, area.y, area.width.saturating_sub(4), 1);
        let cursor_col = self.query.render(
            canvas,
            field,
            Style::colors(theme.text, bar_bg),
            Style::colors(theme.text, theme.selection_bg),
            label,
            Style::colors(theme.text_muted, bar_bg),
        );
        let status_style = match self.state {
            State::Failed(_) => Style::colors(theme.error, theme.editor_bg),
            _ => Style::colors(theme.text_muted, theme.editor_bg),
        };
        canvas.text_truncated(area.x + 2, area.y + 1, &self.summary(), status_style, area.width.saturating_sub(4));

        let list = Rect::new(area.x, area.y + 2, area.width, area.height - 2);
        self.list = list;
        let height = list.height as usize;
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if height > 0 && self.selected >= self.scroll + height {
            self.scroll = self.selected + 1 - height;
        }
        for (offset, row) in self.rows.iter().enumerate().skip(self.scroll).take(height) {
            let y = list.y + (offset - self.scroll) as u16;
            let selected = offset == self.selected && !self.in_query;
            let bg = if selected { theme.element_selected } else { theme.editor_bg };
            canvas.fill(Rect::new(list.x, y, list.width, 1), bg);
            match *row {
                ResultRow::File(file) => {
                    let results = &self.files[file];
                    let name = results.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let (glyph, color) = file_icon(cx.settings, &name, false, false);
                    let mut x = list.x + 2;
                    x += canvas.text(x, y, &glyph, Style::colors(color, bg), 2) + 1;
                    let count = results.matches.len().to_string();
                    let room = list.right().saturating_sub(x + ui::width(&count) as u16 + 3);
                    let path = ui::truncate_start(&results.path.display().to_string(), room as usize);
                    canvas.text(x, y, &path, Style::colors(theme.text, bg).bold(), room);
                    canvas.text_right(list.right() - 2, y, &count, Style::colors(theme.text_muted, bg));
                }
                ResultRow::Match(file, m) => {
                    let found = &self.files[file].matches[m];
                    canvas.text_right(list.x + NUMBER_WIDTH, y, &found.line_number.to_string(), Style::colors(theme.line_number, bg));
                    let x = list.x + NUMBER_WIDTH + 2;
                    // Leading blanks are noise in a result list: start at the code.
                    let skip = found.line.chars().take_while(|c| c.is_whitespace()).count();
                    let mut col = x;
                    for (index, ch) in found.line.chars().enumerate().skip(skip) {
                        if col >= list.right().saturating_sub(1) {
                            break;
                        }
                        let hit = found.ranges.iter().any(|r| r.start <= index && index < r.end);
                        let style = if hit { Style::colors(theme.text, theme.search_match_bg) } else { Style::colors(theme.editor_fg, bg) };
                        let mut encoded = [0u8; 4];
                        let shown = if ch == '\t' { " " } else { ch.encode_utf8(&mut encoded) };
                        col += canvas.put(col, y, shown, style).max(1);
                    }
                }
            }
        }
        if !cx.focused || !self.in_query {
            return None;
        }
        Some(Cursor { col: cursor_col, row: area.y, shape: CursorShape::Bar })
    }

    fn contexts(&self, out: &mut Vec<Context>) {
        out.push(Context::new("ProjectSearchView"));
        if self.in_query {
            out.push(Context::new("ProjectSearchBar").flag_if(self.replace, "in_replace"));
            out.push(Context::new("Editor").value("mode", "single_line"));
        }
    }

    fn handle_action(&mut self, action: &Action, cx: &mut Cx<'_>) -> bool {
        match action.name.as_str() {
            "search::FocusSearch" => {
                self.in_query = true;
                if let Some(query) = action.arg_str("query") {
                    self.query.set_text(query);
                }
                self.query.select_all();
            }
            "project_search::ToggleFocus" => {
                self.in_query = !self.in_query || self.rows.is_empty();
            }
            "search::ToggleCaseSensitive" => {
                self.case_sensitive = !self.case_sensitive;
                self.run(cx);
            }
            "search::ToggleWholeWord" => {
                self.whole_word = !self.whole_word;
                self.run(cx);
            }
            "search::ToggleRegex" => {
                self.regex = !self.regex;
                self.run(cx);
            }
            "menu::Confirm" if self.in_query => self.run(cx),
            "menu::Confirm" | "menu::SecondaryConfirm" => return self.open_selected(cx),
            "menu::SelectNext" if !self.in_query => self.selected = (self.selected + 1).min(self.rows.len().saturating_sub(1)),
            "menu::SelectPrevious" if !self.in_query => self.selected = self.selected.saturating_sub(1),
            "menu::SelectFirst" if !self.in_query => self.selected = 0,
            "menu::SelectLast" if !self.in_query => self.selected = self.rows.len().saturating_sub(1),
            // In the results a down arrow from the query goes to the first result.
            "menu::SelectNext" if !self.rows.is_empty() => self.in_query = false,
            "search::PreviousHistoryQuery" | "search::NextHistoryQuery" => {
                let history = cx.project.search_history(SearchKind::Project);
                let at = history.iter().position(|q| q == self.query.text());
                let next = match (action.name.as_str(), at) {
                    ("search::PreviousHistoryQuery", None) => history.first(),
                    ("search::PreviousHistoryQuery", Some(i)) => history.get(i + 1),
                    (_, Some(i)) if i > 0 => history.get(i - 1),
                    _ => None,
                };
                let Some(next) = next.cloned() else { return false };
                self.query.set_text(&next);
            }
            _ if self.in_query => match self.query.handle_action(action) {
                InputOutcome::Ignored => return false,
                InputOutcome::Copied { text, .. } => {
                    cx.clipboard.set(&text, false);
                }
                InputOutcome::WantsPaste => {
                    if let Some(clip) = cx.clipboard.get() {
                        self.query.insert(&clip.text.replace('\n', " "));
                    }
                }
                InputOutcome::Edited | InputOutcome::Moved => {}
            },
            _ => return false,
        }
        true
    }

    fn handle_key(&mut self, key: &KeyEvent, _cx: &mut Cx<'_>) -> bool {
        if !self.in_query || !key.is_down() {
            return false;
        }
        match key.text.as_deref() {
            Some(text) if !text.chars().any(char::is_control) => {
                self.query.insert(text);
                true
            }
            _ => false,
        }
    }

    fn handle_paste(&mut self, text: &str, _cx: &mut Cx<'_>) -> bool {
        self.in_query = true;
        self.query.insert(&text.replace('\n', " "));
        true
    }

    fn handle_mouse(&mut self, event: &MouseEvent, area: Rect, cx: &mut Cx<'_>) -> bool {
        match event.kind {
            MouseKind::ScrollDown => self.scroll = (self.scroll + WHEEL_ROWS).min(self.rows.len().saturating_sub(1)),
            MouseKind::ScrollUp => self.scroll = self.scroll.saturating_sub(WHEEL_ROWS),
            MouseKind::Press(MouseButton::Left) => {
                if event.row == area.y {
                    self.in_query = true;
                } else if let Some(row) = event.row.checked_sub(self.list.y) {
                    let index = self.scroll + row as usize;
                    if index < self.rows.len() {
                        self.selected = index;
                        self.in_query = false;
                        return self.open_selected(cx);
                    }
                }
            }
            _ => return false,
        }
        true
    }

    fn tick(&mut self, _cx: &mut Cx<'_>) -> bool {
        self.collect()
    }
}

impl Item for ProjectSearch {
    fn kind(&self) -> &'static str {
        "project_search"
    }

    fn tab(&self, cx: &RenderCx<'_>) -> Tab {
        let title = if self.searched.is_empty() { "Search".to_string() } else { format!("Search: {}", ui::truncate_end(&self.searched, 24)) };
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
mod tests {
    use std::time::{Duration, Instant};

    use serde_json::json;

    use super::*;
    use crate::view::test_support::Harness;

    fn dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "fn needle() {}\n    let needle = 1;\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "no match here\nNeedle again\n").unwrap();
        dir
    }

    fn search_for(search: &mut ProjectSearch, harness: &mut Harness, query: &str) {
        let focus = Action::with_args("search::FocusSearch", json!({ "query": query }));
        search.handle_action(&focus, &mut harness.cx());
        assert!(search.handle_action(&Action::new("menu::Confirm"), &mut harness.cx()));
        let started = Instant::now();
        while search.state == State::Running {
            search.tick(&mut harness.cx());
            assert!(started.elapsed() < Duration::from_secs(20), "the search never finished");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn finds_matches_grouped_by_file() {
        let dir = dir();
        let mut harness = Harness::new(dir.path());
        let mut search = ProjectSearch::new(false, &mut harness.cx());
        let (text, cursor, _) = harness.draw(&mut search, 60, 10);
        assert!(text.contains("Find in files") && text.contains("Type what to find and press enter"), "{text}");
        assert_eq!(cursor.map(|c| c.row), Some(0), "the cursor is in the query");
        search_for(&mut search, &mut harness, "needle");
        let (text, _, _) = harness.draw(&mut search, 60, 10);
        let rows: Vec<&str> = text.lines().collect();
        assert!(rows[1].contains("3 results in 2 files"), "smart case off: Needle matches too: {:?}", rows[1]);
        assert!(rows[2].contains("b.txt") && rows[2].trim_end().ends_with('1'), "{:?}", rows[2]);
        assert!(rows[3].trim_start().starts_with("2  Needle again"), "{:?}", rows[3]);
        assert!(rows[4].contains("src/a.rs") && rows[4].trim_end().ends_with('2'));
        assert!(rows[6].trim_start().starts_with("2  let needle = 1;"), "leading blanks are dropped: {:?}", rows[6]);
        assert_eq!(harness.project.search_history(SearchKind::Project), vec!["needle".to_string()]);
    }

    #[test]
    fn results_open_at_their_line_and_column() {
        let dir = dir();
        let mut harness = Harness::new(dir.path());
        let mut search = ProjectSearch::new(false, &mut harness.cx());
        search_for(&mut search, &mut harness, "let needle");
        assert!(search.handle_action(&Action::new("project_search::ToggleFocus"), &mut harness.cx()));
        let mut contexts = Vec::new();
        search.contexts(&mut contexts);
        assert_eq!(contexts.iter().map(|c| c.name).collect::<Vec<_>>(), ["ProjectSearchView"]);
        search.handle_action(&Action::new("menu::SelectNext"), &mut harness.cx());
        assert!(search.handle_action(&Action::new("menu::Confirm"), &mut harness.cx()));
        assert_eq!(
            harness.take_effects(),
            vec![Effect::OpenFile { path: dir.path().join("src/a.rs"), line: Some(2), column: Some(5), preview: false, focus: true }]
        );
    }

    #[test]
    fn options_and_errors_show_in_the_summary() {
        let dir = dir();
        let mut harness = Harness::new(dir.path());
        let mut search = ProjectSearch::new(false, &mut harness.cx());
        search_for(&mut search, &mut harness, "zzz-nothing");
        assert!(harness.draw(&mut search, 60, 6).0.contains("No results for \"zzz-nothing\""));
        search.handle_action(&Action::new("search::ToggleRegex"), &mut harness.cx());
        search_for(&mut search, &mut harness, "(unclosed");
        let text = harness.draw(&mut search, 80, 6).0;
        assert!(text.contains("regex"), "the active option is listed: {text}");
        assert!(matches!(search.state, State::Failed(_)), "{:?}", search.state);
        let mut contexts = Vec::new();
        search.contexts(&mut contexts);
        assert_eq!(contexts.iter().map(|c| c.name).collect::<Vec<_>>(), ["ProjectSearchView", "ProjectSearchBar", "Editor"]);
        assert_eq!(search.kind(), "project_search");
        assert!(search.tab(&harness.render_cx(false)).title.starts_with("Search: "));
    }
}
