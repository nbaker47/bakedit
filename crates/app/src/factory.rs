//! The real makers of what the shell cannot make itself: the items of the centre and the
//! rows of the pickers that come from the project.
//!
//! * [`AppFactory`] makes editors, pages, the project diff and project search.
//! * [`AppSource`] fills the file finder (from the file index, `path:line:column` queries
//!   included, recent files first), the branch list (from the project) and the recent
//!   projects, and acts on the choice.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::editor::EditorItem;
use crate::fuzzy;
use crate::icons::file_icon;
use crate::items::diff::ProjectDiff;
use crate::items::search::ProjectSearch;
use crate::picker::PickerItem;
use crate::view::{Cx, Effect, Item, ItemFactory, PickerKind, PickerSource};

/// How many files the finder lists at once.
const FINDER_LIMIT: usize = 200;

/// Makes the app's items.
pub struct AppFactory;

impl ItemFactory for AppFactory {
    fn open_path(&mut self, path: &Path, cx: &mut Cx<'_>) -> Result<Box<dyn Item>, String> {
        EditorItem::open(path, cx).map(|item| Box::new(item) as Box<dyn Item>)
    }

    fn untitled(&mut self, cx: &mut Cx<'_>) -> Box<dyn Item> {
        Box::new(EditorItem::untitled(cx))
    }

    fn project_diff(&mut self, cx: &mut Cx<'_>) -> Box<dyn Item> {
        Box::new(ProjectDiff::new(cx))
    }

    fn project_search(&mut self, replace: bool, cx: &mut Cx<'_>) -> Box<dyn Item> {
        Box::new(ProjectSearch::new(replace, cx))
    }

    fn read_only(&mut self, title: &str, text: &str, language: Option<&str>, cx: &mut Cx<'_>) -> Box<dyn Item> {
        Box::new(EditorItem::page(title, text, language, cx))
    }
}

/// A file the finder listed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileRow {
    path: PathBuf,
    line: Option<u32>,
    column: Option<u32>,
}

/// Fills the pickers whose rows come from the project.
pub struct AppSource {
    /// Where `recent-projects.json` lives.
    state_dir: PathBuf,
    /// What the last `rows` listed, for `confirm`.
    files: Vec<FileRow>,
    branches: Vec<String>,
    repos: Vec<PathBuf>,
    diff_bases: Vec<Option<String>>,
    projects: Vec<PathBuf>,
}

impl AppSource {
    /// A source that keeps the recent projects in `state_dir`.
    pub fn new(state_dir: PathBuf) -> AppSource {
        AppSource { state_dir, files: Vec::new(), branches: Vec::new(), repos: Vec::new(), diff_bases: Vec::new(), projects: Vec::new() }
    }

    fn file_rows(&mut self, query: &str, cx: &mut Cx<'_>) -> Vec<PickerItem> {
        let parsed = bakedit_project::parse_query(query);
        let mut found = cx.project.find_files(&parsed.path_query, FINDER_LIMIT);
        // Nothing typed and nothing opened yet: the project's files, so there is something
        // to pick from on the first cmd-p.
        if parsed.path_query.is_empty() && found.is_empty() {
            if let Some(index) = cx.project.index() {
                found = index
                    .snapshot()
                    .iter()
                    .take(FINDER_LIMIT)
                    .map(|display| bakedit_project::FileMatch { path: PathBuf::from(display), score: 0, indices: Vec::new() })
                    .collect();
            }
        }
        self.files.clear();
        found
            .into_iter()
            .map(|found| {
                let display = found.path.to_string_lossy().into_owned();
                let name_start = display.rfind('/').map(|slash| display[..slash + 1].chars().count()).unwrap_or(0);
                let name = display.chars().skip(name_start).collect::<String>();
                let matched: Vec<usize> = found.indices.iter().filter(|&&i| i >= name_start).map(|i| i - name_start).collect();
                let mut item = PickerItem::new(name.clone()).matched(matched);
                if name_start > 0 {
                    item.detail = Some(display.chars().take(name_start - 1).collect());
                }
                item.icon = Some(file_icon(cx.settings, &name, false, false));
                if let Some(line) = parsed.line {
                    item.hint = Some(match parsed.col {
                        Some(col) => format!("line {line}, column {col}"),
                        None => format!("line {line}"),
                    });
                }
                // Tab puts the whole path in the query, keeping the line typed after it.
                let position = match (parsed.line, parsed.col) {
                    (Some(line), Some(col)) => format!(":{line}:{col}"),
                    (Some(line), None) => format!(":{line}"),
                    _ => String::new(),
                };
                item.completion = Some(format!("{display}{position}"));
                self.files.push(FileRow { path: cx.root.join(&found.path), line: parsed.line, column: parsed.col });
                item
            })
            .collect()
    }

