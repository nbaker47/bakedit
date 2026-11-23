//! The git panel in the left dock: what changed, and the commands that act on it.
//!
//! # What the app calls (this signature stays)
//!
//! * `GitPanel::new(cx: &mut Cx<'_>) -> GitPanel`: the changes of the project's
//!   repository, kept up to date from `cx.project.status()`.
//!
//! Everything else goes through `View` and the keymap's `GitPanel` sections: moving through
//! the list (`menu::*`, `git_panel::*Entry`), `menu::Confirm` to show the file's diff
//! without leaving the list, staging (`git::ToggleStaged`, `git::StageFile`,
//! `git::UnstageFile`, `git::StageAll`, `git::UnstageAll`) and `git::Fetch`, `git::Pull`,
//! `git::PullRebase`, `git::Push`, `git::ForcePush`. Every git command runs through
//! `Project::git_job`, off the UI thread; failures reach the status bar from there.
//!
//! The commit message editor (`GitPanel{CommitEditor}`) and discarding changes are for the
//! panel's next owner.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bakedit_core::{Action, KeyEvent, MouseButton, MouseEvent, MouseKind, Rgb, Theme};
use bakedit_git::{DisplayStatus, StageState, Status, StatusEntry};
use ratatui::layout::Rect;

use crate::ui::{self, Canvas, Style};
use crate::view::{Context, Cursor, Cx, Effect, RenderCx, View};

/// One changed file as the list shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Change {
    /// Relative to the top of the work tree, as git names it.
    path: PathBuf,
    status: DisplayStatus,
    stage: StageState,
}

/// The git panel.
pub struct GitPanel {
    changes: Vec<Change>,
    /// The status the list was built from.
    status: Option<Arc<Status>>,
    generation: Option<u64>,
    selected: usize,
    scroll: usize,
    area: Rect,
}

fn status_letter(status: DisplayStatus) -> &'static str {
    match status {
        DisplayStatus::Added => "A",
        DisplayStatus::Modified => "M",
        DisplayStatus::Deleted => "D",
        DisplayStatus::Renamed => "R",
        DisplayStatus::Untracked => "U",
        DisplayStatus::Conflict => "!",
    }
}

fn status_color(status: DisplayStatus, theme: &Theme) -> Rgb {
    match status {
        DisplayStatus::Added | DisplayStatus::Untracked => theme.created,
        DisplayStatus::Modified | DisplayStatus::Renamed => theme.modified,
        DisplayStatus::Deleted => theme.deleted,
        DisplayStatus::Conflict => theme.conflict,
    }
}

impl GitPanel {
    /// The changes of the project's repository. Empty until the first status arrives.
    pub fn new(cx: &mut Cx<'_>) -> GitPanel {
        let mut panel = GitPanel { changes: Vec::new(), status: None, generation: None, selected: 0, scroll: 0, area: Rect::default() };
        panel.refresh(cx.project);
        panel
    }

    /// Rebuild the list when the project has a newer status. Returns true if it changed.
    fn refresh(&mut self, project: &crate::project::Project) -> bool {
        if self.generation == Some(project.git_generation()) {
            return false;
        }
        self.generation = Some(project.git_generation());
        let status = project.status().cloned();
        if status.as_ref().map(Arc::as_ptr) == self.status.as_ref().map(Arc::as_ptr) {
            return false;
        }
        let selected_path = self.changes.get(self.selected).map(|change| change.path.clone());
        self.changes = status
            .as_deref()
            .map(|status| {
                let entry = |e: &StatusEntry| Change { path: e.path.clone(), status: e.display_status(), stage: e.stage_state() };
                status.entries.iter().map(entry).collect()
            })
            .unwrap_or_default();
        self.status = status;
        // The selection stays on its file when the list is rebuilt under it.
        if let Some(path) = selected_path {
            if let Some(index) = self.changes.iter().position(|change| change.path == path) {
                self.selected = index;
            }
        }
        self.selected = self.selected.min(self.changes.len().saturating_sub(1));
        true
    }

    fn selected_change(&self) -> Option<&Change> {
        self.changes.get(self.selected)
    }

    fn selected_abs_path(&self, cx: &Cx<'_>) -> Option<PathBuf> {
        cx.project.abs_path(&self.selected_change()?.path)
    }

    fn stage(&self, paths: Vec<PathBuf>, stage: bool, cx: &mut Cx<'_>) -> bool {
        if paths.is_empty() {
            return false;
        }
        let label = match (&paths[..], stage) {
            ([one], true) => format!("Stage {}", one.display()),
            ([one], false) => format!("Unstage {}", one.display()),
            (many, true) => format!("Stage {} files", many.len()),
            (many, false) => format!("Unstage {} files", many.len()),
        };
        let _ = cx.project.git_job(label, move |repo| {
            if stage {
                repo.stage(&paths).map(|()| String::new())
            } else {
                repo.unstage(&paths).map(|()| String::new())
            }
        });
        true
    }

