//! The editor: a file, an unsaved buffer or a read-only page in a tab of the centre.
//!
//! # What the app calls (these signatures stay)
//!
//! * `EditorItem::open(path: &Path, cx: &mut Cx<'_>) -> Result<EditorItem, String>`: the
//!   file at `path` (absolute). The error is one line for the status bar.
//! * `EditorItem::untitled(cx: &mut Cx<'_>) -> EditorItem`: an empty buffer with no file.
//! * `EditorItem::page(title: &str, text: &str, language: Option<&str>, cx: &mut Cx<'_>) -> EditorItem`:
//!   text nobody edits (the key bindings, the built-in keymap); `kind()` is `"page"` and
//!   `language` is a language name such as `"JSON"` to colour it as.
//!
//! Beyond those the app reaches it only through the `View` and `Item` traits: `editor::*`
//! actions from the keymap, `go_to` when a file is opened at a line, `picker_rows`,
//! `picker_preview` and `picker_confirm` for the outline (`PickerKind::Outline`),
//! `paths_changed` when the file changes on disk, `position`, `scroll_top` and
//! `set_scroll_top` for the session file, and `query_suggestion` to seed a search.
//!
//! # What it does now
//!
//! It shows the text read-only on top of `bakedit_text::Editor`, so every motion and
//! selection command in the keymap already moves as in Zed (cmd-up and cmd-down by five
//! lines, word and paragraph motions, the nav-mode letters); commands that would change the
//! text are left for the next candidate. It draws through `paint.rs`: syntax colours from
//! the project's highlighting thread, rainbow brackets and the matching pair, indent guides,
//! the current line, and git change marks against HEAD in the gutter and on the scrollbar.
//! A file changed on disk is reloaded.

use std::any::Any;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bakedit_core::{Action, KeyEvent, Mods, MouseButton, MouseEvent, MouseKind, Theme};
use bakedit_git::{LineHunk, LineHunkKind};
use bakedit_syntax::{Language, StyleCache, Symbol};
use bakedit_text::{Command, DisplayPosition, Editor, IndentStyle, LineEnding, Motion};
use ratatui::layout::Rect;

use crate::chrome::{ModeIndicator, Tab};
use crate::icons::file_icon;
use crate::paint::{self, GitMark, GutterInfo, PaintOptions, Row, ScrollMarks};
use crate::picker::PickerItem;
use crate::project::GitBase;
use crate::settings::Settings;
use crate::ui::{Canvas, Style};
use crate::view::{Context, Cursor, CursorShape, Cx, FsChange, Item, ItemStatus, PickerKind, RenderCx, View};

/// How long working out the git marks of one file may take on the UI thread.
const GIT_DIFF_BUDGET: Duration = Duration::from_millis(30);
/// Lines a wheel step scrolls.
const WHEEL_LINES: usize = 3;
/// How far below a blank line to look for the indentation its guides follow.
const GUIDE_LOOKAHEAD: usize = 64;

/// Where the next draw should put the cursor line, if it moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reveal {
    /// Leave the scroll alone.
    No,
    /// Scroll just enough to show the cursor, keeping the scroll margin.
    Fit,
    /// Put the cursor line in the middle.
    Center,
}

/// Git change marks for the text as it is, against the committed text.
#[derive(Default)]
struct GitMarks {
    /// The committed text the marks were worked out against.
    base: Option<Arc<GitBase>>,
    /// The buffer version they were worked out for.
    version: Option<u64>,
    hunks: Vec<LineHunk>,
}

