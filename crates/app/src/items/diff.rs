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
//! cursor, and staging the file under the cursor (`git::ToggleStaged`, `git::StageFile`,
//! `git::UnstageFile`, `git::StageAll`, `git::UnstageAll`).
//!
//! The diff is read on a background thread and read again when git or the files change.

use std::any::Any;
use std::path::{Path, PathBuf};

use bakedit_core::{Action, KeyEvent, MouseButton, MouseEvent, MouseKind, Rgb, Theme};
use bakedit_git::{DiffLineKind, DiffStatus, DiffTarget, FileDiff};
use ratatui::layout::Rect;

use crate::background::Task;
use crate::chrome::Tab;
use crate::icons::file_icon;
use crate::settings::Icons;
use crate::ui::{Canvas, Style};
use crate::view::{Context, Cursor, Cx, Effect, FsChange, Item, RenderCx, View};

/// Lines a wheel step scrolls.
const WHEEL_ROWS: usize = 3;
/// Columns of each line-number column.
const NUMBER_WIDTH: u16 = 5;

/// One row of the drawn diff.
#[derive(Debug, Clone, PartialEq)]
enum DiffRow {
    /// The name of a file and how much changed in it.
    File(usize),
    /// A hunk's `@@ ... @@` line.
    Hunk { file: usize, header: String },
    Line { file: usize, kind: DiffLineKind, old: Option<u32>, new: Option<u32>, text: String },
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

/// The project diff.
pub struct ProjectDiff {
    files: Vec<FileDiff>,
    rows: Vec<DiffRow>,
    task: Option<Task<Result<Vec<FileDiff>, String>>>,
    /// The git generation the diff was last read at.
    generation: Option<u64>,
    /// Files changed on disk since the diff was read.
    stale: bool,
    error: Option<String>,
    cursor: usize,
    scroll: usize,
    height: usize,
    area: Rect,
    /// A file to scroll to once the diff has arrived.
    wanted: Option<PathBuf>,
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

fn build_rows(files: &[FileDiff]) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    for (index, file) in files.iter().enumerate() {
        rows.push(DiffRow::File(index));
        if file.binary {
            rows.push(DiffRow::Note { file: index, text: "Binary file".to_string() });
        } else if file.too_large {
            rows.push(DiffRow::Note { file: index, text: "Too large to show".to_string() });
        }
        for hunk in &file.hunks {
            let header = format!("@@ -{},{} +{},{} @@ {}", hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines, hunk.header.trim());
            rows.push(DiffRow::Hunk { file: index, header: header.trim_end().to_string() });
            for line in &hunk.lines {
                let text = line.text.trim_end_matches(['\r', '\n']).to_string();
                rows.push(DiffRow::Line { file: index, kind: line.kind, old: line.old_no, new: line.new_no, text });
            }
        }
        rows.push(DiffRow::Gap);
    }
    rows
}

impl ProjectDiff {
    /// The diff of the project's repository against HEAD, read in the background.
    pub fn new(cx: &mut Cx<'_>) -> ProjectDiff {
        let mut diff = ProjectDiff {
            files: Vec::new(),
            rows: Vec::new(),
            task: None,
            generation: None,
            stale: true,
            error: None,
            cursor: 0,
            scroll: 0,
            height: 0,
            area: Rect::default(),
            wanted: None,
        };
        diff.reload(cx);
        diff
    }

    /// Start reading the diff again if git or the files changed since it was read.
    fn reload(&mut self, cx: &Cx<'_>) {
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
        self.task = Some(cx.project.spawn(move || repo.diff_files(DiffTarget::HeadToWorktree).map_err(|error| error.to_string())));
    }