    fn header(&self, cx: &RenderCx<'_>) -> String {
        let summary = cx.project.git_summary();
        let branch = summary.branch.unwrap_or_else(|| "no branch".to_string());
        match self.changes.len() {
            0 => branch,
            1 => format!("{branch}  ·  1 change"),
            n => format!("{branch}  ·  {n} changes"),
        }
    }
}

impl View for GitPanel {
    fn render(&mut self, canvas: &mut Canvas<'_>, area: Rect, cx: &RenderCx<'_>) -> Option<Cursor> {
        let theme = cx.theme;
        let bg = theme.panel_bg;
        canvas.fill(area, bg);
        self.area = area;
        if area.width < 4 || area.height == 0 {
            return None;
        }
        self.refresh(cx.project);
        let muted = Style::colors(theme.text_muted, bg);
        let inner = area.width.saturating_sub(2);
        if cx.project.repo().is_none() {
            canvas.text_truncated(area.x + 1, area.y, "Not a git repository", Style::colors(theme.text, bg), inner);
            if area.height > 1 {
                canvas.text_truncated(area.x + 1, area.y + 1, "Run git init in the terminal to track changes", muted, inner);
            }
            return None;
        }
        canvas.text_truncated(area.x + 1, area.y, &self.header(cx), Style::colors(theme.text, bg).bold(), inner);
        if self.status.is_none() {
            if area.height > 1 {
                canvas.text_truncated(area.x + 1, area.y + 1, "Reading the status…", muted, inner);
            }
            return None;
        }
        if self.changes.is_empty() {
            if area.height > 1 {
                canvas.text_truncated(area.x + 1, area.y + 1, "No changes: everything is committed", muted, inner);
            }
            return None;
        }
        let list = Rect::new(area.x, area.y + 1, area.width, area.height - 1);
        let rows = list.height as usize;
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if rows > 0 && self.selected >= self.scroll + rows {
            self.scroll = self.selected + 1 - rows;
        }
        for (index, change) in self.changes.iter().enumerate().skip(self.scroll).take(rows) {
            let y = list.y + (index - self.scroll) as u16;
            let selected = index == self.selected;
            let row_bg = if selected { theme.element_selected } else { bg };
            if selected {
                canvas.fill(Rect::new(area.x, y, area.width, 1), row_bg);
            }
            // [staged mark] [status letter] name  folder
            let mark = match change.stage {
                StageState::Staged => "✓",
                StageState::PartiallyStaged => "~",
                StageState::Unstaged => " ",
            };
            let mut x = area.x + 1;
            x += canvas.text(x, y, mark, Style::colors(theme.success, row_bg), 1);
            x += 1;
            x += canvas.text(x, y, status_letter(change.status), Style::colors(status_color(change.status, theme), row_bg).bold(), 1);
            x += 1;
            let name = change.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let room = area.right().saturating_sub(x + 1);
            let used = canvas.text_truncated(x, y, &name, Style::colors(theme.text, row_bg), room);
            let folder = change.path.parent().map(Path::to_string_lossy).unwrap_or_default().into_owned();
            let left = room.saturating_sub(used + 2);
            if !folder.is_empty() && left >= 4 {
                canvas.text(x + used + 2, y, &ui::truncate_start(&folder, left as usize), Style::colors(theme.text_muted, row_bg), left);
            }
        }
        None
    }

    fn contexts(&self, out: &mut Vec<Context>) {
        out.push(Context::new("GitPanel").flag("ChangesList"));
    }

    fn handle_action(&mut self, action: &Action, cx: &mut Cx<'_>) -> bool {
        let last = self.changes.len().saturating_sub(1);
        match action.name.as_str() {
            "menu::SelectNext" | "git_panel::NextEntry" => self.selected = (self.selected + 1).min(last),
            "menu::SelectPrevious" | "git_panel::PreviousEntry" => self.selected = self.selected.saturating_sub(1),
            "menu::SelectFirst" | "git_panel::FirstEntry" => self.selected = 0,
            "menu::SelectLast" | "git_panel::LastEntry" => self.selected = last,
            "menu::Confirm" => {
                let Some(path) = self.selected_abs_path(cx) else { return false };
                cx.push(Effect::ShowProjectDiff { path: Some(path), focus: false });
            }
            // The arrow keys' sequence ends by putting the focus back on the list.
            "git_panel::FocusChanges" => cx.push(Effect::Focus(crate::workspace::Focus::Panel(crate::workspace::PanelId::Git))),
            "git::ToggleStaged" => {
                let Some(change) = self.selected_change() else { return false };
                let stage = change.stage != StageState::Staged;
                return self.stage(vec![change.path.clone()], stage, cx);
            }
            "git::StageFile" | "git::UnstageFile" => {
                let Some(change) = self.selected_change() else { return false };
                return self.stage(vec![change.path.clone()], action.name == "git::StageFile", cx);
            }
            "git::StageAll" => {
                let _ = cx.project.git_job("Stage all", |repo| repo.stage_all().map(|()| String::new()));
            }
            "git::UnstageAll" => {
                let _ = cx.project.git_job("Unstage all", |repo| repo.unstage_all().map(|()| String::new()));
            }
            "git::Fetch" => {
                let _ = cx.project.git_job("Fetch", |repo| repo.fetch().map(|_| "Fetched".to_string()));
            }
            "git::Pull" | "git::PullRebase" => {
                let rebase = action.name == "git::PullRebase";
                let _ = cx.project.git_job("Pull", move |repo| repo.pull(rebase).map(|_| "Pulled".to_string()));
            }
            "git::Push" | "git::ForcePush" => {
                let force = action.name == "git::ForcePush";
                let _ = cx.project.git_job("Push", move |repo| repo.push(force).map(|_| "Pushed".to_string()));
            }
            _ => return false,
        }
        true
    }

