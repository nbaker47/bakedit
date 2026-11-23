//! The pieces wired together the way the app wires them: the watcher's batches feed the
//! tree and the index, and all three end up telling the same story as the disk.

mod common;

use std::fs;
use std::time::{Duration, Instant};

use bakedit_project::{FileIndex, FileTree, FsBatch, IndexOptions, Watcher};
use common::Project;

const TIMEOUT: Duration = Duration::from_secs(20);

/// A tree, an index and a watcher on one project, as the app holds them.
struct Workspace {
    tree: FileTree,
    index: FileIndex,
    watcher: Watcher,
}

impl Workspace {
    fn open(project: &Project) -> Workspace {
        let watcher = Watcher::new(project.root(), Box::new(|| {})).expect("the watcher starts");
        let tree = FileTree::new(project.root());
        let index = FileIndex::new(project.root(), IndexOptions::default());
        assert!(index.wait_ready(TIMEOUT));
        Workspace {
            tree,
            index,
            watcher,
        }
    }

    /// What the app does with each batch.
    fn apply(&mut self, batch: &FsBatch) {
        if batch.rescan {
            self.tree.refresh_all();
            self.index.rebuild();
        } else {
            self.tree.refresh(&batch.paths);
            self.index.update(&batch.paths);
        }
        assert!(self.index.wait_idle(TIMEOUT));
    }

    /// What the app does after the user expands or collapses something.
    fn expansion_changed(&mut self) {
        self.watcher
            .watch_ignored_dirs(&self.tree.expanded_ignored_dirs());
    }

    /// Apply batches as they arrive until `done` holds, or give up after the timeout.
    fn settle(&mut self, done: impl Fn(&Workspace) -> bool) -> bool {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if done(self) {
                return true;
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            let wait = left.min(Duration::from_millis(100));
            if let Ok(batch) = self.watcher.events().recv_timeout(wait) {
                self.apply(&batch);
            }
        }
    }

    fn shows(&self, project: &Project, relative: &str) -> bool {
        self.tree.index_of(&project.path(relative)).is_some()
    }

    fn lists(&self, relative: &str) -> bool {
        self.index.snapshot().contains(relative)
    }
}

#[test]
fn changes_on_disk_reach_the_tree_and_the_index() {
    let project = Project::with_files(&["src/main.rs", "README.md"]);
    let mut workspace = Workspace::open(&project);
    workspace.tree.expand(&project.path("src"));
    workspace.tree.select(
        workspace
            .tree
            .index_of(&project.path("src/main.rs"))
            .unwrap(),
    );

    // A new file.
    project.write("src/new.rs", "pub fn new() {}\n");
    assert!(
        workspace.settle(|w| w.shows(&project, "src/new.rs") && w.lists("src/new.rs")),
        "a created file shows up in both"
    );
    assert_eq!(
        workspace.tree.selected_path(),
        Some(project.path("src/main.rs").as_path()),
        "the selection stays on its file"
    );

    // A rename.
    fs::rename(project.path("src/new.rs"), project.path("src/renamed.rs")).unwrap();
    assert!(workspace.settle(|w| {
        w.shows(&project, "src/renamed.rs")
            && !w.shows(&project, "src/new.rs")
            && w.lists("src/renamed.rs")
            && !w.lists("src/new.rs")
    }));

    // A new directory with files in it, created in one go.
    project.write("docs/guide/intro.md", "# Intro\n");
    project.write("docs/guide/setup.md", "# Setup\n");
    assert!(workspace.settle(|w| {
        w.shows(&project, "docs")
            && w.lists("docs/guide/intro.md")
            && w.lists("docs/guide/setup.md")
    }));
    assert!(
        !workspace.shows(&project, "docs/guide/intro.md"),
        "nobody expanded docs, so its contents are not rows"
    );

    // An ignore rule arrives: the tree dims, the index drops.
    project.write(".gitignore", "docs/\n");
    assert!(workspace.settle(|w| {
        let dimmed = w
            .tree
            .rows()
            .iter()
            .any(|row| row.name == "docs" && row.ignored);
        dimmed && !w.lists("docs/guide/intro.md") && w.lists(".gitignore")
    }));

    // The selected file is deleted: the selection moves on, the index forgets it.
    project.remove("src/main.rs");
    assert!(workspace.settle(|w| !w.shows(&project, "src/main.rs") && !w.lists("src/main.rs")));
    assert_eq!(
        workspace.tree.selected_path(),
        Some(project.path("src/renamed.rs").as_path())
    );

    // A whole directory goes.
    project.remove("src");
    assert!(workspace.settle(|w| !w.shows(&project, "src") && !w.lists("src/renamed.rs")));
    let listed: Vec<String> = workspace
        .index
        .snapshot()
        .iter()
        .map(str::to_string)
        .collect();
    assert_eq!(listed, [".gitignore", "README.md"]);
}

#[test]
fn an_ignored_directory_stays_fresh_only_while_it_is_open() {
    let project = Project::with_files(&["src/main.rs", "out/first.bin"]);
    project.write(".gitignore", "out/\n");
    let mut workspace = Workspace::open(&project);

    // Open the ignored directory and tell the watcher.
    workspace.tree.expand(&project.path("out"));
    workspace.expansion_changed();
    project.write("out/second.bin", "x");
    assert!(
        workspace.settle(|w| w.shows(&project, "out/second.bin")),
        "an open ignored directory follows the disk"
    );
    assert!(
        !workspace.lists("out/second.bin"),
        "but its files still stay out of the index"
    );

    // Close it again: what happens inside no longer reaches anyone.
    workspace.tree.collapse(&project.path("out"));
    workspace.expansion_changed();
    project.write("out/third.bin", "x");
    project.write("src/marker.rs", "");
    assert!(workspace.settle(|w| w.lists("src/marker.rs")));
    assert!(!workspace.lists("out/third.bin"));
    // Reopening reads the directory afresh, so nothing was lost.
    workspace.tree.expand(&project.path("out"));
    assert!(workspace.shows(&project, "out/third.bin"));
}

#[test]
fn file_operations_made_through_the_tree_reach_the_index() {
    let project = Project::with_files(&["src/main.rs"]);
    let mut workspace = Workspace::open(&project);

    let created = workspace
        .tree
        .create_file(project.root(), "src/ui/button.rs")
        .unwrap();
    assert!(
        workspace.shows(&project, "src/ui/button.rs"),
        "the tree knows at once"
    );
    assert!(
        workspace.settle(|w| w.lists("src/ui/button.rs")),
        "the index follows the watcher"
    );

    let renamed = workspace.tree.rename(&created, "widget.rs").unwrap();
    assert!(workspace.settle(|w| w.lists("src/ui/widget.rs") && !w.lists("src/ui/button.rs")));

    let copy = workspace.tree.duplicate(&renamed).unwrap();
    assert_eq!(copy, project.path("src/ui/widget copy.rs"));
    assert!(workspace.settle(|w| w.lists("src/ui/widget copy.rs")));

    workspace.tree.delete(&project.path("src/ui")).unwrap();
    assert!(workspace.settle(|w| !w.lists("src/ui/widget.rs") && !w.lists("src/ui/widget copy.rs")));
    let listed: Vec<String> = workspace
        .index
        .snapshot()
        .iter()
        .map(str::to_string)
        .collect();
    assert_eq!(
        listed,
        ["src/main.rs"],
        "no temporary files were ever listed"
    );
}
