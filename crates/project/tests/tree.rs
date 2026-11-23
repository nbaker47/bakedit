//! The file tree on real directories: loading, flags, navigation, refresh and file
//! operations.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use bakedit_project::{FileOpError, FileTree, RowKind};
use common::Project;

/// The tree as text: one line per row, indented by depth. `v` is an expanded directory,
/// `>` a collapsed one, `-` a file; `*` marks the selection.
fn outline(tree: &FileTree) -> Vec<String> {
    tree.rows()
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let marker = match row.kind {
                RowKind::Dir { expanded: true } => 'v',
                RowKind::Dir { expanded: false } => '>',
                RowKind::File => '-',
            };
            let name = if row.is_root { "ROOT" } else { &row.name };
            format!(
                "{}{}{marker} {name}{}{}",
                if tree.selected() == Some(index) {
                    "*"
                } else {
                    ""
                },
                "  ".repeat(row.depth),
                if row.ignored { " [ignored]" } else { "" },
                if row.symlink { " [link]" } else { "" },
            )
        })
        .collect()
}

fn names(tree: &FileTree) -> Vec<String> {
    tree.rows()
        .iter()
        .skip(1)
        .map(|row| row.name.clone())
        .collect()
}

fn selected_name(tree: &FileTree) -> Option<String> {
    tree.selected_row().map(|row| row.name.clone())
}

#[test]
fn a_new_tree_shows_the_root_and_its_children() {
    let project = Project::with_files(&["src/main.rs", "README.md", "Cargo.toml"]);
    let tree = FileTree::new(project.root());

    assert_eq!(
        outline(&tree),
        ["v ROOT", "  > src", "  - Cargo.toml", "  - README.md"]
    );
    assert_eq!(tree.selected(), None);
    assert_eq!(tree.selected_path(), None);
    assert_eq!(tree.root(), project.root());

    let root = &tree.rows()[0];
    assert!(root.is_root && root.is_dir() && root.is_expanded());
    assert_eq!(root.depth, 0);
    assert_eq!(root.path, project.root());
    assert_eq!(
        root.name,
        project.root().file_name().unwrap().to_string_lossy()
    );

    let src = &tree.rows()[1];
    assert_eq!(src.path, project.path("src"));
    assert_eq!((src.depth, src.is_root), (1, false));
}

#[test]
fn entries_are_ordered_like_the_project_panel() {
    let project = Project::with_files(&[
        "file10.txt",
        "file2.txt",
        "File1.txt",
        "banana.rs",
        "Apple.rs",
        ".env",
        "zeta/x",
        "Alpha/x",
        ".github/x",
        "a-b.txt",
        "a.txt",
    ]);
    let tree = FileTree::new(project.root());
    assert_eq!(
        names(&tree),
        [
            ".github",
            "Alpha",
            "zeta",
            ".env",
            "a.txt",
            "a-b.txt",
            "Apple.rs",
            "banana.rs",
            "File1.txt",
            "file2.txt",
            "file10.txt",
        ]
    );
}

#[test]
fn directories_are_read_only_when_they_are_expanded() {
    let project = Project::with_files(&["src/main.rs", "src/nested/deep.rs"]);
    let mut tree = FileTree::new(project.root());
    assert_eq!(outline(&tree), ["v ROOT", "  > src"]);

    // Written after the tree was built and before `src` was ever opened: it is there
    // on expansion without any refresh, because nothing was read before.
    project.write("src/late.rs", "");
    assert!(tree.expand(&project.path("src")));
    assert_eq!(
        outline(&tree),
        [
            "v ROOT",
            "  v src",
            "    > nested",
            "    - late.rs",
            "    - main.rs"
        ]
    );
    assert!(tree.is_expanded(&project.path("src")));
    assert!(!tree.is_expanded(&project.path("src/nested")));

    // An expanded directory is a snapshot until it is refreshed.
    project.write("src/later.rs", "");
    assert!(!names(&tree).contains(&"later.rs".to_string()));
}

#[test]
fn expanding_something_that_is_not_a_directory_does_nothing() {
    let project = Project::with_files(&["a.txt"]);
    let mut tree = FileTree::new(project.root());
    assert!(!tree.expand(&project.path("a.txt")));
    assert!(!tree.expand(&project.path("missing")));
    assert!(!tree.expand(Path::new("/")));
    assert!(!tree.collapse(&project.path("a.txt")));
    assert_eq!(outline(&tree), ["v ROOT", "  - a.txt"]);
}

#[test]
fn hidden_names_never_appear_but_dotfiles_do() {
    let project = Project::with_files(&[
        ".git/HEAD",
        ".DS_Store",
        ".gitignore",
        ".env",
        "src/.DS_Store",
        "src/.hidden-but-shown",
        "src/.svn/entries",
        "CVS/Root",
        "Thumbs.db",
        ".jj/repo",
        ".hg/store",
        ".classpath",
        ".settings/x",
    ]);
    fs::write(project.path(".gitignore"), "").unwrap();
    let mut tree = FileTree::new(project.root());
    tree.expand(&project.path("src"));
    assert_eq!(
        outline(&tree),
        [
            "v ROOT",
            "  v src",
            "    - .hidden-but-shown",
            "  - .env",
            "  - .gitignore",
        ]
    );
}

#[test]
fn ignored_entries_are_shown_flagged_and_stay_closed() {
    let project = Project::with_files(&[
        "build-out/artifact.bin",
        "build-out/deep/more.bin",
        "src/main.rs",
        "src/cache.tmpdata",
        "notes.tmpdata",
        "kept.txt",
    ]);
    project.write(".gitignore", "build-out/\n*.tmpdata\n");
    let mut tree = FileTree::new(project.root());
    assert_eq!(
        outline(&tree),
        [
            "v ROOT",
            "  > build-out [ignored]",
            "  > src",
            "  - .gitignore",
            "  - kept.txt",
            "  - notes.tmpdata [ignored]",
        ]
    );

    // Everything inside an ignored directory is ignored too, once it is opened.
    tree.expand(&project.path("build-out"));
    tree.expand(&project.path("src"));
    assert_eq!(
        outline(&tree),
        [
            "v ROOT",
            "  v build-out [ignored]",
            "    > deep [ignored]",
            "    - artifact.bin [ignored]",
            "  v src",
            "    - cache.tmpdata [ignored]",
            "    - main.rs",
            "  - .gitignore",
            "  - kept.txt",
            "  - notes.tmpdata [ignored]",
        ]
    );
}

#[test]
fn open_ignored_directories_are_listed_for_the_watcher() {
    let project = Project::with_files(&["out/deep/x.bin", "src/main.rs"]);
    project.write(".gitignore", "out/\n");
    let mut tree = FileTree::new(project.root());
    tree.expand(&project.path("src"));
    assert!(tree.expanded_ignored_dirs().is_empty());

    tree.expand(&project.path("out"));
    assert_eq!(tree.expanded_ignored_dirs(), [project.path("out")]);
    tree.expand(&project.path("out/deep"));
    assert_eq!(
        tree.expanded_ignored_dirs(),
        [project.path("out"), project.path("out/deep")]
    );
    tree.collapse(&project.path("out"));
    assert!(
        tree.expanded_ignored_dirs().is_empty(),
        "hidden ones do not count"
    );
}