    fn branch_rows(&mut self, query: &str, cx: &mut Cx<'_>) -> Vec<PickerItem> {
        self.branches.clear();
        let Some(branches) = cx.project.branches() else { return Vec::new() };
        let names: Vec<&str> = branches.iter().map(|branch| branch.name.as_str()).collect();
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        fuzzy::filter(names, query)
            .into_iter()
            .map(|found| {
                let branch = &branches[found.index];
                self.branches.push(branch.name.clone());
                let mut item = PickerItem::new(branch.name.clone()).matched(found.positions);
                item.detail = Some(branch.subject.clone()).filter(|s| !s.is_empty());
                item.hint = Some(if branch.current { "current".to_string() } else { age(now - branch.last_commit_time) });
                item
            })
            .collect()
    }

    fn repo_rows(&mut self, query: &str, cx: &mut Cx<'_>) -> Vec<PickerItem> {
        self.repos.clear();
        let repos = cx.project.repos();
        if repos.is_empty() {
            return Vec::new();
        }
        let active = cx.project.repo().map(|r| r.root().to_path_buf());
        let labels: Vec<&str> = repos.iter().map(|r| r.label.as_str()).collect();
        fuzzy::filter(labels, query)
            .into_iter()
            .map(|found| {
                let entry = &repos[found.index];
                self.repos.push(entry.repo.root().to_path_buf());
                let name = entry.top.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| entry.label.clone());
                let mut item = PickerItem::new(entry.label.clone()).matched(found.positions);
                item.icon = Some(file_icon(cx.settings, &name, true, false));
                if active.as_deref() == Some(entry.repo.root()) {
                    item.hint = Some("active".to_string());
                }
                item
            })
            .collect()
    }

    fn diff_base_rows(&mut self, query: &str, cx: &mut Cx<'_>) -> Vec<PickerItem> {
        self.diff_bases.clear();
        let active = cx.project.diff_base();
        let mut entries: Vec<(Option<String>, String, Option<String>, bool)> = vec![(
            None,
            "Uncommitted changes".to_string(),
            Some("staged & unstaged vs HEAD".to_string()),
            *active == crate::project::DiffBase::Uncommitted,
        )];
        if let Some(branches) = cx.project.branches() {
            for branch in branches.iter() {
                let is_active = matches!(active, crate::project::DiffBase::Branch(b) if b == &branch.name);
                let detail = if branch.subject.is_empty() { None } else { Some(branch.subject.clone()) };
                entries.push((Some(branch.name.clone()), branch.name.clone(), detail, is_active));
            }
        }
        let labels: Vec<&str> = entries.iter().map(|(_, label, _, _)| label.as_str()).collect();
        fuzzy::filter(labels, query)
            .into_iter()
            .map(|found| {
                let (value, label, detail, is_active) = &entries[found.index];
                self.diff_bases.push(value.clone());
                let mut item = PickerItem::new(label.clone()).matched(found.positions);
                item.detail = detail.clone();
                if *is_active {
                    item.hint = Some("active".to_string());
                }
                item
            })
            .collect()
    }

    fn project_rows(&mut self, query: &str, cx: &mut Cx<'_>) -> Vec<PickerItem> {
        let home = dirs::home_dir();
        let shown = |path: &Path| match home.as_deref().and_then(|home| path.strip_prefix(home).ok()) {
            Some(rest) => format!("~/{}", rest.display()),
            None => path.display().to_string(),
        };
        let candidates: Vec<PathBuf> =
            crate::session::recent_projects(&self.state_dir).into_iter().filter(|path| path != cx.root).collect();
        let labels: Vec<String> = candidates.iter().map(|path| shown(path)).collect();
        self.projects.clear();
        fuzzy::filter(labels.iter().map(String::as_str), query)
            .into_iter()
            .map(|found| {
                let path = &candidates[found.index];
                self.projects.push(path.clone());
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| labels[found.index].clone());
                let mut item = PickerItem::new(name.clone()).detail(labels[found.index].clone());
                item.icon = Some(file_icon(cx.settings, &name, true, false));
                item
            })
            .collect()
    }
}

