//! Finding repositories: plain clones, linked work trees, submodules, and the places
//! that are not a work tree at all.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use bakedit_git::{GitError, Repo};
use common::{git_in, init_repository, isolate_git, TestRepo};

#[test]
fn discovers_from_the_root_a_subdirectory_and_a_file() {
    let test = TestRepo::with_commit();
    test.write("src/deep/module.rs", "fn main() {}\n");

    for start in [
        test.root().to_path_buf(),
        test.path("src"),
        test.path("src/deep"),
        test.path("src/deep/module.rs"),
        test.path("src/deep/not-written-yet.rs"),
        test.path("no/such/directory/yet/file.rs"),
    ] {
        let repo = Repo::discover(&start).unwrap_or_else(|| panic!("{start:?} is in the repo"));
        assert_eq!(repo.root(), test.root(), "from {start:?}");
        assert_eq!(repo.git_dir(), test.path(".git"), "from {start:?}");
        assert_eq!(repo.common_dir(), test.path(".git"), "from {start:?}");
    }
}

#[test]
fn discovers_a_repository_without_commits() {
    let test = TestRepo::new();
    assert_eq!(test.repo.root(), test.root());
    assert_eq!(test.repo.head_sha().unwrap(), None);
    assert_eq!(
        test.repo.current_branch().unwrap(),
        Some("main".to_string())
    );
}

#[test]
fn places_outside_a_work_tree_are_not_repositories() {
    let test = TestRepo::with_commit();

    // The directory above the work tree is no repository.
    assert_eq!(Repo::discover(test.sandbox()), None);
    // The inside of the git directory is not a work tree.
    assert_eq!(Repo::discover(test.path(".git")), None);
    assert_eq!(Repo::discover(test.path(".git/objects")), None);

    // Neither is a bare repository.
    let bare = test.sandbox().join("bare.git");
    fs::create_dir_all(&bare).unwrap();
    git_in(&bare, &["init", "--quiet", "--bare"]);
    assert_eq!(Repo::discover(&bare), None);

    // A path with nothing existing above it but the file system root.
    isolate_git();
    assert_eq!(
        Repo::discover("/nonexistent-bakedit-git-test/file.txt"),
        None
    );
}

#[test]
fn a_linked_work_tree_is_its_own_repo_with_a_shared_git_directory() {
    let test = TestRepo::with_commit();
    let linked = test.sandbox().join("linked");
    test.git(&[
        "worktree",
        "add",
        "--quiet",
        "-b",
        "feature",
        linked.to_str().unwrap(),
    ]);
    let linked = linked.canonicalize().unwrap();
    fs::create_dir_all(linked.join("nested")).unwrap();

    let repo = Repo::discover(linked.join("nested")).expect("the linked work tree is found");
    assert_eq!(repo.root(), linked);
    assert_eq!(repo.git_dir(), test.path(".git/worktrees/linked"));
    assert_eq!(repo.common_dir(), test.path(".git"));
    assert_eq!(repo.current_branch().unwrap(), Some("feature".to_string()));
    assert_eq!(repo.head_sha().unwrap(), Some(test.head()));

    // The main work tree is unaffected.
    assert_eq!(
        test.repo.current_branch().unwrap(),
        Some("main".to_string())
    );
}

#[test]
fn a_submodule_is_its_own_repo() {
    let test = TestRepo::with_commit();
    let library = test.sandbox().join("library");
    init_repository(&library);
    fs::write(library.join("lib.txt"), "library\n").unwrap();
    git_in(&library, &["add", "--all"]);
    git_in(&library, &["commit", "--quiet", "--message", "library"]);
    let library_head = git_in(&library, &["rev-parse", "HEAD"]);

    test.git(&[
        "-c",
        "protocol.file.allow=always",
        "submodule",
        "--quiet",
        "add",
        library.to_str().unwrap(),
        "vendor/library",
    ]);

    let inside = Repo::discover(test.path("vendor/library/lib.txt")).expect("the submodule");
    assert_eq!(inside.root(), test.path("vendor/library"));
    assert_eq!(inside.git_dir(), test.path(".git/modules/vendor/library"));
    assert_eq!(
        inside.head_sha().unwrap(),
        Some(library_head.trim().to_string())
    );

    // From the superproject's own files the superproject is found.
    let outer = Repo::discover(test.path("README.md")).unwrap();
    assert_eq!(outer.root(), test.root());
}

#[test]
fn paths_through_a_symbolic_link_are_resolved() {
    let test = TestRepo::with_commit();
    test.write("src/lib.rs", "pub fn lib() {}\n");
    let link = test.sandbox().join("link-to-work");
    std::os::unix::fs::symlink(test.root(), &link).unwrap();

    let repo = Repo::discover(link.join("src")).expect("found through the link");
    assert_eq!(repo.root(), test.root());

    let relative = |path: PathBuf| repo.relative_path(path).unwrap();
    assert_eq!(relative(link.join("src/lib.rs")), Path::new("src/lib.rs"));
    // A file that does not exist (deleted, or not saved yet) resolves as well.
    assert_eq!(
        relative(link.join("src/gone/deleted.rs")),
        Path::new("src/gone/deleted.rs")
    );
    assert_eq!(relative(link.clone()), Path::new(""));
    assert_eq!(relative(test.path("README.md")), Path::new("README.md"));

    match repo.relative_path(test.sandbox().join("elsewhere.txt")) {
        Err(GitError::PathOutsideRepo(_)) => {}
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn a_path_spelled_in_another_case_is_resolved_where_the_file_system_ignores_case() {
    let test = TestRepo::with_commit();
    test.write("src/Lib.rs", "pub fn lib() {}\n");
    let shouting = PathBuf::from(test.root().to_str().unwrap().to_uppercase());
    // On a file system that tells cases apart this other spelling is another path.
    if !shouting.join("README.md").exists() {
        return;
    }

    let repo = Repo::discover(&shouting).expect("found whatever the spelling");
    assert_eq!(repo.root(), test.root());
    // The part above the work tree is put right; the part inside it is the caller's.
    assert_eq!(
        repo.relative_path(shouting.join("src/Lib.rs")).unwrap(),
        Path::new("src/Lib.rs")
    );
    assert_eq!(
        repo.relative_path(shouting.join("src/not-there-yet.rs"))
            .unwrap(),
        Path::new("src/not-there-yet.rs")
    );
}

#[test]
fn a_repo_handle_can_be_used_from_other_threads() {
    let test = TestRepo::with_commit();
    let expected = test.head();
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let repo = test.repo.clone();
            std::thread::spawn(move || repo.head_sha().unwrap())
        })
        .collect();
    for handle in handles {
        assert_eq!(handle.join().unwrap(), Some(expected.clone()));
    }
}