#[test]
fn dot_ignore_files_and_nested_gitignores_count() {
    let project = Project::with_files(&["docs/draft.md", "docs/final.md", "scratch/x", "keep/x"]);
    project.write(".ignore", "scratch/\n");
    project.write("docs/.gitignore", "draft.md\n");
    let mut tree = FileTree::new(project.root());
    tree.expand(&project.path("docs"));
    assert_eq!(
        outline(&tree),
        [
            "v ROOT",
            "  v docs",
            "    - .gitignore",
            "    - draft.md [ignored]",
            "    - final.md",
            "  > keep",
            "  > scratch [ignored]",
            "  - .ignore",
        ]
    );
}

#[cfg(unix)]
#[test]
fn symlinks_are_flagged_and_linked_directories_expand() {
    let project = Project::with_files(&["real/inside.txt", "target.txt"]);
    project.symlink("target.txt", "link-to-file");
    project.symlink("real", "link-to-dir");
    project.symlink("nowhere", "dangling");
    let mut tree = FileTree::new(project.root());
    assert_eq!(
        outline(&tree),
        [
            "v ROOT",
            "  > link-to-dir [link]",
            "  > real",
            "  - dangling [link]",
            "  - link-to-file [link]",
            "  - target.txt",
        ]
    );

    assert!(tree.expand(&project.path("link-to-dir")));
    let inside = tree
        .rows()
        .iter()
        .find(|row| row.name == "inside.txt")
        .expect("the linked directory's contents are shown");
    // The path goes through the link, and the entry itself is not a link.
    assert_eq!(inside.path, project.path("link-to-dir/inside.txt"));
    assert!(!inside.symlink);
    assert_eq!(inside.depth, 2);
}

#[cfg(unix)]
#[test]
fn symlink_cycles_do_not_expand() {
    let project = Project::with_files(&["a/b/file.txt"]);
    project.symlink("..", "a/b/up"); // a/b/up -> a
    project.symlink(project.root(), "a/to-root"); // absolute link to the root
    project.symlink("b", "a/sibling"); // harmless: a/sibling -> a/b
    let mut tree = FileTree::new(project.root());
    tree.expand(&project.path("a"));
    tree.expand(&project.path("a/b"));

    assert!(
        !tree.expand(&project.path("a/b/up")),
        "a/b/up leads back to a"
    );
    assert!(
        !tree.expand(&project.path("a/to-root")),
        "a/to-root leads back to the root"
    );
    assert!(!tree.is_expanded(&project.path("a/b/up")));

    // A link that does not loop expands, and loops further down are still caught.
    assert!(tree.expand(&project.path("a/sibling")));
    assert!(!tree.expand(&project.path("a/sibling/up")));
    assert!(tree
        .rows()
        .iter()
        .any(|row| row.path == project.path("a/sibling/file.txt")));

    // "Expand all" never follows links, so it terminates.
    tree.collapse_all();
    assert!(tree.expand_all(project.root()));
    assert!(tree.rows().len() < 20);
}

#[cfg(unix)]
#[test]
fn a_symlinked_root_works() {
    let project = Project::with_files(&["real/src/main.rs"]);
    let link = project.symlink("real", "link");
    let mut tree = FileTree::new(&link);
    assert_eq!(outline(&tree), ["v ROOT", "  > src"]);
    assert_eq!(tree.root(), link);
    assert!(tree.expand(&link.join("src")));
    assert_eq!(tree.rows()[2].path, link.join("src/main.rs"));
}

#[cfg(unix)]
#[test]
fn an_unreadable_directory_stays_collapsed() {
    use std::os::unix::fs::PermissionsExt;
    let project = Project::with_files(&["locked/secret.txt", "open/file.txt"]);
    let locked = project.path("locked");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let unreadable = fs::read_dir(&locked).is_err();

    let mut tree = FileTree::new(project.root());
    let expanded = tree.expand(&locked);
    let revealed = tree.reveal(&project.path("locked/secret.txt"));
    let rows = outline(&tree);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    if !unreadable {
        // Running as a user who can read anything (root): nothing to check.
        return;
    }
    assert!(!expanded);
    assert_eq!(revealed, None);
    assert_eq!(rows, ["v ROOT", "  > locked", "  > open"]);

    // Once it can be read, it expands like any other.
    assert!(tree.expand(&locked));
    assert_eq!(tree.rows().len(), 4);
}

#[cfg(unix)]
#[test]
fn names_that_are_not_utf8_are_shown_lossily_and_stay_usable() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let project = Project::new();
    let path = project.root().join(OsStr::from_bytes(b"caf\xe9.txt"));
    if fs::write(&path, "x").is_err() {
        // This file system (APFS, for one) only takes names that are valid UTF-8.
        return;
    }
    let mut tree = FileTree::new(project.root());
    assert_eq!(names(&tree), ["caf\u{fffd}.txt"]);
    assert_eq!(tree.rows()[1].path, path, "the real path is kept");
    assert_eq!(tree.reveal(&path), Some(1));
    let copy = tree.duplicate(&path).unwrap();
    assert!(copy.exists());
    tree.delete(&path).unwrap();
    assert!(!path.exists());
}

#[test]
fn an_unreadable_root_is_just_the_root_row() {
    let project = Project::new();
    let missing = project.path("does-not-exist");
    let mut tree = FileTree::new(&missing);
    assert_eq!(outline(&tree), ["v ROOT"]);

    // It fills in when the directory appears.
    project.write("does-not-exist/a.txt", "");
    tree.refresh_all();
    assert_eq!(outline(&tree), ["v ROOT", "  - a.txt"]);
}

#[test]
fn the_selection_moves_and_stops_at_the_ends() {
    let project = Project::with_files(&["a", "b", "c", "d", "e"]);
    let mut tree = FileTree::new(project.root());
    assert_eq!(tree.rows().len(), 6);

    tree.select_next();
    assert_eq!(
        tree.selected(),
        Some(0),
        "from nothing, next selects the first row"
    );
    tree.clear_selection();
    tree.select_prev();
    assert_eq!(
        tree.selected(),
        Some(0),
        "from nothing, prev selects the first row"
    );

    tree.select_next();
    tree.select_next();
    assert_eq!(selected_name(&tree).as_deref(), Some("b"));
    assert_eq!(tree.selected_path(), Some(project.path("b").as_path()));
    tree.select_prev();
    assert_eq!(selected_name(&tree).as_deref(), Some("a"));

    tree.select_last();
    assert_eq!(selected_name(&tree).as_deref(), Some("e"));
    tree.select_next();
    assert_eq!(
        selected_name(&tree).as_deref(),
        Some("e"),
        "no wrap at the bottom"
    );
    tree.select_first();
    assert_eq!(tree.selected(), Some(0));
    tree.select_prev();
    assert_eq!(tree.selected(), Some(0), "no wrap at the top");

    tree.select_page_down(3);
    assert_eq!(tree.selected(), Some(3));
    tree.select_page_down(100);
    assert_eq!(tree.selected(), Some(5));
    tree.select_page_up(2);
    assert_eq!(tree.selected(), Some(3));
    tree.select_page_up(100);
    assert_eq!(tree.selected(), Some(0));

    tree.select(4);
    assert_eq!(selected_name(&tree).as_deref(), Some("d"));
    tree.select(999);
    assert_eq!(
        tree.selected(),
        Some(5),
        "an index past the end selects the last row"
    );
    assert_eq!(tree.index_of(&project.path("c")), Some(3));
    assert_eq!(
        tree.index_of(Path::new("c")),
        Some(3),
        "relative paths are accepted"
    );
    assert_eq!(tree.index_of(&project.path("zzz")), None);
}