impl GitMarks {
    /// The mark for `line` of a text of `line_count` lines.
    fn mark(&self, line: usize, line_count: usize) -> Option<GitMark> {
        // Hunks are sorted by `new_start`: only those starting at or before the line matter.
        let end = self.hunks.partition_point(|hunk| hunk.new_start <= line);
        for hunk in self.hunks[..end].iter().rev() {
            match hunk.kind {
                LineHunkKind::Deleted if hunk.new_start == line => return Some(GitMark::DeletedAbove),
                LineHunkKind::Deleted if hunk.new_start >= line_count && line + 1 == line_count => return Some(GitMark::DeletedBelow),
                LineHunkKind::Added if line < hunk.new_start + hunk.new_lines => return Some(GitMark::Added),
                LineHunkKind::Modified if line < hunk.new_start + hunk.new_lines => return Some(GitMark::Modified),
                _ => {}
            }
            if hunk.new_start + hunk.new_lines.max(1) <= line {
                break;
            }
        }
        // A deletion at the very end is drawn on the last line.
        if line + 1 == line_count {
            if let Some(last) = self.hunks.last() {
                if last.kind == LineHunkKind::Deleted && last.new_start >= line_count {
                    return Some(GitMark::DeletedBelow);
                }
            }
        }
        None
    }
}

/// A file, an unsaved buffer or a read-only page.
pub struct EditorItem {
    editor: Editor,
    title: String,
    kind: &'static str,
    language: &'static Language,
    doc_id: u64,
    /// The buffer version last sent to the highlighting thread.
    highlighted: Option<u64>,
    /// The version of the highlights the last draw used.
    shown_highlights: Option<u64>,
    styles: StyleCache,
    /// The theme the style cache was filled from.
    styles_theme: String,
    scroll_top: usize,
    /// First display column shown.
    left: usize,
    /// Where the text (not the gutter) was last drawn, for mouse clicks.
    text_area: Rect,
    reveal: Reveal,
    /// Nav and edit modes are on (`vim_mode`): this view reports nav mode to the keymap.
    nav: bool,
    git: GitMarks,
    /// The outline rows last handed to the outline picker.
    outline: Vec<Symbol>,
    /// Where the view and cursor were before the outline picker previewed a symbol.
    before_preview: Option<(usize, usize)>,
}

