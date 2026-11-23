//! Files: saving (with the clean-ups, formatting and autosave), reloading when the file
//! changes on disk, read-only handling, and the permalink.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bakedit_text::{Outcome, SaveOptions};

use super::layout::Reveal;
use super::shift;
use super::EditorItem;
use crate::background::Task;
use crate::format::{self, FormatError};
use crate::settings::{Autosave, Settings};
use crate::view::{Cx, Effect, FsChange, Item, Question};
use bakedit_core::Action;
use bakedit_text::DiskState;

/// How long a save waits for the formatter before it saves the text as it is and applies the
/// formatting when it arrives.
const FORMAT_WAIT: Duration = Duration::from_millis(250);

/// A formatter still running when the save did not wait for it any longer.
struct Formatting {
    task: Task<Result<String, FormatError>>,
    /// The buffer version the text was taken at.
    version: u64,
    /// Save again once the result is in (a save started it).
    save_after: bool,
}

#[derive(Default)]
pub(super) struct FileState {
    formatting: Option<Formatting>,
    /// The file changed on disk while there were unsaved changes and nobody has been asked.
    ask_when_shown: bool,
    /// When the text last changed, for autosave after a delay.
    last_edit: Option<Instant>,
}

/// `path` relative to the project root when it is inside it, for messages.
pub(super) fn short_path(path: &Path, root: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).display().to_string()
}

/// The text of `old` turned into `new` by replacing only what lies between the common start
/// and the common end, so a cursor outside that stretch stays where it is.
pub(super) fn minimal_edit(old: &str, new: &str) -> Option<(std::ops::Range<usize>, String)> {
    if old == new {
        return None;
    }
    let prefix = old.chars().zip(new.chars()).take_while(|(a, b)| a == b).count();
    let old_len = old.chars().count();
    let new_len = new.chars().count();
    let max_suffix = old_len.min(new_len) - prefix;
    let suffix = old.chars().rev().zip(new.chars().rev()).take(max_suffix).take_while(|(a, b)| a == b).count();
    let replacement: String = new.chars().skip(prefix).take(new_len - prefix - suffix).collect();
    Some((prefix..old_len - suffix, replacement))
}

impl EditorItem {
    /// Say why the text cannot change, if it cannot. Returns whether it is blocked.
    pub(super) fn read_only_blocks(&self, cx: &mut Cx<'_>) -> bool {
        match &self.read_only {
            Some(reason) => {
                cx.error(reason.clone());
                true
            }
            None => false,
        }
    }

    /// Bookkeeping after the editor changed the text or the selections: what the colours,
    /// git marks, scroll and autosave need to know, and what was cut or copied.
    pub(super) fn after_outcome(&mut self, outcome: &Outcome, cx: &mut Cx<'_>) {
        if let Some(clip) = &outcome.clipboard {
            cx.clipboard.set(&clip.text, clip.linewise);
            super::actions::remember_copy(clip.clone());
        }
        if outcome.text_changed() {
            let version = self.editor.buffer().version();
            if !self.large {
                self.shifts.push(version, &outcome.edits);
            }
            shift::shift_hunks(&mut self.git.hunks, version, &outcome.edits);
            self.files.last_edit = Some(cx.now);
            if let Autosave::AfterDelay { milliseconds } = cx.settings.autosave {
                cx.project.waker().wake_at(cx.now + Duration::from_millis(milliseconds));
            }
        }
        if outcome.selections_changed || outcome.text_changed() {
            self.reveal = Reveal::Fit;
        }
    }

    fn save_options(&self, settings: &Settings) -> SaveOptions {
        let configured = settings.language(self.language.name);
        // Trailing spaces mean a line break in Markdown.
        let trim_default = if self.language.id == "markdown" { false } else { settings.remove_trailing_whitespace_on_save };
        SaveOptions {
            ensure_final_newline: configured.and_then(|l| l.ensure_final_newline_on_save).unwrap_or(settings.ensure_final_newline_on_save),
            remove_trailing_whitespace: configured.and_then(|l| l.remove_trailing_whitespace_on_save).unwrap_or(trim_default),
        }
    }