#[test]
fn expand_and_collapse_follow_the_arrow_keys() {
    let project =
        Project::with_files(&["dir/inner/x.txt", "dir/a.txt", "empty-holder/keep", "z.txt"]);
    fs::remove_file(project.path("empty-holder/keep")).unwrap();
    let mut tree = FileTree::new(project.root());

    // Right on a collapsed directory expands it and stays put.
    tree.select(tree.index_of(&project.path("dir")).unwrap());
    tree.expand_selected();
    assert_eq!(
        outline(&tree),
        [
            "v ROOT",
            "*  v dir",
            "    > inner",
            "    - a.txt",
            "  > empty-holder",
            "  - z.txt",
        ]
    );
    // Right on an expanded directory moves to its first child.
    tree.expand_selected();
    assert_eq!(selected_name(&tree).as_deref(), Some("inner"));
    // Left on a collapsed directory moves to the parent.
    tree.collapse_selected();
    assert_eq!(selected_name(&tree).as_deref(), Some("dir"));
    // Left on an expanded directory collapses it and stays put.
    tree.collapse_selected();
    assert_eq!(selected_name(&tree).as_deref(), Some("dir"));
    assert!(!tree.is_expanded(&project.path("dir")));
    // Left again moves to the root; left on the expanded root collapses it.
    tree.collapse_selected();
    assert_eq!(tree.selected(), Some(0));
    tree.collapse_selected();
    assert_eq!(outline(&tree), ["*> ROOT"]);
    tree.collapse_selected();
    assert_eq!(outline(&tree), ["*> ROOT"], "nothing above the root");
    tree.expand_selected();
    assert_eq!(tree.rows().len(), 4);

    // Right on a file does nothing; left on a file moves to its directory.
    tree.select(tree.index_of(&project.path("z.txt")).unwrap());
    tree.expand_selected();
    assert_eq!(selected_name(&tree).as_deref(), Some("z.txt"));
    tree.collapse_selected();
    assert_eq!(tree.selected(), Some(0));

    // Right on an expanded but empty directory keeps the selection.
    tree.select(tree.index_of(&project.path("empty-holder")).unwrap());
    tree.expand_selected();
    assert!(tree.is_expanded(&project.path("empty-holder")));
    tree.expand_selected();
    assert_eq!(selected_name(&tree).as_deref(), Some("empty-holder"));

    // Toggle flips directories and leaves files alone.
    tree.toggle_selected();
    assert!(!tree.is_expanded(&project.path("empty-holder")));
    tree.toggle_selected();
    assert!(tree.is_expanded(&project.path("empty-holder")));
    tree.select(tree.index_of(&project.path("z.txt")).unwrap());
    let before = outline(&tree);
    tree.toggle_selected();
    assert_eq!(outline(&tree), before);

    // With nothing selected, none of them do anything.
    tree.clear_selection();
    tree.expand_selected();
    tree.collapse_selected();
    tree.toggle_selected();
    tree.expand_all_selected();
    assert_eq!(tree.selected(), None);
}

#[test]
fn collapsing_remembers_what_was_open_inside() {
    let project = Project::with_files(&["a/b/c/file.txt", "a/other.txt"]);
    let mut tree = FileTree::new(project.root());
    tree.expand(&project.path("a"));
    tree.expand(&project.path("a/b"));
    tree.expand(&project.path("a/b/c"));
    let open = outline(&tree);
    assert_eq!(open.len(), 6);

    tree.collapse(&project.path("a"));
    assert_eq!(outline(&tree), ["v ROOT", "  > a"]);
    assert!(
        tree.is_expanded(&project.path("a/b/c")),
        "remembered while hidden"
    );
    tree.expand(&project.path("a"));
    assert_eq!(outline(&tree), open);

    assert_eq!(
        tree.expanded_paths(),
        [
            project.root().to_path_buf(),
            project.path("a"),
            project.path("a/b"),
            project.path("a/b/c"),
        ]
    );
}

#[test]
fn collapse_all_keeps_only_the_root_open() {
    let project = Project::with_files(&["a/b/c/file.txt", "d/e.txt"]);
    let mut tree = FileTree::new(project.root());
    tree.reveal(&project.path("a/b/c/file.txt"));
    tree.expand(&project.path("d"));
    assert_eq!(selected_name(&tree).as_deref(), Some("file.txt"));

    tree.collapse_all();
    assert_eq!(outline(&tree), ["v ROOT", "*  > a", "  > d"]);
    assert_eq!(tree.expanded_paths(), [project.root().to_path_buf()]);
    // Nothing is remembered: opening `a` shows it collapsed inside.
    tree.expand(&project.path("a"));
    assert_eq!(outline(&tree), ["v ROOT", "*  v a", "    > b", "  > d"]);
}

#[test]
fn expand_all_opens_everything_below_but_not_ignored_directories() {
    let project = Project::with_files(&[
        "src/a/b/deep.rs",
        "src/c/other.rs",
        "out/x/y/z.bin",
        "docs/guide/intro.md",
    ]);
    project.write(".gitignore", "out/\n");
    let mut tree = FileTree::new(project.root());

    tree.select(tree.index_of(&project.path("src")).unwrap());
    tree.expand_all_selected();
    assert_eq!(
        outline(&tree),
        [
            "v ROOT",
            "  > docs",
            "  > out [ignored]",
            "*  v src",
            "    v a",
            "      v b",
            "        - deep.rs",
            "    v c",
            "      - other.rs",
            "  - .gitignore",
        ]
    );

    // From the root: everything except the ignored directory.
    assert!(tree.expand_all(project.root()));
    assert!(tree.is_expanded(&project.path("docs/guide")));
    assert!(!tree.is_expanded(&project.path("out")));

    // Asked for explicitly, an ignored directory expands all the way down.
    assert!(tree.expand_all(&project.path("out")));
    assert!(tree.is_expanded(&project.path("out/x/y")));
    assert!(tree
        .rows()
        .iter()
        .any(|row| row.path == project.path("out/x/y/z.bin") && row.ignored));

    // Files and paths that are not visible cannot be expanded.
    assert!(!tree.expand_all(&project.path(".gitignore")));
    tree.collapse_all();
    assert!(!tree.expand_all(&project.path("src/a/b")));
}