impl EditorItem {
    /// The file at `path` (absolute). Fails with one line for the status bar when the file
    /// cannot be read (missing, a directory, binary).
    pub fn open(path: &Path, cx: &mut Cx<'_>) -> Result<EditorItem, String> {
        let shown = short_path(path, cx.root);
        match std::fs::metadata(path) {
            Ok(meta) if meta.is_dir() => return Err(format!("{shown} is a folder")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Err(format!("{shown} does not exist")),
            _ => {}
        }
        let editor = Editor::open(path).map_err(|error| format!("Cannot open {shown}: {error}"))?;
        let first_line = editor.buffer().line_text(0).into_owned();
        let language = bakedit_syntax::detect(path, Some(&first_line));
        let title = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
        Ok(EditorItem::new(editor, title, "editor", language, cx))
    }

    /// An empty buffer that belongs to no file yet.
    pub fn untitled(cx: &mut Cx<'_>) -> EditorItem {
        EditorItem::new(Editor::from_str(""), "untitled".to_string(), "editor", bakedit_syntax::plain_text(), cx)
    }

    /// A page of text nobody edits, coloured as `language` (a name such as "JSON") if given.
    pub fn page(title: &str, text: &str, language: Option<&str>, cx: &mut Cx<'_>) -> EditorItem {
        let language = language.and_then(bakedit_syntax::language_by_name).unwrap_or_else(bakedit_syntax::plain_text);
        EditorItem::new(Editor::from_str(text), title.to_string(), "page", language, cx)
    }

    fn new(editor: Editor, title: String, kind: &'static str, language: &'static Language, cx: &mut Cx<'_>) -> EditorItem {
        let mut item = EditorItem {
            editor,
            title,
            kind,
            language,
            doc_id: cx.project.next_doc_id(),
            highlighted: None,
            shown_highlights: None,
            styles: StyleCache::new(),
            styles_theme: cx.theme.name.clone(),
            scroll_top: 0,
            left: 0,
            text_area: Rect::default(),
            reveal: Reveal::No,
            nav: cx.settings.vim_mode,
            git: GitMarks::default(),
            outline: Vec::new(),
            before_preview: None,
        };
        item.request_highlights(cx.project);
        item
    }

    fn cursor(&self) -> usize {
        self.editor.selections().primary().head
    }

    fn cursor_line(&self) -> usize {
        self.editor.buffer().char_to_line(self.cursor())
    }

    fn tab_size(&self, settings: &Settings) -> usize {
        effective_tab_size(settings, self.language)
    }

    /// Send the text to the highlighting thread if it changed since it was last sent.
    fn request_highlights(&mut self, project: &crate::project::Project) {
        let version = self.editor.buffer().version();
        if !self.language.has_grammar() || self.highlighted == Some(version) {
            return;
        }
        self.highlighted = Some(version);
        project.request_syntax(self.doc_id, version, self.language, self.editor.buffer().to_string());
    }

    /// Work the git marks out again when the text or the committed text changed.
    fn refresh_git_marks(&mut self, project: &crate::project::Project) {
        let Some(path) = self.editor.buffer().path().map(Path::to_path_buf) else { return };
        let base = project.git_base(&path);
        let version = self.editor.buffer().version();
        let same_base = match (&base, &self.git.base) {
            (Some(new), Some(old)) => Arc::ptr_eq(new, old),
            (None, None) => true,
            _ => false,
        };
        if same_base && self.git.version == Some(version) {
            return;
        }
        // An untracked file has nothing to compare with and gets no marks, as in Zed.
        let hunks = match base.as_ref().and_then(|base| base.head.clone()) {
            Some(head) => {
                let current = self.editor.buffer().to_string();
                bakedit_git::line_hunks_with_deadline(&head, &current, Some(Instant::now() + GIT_DIFF_BUDGET))
            }
            None => Vec::new(),
        };
        self.git = GitMarks { base, version: Some(version), hunks };
    }

    /// Scroll so the cursor line is shown, as the last move asked.
    fn apply_reveal(&mut self, rows: usize, margin: usize) {
        let line = self.cursor_line();
        let line_count = self.editor.buffer().len_lines();
        let margin = margin.min(rows.saturating_sub(1) / 2);
        match self.reveal {
            Reveal::No => {}
            Reveal::Center => self.scroll_top = line.saturating_sub(rows / 2),
            Reveal::Fit => {
                if line < self.scroll_top + margin {
                    self.scroll_top = line.saturating_sub(margin);
                } else if line + margin >= self.scroll_top + rows {
                    self.scroll_top = (line + margin + 1).saturating_sub(rows);
                }
            }
        }
        self.reveal = Reveal::No;
        self.scroll_top = self.scroll_top.min(line_count.saturating_sub(1));
    }

    fn scroll_by(&mut self, lines: isize) {
        let line_count = self.editor.buffer().len_lines();
        let top = self.scroll_top as isize + lines;
        self.scroll_top = top.clamp(0, line_count.saturating_sub(1) as isize) as usize;
    }

    /// The char position under a screen cell of the text area.
    fn position_at_cell(&self, col: u16, row: u16) -> usize {
        let line_count = self.editor.buffer().len_lines();
        let line = (self.scroll_top + row.saturating_sub(self.text_area.y) as usize).min(line_count.saturating_sub(1));
        let col = self.left + col.saturating_sub(self.text_area.x) as usize;
        self.editor.position_at(DisplayPosition { line, row: 0, col })
    }

    /// Run one keymap command on the text. Commands that change the text are refused.
    fn run(&mut self, command: Command, cx: &mut Cx<'_>) -> bool {
        if command.modifies_text() {
            return false;
        }
        let rows = (self.text_area.height as usize).max(1);
        let outcome = self.editor.apply(command.with_page_rows(rows));
        if let Some(clip) = outcome.clipboard {
            cx.clipboard.set(&clip.text, clip.linewise);
        }
        if outcome.selections_changed {
            self.reveal = Reveal::Fit;
        }
        true
    }

    fn indent_label(&self) -> String {
        match self.editor.detect_indent() {
            Some(IndentStyle::Tabs) => "Tabs".to_string(),
            Some(IndentStyle::Spaces(width)) => format!("Spaces: {width}"),
            None => format!("Spaces: {}", self.language.tab_size),
        }
    }

    /// How many indent guides a line gets: its own indent level, or for a blank line the
    /// level of the code below it.
    fn guide_level(&self, line: usize, tab: usize) -> usize {
        let buffer = self.editor.buffer();
        let line_count = buffer.len_lines();
        let level = |line: usize| self.editor.indent_of_line(line) / tab.max(1);
        if !buffer.line_text(line).trim().is_empty() {
            return level(line);
        }
        (line + 1..line_count.min(line + GUIDE_LOOKAHEAD)).find(|&l| !buffer.line_text(l).trim().is_empty()).map(level).unwrap_or(0)
    }
}

/// The tab size for a language: the user's setting for that language, then the language's
/// own convention where it has one (2 for JSON, YAML, JavaScript), then the global setting.
pub fn effective_tab_size(settings: &Settings, language: &Language) -> usize {
    let configured = settings.language(language.name).and_then(|l| l.tab_size);
    let own = (language.tab_size != 4).then_some(language.tab_size);
    usize::from(configured.or(own).unwrap_or(settings.tab_size)).max(1)
}

/// `path` relative to the project root when it is inside it, for messages.
fn short_path(path: &Path, root: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).display().to_string()
}