    fn take_result(&mut self, files: Result<Vec<FileDiff>, String>) {
        let at = self.rows.get(self.cursor).and_then(DiffRow::file).and_then(|file| self.files.get(file)).map(|f| f.path.clone());
        match files {
            Ok(files) => {
                self.files = files;
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
        self.rows = build_rows(&self.files);
        // Stay on the same file when the diff is read again under the cursor.
        let wanted = self.wanted.take().or(at);
        match wanted.and_then(|path| self.file_row(&path)) {
            Some(row) => self.cursor = row,
            None => self.cursor = self.cursor.min(self.rows.len().saturating_sub(1)),
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

    fn file_under_cursor(&self) -> Option<&FileDiff> {
        self.rows.get(self.cursor).and_then(DiffRow::file).and_then(|file| self.files.get(file))
    }

    /// Where `project_diff::OpenFile` goes: the file and the line of the cursor row on the
    /// new side (a removed line opens where it was).
    fn open_target(&self, cx: &Cx<'_>) -> Option<Effect> {
        let file = self.file_under_cursor()?;
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

    fn stage_file(&self, stage: bool, cx: &mut Cx<'_>) -> bool {
        let Some(file) = self.file_under_cursor() else { return false };
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

    fn draw_row(&self, canvas: &mut Canvas<'_>, area: Rect, y: u16, index: usize, cx: &RenderCx<'_>) {
        let theme = cx.theme;
        let selected = index == self.cursor && cx.focused;
        let gutter = NUMBER_WIDTH * 2 + 1;
        match &self.rows[index] {
            DiffRow::File(file) => {
                let file = &self.files[*file];
                let bg = if selected { theme.element_selected } else { theme.surface };
                canvas.fill(Rect::new(area.x, y, area.width, 1), bg);
                let name = file.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let (glyph, color) = file_icon(cx.settings, &name, false, false);
                let mut x = area.x + 1;
                x += canvas.text(x, y, &glyph, Style::colors(color, bg), 2) + 1;
                // The counts go at the right edge, the path gets what is left.
                let counts = format!("+{} −{}", file.additions(), file.deletions());
                let counts_width = crate::ui::width(&counts) as u16;
                let letter = status_letter(file.status);
                x += canvas.text(x, y, letter, Style::colors(status_color(file.status, theme), bg).bold(), 1) + 1;
                let path = match &file.old_path {
                    Some(old) => format!("{} → {}", old.display(), file.path.display()),
                    None => file.path.display().to_string(),
                };
                let room = area.right().saturating_sub(x + counts_width + 3);
                canvas.text(x, y, &crate::ui::truncate_start(&path, room as usize), Style::colors(theme.text, bg).bold(), room);
                if area.width > counts_width + 2 {
                    let right = area.right() - 1;
                    let added = format!("+{}", file.additions());
                    let removed = format!("−{}", file.deletions());
                    canvas.text_right(right, y, &removed, Style::colors(theme.deleted, bg));
                    canvas.text_right(right - crate::ui::width(&removed) as u16 - 1, y, &added, Style::colors(theme.created, bg));
                }
            }
            DiffRow::Hunk { header, .. } => {
                let bg = if selected { theme.element_selected } else { theme.editor_bg };
                canvas.fill(Rect::new(area.x, y, area.width, 1), bg);
                canvas.text_truncated(area.x + gutter + 1, y, header, Style::colors(theme.text_muted, bg), area.width.saturating_sub(gutter + 2));
            }
            DiffRow::Line { kind, old, new, text, .. } => {
                let tint = match kind {
                    DiffLineKind::Added => Some(theme.created),
                    DiffLineKind::Removed => Some(theme.deleted),
                    DiffLineKind::Context => None,
                };
                let mut bg = tint.map(|t| t.lerp(theme.editor_bg, 0.82)).unwrap_or(theme.editor_bg);
                if selected {
                    bg = theme.element_selected;
                }
                canvas.fill(Rect::new(area.x, y, area.width, 1), bg);
                let numbers = Style::colors(theme.line_number, bg);
                if let Some(old) = old {
                    canvas.text_right(area.x + NUMBER_WIDTH, y, &old.to_string(), numbers);
                }
                if let Some(new) = new {
                    canvas.text_right(area.x + NUMBER_WIDTH * 2, y, &new.to_string(), numbers);
                }
                let marker = match kind {
                    DiffLineKind::Added => "+",
                    DiffLineKind::Removed => "-",
                    DiffLineKind::Context => " ",
                };
                let marker_color = tint.unwrap_or(theme.text_muted);
                canvas.text(area.x + gutter, y, marker, Style::colors(marker_color, bg), 1);
                let x = area.x + gutter + 2;
                canvas.text(x, y, text, Style::colors(theme.editor_fg, bg), area.right().saturating_sub(x));
            }
            DiffRow::Note { text, .. } => {
                let bg = if selected { theme.element_selected } else { theme.editor_bg };
                canvas.fill(Rect::new(area.x, y, area.width, 1), bg);
                canvas.text_truncated(area.x + gutter + 2, y, text, Style::colors(theme.text_muted, bg).italic(), area.width.saturating_sub(gutter + 3));
            }
            DiffRow::Gap => {}
        }
    }
}

impl View for ProjectDiff {
    fn render(&mut self, canvas: &mut Canvas<'_>, area: Rect, cx: &RenderCx<'_>) -> Option<Cursor> {
        let theme = cx.theme;
        canvas.fill(area, theme.editor_bg);
        self.area = area;
        self.height = area.height as usize;
        if area.width < 4 || area.height == 0 {
            return None;
        }
        let muted = Style::colors(theme.text_muted, theme.editor_bg);
        let message = if cx.project.repo().is_none() {
            Some("Not a git repository: there is nothing to compare with".to_string())
        } else if let Some(error) = &self.error {
            Some(format!("Could not read the changes: {error}"))
        } else if self.rows.is_empty() && self.task.is_some() {
            Some("Reading the changes…".to_string())
        } else if self.rows.is_empty() {
            Some("No changes: the working tree matches the last commit".to_string())
        } else {
            None
        };
        if let Some(message) = message {
            canvas.text_truncated(area.x + 2, area.y + 1.min(area.height - 1), &message, muted, area.width.saturating_sub(4));
            return None;
        }
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + self.height {
            self.scroll = self.cursor + 1 - self.height;
        }
        for (offset, index) in (self.scroll..self.rows.len()).take(self.height).enumerate() {
            self.draw_row(canvas, area, area.y + offset as u16, index, cx);
        }
        None
    }

    fn contexts(&self, out: &mut Vec<Context>) {
        out.push(Context::new("ProjectDiff"));
    }

    fn handle_action(&mut self, action: &Action, cx: &mut Cx<'_>) -> bool {
        let page = self.height.max(1) as isize;
        match action.name.as_str() {
            "project_diff::LineDown" => self.move_cursor(1),
            "project_diff::LineUp" => self.move_cursor(-1),
            "project_diff::PageDown" => self.move_cursor(page),
            "project_diff::PageUp" => self.move_cursor(-page),
            "project_diff::HalfPageDown" => self.move_cursor(page / 2),
            "project_diff::HalfPageUp" => self.move_cursor(-page / 2),
            "project_diff::Top" => self.cursor = 0,
            "project_diff::Bottom" => self.cursor = self.rows.len().saturating_sub(1),
            "project_diff::NextHunk" => self.jump(true, |row| matches!(row, DiffRow::Hunk { .. })),
            "project_diff::PreviousHunk" => self.jump(false, |row| matches!(row, DiffRow::Hunk { .. })),
            "project_diff::NextFile" => self.jump(true, |row| matches!(row, DiffRow::File(_))),
            "project_diff::PreviousFile" => self.jump(false, |row| matches!(row, DiffRow::File(_))),
            "project_diff::ShowFile" => {
                let Some(path) = action.arg_str("path").map(PathBuf::from) else { return false };
                let Some(relative) = cx.project.git_path(&path) else { return false };
                match self.file_row(&relative) {
                    Some(row) => {
                        self.cursor = row;
                        self.scroll = row;
                    }
                    None => self.wanted = Some(relative),
                }
            }
            "project_diff::OpenFile" => match self.open_target(cx) {
                Some(effect) => cx.push(effect),
                None => return false,
            },
            "git::ToggleStaged" => {
                let Some(file) = self.file_under_cursor() else { return false };
                let staged = cx.project.status_of(&cx.project.abs_path(&file.path).unwrap_or_default()).is_some_and(|e| e.is_fully_staged());
                return self.stage_file(!staged, cx);
            }
            "git::StageFile" => return self.stage_file(true, cx),
            "git::UnstageFile" => return self.stage_file(false, cx),
            "git::StageAll" => {
                let _ = cx.project.git_job("Stage all", |repo| repo.stage_all().map(|()| String::new()));
            }
            "git::UnstageAll" => {
                let _ = cx.project.git_job("Unstage all", |repo| repo.unstage_all().map(|()| String::new()));
            }
            _ => return false,
        }
        true
    }

    fn handle_key(&mut self, _key: &KeyEvent, _cx: &mut Cx<'_>) -> bool {
        false
    }

    fn handle_mouse(&mut self, event: &MouseEvent, _area: Rect, _cx: &mut Cx<'_>) -> bool {
        match event.kind {
            MouseKind::ScrollDown => {
                let last = self.rows.len().saturating_sub(self.height.max(1));
                self.scroll = (self.scroll + WHEEL_ROWS).min(last);
                self.cursor = self.cursor.clamp(self.scroll, (self.scroll + self.height).saturating_sub(1).max(self.scroll));
            }
            MouseKind::ScrollUp => {
                self.scroll = self.scroll.saturating_sub(WHEEL_ROWS);
                self.cursor = self.cursor.clamp(self.scroll, (self.scroll + self.height).saturating_sub(1).max(self.scroll));
            }
            MouseKind::Press(MouseButton::Left) => {
                if let Some(row) = event.row.checked_sub(self.area.y) {
                    let index = self.scroll + row as usize;
                    if index < self.rows.len() {
                        self.cursor = index;
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
        }
        if finished {
            self.task = None;
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

    fn tab(&self, cx: &RenderCx<'_>) -> Tab {
        let icon = match cx.settings.icons {
            Icons::Nerd => Some(("\u{f1d3}".to_string(), cx.theme.modified)),
            Icons::Ascii => None,
        };
        Tab { title: "Project Diff".to_string(), icon, ..Default::default() }
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
        assert!(rows[2].trim_start().starts_with("1    1   one"), "{:?}", rows[2]);
        assert!(rows[3].contains("- two"));
        assert!(rows[4].contains("+ TWO"));
        assert!(rows.iter().any(|r| r.contains("U fresh.md")), "untracked files are listed: {text}");
    }

    #[test]
    fn moves_by_file_shows_a_file_and_opens_at_the_line() {
        let dir = repo();
        std::fs::write(dir.path().join("a.txt"), "one\nTWO\n").unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn answer() -> u32 {\n    43\n}\n").unwrap();
        let (mut harness, mut diff) = loaded(dir.path());
        assert!(diff.handle_action(&Action::new("project_diff::NextFile"), &mut harness.cx()));
        assert!(matches!(diff.rows[diff.cursor], DiffRow::File(1)));
        let show = Action::with_args("project_diff::ShowFile", json!({ "path": dir.path().join("a.txt").to_string_lossy() }));
        assert!(diff.handle_action(&show, &mut harness.cx()));
        assert_eq!(diff.cursor, 0);
        diff.handle_action(&Action::new("project_diff::NextHunk"), &mut harness.cx());
        diff.handle_action(&Action::new("project_diff::LineDown"), &mut harness.cx());
        assert!(diff.handle_action(&Action::new("project_diff::OpenFile"), &mut harness.cx()));
        assert_eq!(
            harness.take_effects(),
            vec![Effect::OpenFile { path: dir.path().join("a.txt"), line: Some(1), column: None, preview: false, focus: true }]
        );
    }

    #[test]
    fn a_clean_tree_and_no_repository_say_so() {
        let dir = repo();
        let (harness, mut diff) = loaded(dir.path());
        assert!(harness.draw(&mut diff, 70, 4).0.contains("No changes: the working tree matches the last commit"));
        let plain = tempfile::tempdir().unwrap();
        let (harness, mut diff) = loaded(plain.path());
        assert!(harness.draw(&mut diff, 70, 4).0.contains("Not a git repository"));
        assert_eq!(diff.kind(), "project_diff");
        assert_eq!(diff.tab(&harness.render_cx(false)).title, "Project Diff");
    }

    #[test]
    fn edits_on_disk_are_read_again() {
        let dir = repo();
        std::fs::write(dir.path().join("a.txt"), "one\nTWO\n").unwrap();
        let (mut harness, mut diff) = loaded(dir.path());
        std::fs::write(dir.path().join("a.txt"), "one\nTWO\nmore\n").unwrap();
        diff.paths_changed(&FsChange::of(vec![dir.path().join("a.txt")]), &mut harness.cx());
        assert!(diff.task.is_some(), "a fresh read started");
        let started = Instant::now();
        while !harness.draw(&mut diff, 70, 10).0.contains("+ more") {
            diff.tick(&mut harness.cx());
            assert!(started.elapsed() < Duration::from_secs(20));
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