#[test]
fn reveal_opens_the_way_to_a_path_and_selects_it() {
    let project = Project::with_files(&["a/b/c/target.rs", "a/b/sibling.rs", "top.rs"]);
    let mut tree = FileTree::new(project.root());
    assert_eq!(outline(&tree), ["v ROOT", "  > a", "  - top.rs"]);

    let index = tree.reveal(&project.path("a/b/c/target.rs"));
    assert_eq!(index, Some(4));
    assert_eq!(
        outline(&tree),
        [
            "v ROOT",
            "  v a",
            "    v b",
            "      v c",
            "*        - target.rs",
            "      - sibling.rs",
            "  - top.rs",
        ]
    );

    // A directory is revealed without being opened; relative paths work.
    tree.collapse_all();
    assert_eq!(tree.reveal(Path::new("a/b/c")), Some(3));
    assert_eq!(outline(&tree)[3], "*      > c");
    assert_eq!(tree.reveal(project.root()), Some(0));
    assert_eq!(
        tree.reveal(Path::new("a/../top.rs")),
        tree.index_of(&project.path("top.rs"))
    );
}

#[test]
fn reveal_finds_a_file_created_after_its_directory_was_read() {
    let project = Project::with_files(&["src/old.rs"]);
    let mut tree = FileTree::new(project.root());
    tree.expand(&project.path("src"));
    let new = project.write("src/new.rs", "");
    assert_eq!(tree.index_of(&new), None, "the tree has not been told yet");
    assert_eq!(tree.reveal(&new), Some(2));
    assert_eq!(selected_name(&tree).as_deref(), Some("new.rs"));
}

#[test]
fn reveal_refuses_what_cannot_be_shown() {
    let project = Project::with_files(&["a/file.rs", ".git/HEAD"]);
    let mut tree = FileTree::new(project.root());
    tree.select(1);
    let before = outline(&tree);

    assert_eq!(tree.reveal(&project.path("a/missing.rs")), None);
    assert_eq!(tree.reveal(&project.path(".git/HEAD")), None);
    assert_eq!(tree.reveal(Path::new("/etc/hosts")), None);
    assert_eq!(tree.reveal(&project.path("a/file.rs/below")), None);
    assert_eq!(outline(&tree), before, "a failed reveal changes nothing");
}

#[test]
fn reveal_finds_files_inside_ignored_directories() {
    let project = Project::with_files(&["out/gen/code.rs"]);
    project.write(".gitignore", "out/\n");
    let mut tree = FileTree::new(project.root());
    assert!(tree.reveal(&project.path("out/gen/code.rs")).is_some());
    assert!(tree.selected_row().unwrap().ignored);
}

#[test]
fn refresh_rereads_only_what_changed_and_keeps_state() {
    let project = Project::with_files(&["src/a.rs", "src/sub/x.rs", "docs/readme.md", "top.txt"]);
    let mut tree = FileTree::new(project.root());
    tree.expand(&project.path("src"));
    tree.expand(&project.path("src/sub"));
    tree.select(tree.index_of(&project.path("top.txt")).unwrap());

    let created = project.write("src/b.rs", "");
    let hidden_change = project.write("docs/new.md", "");
    project.remove("src/sub/x.rs");
    // Nothing moves until the tree is told.
    assert!(tree.index_of(&created).is_none());

    tree.refresh(&[created.clone(), hidden_change, project.path("src/sub/x.rs")]);
    assert_eq!(
        outline(&tree),
        [
            "v ROOT",
            "  > docs",
            "  v src",
            "    v sub",
            "    - a.rs",
            "    - b.rs",
            "*  - top.txt",
        ]
    );
    // The collapsed directory was not read, but shows its new file when opened.
    tree.expand(&project.path("docs"));
    assert!(tree.index_of(&project.path("docs/new.md")).is_some());
}

#[test]
fn refresh_only_touches_the_directories_it_is_told_about() {
    let project = Project::with_files(&["a/one.txt", "b/one.txt"]);
    let mut tree = FileTree::new(project.root());
    tree.expand(&project.path("a"));
    tree.expand(&project.path("b"));

    project.write("a/two.txt", "");
    project.write("b/two.txt", "");
    tree.refresh(&[project.path("a/two.txt")]);
    assert!(tree.index_of(&project.path("a/two.txt")).is_some());
    assert!(
        tree.index_of(&project.path("b/two.txt")).is_none(),
        "b was not mentioned, so it was not read again"
    );

    // Naming a directory re-reads it; relative paths and paths outside are fine.
    tree.refresh(&[PathBuf::from("b"), PathBuf::from("/somewhere/else")]);
    assert!(tree.index_of(&project.path("b/two.txt")).is_some());
    tree.refresh::<PathBuf>(&[]);
}

#[test]
fn the_selection_follows_its_path_through_a_refresh() {
    let project = Project::with_files(&["b.txt", "d.txt"]);
    let mut tree = FileTree::new(project.root());
    tree.select(tree.index_of(&project.path("d.txt")).unwrap());
    assert_eq!(tree.selected(), Some(2));

    project.write("a.txt", "");
    project.write("c.txt", "");
    tree.refresh(&[project.path("a.txt"), project.path("c.txt")]);
    assert_eq!(tree.selected(), Some(4));
    assert_eq!(selected_name(&tree).as_deref(), Some("d.txt"));
}

#[test]
fn a_vanished_selection_moves_to_the_nearest_survivor() {
    let project = Project::with_files(&["dir/a.txt", "dir/b.txt", "dir/c.txt", "z.txt"]);
    let mut tree = FileTree::new(project.root());
    tree.expand(&project.path("dir"));

    // The next sibling takes its place.
    tree.select(tree.index_of(&project.path("dir/b.txt")).unwrap());
    project.remove("dir/b.txt");
    tree.refresh(&[project.path("dir/b.txt")]);
    assert_eq!(selected_name(&tree).as_deref(), Some("c.txt"));

    // No next sibling: the previous one.
    project.remove("dir/c.txt");
    tree.refresh(&[project.path("dir/c.txt")]);
    assert_eq!(selected_name(&tree).as_deref(), Some("a.txt"));

    // No siblings left: the parent.
    project.remove("dir/a.txt");
    tree.refresh(&[project.path("dir/a.txt")]);
    assert_eq!(selected_name(&tree).as_deref(), Some("dir"));

    // The parent goes too: what stands where it stood.
    project.write("dir/deep/file.txt", "");
    tree.reveal(&project.path("dir/deep/file.txt"));
    project.remove("dir");
    tree.refresh(&[project.path("dir")]);
    assert_eq!(selected_name(&tree).as_deref(), Some("z.txt"));
    assert_eq!(outline(&tree), ["v ROOT", "*  - z.txt"]);
    assert_eq!(tree.expanded_paths(), [project.root().to_path_buf()]);
}

