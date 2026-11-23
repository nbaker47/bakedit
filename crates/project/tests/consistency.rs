//! The tree, the index and project search must agree on which files are ignored, because
//! they answer the same question in three different ways.

mod common;

use std::collections::BTreeSet;
use std::time::Duration;

use bakedit_project::{search, FileIndex, FileTree, IndexOptions, SearchEvent, SearchOptions};
use common::Project;

const TIMEOUT: Duration = Duration::from_secs(30);

/// Every file the tree shows without the ignored flag, with every directory opened.
fn tree_files(project: &Project) -> BTreeSet<String> {
    let mut tree = FileTree::new(project.root());
    // Open everything, ignored directories included, so their contents get judged too.
    loop {
        let closed: Vec<_> = tree
            .rows()
            .iter()
            .filter(|row| row.is_dir() && !row.is_expanded() && !row.symlink)
            .map(|row| row.path.clone())
            .collect();
        if closed.is_empty() {
            break;
        }
        for dir in closed {
            tree.expand(&dir);
        }
    }
    tree.rows()
        .iter()
        .filter(|row| !row.is_dir() && !row.ignored)
        .map(|row| relative(project, &row.path))
        .collect()
}

fn relative(project: &Project, path: &std::path::Path) -> String {
    path.strip_prefix(project.root())
        .expect("inside the project")
        .to_string_lossy()
        .into_owned()
}

fn index_files(project: &Project) -> BTreeSet<String> {
    let index = FileIndex::new(project.root(), IndexOptions::default());
    assert!(index.wait_ready(TIMEOUT));
    index.snapshot().iter().map(str::to_string).collect()
}

/// Every file a search for a pattern that matches anything reports.
fn searched_files(project: &Project) -> BTreeSet<String> {
    let options = SearchOptions {
        regex: true,
        max_results: usize::MAX,
        ..SearchOptions::new("^")
    };
    let handle = search(project.root(), options).expect("the search starts");
    let mut files = BTreeSet::new();
    while let Ok(event) = handle.events().recv_timeout(TIMEOUT) {
        if let SearchEvent::File { path, .. } = event {
            files.insert(path.to_string_lossy().into_owned());
        }
    }
    files
}

fn assert_all_agree(project: &Project, expected: &[&str]) {
    let expected: BTreeSet<String> = expected.iter().map(|path| path.to_string()).collect();
    assert_eq!(index_files(project), expected, "the index");
    assert_eq!(tree_files(project), expected, "the tree");
    assert_eq!(searched_files(project), expected, "project search");
}

/// A project that uses every kind of ignore rule.
fn rule_zoo() -> Project {
    let project = Project::new();
    for path in [
        "src/main.rs",
        "src/gen/schema.rs",
        "src/gen/keep.rs",
        "build/out.bin",
        "sub/build/kept.bin",
        "logs/app.log",
        "logs/keep.log",
        "sub/logs/deep.log",
        "docs/draft.md",
        "docs/final.md",
        "docs/api/draft.md",
        "tmp/a.o",
        "x/tmp/b.o",
        "x/tmp/c.txt",
        "notes.swp",
        "sub/notes.swp",
        "important.swp",
        "vendor/lib/code.c",
        "vendor/lib/.gitignore",
        "scratch/idea.txt",
        "data/big.csv",
        "data/small.csv",
    ] {
        project.write(path, "line\n");
    }
    project.write(
        ".gitignore",
        "\
# anchored: only the top-level build directory
/build
# directory-only, at any depth
logs/
# a wildcard with an exception
*.swp
!important.swp
# a path pattern
src/gen/*
!src/gen/keep.rs
# a deep wildcard
**/tmp/*.o
",
    );
    project.write("docs/.gitignore", "/draft.md\n");
    project.write("vendor/lib/.gitignore", "*.c\n");
    project.write(".ignore", "scratch/\n");
    project.write("data/.ignore", "big.*\n");
    project
}

const RULE_ZOO_FILES: &[&str] = &[
    ".gitignore",
    ".ignore",
    "data/.ignore",
    "data/small.csv",
    "docs/.gitignore",
    "docs/api/draft.md",
    "docs/final.md",
    "important.swp",
    "src/gen/keep.rs",
    "src/main.rs",
    "sub/build/kept.bin",
    "vendor/lib/.gitignore",
    "x/tmp/c.txt",
];

#[test]
fn every_kind_of_ignore_rule_is_judged_the_same_everywhere() {
    let project = rule_zoo();
    assert_all_agree(&project, RULE_ZOO_FILES);
}

#[test]
fn the_same_holds_inside_a_git_repository() {
    let project = rule_zoo();
    // A `.git` directory makes it a repository, which switches to git's own rules and
    // brings `.git/info/exclude` into play.
    project.write(".git/HEAD", "ref: refs/heads/main\n");
    project.write(".git/info/exclude", "docs/final.md\n");
    let expected: Vec<&str> = RULE_ZOO_FILES
        .iter()
        .copied()
        .filter(|path| *path != "docs/final.md")
        .collect();
    assert_all_agree(&project, &expected);
}

#[test]
fn a_nested_repository_keeps_its_own_rules() {
    let project = Project::new();
    project.write(".git/HEAD", "ref: refs/heads/main\n");
    project.write(".gitignore", "*.outer\n");
    project.write("a.outer", "x\n");
    project.write("kept.txt", "x\n");
    // A repository inside the repository: the outer rules stop at its door.
    project.write("nested/.git/HEAD", "ref: refs/heads/main\n");
    project.write("nested/.gitignore", "*.inner\n");
    project.write("nested/b.outer", "x\n");
    project.write("nested/c.inner", "x\n");
    project.write("nested/kept.txt", "x\n");
    assert_all_agree(
        &project,
        &[
            ".gitignore",
            "kept.txt",
            "nested/.gitignore",
            "nested/b.outer",
            "nested/kept.txt",
        ],
    );
}