/// `3 minutes ago`, `2 days ago`: how long ago a commit was made.
fn age(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let (amount, unit) = match seconds {
        0..=59 => return "just now".to_string(),
        60..=3_599 => (seconds / 60, "minute"),
        3_600..=86_399 => (seconds / 3_600, "hour"),
        86_400..=2_591_999 => (seconds / 86_400, "day"),
        2_592_000..=31_535_999 => (seconds / 2_592_000, "month"),
        _ => (seconds / 31_536_000, "year"),
    };
    format!("{amount} {unit}{} ago", if amount == 1 { "" } else { "s" })
}

impl PickerSource for AppSource {
    fn rows(&mut self, kind: PickerKind, query: &str, cx: &mut Cx<'_>) -> Vec<PickerItem> {
        match kind {
            PickerKind::FileFinder => self.file_rows(query, cx),
            PickerKind::Branches => self.branch_rows(query, cx),
            PickerKind::Repositories => self.repo_rows(query, cx),
            PickerKind::DiffBase => self.diff_base_rows(query, cx),
            PickerKind::RecentProjects => self.project_rows(query, cx),
            // The shell and the views fill the others.
            PickerKind::CommandPalette | PickerKind::TabSwitcher | PickerKind::Outline | PickerKind::Links => Vec::new(),
        }
    }

    fn confirm(&mut self, kind: PickerKind, row: Option<usize>, query: &str, _secondary: bool, cx: &mut Cx<'_>) {
        match (kind, row) {
            (PickerKind::FileFinder, Some(row)) => {
                if let Some(file) = self.files.get(row).cloned() {
                    cx.push(Effect::OpenFile { path: file.path, line: file.line, column: file.column, preview: false, focus: true });
                }
            }
            // Nothing in the index matched, or the query itself was confirmed (alt-enter): a
            // path typed out in full still opens, inside the project or not (`~/notes.md:3`,
            // `/etc/hosts`).
            (PickerKind::FileFinder, None) => {
                let query = query.trim();
                if query.is_empty() {
                    return;
                }
                let spec = crate::cli::parse_path_spec(query, cx.root);
                if spec.path.is_file() {
                    cx.push(Effect::OpenFile { path: spec.path, line: spec.line, column: spec.column, preview: false, focus: true });
                } else {
                    cx.error(format!("No file at {}", spec.path.strip_prefix(cx.root).unwrap_or(&spec.path).display()));
                }
            }
            (PickerKind::Branches, Some(row)) => {
                let Some(name) = self.branches.get(row).cloned() else { return };
                let _ = cx.project.git_job(format!("Switch to {name}"), move |repo| repo.checkout(&name).map(|()| String::new()));
            }
            // A name that is not a branch yet: make it and switch to it.
            (PickerKind::Branches, None) => {
                let name = query.trim().to_string();
                if name.is_empty() {
                    return;
                }
                let label = format!("Create branch {name}");
                let _ = cx.project.git_job(label, move |repo| repo.create_branch(&name, true).map(|()| format!("Switched to a new branch {name}")));
            }
            (PickerKind::Repositories, Some(row)) => {
                if let Some(path) = self.repos.get(row).cloned() {
                    cx.push(Effect::SelectRepo(path));
                }
            }
            (PickerKind::DiffBase, Some(row)) => {
                if let Some(base) = self.diff_bases.get(row).cloned() {
                    cx.push(Effect::SetDiffBase(base));
                }
            }
            (PickerKind::DiffBase, None) => {
                let name = query.trim();
                if !name.is_empty() {
                    cx.push(Effect::SetDiffBase(Some(name.to_string())));
                }
            }
            (PickerKind::RecentProjects, Some(row)) => {
                if let Some(path) = self.projects.get(row).cloned() {
                    cx.push(Effect::OpenProject(path));
                }
            }
            // A folder typed out in full (alt-enter, or nothing recent matched it).
            (PickerKind::RecentProjects, None) => {
                let query = query.trim();
                if query.is_empty() {
                    return;
                }
                let path = crate::cli::parse_path_spec(query, cx.root).path;
                if path.is_dir() {
                    cx.push(Effect::OpenProject(path));
                } else {
                    cx.error(format!("No folder at {}", path.display()));
                }
            }
            _ => {}
        }
    }