#[test]
fn a_directory_that_is_replaced_by_a_file_is_no_longer_expanded() {
    let project = Project::with_files(&["thing/inside.txt"]);
    let mut tree = FileTree::new(project.root());
    tree.expand(&project.path("thing"));
    assert_eq!(outline(&tree), ["v ROOT", "  v thing", "    - inside.txt"]);

    project.remove("thing");
    project.write("thing", "now a file");
    tree.refresh(&[project.path("thing")]);
    assert_eq!(outline(&tree), ["v ROOT", "  - thing"]);
    assert!(!tree.is_expanded(&project.path("thing")));
}

#[test]
fn a_changed_gitignore_updates_the_flags() {
    let project = Project::with_files(&["logs/app.log", "src/main.rs"]);
    let mut tree = FileTree::new(project.root());
    tree.expand(&project.path("logs"));
    assert!(tree.rows().iter().all(|row| !row.ignored));

    let gitignore = project.write(".gitignore", "logs/\n");
    tree.refresh(std::slice::from_ref(&gitignore));
    assert_eq!(
        outline(&tree),
        [
            "v ROOT",
            "  v logs [ignored]",
            "    - app.log [ignored]",
            "  > src",
            "  - .gitignore",
        ]
    );

    project.write(".gitignore", "");
    tree.refresh(&[gitignore]);
    assert!(tree.rows().iter().all(|row| !row.ignored));

    // `.git/info/exclude` is an ignore file too.
    project.write(".git/info/exclude", "src/\n");
    tree.refresh(&[project.path(".git/info/exclude")]);
    assert!(
        tree.rows()
            .iter()
            .find(|row| row.name == "src")
            .unwrap()
            .ignored
    );
}

#[test]
fn refresh_all_rereads_everything_visible() {
    let project = Project::with_files(&["a/one.txt", "b/one.txt"]);
    let mut tree = FileTree::new(project.root());
    tree.expand(&project.path("a"));
    tree.expand(&project.path("b"));
    tree.select(tree.index_of(&project.path("b/one.txt")).unwrap());

    project.write("a/two.txt", "");
    project.write("b/two.txt", "");
    project.write(".gitignore", "two.txt\n");
    tree.refresh_all();
    assert_eq!(
        outline(&tree),
        [
            "v ROOT",
            "  v a",
            "    - one.txt",
            "    - two.txt [ignored]",
            "  v b",
            "*    - one.txt",
            "    - two.txt [ignored]",
            "  - .gitignore",
        ]
    );
}

#[test]
fn creating_files_and_directories() {
    let project = Project::with_files(&["src/main.rs"]);
    let mut tree = FileTree::new(project.root());

    let created = tree.create_file(project.root(), "notes.txt").unwrap();
    assert_eq!(created, project.path("notes.txt"));
    assert_eq!(project.read("notes.txt"), "");
    assert_eq!(outline(&tree), ["v ROOT", "  > src", "*  - notes.txt"]);

    // Slashes create the directories in between, and the new file is revealed.
    let nested = tree
        .create_file(&project.path("src"), "ui/widgets/button.rs")
        .unwrap();
    assert_eq!(nested, project.path("src/ui/widgets/button.rs"));
    assert_eq!(
        outline(&tree),
        [
            "v ROOT",
            "  v src",
            "    v ui",
            "      v widgets",
            "*        - button.rs",
            "    - main.rs",
            "  - notes.txt",
        ]
    );

    // A trailing slash asks for a directory.
    let dir = tree.create_file(project.root(), "assets/icons/").unwrap();
    assert!(dir.is_dir());
    assert_eq!(selected_name(&tree).as_deref(), Some("icons"));

    let made = tree.create_dir(project.root(), "docs/guide").unwrap();
    assert!(made.is_dir());
    assert_eq!(tree.selected_path(), Some(made.as_path()));

    // Relative directories are taken from the root.
    let relative = tree.create_file(Path::new("docs"), "index.md").unwrap();
    assert_eq!(relative, project.path("docs/index.md"));
}

#[test]
fn creating_never_overwrites_and_rejects_bad_names() {
    let project = Project::with_files(&["existing.txt", "dir/x"]);
    project.write("existing.txt", "precious");
    let mut tree = FileTree::new(project.root());
    let before = outline(&tree);

    assert!(matches!(
        tree.create_file(project.root(), "existing.txt"),
        Err(FileOpError::AlreadyExists(path)) if path == project.path("existing.txt")
    ));
    assert_eq!(project.read("existing.txt"), "precious");
    assert!(matches!(
        tree.create_dir(project.root(), "dir"),
        Err(FileOpError::AlreadyExists(_))
    ));
    assert!(matches!(
        tree.create_file(project.root(), "dir"),
        Err(FileOpError::AlreadyExists(_))
    ));
    for bad in [
        "",
        "  ",
        "../escape.txt",
        "a/../../b",
        "/abs.txt",
        ".",
        "x/./y",
    ] {
        assert!(
            matches!(
                tree.create_file(project.root(), bad),
                Err(FileOpError::InvalidName(_))
            ),
            "{bad:?}"
        );
        assert!(
            matches!(
                tree.create_dir(project.root(), bad),
                Err(FileOpError::InvalidName(_))
            ),
            "{bad:?}"
        );
    }
    // A file in the way of a directory.
    assert!(matches!(
        tree.create_file(project.root(), "existing.txt/child.txt"),
        Err(FileOpError::NotADirectory(_) | FileOpError::AlreadyExists(_) | FileOpError::Io { .. })
    ));
    assert!(matches!(
        tree.create_file(&project.path("existing.txt"), "child.txt"),
        Err(FileOpError::NotADirectory(_))
    ));
    assert!(matches!(
        tree.create_file(&project.path("no-such-dir"), "child.txt"),
        Err(FileOpError::NotFound(_))
    ));
    assert!(matches!(
        tree.create_file(Path::new("/tmp"), "outside.txt"),
        Err(FileOpError::OutsideRoot(_))
    ));
    assert_eq!(outline(&tree), before);
    assert_eq!(project.listing(), ["dir/", "dir/x", "existing.txt"]);
}