    fn handle_key(&mut self, _key: &KeyEvent, _cx: &mut Cx<'_>) -> bool {
        false
    }

    fn handle_mouse(&mut self, event: &MouseEvent, _area: Rect, cx: &mut Cx<'_>) -> bool {
        match event.kind {
            MouseKind::ScrollDown => self.selected = (self.selected + 1).min(self.changes.len().saturating_sub(1)),
            MouseKind::ScrollUp => self.selected = self.selected.saturating_sub(1),
            MouseKind::Press(MouseButton::Left) => {
                let Some(row) = event.row.checked_sub(self.area.y + 1) else { return true };
                let index = self.scroll + row as usize;
                if index < self.changes.len() {
                    self.selected = index;
                    if let Some(path) = self.selected_abs_path(cx) {
                        cx.push(Effect::ShowProjectDiff { path: Some(path), focus: false });
                    }
                }
            }
            _ => return false,
        }
        true
    }

    fn tick(&mut self, cx: &mut Cx<'_>) -> bool {
        self.refresh(cx.project)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::project::test_support::{git, repo};
    use crate::project::Project;
    use crate::view::test_support::Harness;

    fn settled(dir: &Path) -> Harness {
        let mut project = Project::open(dir, crate::background::Waker::detached());
        project.settle(Duration::from_secs(20));
        Harness::with_project(project)
    }

    #[test]
    fn lists_the_changes_with_their_state() {
        let dir = repo();
        std::fs::write(dir.path().join("a.txt"), "one\nchanged\n").unwrap();
        std::fs::write(dir.path().join("src/new.rs"), "").unwrap();
        git(dir.path(), &["add", "src/new.rs"]);
        std::fs::write(dir.path().join("notes.md"), "").unwrap();
        let mut harness = settled(dir.path());
        let mut panel = GitPanel::new(&mut harness.cx());
        let (text, _, _) = harness.draw(&mut panel, 40, 6);
        let rows: Vec<&str> = text.lines().collect();
        assert_eq!(rows[0].trim(), "main  ·  3 changes");
        assert_eq!(rows[1], "   M a.txt");
        assert_eq!(rows[2], " ✓ A new.rs  src", "staged, added, its folder after the name");
        assert_eq!(rows[3], "   U notes.md");
    }

    #[test]
    fn enter_shows_the_diff_and_space_stages() {
        let dir = repo();
        std::fs::write(dir.path().join("a.txt"), "one\nchanged\n").unwrap();
        let mut harness = settled(dir.path());
        let mut panel = GitPanel::new(&mut harness.cx());
        assert!(panel.handle_action(&Action::new("menu::Confirm"), &mut harness.cx()));
        assert_eq!(harness.take_effects(), vec![Effect::ShowProjectDiff { path: Some(dir.path().join("a.txt")), focus: false }]);
        assert!(panel.handle_action(&Action::new("git::ToggleStaged"), &mut harness.cx()));
        let started = Instant::now();
        loop {
            harness.project.poll();
            if panel.tick(&mut harness.cx()) && panel.changes.first().is_some_and(|c| c.stage == StageState::Staged) {
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(20), "the file was never staged");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(harness.draw(&mut panel, 40, 4).0.contains(" ✓ M a.txt"));
    }

    #[test]
    fn empty_states_say_what_is_going_on() {
        let dir = repo();
        let mut harness = settled(dir.path());
        let mut panel = GitPanel::new(&mut harness.cx());
        let text = harness.draw(&mut panel, 44, 4).0;
        assert!(text.contains("No changes: everything is committed"), "{text}");
        let plain = tempfile::tempdir().unwrap();
        let mut harness = settled(plain.path());
        let mut panel = GitPanel::new(&mut harness.cx());
        let text = harness.draw(&mut panel, 50, 4).0;
        assert!(text.contains("Not a git repository") && text.contains("git init"), "{text}");
        assert!(!panel.handle_action(&Action::new("git::ToggleStaged"), &mut harness.cx()));
    }
}