    fn version(&mut self, kind: PickerKind, cx: &mut Cx<'_>) -> u64 {
        match kind {
            PickerKind::FileFinder => cx.project.index_generation(),
            PickerKind::Branches => cx.project.branches_version(),
            PickerKind::Repositories => cx.project.repos_version(),
            PickerKind::DiffBase => cx.project.branches_version().wrapping_add(cx.project.git_generation()),
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

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
    fn the_finder_lists_names_then_folders_and_opens_at_a_line() {
        let dir = repo();
        let mut harness = settled(dir.path());
        let mut source = AppSource::new(dir.path().join("state"));
        let rows = source.rows(PickerKind::FileFinder, "lib:2", &mut harness.cx());
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].label, "lib.rs");
        assert_eq!(rows[0].detail.as_deref(), Some("src"));
        assert_eq!(rows[0].matched, vec![0, 1, 2], "matches are drawn on the name");
        assert_eq!(rows[0].hint.as_deref(), Some("line 2"));
        assert!(rows[0].icon.is_some());
        source.confirm(PickerKind::FileFinder, Some(0), "lib:2", false, &mut harness.cx());
        assert_eq!(
            harness.take_effects(),
            vec![Effect::OpenFile { path: dir.path().join("src/lib.rs"), line: Some(2), column: None, preview: false, focus: true }]
        );
        assert!(source.version(PickerKind::FileFinder, &mut harness.cx()) > 0);
    }

    #[test]
    fn an_empty_query_lists_recent_files_or_else_the_project() {
        let dir = repo();
        let mut harness = settled(dir.path());
        let mut source = AppSource::new(dir.path().join("state"));
        let labels = |rows: Vec<PickerItem>| rows.into_iter().map(|row| row.label).collect::<Vec<_>>();
        let all = labels(source.rows(PickerKind::FileFinder, "", &mut harness.cx()));
        assert_eq!(all.len(), 2, "nothing opened yet: every file: {all:?}");
        harness.project.touch_recent(&dir.path().join("src/lib.rs"));
        assert_eq!(labels(source.rows(PickerKind::FileFinder, "", &mut harness.cx())), ["lib.rs"]);
    }

    #[test]
    fn a_full_path_outside_the_index_still_opens() {
        let dir = repo();
        let outside = tempfile::tempdir().unwrap();
        let file = outside.path().join("notes.md");
        std::fs::write(&file, "x\n").unwrap();
        let mut harness = settled(dir.path());
        let mut source = AppSource::new(dir.path().join("state"));
        let query = format!("{}:7", file.display());
        assert!(source.rows(PickerKind::FileFinder, &query, &mut harness.cx()).is_empty());
        source.confirm(PickerKind::FileFinder, None, &query, false, &mut harness.cx());
        assert_eq!(harness.take_effects(), vec![Effect::OpenFile { path: file, line: Some(7), column: None, preview: false, focus: true }]);
        source.confirm(PickerKind::FileFinder, None, "no/such/file", false, &mut harness.cx());
        assert_eq!(harness.take_effects(), vec![Effect::error("No file at no/such/file")], "says why nothing opened");
        source.confirm(PickerKind::FileFinder, None, "  ", false, &mut harness.cx());
        assert!(harness.take_effects().is_empty(), "nothing typed: nothing to say");
    }

    #[test]
    fn branches_come_from_the_project_and_switch() {
        let dir = repo();
        git(dir.path(), &["branch", "feature/login"]);
        let mut harness = settled(dir.path());
        let mut source = AppSource::new(dir.path().join("state"));
        // The first ask starts reading them; they arrive in the background.
        assert!(source.rows(PickerKind::Branches, "", &mut harness.cx()).is_empty());
        let before = source.version(PickerKind::Branches, &mut harness.cx());
        harness.project.settle(Duration::from_secs(20));
        assert!(source.version(PickerKind::Branches, &mut harness.cx()) > before);
        let rows = source.rows(PickerKind::Branches, "", &mut harness.cx());
        let main = rows.iter().find(|row| row.label == "main").expect("main is listed");
        assert_eq!(main.hint.as_deref(), Some("current"));
        assert_eq!(main.detail.as_deref(), Some("first"));
        let rows = source.rows(PickerKind::Branches, "login", &mut harness.cx());
        assert_eq!(rows.len(), 1);
        source.confirm(PickerKind::Branches, Some(0), "login", false, &mut harness.cx());
        harness.project.settle(Duration::from_secs(20));
        assert_eq!(harness.project.git_summary().branch.as_deref(), Some("feature/login"));
    }