#[test]
fn renaming_keeps_expansion_and_refuses_to_overwrite() {
    let project =
        Project::with_files(&["old/inner/file.txt", "old/top.txt", "other.txt", "taken/x"]);
    project.write("other.txt", "other contents");
    let mut tree = FileTree::new(project.root());
    tree.expand(&project.path("old"));
    tree.expand(&project.path("old/inner"));

    let renamed = tree.rename(&project.path("old"), "new").unwrap();
    assert_eq!(renamed, project.path("new"));
    assert_eq!(
        outline(&tree),
        [
            "v ROOT",
            "*  v new",
            "    v inner",
            "      - file.txt",
            "    - top.txt",
            "  > taken",
            "  - other.txt",
        ]
    );
    assert!(!project.exists("old"));

    // Refuses to overwrite a file or a directory, and changes nothing.
    assert!(matches!(
        tree.rename(&project.path("new/top.txt"), "../other.txt"),
        Err(FileOpError::InvalidName(_))
    ));
    assert!(matches!(
        tree.rename(&project.path("other.txt"), "taken"),
        Err(FileOpError::AlreadyExists(path)) if path == project.path("taken")
    ));
    assert!(matches!(
        tree.rename(&project.path("new"), "taken"),
        Err(FileOpError::AlreadyExists(_))
    ));
    project.write("clash.txt", "clash contents");
    assert!(matches!(
        tree.rename(&project.path("other.txt"), "clash.txt"),
        Err(FileOpError::AlreadyExists(_))
    ));
    assert_eq!(project.read("other.txt"), "other contents");
    assert_eq!(project.read("clash.txt"), "clash contents");
    assert!(project.exists("taken/x"));

    // Renaming to the same name is a no-op; a missing source is an error.
    assert_eq!(
        tree.rename(&project.path("other.txt"), "other.txt")
            .unwrap(),
        project.path("other.txt")
    );
    assert!(matches!(
        tree.rename(&project.path("ghost.txt"), "x.txt"),
        Err(FileOpError::NotFound(_))
    ));
}

#[test]
fn renaming_with_a_slash_moves_into_a_subdirectory() {
    let project = Project::with_files(&["notes.txt", "dir/x"]);
    let mut tree = FileTree::new(project.root());
    let moved = tree
        .rename(&project.path("notes.txt"), "archive/2026/notes.md")
        .unwrap();
    assert_eq!(moved, project.path("archive/2026/notes.md"));
    assert!(!project.exists("notes.txt"));
    assert_eq!(tree.selected_path(), Some(moved.as_path()));

    assert!(matches!(
        tree.rename(&project.path("dir"), "dir/inside-itself"),
        Err(FileOpError::IntoItself(_))
    ));
    assert!(project.exists("dir/x"));
}

#[test]
fn renaming_only_the_case_of_a_name_works() {
    let project = Project::with_files(&["readme.md"]);
    let mut tree = FileTree::new(project.root());
    let renamed = tree
        .rename(&project.path("readme.md"), "README.md")
        .unwrap();
    assert_eq!(renamed, project.path("README.md"));
    assert_eq!(names(&tree), ["README.md"]);
    assert_eq!(project.listing(), ["README.md"]);
}

#[test]
fn the_root_cannot_be_renamed_moved_or_deleted() {
    let project = Project::with_files(&["dir/a.txt"]);
    let mut tree = FileTree::new(project.root());
    let root = project.root().to_path_buf();
    assert!(matches!(
        tree.rename(&root, "other"),
        Err(FileOpError::IsRoot)
    ));
    assert!(matches!(tree.trash(&root), Err(FileOpError::IsRoot)));
    assert!(matches!(tree.delete(&root), Err(FileOpError::IsRoot)));
    assert!(matches!(tree.duplicate(&root), Err(FileOpError::IsRoot)));
    assert!(matches!(
        tree.move_into(&root, &project.path("dir")),
        Err(FileOpError::IsRoot)
    ));
    assert!(matches!(
        tree.delete(Path::new("/etc/hosts")),
        Err(FileOpError::OutsideRoot(_))
    ));
    assert!(matches!(
        tree.move_into(&project.path("dir/a.txt"), Path::new("/tmp")),
        Err(FileOpError::OutsideRoot(_))
    ));
    assert!(project.exists("dir/a.txt"));
}

#[test]
fn duplicating_counts_up_and_copies_whole_directories() {
    let project = Project::with_files(&["notes.txt", "pkg/src/lib.rs", "pkg/Cargo.toml", ".env"]);
    project.write("notes.txt", "original");
    let mut tree = FileTree::new(project.root());

    let first = tree.duplicate(&project.path("notes.txt")).unwrap();
    assert_eq!(first, project.path("notes copy.txt"));
    assert_eq!(project.read("notes copy.txt"), "original");
    assert_eq!(selected_name(&tree).as_deref(), Some("notes copy.txt"));
    let second = tree.duplicate(&project.path("notes.txt")).unwrap();
    assert_eq!(second, project.path("notes copy 2.txt"));
    let third = tree.duplicate(&project.path("notes.txt")).unwrap();
    assert_eq!(third, project.path("notes copy 3.txt"));
    assert_eq!(project.read("notes.txt"), "original");

    let dotfile = tree.duplicate(&project.path(".env")).unwrap();
    assert_eq!(dotfile, project.path(".env copy"));

    let dir = tree.duplicate(&project.path("pkg")).unwrap();
    assert_eq!(dir, project.path("pkg copy"));
    assert_eq!(project.read("pkg copy/src/lib.rs"), "pkg/src/lib.rs");
    assert_eq!(project.read("pkg copy/Cargo.toml"), "pkg/Cargo.toml");
    assert_eq!(selected_name(&tree).as_deref(), Some("pkg copy"));
    // No temporary files are left behind.
    assert!(project
        .listing()
        .iter()
        .all(|path| !path.contains(".bakedit-tmp-")));
}