    fn format_command(&self, cx: &Cx<'_>) -> Option<Vec<String>> {
        let path = self.editor.buffer().path()?;
        format::formatter_for(self.language.name, path, cx.root, &cx.settings.formatters)
    }

    fn format_on_save(&self, settings: &Settings) -> bool {
        settings.language(self.language.name).and_then(|l| l.format_on_save).unwrap_or(settings.format_on_save)
    }

    /// Start the formatter on the text as it is.
    fn start_format(&mut self, command: Vec<String>, save_after: bool, cx: &mut Cx<'_>) -> Option<Formatting> {
        let path = self.editor.buffer().path()?.to_path_buf();
        let root = cx.root.to_path_buf();
        let text = self.editor.buffer().to_string();
        let task = cx.project.spawn(move || format::run(&command, &path, &root, &text, format::TIMEOUT));
        Some(Formatting { task, version: self.editor.buffer().version(), save_after })
    }

    /// `editor::Format`: format the file now, in the background.
    pub(super) fn format_now(&mut self, cx: &mut Cx<'_>) -> bool {
        if self.read_only_blocks(cx) {
            return true;
        }
        let Some(command) = self.format_command(cx) else {
            let reason = if self.editor.buffer().path().is_some() { "No formatter is set up for this project" } else { "Save the file first to format it" };
            cx.status(format!("{reason} ({})", self.language.name));
            return true;
        };
        self.files.formatting = self.start_format(command, false, cx);
        true
    }

    /// Replace the text with what the formatter printed. Returns whether anything changed.
    fn apply_formatted(&mut self, text: &str, cx: &mut Cx<'_>) -> bool {
        let old = self.editor.buffer().to_string();
        let Some((range, replacement)) = minimal_edit(&old, text) else { return false };
        let outcome = self.editor.edit(&[(range, replacement)]);
        self.after_outcome(&outcome, cx);
        true
    }

    /// Write the file. `format` is false for "save without formatting".
    pub(super) fn save_file(&mut self, format: bool, cx: &mut Cx<'_>) -> Result<(), String> {
        if self.kind == "page" {
            return Ok(());
        }
        if let Some(reason) = &self.read_only {
            return Err(reason.clone());
        }
        let Some(path) = self.editor.buffer().path().map(Path::to_path_buf) else {
            cx.push(Effect::Dispatch(Action::new("workspace::SaveAs")));
            return Err("This buffer has no file yet: choose where to save it".to_string());
        };
        self.files.formatting = None;
        let mut late = None;
        let mut note = None;
        if format && self.format_on_save(cx.settings) {
            if let Some(command) = self.format_command(cx) {
                if let Some(mut running) = self.start_format(command, true, cx) {
                    let started = Instant::now();
                    let mut result = None;
                    while started.elapsed() < FORMAT_WAIT {
                        if let Some(done) = running.task.poll() {
                            result = Some(done);
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(4));
                    }
                    match result {
                        Some(Ok(text)) => {
                            self.apply_formatted(&text, cx);
                        }
                        Some(Err(error)) => note = Some(format!("Saved without formatting: {error}")),
                        None if running.task.is_finished() => note = Some("Saved without formatting: the formatter stopped".to_string()),
                        None => late = Some(running),
                    }
                }
            }
        }
        let options = self.save_options(cx.settings);
        let outcome = self.editor.save(options).map_err(|error| save_error(&path, cx.root, &error.to_string()))?;
        self.after_outcome(&outcome, cx);
        self.files.ask_when_shown = false;
        if let Some(mut running) = late {
            running.version = self.editor.buffer().version();
            self.files.formatting = Some(running);
        }
        if let Some(note) = note {
            cx.error(note);
        }
        cx.push(Effect::RefreshGit);
        Ok(())
    }