fn syntax_style(styles: &mut StyleCache, theme: &Theme, capture: bakedit_syntax::CaptureId) -> Style {
    let mut style = Style::new();
    if let Some(syntax) = styles.style(theme, capture) {
        style.fg = syntax.color;
        style.italic = syntax.italic;
        style.bold = syntax.bold;
    }
    style
}

impl View for EditorItem {
    fn render(&mut self, canvas: &mut Canvas<'_>, area: Rect, cx: &RenderCx<'_>) -> Option<Cursor> {
        let theme = cx.theme;
        canvas.fill(area, theme.editor_bg);
        if area.width == 0 || area.height == 0 {
            return None;
        }
        if self.styles_theme != theme.name {
            self.styles = StyleCache::new();
            self.styles_theme = theme.name.clone();
        }
        self.request_highlights(cx.project);
        self.refresh_git_marks(cx.project);
        let settings = cx.settings;
        let tab = self.tab_size(settings);
        let line_count = self.editor.buffer().len_lines();
        let gutter_width = paint::gutter_width(line_count, settings.line_numbers).min(area.width);
        let scrollbar = u16::from(area.width > gutter_width + 4);
        let text_width = area.width - gutter_width - scrollbar;
        self.text_area = Rect::new(area.x + gutter_width, area.y, text_width, area.height);
        let rows = area.height as usize;
        self.apply_reveal(rows, settings.vertical_scroll_margin as usize);

        // Horizontal scroll: keep the cursor's column in view.
        let cursor = self.cursor();
        let cursor_line = self.editor.buffer().char_to_line(cursor);
        let cursor_col = self.editor.display_position(cursor).col;
        let margin = (settings.horizontal_scroll_margin as usize).min((text_width as usize).saturating_sub(1) / 2);
        if cursor_col < self.left + margin {
            self.left = cursor_col.saturating_sub(margin);
        } else if text_width > 0 && cursor_col + margin + 1 > self.left + text_width as usize {
            self.left = cursor_col + margin + 1 - text_width as usize;
        }

        let syntax = cx.project.syntax(self.doc_id);
        self.shown_highlights = syntax.as_ref().map(|result| result.version);
        let fresh = syntax.as_ref().is_some_and(|result| Some(result.version) == self.highlighted);
        let selections: Vec<std::ops::Range<usize>> =
            self.editor.selections().iter().filter(|s| !s.is_empty()).map(|s| s.range()).collect();
        let primary = self.editor.selections().primary_index();
        let extra_cursors: Vec<usize> = self
            .editor
            .selections()
            .iter()
            .enumerate()
            .filter(|(index, s)| *index != primary && s.is_empty())
            .map(|(_, s)| s.head)
            .collect();
        let pair = self.editor.matching_bracket(cursor);
        let options = PaintOptions {
            tab_size: tab,
            left: self.left,
            show_indent_guides: settings.indent_guides,
            colorize_brackets: settings.colorize_brackets,
            show_whitespace_in_selection: true,
        };

        let buffer = self.editor.buffer();
        for screen_row in 0..rows {
            let y = area.y + screen_row as u16;
            let line = self.scroll_top + screen_row;
            if line >= line_count {
                canvas.fill(Rect::new(area.x, y, gutter_width, 1), theme.gutter_bg);
                continue;
            }
            let current = line == cursor_line && settings.current_line_highlight;
            let info = GutterInfo { number: Some(line + 1), current, git: self.git.mark(line, line_count) };
            paint::gutter(canvas, area.x, y, gutter_width, info, settings.line_numbers, theme);

            let text = buffer.line_text(line);
            let line_start = buffer.line_to_char(line);
            let line_len = buffer.line_len(line);
            let line_end = line_start + line_len;
            let spans: Vec<(std::ops::Range<usize>, Style)> = match &syntax {
                Some(result) => result
                    .lines
                    .spans_for_line(line)
                    .iter()
                    .map(|(range, capture)| (range.clone(), syntax_style(&mut self.styles, theme, *capture)))
                    .collect(),
                None => Vec::new(),
            };
            let brackets: Vec<(usize, u16)> = match (&syntax, fresh && settings.colorize_brackets) {
                (Some(result), true) => {
                    let start_byte = buffer.char_to_byte(line_start);
                    let end_byte = buffer.char_to_byte(line_end);
                    let first = result.brackets.partition_point(|mark| mark.byte < start_byte);
                    result.brackets[first..]
                        .iter()
                        .take_while(|mark| mark.byte < end_byte)
                        .map(|mark| (buffer.byte_to_char(mark.byte) - line_start, mark.depth))
                        .collect()
                }
                _ => Vec::new(),
            };
            let line_selections: Vec<std::ops::Range<usize>> = selections
                .iter()
                .filter(|range| range.start <= line_end && range.end > line_start)
                .map(|range| range.start.saturating_sub(line_start)..(range.end - line_start).min(line_len + 1))
                .collect();
            let cursors: Vec<usize> =
                extra_cursors.iter().filter(|&&pos| pos >= line_start && pos <= line_end).map(|pos| pos - line_start).collect();
            let matching: Vec<usize> = pair
                .map(|(open, close)| [open, close].into_iter().filter(|&p| p >= line_start && p < line_end).map(|p| p - line_start).collect())
                .unwrap_or_default();
            let mut row = Row::plain(&text);
            row.syntax = &spans;
            row.selections = &line_selections;
            row.cursors = &cursors;
            row.brackets = &brackets;
            row.matching_brackets = &matching;
            row.current_line = current;
            row.indent_guides = if settings.indent_guides { self.guide_level(line, tab) } else { 0 };
            paint::row(canvas, self.text_area.x, y, text_width, &row, &options, theme);
        }

        if scrollbar == 1 {
            let marks = ScrollMarks {
                marks: self
                    .git
                    .hunks
                    .iter()
                    .map(|hunk| {
                        let color = match hunk.kind {
                            LineHunkKind::Added => theme.created,
                            LineHunkKind::Modified => theme.modified,
                            LineHunkKind::Deleted => theme.deleted,
                        };
                        (hunk.new_start, color)
                    })
                    .collect(),
                cursor_line: Some(cursor_line),
            };
            paint::scrollbar(canvas, area.right() - 1, area.y, area.height, line_count, self.scroll_top, rows, &marks, theme);
        }

        let visible = cursor_line >= self.scroll_top && cursor_line < self.scroll_top + rows;
        let col = cursor_col.checked_sub(self.left).filter(|col| *col < text_width as usize);
        match (visible, col) {
            (true, Some(col)) => Some(Cursor {
                col: self.text_area.x + col as u16,
                row: area.y + (cursor_line - self.scroll_top) as u16,
                shape: CursorShape::Block,
            }),
            _ => None,
        }
    }