#[cfg(unix)]
#[test]
fn copies_keep_links_and_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let project = Project::with_files(&["pkg/run.sh", "pkg/data.txt"]);
    fs::set_permissions(
        project.path("pkg/run.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    project.symlink("data.txt", "pkg/link");
    project.symlink("missing", "pkg/dangling");
    let mut tree = FileTree::new(project.root());

    let copy = tree.duplicate(&project.path("pkg")).unwrap();
    let mode = fs::metadata(copy.join("run.sh"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o755);
    assert_eq!(
        fs::read_link(copy.join("link")).unwrap(),
        PathBuf::from("data.txt")
    );
    assert_eq!(
        fs::read_link(copy.join("dangling")).unwrap(),
        PathBuf::from("missing")
    );
    assert!(fs::symlink_metadata(copy.join("link"))
        .unwrap()
        .file_type()
        .is_symlink());
}

/// A stand-in for the system trash: moves entries into a directory of its own.
fn fake_trash(tree: &mut FileTree, bin: PathBuf) -> Arc<Mutex<Vec<PathBuf>>> {
    let trashed = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&trashed);
    fs::create_dir_all(&bin).unwrap();
    tree.set_trash_handler(move |path| {
        let name = path.file_name().expect("a named entry");
        fs::rename(path, bin.join(name)).map_err(|source| FileOpError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        record.lock().unwrap().push(path.to_path_buf());
        Ok(())
    });
    trashed
}

#[test]
fn trashing_removes_the_entry_and_moves_the_selection() {
    let project = Project::with_files(&["a.txt", "b.txt", "dir/inner.txt"]);
    let bin = tempfile::tempdir().unwrap();
    let mut tree = FileTree::new(project.root());
    let trashed = fake_trash(&mut tree, bin.path().to_path_buf());
    tree.expand(&project.path("dir"));

    tree.select(tree.index_of(&project.path("a.txt")).unwrap());
    tree.trash(&project.path("a.txt")).unwrap();
    assert_eq!(
        outline(&tree),
        ["v ROOT", "  v dir", "    - inner.txt", "*  - b.txt"]
    );
    assert!(
        bin.path().join("a.txt").exists(),
        "the file went to the trash, not away"
    );

    tree.trash(&project.path("dir")).unwrap();
    assert_eq!(outline(&tree), ["v ROOT", "*  - b.txt"]);
    assert!(bin.path().join("dir/inner.txt").exists());
    assert_eq!(
        *trashed.lock().unwrap(),
        [project.path("a.txt"), project.path("dir")]
    );
    assert_eq!(tree.expanded_paths(), [project.root().to_path_buf()]);

    assert!(matches!(
        tree.trash(&project.path("ghost.txt")),
        Err(FileOpError::NotFound(_) | FileOpError::Io { .. })
    ));
}

#[test]
fn a_failed_trash_leaves_everything_in_place() {
    let project = Project::with_files(&["a.txt"]);
    let mut tree = FileTree::new(project.root());
    tree.set_trash_handler(|path| {
        Err(FileOpError::Trash {
            path: path.to_path_buf(),
            message: "the trash is full".to_string(),
        })
    });
    let error = tree.trash(&project.path("a.txt")).unwrap_err();
    assert!(error.to_string().contains("the trash is full"), "{error}");
    assert!(project.exists("a.txt"));
    assert_eq!(names(&tree), ["a.txt"]);
}

#[test]
fn deleting_is_permanent_and_never_follows_links() {
    let project = Project::with_files(&["file.txt", "dir/a/b.txt", "keep/precious.txt"]);
    let mut tree = FileTree::new(project.root());
    tree.expand(&project.path("dir"));

    tree.delete(&project.path("file.txt")).unwrap();
    tree.delete(&project.path("dir")).unwrap();
    assert_eq!(project.listing(), ["keep/", "keep/precious.txt"]);
    assert_eq!(outline(&tree), ["v ROOT", "  > keep"]);

    #[cfg(unix)]
    {
        project.symlink("keep", "link-to-keep");
        tree.refresh_all();
        tree.delete(&project.path("link-to-keep")).unwrap();
        assert!(!project.exists("link-to-keep"));
        assert!(
            project.exists("keep/precious.txt"),
            "only the link was deleted"
        );
    }

    assert!(matches!(
        tree.delete(&project.path("gone.txt")),
        Err(FileOpError::NotFound(_))
    ));
}

#[test]
fn moving_into_a_directory() {
    let project = Project::with_files(&["a.txt", "from/deep/x.txt", "into/keep.txt"]);
    project.write("into/a-clash.txt", "resident");
    let mut tree = FileTree::new(project.root());
    tree.expand(&project.path("from"));
    tree.expand(&project.path("from/deep"));

    let moved = tree
        .move_into(&project.path("a.txt"), &project.path("into"))
        .unwrap();
    assert_eq!(moved, project.path("into/a.txt"));
    assert!(!project.exists("a.txt"));
    assert_eq!(tree.selected_path(), Some(moved.as_path()));
    assert!(
        tree.is_expanded(&project.path("into")),
        "the target is opened to show it"
    );

    // A directory moves with its contents and stays expanded.
    let moved_dir = tree
        .move_into(&project.path("from/deep"), &project.path("into"))
        .unwrap();
    assert_eq!(moved_dir, project.path("into/deep"));
    assert!(tree.is_expanded(&project.path("into/deep")));
    assert!(tree.index_of(&project.path("into/deep/x.txt")).is_some());
    assert!(!tree.is_expanded(&project.path("from/deep")));

    // Moving where it already is does nothing.
    assert_eq!(
        tree.move_into(&project.path("into/a.txt"), &project.path("into"))
            .unwrap(),
        project.path("into/a.txt")
    );
}

#[test]
fn moving_refuses_to_overwrite_or_to_swallow_itself() {
    let project = Project::with_files(&["a.txt", "into/a.txt", "dir/sub/x.txt", "file.txt"]);
    project.write("a.txt", "mine");
    project.write("into/a.txt", "theirs");
    let mut tree = FileTree::new(project.root());

    assert!(matches!(
        tree.move_into(&project.path("a.txt"), &project.path("into")),
        Err(FileOpError::AlreadyExists(path)) if path == project.path("into/a.txt")
    ));
    assert_eq!(project.read("a.txt"), "mine");
    assert_eq!(project.read("into/a.txt"), "theirs");

    assert!(matches!(
        tree.move_into(&project.path("dir"), &project.path("dir/sub")),
        Err(FileOpError::IntoItself(_))
    ));
    assert!(matches!(
        tree.move_into(&project.path("dir"), &project.path("dir")),
        Err(FileOpError::IntoItself(_))
    ));
    assert!(matches!(
        tree.move_into(&project.path("a.txt"), &project.path("file.txt")),
        Err(FileOpError::NotADirectory(_))
    ));
    assert!(matches!(
        tree.move_into(&project.path("ghost"), &project.path("into")),
        Err(FileOpError::NotFound(_))
    ));
    assert!(project.exists("dir/sub/x.txt"));
}

#[test]
fn copying_into_a_directory_picks_a_free_name() {
    let project = Project::with_files(&["a.txt", "into/a.txt", "dir/sub/x.txt"]);
    project.write("a.txt", "mine");
    project.write("into/a.txt", "theirs");
    let mut tree = FileTree::new(project.root());

    let copy = tree
        .copy_into(&project.path("a.txt"), &project.path("into"))
        .unwrap();
    assert_eq!(copy, project.path("into/a copy.txt"));
    assert_eq!(project.read("into/a copy.txt"), "mine");
    assert_eq!(project.read("into/a.txt"), "theirs");
    assert_eq!(project.read("a.txt"), "mine", "the source stays");
    assert_eq!(tree.selected_path(), Some(copy.as_path()));

    let plain = tree
        .copy_into(&project.path("dir"), &project.path("into"))
        .unwrap();
    assert_eq!(plain, project.path("into/dir"));
    assert_eq!(project.read("into/dir/sub/x.txt"), "dir/sub/x.txt");

    assert!(matches!(
        tree.copy_into(&project.path("dir"), &project.path("dir/sub")),
        Err(FileOpError::IntoItself(_))
    ));
    assert!(!project.exists("dir/sub/dir"));
}

#[test]
fn cut_and_paste_moves_entries() {
    let project = Project::with_files(&["a.txt", "b/inner.txt", "dest/a.txt"]);
    project.write("a.txt", "moved");
    project.write("dest/a.txt", "resident");
    let mut tree = FileTree::new(project.root());
    tree.expand(&project.path("b"));

    assert!(
        tree.paste(&project.path("dest")).unwrap().is_empty(),
        "nothing to paste yet"
    );

    tree.cut(&[project.path("a.txt"), project.path("b")]);
    let clipboard = tree.clipboard().unwrap();
    assert!(clipboard.cut);
    assert_eq!(clipboard.paths, [project.path("a.txt"), project.path("b")]);

    let pasted = tree.paste(&project.path("dest")).unwrap();
    assert_eq!(
        pasted,
        [project.path("dest/a copy.txt"), project.path("dest/b")]
    );
    assert_eq!(project.read("dest/a copy.txt"), "moved");
    assert_eq!(
        project.read("dest/a.txt"),
        "resident",
        "nothing is overwritten"
    );
    assert!(!project.exists("a.txt") && !project.exists("b"));
    assert!(tree.clipboard().is_none(), "a cut is pasted once");
    assert_eq!(
        tree.selected_path(),
        Some(project.path("dest/a copy.txt").as_path())
    );
    assert!(
        tree.is_expanded(&project.path("dest/b")),
        "expansion travels with the move"
    );
    assert_eq!(
        outline(&tree),
        [
            "v ROOT",
            "  v dest",
            "    v b",
            "      - inner.txt",
            "    - a.txt",
            "*    - a copy.txt",
        ]
    );
}

#[test]
fn copy_and_paste_copies_and_can_repeat() {
    let project = Project::with_files(&["a.txt", "dest/placeholder"]);
    let mut tree = FileTree::new(project.root());
    tree.select(tree.index_of(&project.path("a.txt")).unwrap());
    assert!(tree.copy_selected());
    assert!(!tree.clipboard().unwrap().cut);

    // Pasting onto a file pastes next to it.
    let pasted = tree.paste(&project.path("dest/placeholder")).unwrap();
    assert_eq!(pasted, [project.path("dest/a.txt")]);
    let again = tree.paste(&project.path("dest")).unwrap();
    assert_eq!(again, [project.path("dest/a copy.txt")]);
    assert!(tree.clipboard().is_some(), "a copy can be pasted again");
    assert!(project.exists("a.txt"));

    // Pasting at the selection: the selected file's directory.
    assert_eq!(
        tree.selected_path(),
        Some(project.path("dest/a copy.txt").as_path())
    );
    assert_eq!(tree.creation_dir(), project.path("dest"));
    let third = tree.paste_at_selection().unwrap();
    assert_eq!(third, [project.path("dest/a copy 2.txt")]);

    tree.clear_clipboard();
    assert!(tree.clipboard().is_none());
    assert!(tree.paste(&project.path("dest")).unwrap().is_empty());
}

#[test]
fn the_clipboard_only_takes_sensible_entries() {
    let project = Project::with_files(&["dir/inner.txt", "a.txt"]);
    let mut tree = FileTree::new(project.root());

    // The root, outside paths and duplicates are dropped; a child of a listed
    // directory travels with it.
    tree.copy(&[
        project.root().to_path_buf(),
        PathBuf::from("/etc/hosts"),
        project.path("dir"),
        project.path("dir/inner.txt"),
        project.path("a.txt"),
        project.path("a.txt"),
    ]);
    assert_eq!(
        tree.clipboard().unwrap().paths,
        [project.path("dir"), project.path("a.txt")]
    );
    tree.cut(&[project.root().to_path_buf()]);
    assert!(
        tree.clipboard().is_none(),
        "nothing sensible clears the clipboard"
    );

    // The selection helpers refuse the root and an empty selection.
    assert!(!tree.cut_selected());
    tree.select_first();
    assert!(!tree.copy_selected());
    tree.select_next();
    assert!(tree.cut_selected());
    assert_eq!(tree.clipboard().unwrap().paths, [project.path("dir")]);
}

#[test]
fn pasting_a_cut_entry_where_it_is_keeps_it() {
    let project = Project::with_files(&["dir/a.txt"]);
    let mut tree = FileTree::new(project.root());
    tree.cut(&[project.path("dir/a.txt")]);
    let pasted = tree.paste(&project.path("dir")).unwrap();
    assert_eq!(pasted, [project.path("dir/a.txt")]);
    assert_eq!(project.listing(), ["dir/", "dir/a.txt"]);
}

#[test]
fn a_failed_paste_keeps_what_is_left_on_the_clipboard() {
    let project = Project::with_files(&["one.txt", "dir/sub/x", "three.txt", "dest/keep"]);
    let mut tree = FileTree::new(project.root());
    // The second entry cannot be moved into its own subdirectory.
    tree.cut(&[
        project.path("one.txt"),
        project.path("dir"),
        project.path("three.txt"),
    ]);
    let error = tree.paste(&project.path("dir/sub")).unwrap_err();
    assert!(matches!(error, FileOpError::IntoItself(_)));

    assert!(
        project.exists("dir/sub/one.txt"),
        "the entry before the failure was moved"
    );
    assert!(
        project.exists("three.txt"),
        "the entry after it was not touched"
    );
    assert_eq!(
        tree.clipboard().unwrap().paths,
        [project.path("dir"), project.path("three.txt")]
    );
    assert!(tree.index_of(&project.path("dir/sub/one.txt")).is_some());
}

#[test]
fn renaming_and_deleting_keep_the_clipboard_in_step() {
    let project = Project::with_files(&["dir/a.txt", "b.txt"]);
    let mut tree = FileTree::new(project.root());
    tree.copy(&[project.path("dir/a.txt"), project.path("b.txt")]);

    tree.rename(&project.path("dir"), "renamed").unwrap();
    assert_eq!(
        tree.clipboard().unwrap().paths,
        [project.path("renamed/a.txt"), project.path("b.txt")]
    );
    tree.delete(&project.path("renamed")).unwrap();
    assert_eq!(tree.clipboard().unwrap().paths, [project.path("b.txt")]);
    tree.delete(&project.path("b.txt")).unwrap();
    assert!(tree.clipboard().is_none());
}

#[test]
fn creation_dir_follows_the_selection() {
    let project = Project::with_files(&["dir/a.txt", "top.txt"]);
    let mut tree = FileTree::new(project.root());
    assert_eq!(tree.creation_dir(), project.root());
    tree.select(tree.index_of(&project.path("dir")).unwrap());
    assert_eq!(tree.creation_dir(), project.path("dir"));
    tree.select(tree.index_of(&project.path("top.txt")).unwrap());
    assert_eq!(tree.creation_dir(), project.root());
    tree.reveal(&project.path("dir/a.txt"));
    assert_eq!(tree.creation_dir(), project.path("dir"));
}

#[test]
fn a_large_directory_loads_in_one_go() {
    let project = Project::new();
    for n in 0..2_000 {
        project.write(&format!("many/file{n}.txt"), "");
    }
    let mut tree = FileTree::new(project.root());
    tree.expand(&project.path("many"));
    assert_eq!(tree.rows().len(), 2_002);
    assert_eq!(tree.rows()[2].name, "file0.txt");
    assert_eq!(tree.rows()[3].name, "file1.txt");
    assert_eq!(tree.rows()[2_001].name, "file1999.txt");
}