    /// `workspace::SaveAsPath`: write the buffer to a new path and make that its file.
    pub(super) fn save_as(&mut self, path: PathBuf, cx: &mut Cx<'_>) -> bool {
        if self.kind == "page" || self.read_only.is_some() {
            cx.error("This tab cannot be saved to a file");
            return true;
        }
        if let Some(folder) = path.parent() {
            if let Err(error) = std::fs::create_dir_all(folder) {
                cx.error(format!("Cannot create {}: {error}", folder.display()));
                return true;
            }
        }
        let options = self.save_options(cx.settings);
        match self.editor.save_as(path.clone(), options) {
            Ok(outcome) => {
                self.after_outcome(&outcome, cx);
                self.title = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let first_line = self.editor.buffer().line_text(0).into_owned();
                self.language = bakedit_syntax::detect(&path, Some(&first_line));
                self.highlighted = None;
                self.git = Default::default();
                self.sync_engine(cx.settings);
                self.request_highlights(cx.project);
                let shown = short_path(&path, cx.root);
                cx.status(format!("Saved {shown}"));
                cx.push(Effect::PathsChanged(vec![path]));
            }
            Err(error) => cx.error(save_error(&path, cx.root, &error.to_string())),
        }
        true
    }

    /// The file changed on disk (or something may have): reload it when there is nothing
    /// unsaved, otherwise ask.
    pub(super) fn on_paths_changed(&mut self, change: &FsChange, cx: &mut Cx<'_>) {
        if self.stand_in_for.is_some() || self.kind == "page" {
            return;
        }
        let Some(path) = self.editor.buffer().path().map(Path::to_path_buf) else { return };
        if !change.touches(&path) {
            return;
        }
        let shown = short_path(&path, cx.root);
        match self.editor.buffer().disk_state() {
            DiskState::Unchanged => {}
            DiskState::Deleted => cx.status(format!("{shown} was deleted on disk; saving will create it again")),
            DiskState::Modified if self.editor.buffer().is_dirty() => {
                if cx.active_path == Some(path.as_path()) {
                    self.ask_reload(&shown, cx);
                } else {
                    self.files.ask_when_shown = true;
                }
            }
            DiskState::Modified => match self.editor.reload() {
                Ok(outcome) => {
                    self.after_outcome(&outcome, cx);
                    self.reveal = Reveal::No;
                    self.request_highlights(cx.project);
                }
                // Unreadable for now (being written, say): keep showing what was there.
                Err(error) => log::info!("not reloading {}: {error}", path.display()),
            },
        }
    }

    fn ask_reload(&mut self, shown: &str, cx: &mut Cx<'_>) {
        self.files.ask_when_shown = false;
        cx.push(Effect::Ask(Question {
            message: format!("{shown} changed on disk"),
            detail: Some("You have unsaved changes. Reloading throws them away.".to_string()),
            answers: vec![
                ("Reload".to_string(), vec![Effect::Dispatch(Action::new("editor::ReloadFromDisk"))]),
                ("Keep mine".to_string(), Vec::new()),
            ],
        }));
    }

    /// `editor::ReloadFromDisk`: take the file's text over the unsaved changes.
    pub(super) fn reload_from_disk(&mut self, cx: &mut Cx<'_>) -> bool {
        match self.editor.reload() {
            Ok(outcome) => {
                self.after_outcome(&outcome, cx);
                self.request_highlights(cx.project);
                true
            }
            Err(error) => {
                cx.error(format!("Cannot reload: {error}"));
                true
            }
        }
    }

    pub(super) fn on_focus(&mut self, focused: bool, cx: &mut Cx<'_>) {
        if focused {
            if self.files.ask_when_shown && self.editor.buffer().disk_state() == DiskState::Modified {
                if let Some(path) = self.editor.buffer().path().map(Path::to_path_buf) {
                    self.ask_reload(&short_path(&path, cx.root), cx);
                }
            }
            return;
        }
        let autosave = matches!(cx.settings.autosave, Autosave::OnFocusChange | Autosave::OnWindowChange);
        if autosave && self.is_dirty() && self.editor.buffer().path().is_some() {
            if let Err(message) = self.save_file(true, cx) {
                cx.error(message);
            }
        }
    }

