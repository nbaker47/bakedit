//! A linked work tree is a repository like any other: everything works in it and nothing
//! leaks into the main work tree.

mod common;

use std::fs;
use std::path::PathBuf;

use bakedit_git::{CommitOptions, DiffTarget, HunkAction, Repo};
use common::TestRepo;

/// A repository with a linked work tree on branch `feature` next to it.
fn with_linked_tree() -> (TestRepo, PathBuf, Repo) {
    let test = TestRepo::new();
    let base: String = (1..=30).map(|line| format!("line {line}\n")).collect();
    test.write("shared.txt", &base);
    test.commit_all("base");
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
    let repo = Repo::discover(&linked).expect("the linked work tree");
    (test, linked, repo)
}

#[test]
fn staging_committing_and_history_in_a_linked_work_tree() {
    let (test, linked, repo) = with_linked_tree();
    fs::write(linked.join("only-in-feature.txt"), "feature\n").unwrap();
    fs::write(linked.join("shared.txt"), "rewritten in the linked tree\n").unwrap();

    repo.stage(&[linked.join("only-in-feature.txt")]).unwrap();
    let status = repo.status().unwrap();
    assert!(status
        .entry("only-in-feature.txt")
        .unwrap()
        .is_fully_staged());
    assert!(!status.entry("shared.txt").unwrap().has_staged());

    let sha = repo
        .commit("feature work", CommitOptions::default())
        .unwrap();
    assert_eq!(repo.head_sha().unwrap(), Some(sha.clone()));
    let log = repo.log(10, None).unwrap();
    assert_eq!(log[0].subject, "feature work");
    assert_eq!(log.len(), 2);
    assert_eq!(
        repo.head_text("only-in-feature.txt").unwrap().as_deref(),
        Some("feature\n")
    );

    let outcome = repo.restore(&["shared.txt"]).unwrap();
    assert_eq!(outcome.restored, [PathBuf::from("shared.txt")]);
    assert!(repo.status().unwrap().is_clean());

    // The main work tree saw none of it.
    assert_eq!(test.repo.log(10, None).unwrap().len(), 1);
    assert!(test.repo.status().unwrap().is_clean());
    assert!(!test.exists("only-in-feature.txt"));
    assert_ne!(test.head(), sha);
}

#[test]
fn diffs_and_hunks_in_a_linked_work_tree() {
    let (test, linked, repo) = with_linked_tree();
    let base = fs::read_to_string(linked.join("shared.txt")).unwrap();
    let changed = base
        .replace("line 2\n", "line two\n")
        .replace("line 29\n", "line twenty-nine\n");
    fs::write(linked.join("shared.txt"), &changed).unwrap();
    fs::write(linked.join("new.txt"), "new\n").unwrap();

    let files = repo.diff_files(DiffTarget::HeadToWorktree).unwrap();
    assert_eq!(files.len(), 2);
    let diff = repo
        .diff_file(linked.join("shared.txt"), DiffTarget::IndexToWorktree)
        .unwrap()
        .unwrap();
    assert_eq!(diff.hunks.len(), 2);

    repo.apply_hunk(&diff, 1, HunkAction::Stage).unwrap();
    assert_eq!(
        repo.index_text("shared.txt").unwrap().unwrap(),
        base.replace("line 29\n", "line twenty-nine\n")
    );
    repo.apply_hunk(&diff, 0, HunkAction::Discard).unwrap();
    assert_eq!(
        fs::read_to_string(linked.join("shared.txt")).unwrap(),
        base.replace("line 29\n", "line twenty-nine\n")
    );
    let staged = repo
        .diff_file("shared.txt", DiffTarget::HeadToIndex)
        .unwrap()
        .unwrap();
    repo.apply_hunk(&staged, 0, HunkAction::Unstage).unwrap();
    assert_eq!(repo.index_text("shared.txt").unwrap().unwrap(), base);

    // The main work tree has its own index and files.
    assert_eq!(test.read("shared.txt"), base);
    assert!(test.repo.status().unwrap().is_clean());
}

#[test]
fn hooks_and_remotes_are_shared_with_the_main_work_tree() {
    let (test, linked, repo) = with_linked_tree();
    let remote = test.add_origin();
    test.install_hook(
        "pre-commit",
        "#!/bin/sh\necho 'shared hook says no' >&2\nexit 1\n",
    );

    fs::write(linked.join("a.txt"), "a\n").unwrap();
    repo.stage_all().unwrap();
    let error = repo
        .commit("refused", CommitOptions::default())
        .unwrap_err();
    assert!(error.to_string().contains("shared hook says no"), "{error}");

    fs::remove_file(test.path(".git/hooks/pre-commit")).unwrap();
    repo.commit("accepted", CommitOptions::default()).unwrap();
    let output = repo.push(false).unwrap();
    assert!(output.contains("feature"), "{output}");
    assert_eq!(
        common::git_in(&remote, &["rev-parse", "feature"]).trim(),
        repo.head_sha().unwrap().unwrap()
    );
    assert_eq!(
        repo.status().unwrap().upstream.as_deref(),
        Some("origin/feature")
    );
    // Seen from the main work tree: the branch exists, with its upstream, checked out
    // elsewhere.
    let branches = test.repo.branches().unwrap();
    let feature = branches
        .iter()
        .find(|branch| branch.name == "feature")
        .unwrap();
    assert_eq!(feature.upstream.as_deref(), Some("origin/feature"));
    assert_eq!(feature.worktree.as_deref(), Some(linked.as_path()));
}