    #[test]
    fn repositories_picker_lists_nested_repos_and_emits_select_repo() {
        let parent = tempfile::tempdir().unwrap();
        for name in ["alpha", "beta"] {
            let sub = parent.path().join(name);
            std::fs::create_dir_all(&sub).unwrap();
            git(&sub, &["init", "-b", "main"]);
            git(&sub, &["config", "user.name", "Test"]);
            git(&sub, &["config", "user.email", "test@example.com"]);
            std::fs::write(sub.join("f.txt"), "hi\n").unwrap();
            git(&sub, &["add", "."]);
            git(&sub, &["commit", "-m", "init"]);
        }
        let mut harness = settled(parent.path());
        let mut source = AppSource::new(parent.path().join("state"));
        let rows = source.rows(PickerKind::Repositories, "", &mut harness.cx());
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].label, "alpha");
        assert_eq!(rows[0].hint.as_deref(), Some("active"));
        assert_eq!(rows[1].label, "beta");
        assert_eq!(rows[1].hint, None);
        source.confirm(PickerKind::Repositories, Some(1), "", false, &mut harness.cx());
        let expected_beta = harness.project.repos()[1].repo.root().to_path_buf();
        assert_eq!(harness.take_effects(), vec![Effect::SelectRepo(expected_beta)]);
    }

    #[test]
    fn rows_complete_to_their_whole_path_keeping_the_line() {
        let dir = repo();
        let mut harness = settled(dir.path());
        let mut source = AppSource::new(dir.path().join("state"));
        let rows = source.rows(PickerKind::FileFinder, "lib:2", &mut harness.cx());
        assert_eq!(rows[0].completion.as_deref(), Some("src/lib.rs:2"));
        let rows = source.rows(PickerKind::FileFinder, "lib", &mut harness.cx());
        assert_eq!(rows[0].completion.as_deref(), Some("src/lib.rs"));
    }

    #[test]
    fn a_folder_typed_in_the_recent_projects_opens() {
        let dir = repo();
        let other = tempfile::tempdir().unwrap();
        let mut harness = settled(dir.path());
        let mut source = AppSource::new(dir.path().join("state"));
        let typed = other.path().to_string_lossy().into_owned();
        source.confirm(PickerKind::RecentProjects, None, &typed, false, &mut harness.cx());
        assert_eq!(harness.take_effects(), vec![Effect::OpenProject(other.path().to_path_buf())]);
        source.confirm(PickerKind::RecentProjects, None, "/no/such/folder", false, &mut harness.cx());
        assert_eq!(harness.take_effects(), vec![Effect::error("No folder at /no/such/folder")]);
    }

    #[test]
    fn recent_projects_leave_out_the_open_one_and_open_another() {
        let dir = repo();
        let other = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        crate::session::remember_project(state.path(), other.path()).unwrap();
        crate::session::remember_project(state.path(), dir.path()).unwrap();
        let mut harness = settled(dir.path());
        let mut source = AppSource::new(state.path().to_path_buf());
        let rows = source.rows(PickerKind::RecentProjects, "", &mut harness.cx());
        assert_eq!(rows.len(), 1, "the project that is open is not offered: {rows:?}");
        source.confirm(PickerKind::RecentProjects, Some(0), "", false, &mut harness.cx());
        assert_eq!(harness.take_effects(), vec![Effect::OpenProject(other.path().to_path_buf())]);
    }

    #[test]
    fn ages_read_naturally() {
        assert_eq!(age(5), "just now");
        assert_eq!(age(60), "1 minute ago");
        assert_eq!(age(7_200), "2 hours ago");
        assert_eq!(age(86_400 * 3), "3 days ago");
        assert_eq!(age(86_400 * 400), "1 year ago");
        assert_eq!(age(-50), "just now", "a clock that is behind");
    }

    #[test]
    fn the_factory_makes_each_kind_of_item() {
        let dir = repo();
        let mut harness = settled(dir.path());
        let mut factory = AppFactory;
        let editor = factory.open_path(&dir.path().join("a.txt"), &mut harness.cx()).unwrap();
        assert_eq!((editor.kind(), editor.path()), ("editor", Some(dir.path().join("a.txt").as_path())));
        assert!(factory.open_path(&dir.path().join("missing.txt"), &mut harness.cx()).is_err());
        assert_eq!(factory.untitled(&mut harness.cx()).kind(), "editor");
        assert_eq!(factory.project_diff(&mut harness.cx()).kind(), "project_diff");
        assert_eq!(factory.project_search(false, &mut harness.cx()).kind(), "project_search");
        assert_eq!(factory.read_only("Key Bindings", "x", None, &mut harness.cx()).kind(), "page");
    }
}