    fn contexts(&self, out: &mut Vec<Context>) {
        let mut context = Context::new("Editor").value("mode", "full");
        if self.nav {
            context = context.value("vim_mode", "normal");
        }
        if let Some(extension) = self.editor.buffer().path().and_then(|p| p.extension()) {
            context = context.value("extension", extension.to_string_lossy().into_owned());
        }
        out.push(context);
    }

    fn handle_action(&mut self, action: &Action, cx: &mut Cx<'_>) -> bool {
        let half_page = (self.text_area.height as usize / 2).max(1);
        match action.name.as_str() {
            "editor::ScrollCursorCenter" => self.reveal = Reveal::Center,
            "editor::LineDown" => self.scroll_by(1),
            "editor::LineUp" => self.scroll_by(-1),
            "editor::PageDown" => self.scroll_by(self.text_area.height.max(1) as isize),
            "editor::PageUp" => self.scroll_by(-(self.text_area.height.max(1) as isize)),
            "vim::ScrollDown" => return self.run(Command::Move(Motion::DownByLines { lines: half_page }), cx),
            "vim::ScrollUp" => return self.run(Command::Move(Motion::UpByLines { lines: half_page }), cx),
            name => match Command::from_action(name, action.args.as_ref()) {
                Some(command) => return self.run(command, cx),
                None => return false,
            },
        }
        true
    }

