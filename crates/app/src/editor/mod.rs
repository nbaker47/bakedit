//! The editor: a file, an unsaved buffer or a read-only page in a tab of the centre.
//!
//! # What the app calls (these signatures stay)
//!
//! * `EditorItem::open(path: &Path, cx: &mut Cx<'_>) -> Result<EditorItem, String>`: the
//!   file at `path` (absolute). The error is one line for the status bar. A binary file or
//!   one too big to load opens as a read-only note saying so instead of failing.
//! * `EditorItem::untitled(cx: &mut Cx<'_>) -> EditorItem`: an empty buffer with no file.
//! * `EditorItem::page(title: &str, text: &str, language: Option<&str>, cx: &mut Cx<'_>) -> EditorItem`:
//!   text nobody edits (the key bindings, the built-in keymap); `kind()` is `"page"` and
//!   `language` is a language name such as `"JSON"` to colour it as.
//!
//! Beyond those the app reaches it only through the `View` and `Item` traits: `editor::*`,
//! `vim::NormalBefore`, `bakedit::ToggleMode` (also sent for a click on the mode in the status bar),
//! `buffer_search::*` and `search::*` actions from the keymap, `workspace::SaveAsPath`
//! `{path}`, `go_to` when a file is opened at a line, `picker_rows`, `picker_preview` and
//! `picker_confirm` for the outline, `paths_changed` when the file changes on disk,
//! `focus_changed` for autosave, `save`, `position`, `scroll_top` and `set_scroll_top` for
//! the session file, and `query_suggestion` to seed a search.
//!
//! # How it is put together
//!
//! * `bakedit_text::Editor` owns the text, the selections, undo and every command.
//! * `render.rs` draws (gutter, text rows, minimap, scrollbar, breadcrumbs) through
//!   `paint.rs` and `minimap.rs`; `layout.rs` holds the scroll model (soft wrap included).
//! * `actions.rs` turns actions and typed keys into commands and runs nav and edit mode;
//!   `mouse.rs` handles the mouse.
//! * `find.rs` is the find-in-file bar; `files.rs` saving, formatting and reloading.
//! * `shift.rs` keeps the colours of the old text in place while new ones are worked out;
//!   `marks.rs` works out the git marks off the keystroke path.

mod actions;
mod blame;
mod files;
mod find;
mod layout;
mod marks;
mod mouse;
mod render;
mod shift;

use std::any::Any;
use std::path::{Path, PathBuf};

use bakedit_core::{Action, KeyEvent, MouseEvent};
use bakedit_syntax::{Language, StyleCache, Symbol};
use bakedit_text::{BufferError, Command, Editor, IndentStyle, LineEnding, Motion};
use ratatui::layout::Rect;

use crate::chrome::{ModeIndicator, Tab};
use crate::icons::file_icon;
use crate::picker::PickerItem;
use crate::settings::Settings;
use crate::ui::Canvas;
use crate::view::{Context, Cursor, Cx, FsChange, Item, ItemStatus, PickerKind, RenderCx, View};

use self::blame::GitBlame;
use self::files::FileState;
use self::find::FindBar;
use self::layout::{Geometry, Reveal, Scroll};
use self::marks::GitMarks;
use self::mouse::MouseState;
use self::shift::ShiftLog;

/// Files above this many bytes are not loaded: the tab says so instead.
const MAX_OPEN_BYTES: u64 = 256 * 1024 * 1024;
/// Files above this many bytes are edited without syntax colours or git marks.
const LARGE_BYTES: u64 = 8 * 1024 * 1024;

/// A file, an unsaved buffer or a read-only page.
pub struct EditorItem {
    editor: Editor,
    title: String,
    kind: &'static str,
    language: &'static Language,
    doc_id: u64,
    /// The file a read-only note stands in for.
    stand_in_for: Option<PathBuf>,
    /// Why the text cannot be changed, shown when a key tries.
    read_only: Option<String>,
    /// Too big for syntax colours and git marks.
    large: bool,

    /// The buffer version last sent to the highlighting thread.
    highlighted: Option<u64>,
    /// The version of the highlights the last draw used.
    shown_highlights: Option<u64>,
    styles: StyleCache,
    /// The theme the style cache was filled from.
    styles_theme: String,
    shifts: ShiftLog,