    /// Everything that arrives in the background: highlights, git marks, matches, the
    /// formatter, the nav-mode setting, the autosave timer. Returns whether to redraw.
    pub(super) fn on_tick(&mut self, cx: &mut Cx<'_>) -> bool {
        let mut dirty = false;
        if self.modal != cx.settings.vim_mode {
            self.modal = cx.settings.vim_mode;
            self.end_insert_run();
            self.insert = false;
            self.selecting = None;
            self.clamp_nav_cursor();
            dirty = true;
        }
        // New highlights arrive through the project: redraw when they differ from the last draw.
        if cx.project.syntax(self.doc_id).map(|result| result.version) != self.shown_highlights {
            dirty = true;
        }
        dirty |= self.refresh_git_marks(cx.project);
        dirty |= self.find_refresh_matches(cx.project);

        if let Some(mut running) = self.files.formatting.take() {
            match running.task.poll() {
                Some(result) => {
                    dirty = true;
                    self.finish_format(result, running.version, running.save_after, cx);
                }
                None if running.task.is_finished() => {}
                None => self.files.formatting = Some(running),
            }
        }

        if let (Autosave::AfterDelay { milliseconds }, Some(last)) = (cx.settings.autosave, self.files.last_edit) {
            let due = last + Duration::from_millis(milliseconds);
            if self.is_dirty() && self.editor.buffer().path().is_some() {
                if cx.now >= due {
                    self.files.last_edit = None;
                    if let Err(message) = self.save_file(true, cx) {
                        cx.error(message);
                    }
                    dirty = true;
                } else {
                    cx.project.waker().wake_at(due);
                }
            }
        }
        dirty
    }

    fn finish_format(&mut self, result: Result<String, FormatError>, version: u64, save_after: bool, cx: &mut Cx<'_>) {
        let name = self.title.clone();
        match result {
            Err(error) => cx.error(format!("Not formatted: {error}")),
            Ok(_) if self.editor.buffer().version() != version => {
                cx.status(format!("Formatting of {name} skipped: it changed while the formatter ran"));
            }
            Ok(text) => {
                let changed = self.apply_formatted(&text, cx);
                if save_after && changed {
                    let options = self.save_options(cx.settings);
                    match self.editor.save(options) {
                        Ok(outcome) => {
                            self.after_outcome(&outcome, cx);
                            cx.push(Effect::RefreshGit);
                        }
                        Err(error) => {
                            cx.error(format!("Formatted but not saved: {error}"));
                            return;
                        }
                    }
                }
                cx.status(if changed { format!("Formatted {name}") } else { format!("{name} is already formatted") });
            }
        }
    }

    /// `editor::CopyPermalinkToLine`: a link to the selected lines at the commit in HEAD.
    pub(super) fn copy_permalink(&mut self, cx: &mut Cx<'_>) -> bool {
        let (Some(path), Some(repo)) = (self.editor.buffer().path().map(Path::to_path_buf), cx.project.repo()) else {
            cx.error("Not in a git repository: no permalink to copy");
            return true;
        };
        let selection = self.editor.selections().primary();
        let buffer = self.editor.buffer();
        let first = buffer.char_to_line(selection.start()) + 1;
        // A selection that ends at the start of a line does not include that line.
        let end = if selection.is_empty() || selection.end() == buffer.line_to_char(buffer.char_to_line(selection.end())) {
            selection.end().saturating_sub(usize::from(!selection.is_empty()))
        } else {
            selection.end()
        };
        let last = (buffer.char_to_line(end) + 1).max(first);
        match repo.permalink(&path, first as u32, last as u32) {
            Ok(url) => {
                cx.clipboard.set(&url, false);
                cx.status(format!("Copied permalink: {url}"));
            }
            Err(error) => cx.error(format!("No permalink: {error}")),
        }
        true
    }
}

fn save_error(path: &Path, root: &Path, error: &str) -> String {
    format!("Cannot save {}: {error}", short_path(path, root))
}

#[cfg(test)]
mod tests {
    use super::minimal_edit;

    #[test]
    fn a_minimal_edit_keeps_the_common_ends() {
        assert_eq!(minimal_edit("a b c", "a b c"), None);
        assert_eq!(minimal_edit("fn  main() {}", "fn main() {}"), Some((3..4, String::new())));
        assert_eq!(minimal_edit("abc", "abXYc"), Some((2..2, "XY".to_string())));
        assert_eq!(minimal_edit("abcabc", "abc"), Some((3..6, String::new())));
        assert_eq!(minimal_edit("", "x"), Some((0..0, "x".to_string())));
        assert_eq!(minimal_edit("é!", "é?!"), Some((1..1, "?".to_string())));
    }
}