    fn handle_key(&mut self, _key: &KeyEvent, _cx: &mut Cx<'_>) -> bool {
        // Nothing types into a read-only view; the keymap already ran every binding.
        false
    }

    fn handle_mouse(&mut self, event: &MouseEvent, area: Rect, _cx: &mut Cx<'_>) -> bool {
        match event.kind {
            MouseKind::ScrollDown => self.scroll_by(WHEEL_LINES as isize),
            MouseKind::ScrollUp => self.scroll_by(-(WHEEL_LINES as isize)),
            MouseKind::Press(MouseButton::Left) => {
                let inside = event.col >= area.x && event.col < area.right() && event.row >= area.y && event.row < area.bottom();
                if !inside || event.col < self.text_area.x {
                    return inside;
                }
                let pos = self.position_at_cell(event.col, event.row);
                if event.mods.contains(Mods::SHIFT) {
                    self.editor.extend_primary_to(pos);
                } else {
                    self.editor.set_cursor(pos);
                }
            }
            MouseKind::Drag(MouseButton::Left) => {
                let pos = self.position_at_cell(event.col, event.row);
                self.editor.extend_primary_to(pos);
                self.reveal = Reveal::Fit;
            }
            _ => return false,
        }
        true
    }

    fn tick(&mut self, cx: &mut Cx<'_>) -> bool {
        let mut dirty = false;
        if self.nav != cx.settings.vim_mode {
            self.nav = cx.settings.vim_mode;
            dirty = true;
        }
        // New highlights and a new committed text arrive through the project: redraw when
        // they differ from what the last draw used.
        if cx.project.syntax(self.doc_id).map(|result| result.version) != self.shown_highlights {
            dirty = true;
        }
        if let Some(path) = self.editor.buffer().path() {
            let base = cx.project.git_base(path);
            let same = match (&base, &self.git.base) {
                (Some(new), Some(old)) => Arc::ptr_eq(new, old),
                (None, None) => true,
                _ => false,
            };
            dirty |= !same;
        }
        dirty
    }

    fn paths_changed(&mut self, change: &FsChange, cx: &mut Cx<'_>) {
        let Some(path) = self.editor.buffer().path().map(Path::to_path_buf) else { return };
        if !change.touches(&path) {
            return;
        }
        match self.editor.reload() {
            Ok(_) => self.request_highlights(cx.project),
            // Gone or unreadable: keep showing what was there.
            Err(error) => log::info!("not reloading {}: {error}", path.display()),
        }
    }