    scroll: Scroll,
    /// First display column shown (no soft wrap only).
    left: usize,
    geo: Geometry,
    reveal: Reveal,
    wrap_override: Option<bool>,
    numbers_override: Option<bool>,
    relative_override: Option<bool>,
    minimap_override: Option<bool>,

    /// Nav and edit modes are on (`vim_mode`).
    modal: bool,
    /// In edit mode (only meaningful when `modal`).
    insert: bool,
    /// How many undo steps there were when edit mode began: leaving it fuses the steps
    /// since into one, so that one undo takes back a whole edit.
    insert_mark: Option<usize>,

    git: GitMarks,
    blame: GitBlame,
    find: Option<Box<FindBar>>,
    /// The search `n` repeats once the bar is closed.
    last_search: Option<bakedit_text::Search>,
    files: FileState,
    mouse: MouseState,

    /// The outline rows last handed to the outline picker.
    outline: Vec<Symbol>,
    /// Where the view and cursor were before the outline picker previewed a symbol.
    before_preview: Option<(Scroll, usize)>,
}

impl EditorItem {
    /// The file at `path` (absolute). Fails with one line for the status bar when the file
    /// cannot be read (missing, a directory, unreadable).
    pub fn open(path: &Path, cx: &mut Cx<'_>) -> Result<EditorItem, String> {
        let shown = files::short_path(path, cx.root);
        let size = match std::fs::metadata(path) {
            Ok(meta) if meta.is_dir() => return Err(format!("{shown} is a folder")),
            Ok(meta) => meta.len(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Err(format!("{shown} does not exist")),
            Err(_) => 0,
        };
        let title = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
        if size > MAX_OPEN_BYTES {
            let note = format!("{shown} is {} MB, too large to open here.\n\nOpen it with another program.\n", size / (1024 * 1024));
            return Ok(EditorItem::stand_in(path, title, &note, cx));
        }
        let editor = match Editor::open(path) {
            Ok(editor) => editor,
            Err(BufferError::Binary { .. }) => {
                let note = format!("{shown} is a binary file ({size} bytes), so it is not shown.\n\nOpen it with another program.\n");
                return Ok(EditorItem::stand_in(path, title, &note, cx));
            }
            Err(error) => return Err(format!("Cannot open {shown}: {error}")),
        };
        let first_line = editor.buffer().line_text(0).into_owned();
        let language = bakedit_syntax::detect(path, Some(&first_line));
        let mut item = EditorItem::new(editor, title, "editor", language, cx);
        if size > LARGE_BYTES {
            item.large = true;
            cx.status(format!("{shown} is large: syntax colours and git marks are off"));
        }
        if editor_had_bad_utf8(&item) {
            item.read_only = Some(format!("{shown} is not valid UTF-8; saving would overwrite the bytes it could not read"));
            cx.error(format!("{shown} is not valid UTF-8: opened read-only"));
        }
        Ok(item)
    }

    /// An empty buffer that belongs to no file yet.
    pub fn untitled(cx: &mut Cx<'_>) -> EditorItem {
        EditorItem::new(Editor::from_str(""), "untitled".to_string(), "editor", bakedit_syntax::plain_text(), cx)
    }

    /// A page of text nobody edits, coloured as `language` (a name such as "JSON") if given.
    pub fn page(title: &str, text: &str, language: Option<&str>, cx: &mut Cx<'_>) -> EditorItem {
        let language = language.and_then(bakedit_syntax::language_by_name).unwrap_or_else(bakedit_syntax::plain_text);
        let mut item = EditorItem::new(Editor::from_str(text), title.to_string(), "page", language, cx);
        item.read_only = Some(format!("{title} is read-only"));
        item
    }

    /// A read-only note in the place of a file that cannot be shown.
    fn stand_in(path: &Path, title: String, note: &str, cx: &mut Cx<'_>) -> EditorItem {
        let mut item = EditorItem::new(Editor::from_str(note), title, "editor", bakedit_syntax::plain_text(), cx);
        item.stand_in_for = Some(path.to_path_buf());
        item.read_only = Some(note.lines().next().unwrap_or("This file cannot be shown").to_string());
        item
    }

    fn new(editor: Editor, title: String, kind: &'static str, language: &'static Language, cx: &mut Cx<'_>) -> EditorItem {
        let mut item = EditorItem {
            editor,
            title,
            kind,
            language,
            doc_id: cx.project.next_doc_id(),
            stand_in_for: None,
            read_only: None,
            large: false,
            highlighted: None,
            shown_highlights: None,
            styles: StyleCache::new(),
            styles_theme: cx.theme.name.clone(),
            shifts: ShiftLog::default(),
            scroll: Scroll::default(),
            left: 0,
            geo: Geometry::default(),
            reveal: Reveal::No,
            wrap_override: None,
            numbers_override: None,
            relative_override: None,
            minimap_override: None,
            modal: cx.settings.vim_mode,
            insert: false,
            insert_mark: None,
            git: GitMarks::default(),
            blame: GitBlame::default(),
            find: None,
            last_search: None,
            files: FileState::default(),
            mouse: MouseState::default(),
            outline: Vec::new(),
            before_preview: None,
        };
        item.sync_engine(cx.settings);
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

    fn file_path(&self) -> Option<&Path> {
        self.stand_in_for.as_deref().or_else(|| self.editor.buffer().path())
    }

    fn indent_label(&self) -> String {
        match self.editor.detect_indent() {
            Some(IndentStyle::Tabs) => "Tabs".to_string(),
            Some(IndentStyle::Spaces(width)) => format!("Spaces: {width}"),
            None => format!("Spaces: {}", self.language.tab_size),
        }
    }

    /// Send the text to the highlighting thread if it changed since it was last sent.
    fn request_highlights(&mut self, project: &crate::project::Project) {
        let version = self.editor.buffer().version();
        if self.large || !self.language.has_grammar() || self.highlighted == Some(version) {
            return;
        }
        self.highlighted = Some(version);
        project.request_syntax(self.doc_id, version, self.language, self.editor.buffer().to_string());
    }

    /// What the editor's own commands need to know from the settings and the language:
    /// indentation, auto-closing pairs, the word characters, the wrap width and page size.
    fn sync_engine(&mut self, settings: &Settings) {
        let language = self.language;
        let tab = self.tab_size(settings);
        let configured = settings.language(language.name);
        let hard_tabs = configured.and_then(|l| l.hard_tabs).unwrap_or(if language.hard_tabs { true } else { settings.hard_tabs });
        let pairs: Vec<(char, char)> = language.autoclose.iter().map(|pair| (pair.open, pair.close)).collect();
        let engine = self.editor.settings_mut();
        engine.tab_size = tab;
        engine.hard_tabs = hard_tabs;
        engine.auto_indent = settings.auto_indent;
        engine.autoclose = settings.use_autoclose;
        engine.autoclose_pairs = if settings.use_autoclose { pairs.clone() } else { Vec::new() };
        engine.surround_pairs = if settings.use_auto_surround { pairs } else { Vec::new() };
        engine.indent_after = language.indent_after.to_string();
        engine.word_chars = language.word_chars.to_string();
        engine.page_rows = (self.geo.text.height as usize).max(1);
    }
}

fn editor_had_bad_utf8(item: &EditorItem) -> bool {
    item.editor.buffer().had_invalid_utf8()
}

/// The tab size for a language: the user's setting for that language, then the language's
/// own convention where it has one (2 for JSON, YAML, JavaScript), then the global setting.
pub fn effective_tab_size(settings: &Settings, language: &Language) -> usize {
    let configured = settings.language(language.name).and_then(|l| l.tab_size);
    let own = (language.tab_size != 4).then_some(language.tab_size);
    usize::from(configured.or(own).unwrap_or(settings.tab_size)).max(1)
}

impl View for EditorItem {
    fn render(&mut self, canvas: &mut Canvas<'_>, area: Rect, cx: &RenderCx<'_>) -> Option<Cursor> {
        self.draw(canvas, area, cx)
    }

    fn contexts(&self, out: &mut Vec<Context>) {
        self.key_contexts(out);
    }

    fn handle_action(&mut self, action: &Action, cx: &mut Cx<'_>) -> bool {
        self.act(action, cx)
    }

    fn handle_key(&mut self, key: &KeyEvent, cx: &mut Cx<'_>) -> bool {
        self.type_key(key, cx)
    }

    fn handle_paste(&mut self, text: &str, cx: &mut Cx<'_>) -> bool {
        self.paste_text(text, cx)
    }

    fn handle_mouse(&mut self, event: &MouseEvent, area: Rect, cx: &mut Cx<'_>) -> bool {
        self.mouse_event(event, area, cx)
    }

    fn focus_changed(&mut self, focused: bool, cx: &mut Cx<'_>) {
        self.on_focus(focused, cx);
    }

    fn tick(&mut self, cx: &mut Cx<'_>) -> bool {
        let mut dirty = self.on_tick(cx);
        dirty |= self.refresh_git_blame(cx.project);
        dirty
    }

    fn paths_changed(&mut self, change: &FsChange, cx: &mut Cx<'_>) {
        self.on_paths_changed(change, cx);
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
                    self.before_preview = Some((self.scroll, self.cursor()));
                }
                let rows = self.geo.text.height.max(1) as usize;
                self.scroll = Scroll { line: symbol.line.saturating_sub(rows / 3), row: 0 };
            }
            None => {
                if let Some((scroll, cursor)) = self.before_preview.take() {
                    self.scroll = scroll;
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
        let mode = match (self.modal, self.insert) {
            (false, _) => None,
            (true, true) => Some(ModeIndicator::Edit),
            (true, false) => Some(ModeIndicator::Nav),
        };
        ItemStatus {
            mode,
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
        Tab { title: self.title.clone(), dirty: self.is_dirty(), preview: false, pinned: false, icon }
    }

    fn path(&self) -> Option<&Path> {
        self.file_path()
    }

    fn is_dirty(&self) -> bool {
        self.read_only.is_none() && self.editor.buffer().is_dirty()
    }

    fn save(&mut self, format: bool, cx: &mut Cx<'_>) -> Result<(), String> {
        let res = self.save_file(format, cx);
        if res.is_ok() {
            self.blame.invalidate();
        }
        res
    }

    fn go_to(&mut self, line: u32, column: Option<u32>) {
        let motion = Motion::GoToLine { line: line.max(1) as usize, column: column.unwrap_or(1).max(1) as usize };
        self.editor.break_undo_group();
        self.editor.apply(Command::Move(motion));
        self.reveal = Reveal::Center;
    }

    fn highlight_lines(&mut self, start_line: u32, end_line: u32) {
        let buffer = self.editor.buffer();
        let max_line = buffer.len_lines().max(1);
        let start_idx = (start_line.max(1) as usize).min(max_line) - 1;
        let end_idx = (end_line.max(1) as usize).min(max_line).max(start_idx + 1) - 1;
        let start_char = buffer.line_to_char(start_idx);
        let end_char = if end_idx + 1 < buffer.len_lines() {
            buffer.line_to_char(end_idx + 1)
        } else {
            buffer.len_chars()
        };
        // Anchor at the end and place head at the start so `Reveal::Center` centers the top of
        // the highlighted block while the selection covers every line in `start_line..=end_line`.
        let selection = bakedit_text::Selection::new(end_char, start_char);
        self.editor.break_undo_group();
        self.editor.set_selections(bakedit_text::Selections::new(vec![selection], 0));
        self.reveal = Reveal::Center;
    }

    fn line_count(&self) -> Option<usize> {
        Some(self.editor.buffer().len_lines())
    }

    fn position(&self) -> Option<(u32, u32)> {
        let point = self.editor.buffer().char_to_point(self.cursor());
        Some((point.line as u32 + 1, point.col as u32 + 1))
    }

    fn scroll_top(&self) -> Option<u32> {
        Some(self.scroll.line as u32)
    }

    fn set_scroll_top(&mut self, line: u32) {
        self.scroll = Scroll { line: (line as usize).min(self.editor.buffer().len_lines().saturating_sub(1)), row: 0 };
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

    fn closed(&mut self, cx: &mut Cx<'_>) {
        cx.project.forget_syntax(self.doc_id);
    }

    fn moved(&mut self, from: &Path, to: &Path, cx: &mut Cx<'_>) {
        if self.stand_in_for.is_some() || self.kind == "page" {
            return;
        }
        let Some(rest) = self.editor.buffer().path().and_then(|path| path.strip_prefix(from).ok()).map(Path::to_path_buf) else { return };
        let path = if rest.as_os_str().is_empty() { to.to_path_buf() } else { to.join(rest) };
        self.title = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
        // A new extension can mean a new language (notes.txt renamed to notes.md).
        let first_line = self.editor.buffer().line_text(0).into_owned();
        let language = bakedit_syntax::detect(&path, Some(&first_line));
        self.editor.follow_move(path);
        if !std::ptr::eq(language, self.language) {
            self.language = language;
            self.highlighted = None;
            self.request_highlights(cx.project);
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[cfg(test)]
mod tests;