    fn picker_rows(&mut self, kind: PickerKind, _cx: &mut Cx<'_>) -> Option<Vec<PickerItem>> {
        if kind != PickerKind::Outline || !self.language.has_grammar() {
            return None;
        }
        self.outline = bakedit_syntax::outline_with_line_breaks(self.language, &self.editor.buffer().to_string(), bakedit_syntax::LineBreaks::Lf);
        self.before_preview = None;
        Some(
            self.outline
                .iter()
                .map(|symbol| PickerItem::new(format!("{}{}", "  ".repeat(symbol.depth), symbol.name)).hint(format!("{}", symbol.line + 1)))
                .collect(),
        )
    }

    fn picker_preview(&mut self, kind: PickerKind, row: Option<usize>, _cx: &mut Cx<'_>) {
        if kind != PickerKind::Outline {
            return;
        }
        match row.and_then(|row| self.outline.get(row)) {
            Some(symbol) => {
                if self.before_preview.is_none() {
                    self.before_preview = Some((self.scroll_top, self.cursor()));
                }
                let rows = (self.text_area.height as usize).max(1);
                self.scroll_top = symbol.line.saturating_sub(rows / 3);
            }
            None => {
                if let Some((top, cursor)) = self.before_preview.take() {
                    self.scroll_top = top;
                    self.editor.set_cursor(cursor);
                }
            }
        }
    }

    fn picker_confirm(&mut self, kind: PickerKind, row: usize, _secondary: bool, _cx: &mut Cx<'_>) {
        if kind != PickerKind::Outline {
            return;
        }
        if let Some(symbol) = self.outline.get(row) {
            let position = self.editor.buffer().byte_to_char(symbol.byte);
            self.editor.set_cursor(position);
            self.before_preview = None;
            self.reveal = Reveal::Center;
        }
    }
}

impl Item for EditorItem {
    fn status(&self) -> ItemStatus {
        let selection = self.editor.selections().primary();
        let point = self.editor.buffer().char_to_point(selection.head);
        ItemStatus {
            mode: self.nav.then_some(ModeIndicator::Nav),
            position: Some((point.line + 1, point.col + 1, selection.len())),
            cursors: self.editor.selections().len(),
            language: Some(self.language.name.to_string()),
            indent: Some(self.indent_label()),
            line_ending: Some(match self.editor.line_ending() {
                LineEnding::Lf => "LF",
                LineEnding::CrLf => "CRLF",
            }),
        }
    }

    fn kind(&self) -> &'static str {
        self.kind
    }

    fn tab(&self, cx: &RenderCx<'_>) -> Tab {
        let icon = Some(file_icon(cx.settings, &self.title, false, false));
        Tab { title: self.title.clone(), dirty: false, preview: false, icon }
    }

    fn path(&self) -> Option<&Path> {
        self.editor.buffer().path()
    }

    fn go_to(&mut self, line: u32, column: Option<u32>) {
        let motion = Motion::GoToLine { line: line.max(1) as usize, column: column.unwrap_or(1).max(1) as usize };
        self.editor.apply(Command::Move(motion));
        self.reveal = Reveal::Center;
    }

    fn position(&self) -> Option<(u32, u32)> {
        let point = self.editor.buffer().char_to_point(self.cursor());
        Some((point.line as u32 + 1, point.col as u32 + 1))
    }

    fn scroll_top(&self) -> Option<u32> {
        Some(self.scroll_top as u32)
    }

    fn set_scroll_top(&mut self, line: u32) {
        self.scroll_top = (line as usize).min(self.editor.buffer().len_lines().saturating_sub(1));
        self.reveal = Reveal::No;
    }

    fn query_suggestion(&self) -> Option<String> {
        let selection = self.editor.selections().primary();
        let range = if selection.is_empty() { self.editor.word_at(selection.head)? } else { selection.range() };
        let text = self.editor.buffer().slice(range).to_string();
        (!text.is_empty() && !text.contains('\n')).then_some(text)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[cfg(test)]
mod tests;
