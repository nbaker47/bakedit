//! File names with accents on macOS, where the file system keeps the decomposed form an
//! application created ("u" plus a combining diaeresis) and git composes it ("ü").

#![cfg(target_os = "macos")]

mod common;

use std::path::{Path, PathBuf};

use bakedit_git::{CommitOptions, DiffTarget, FileState, HunkAction, Repo};
use common::TestRepo;

/// The name as an application would create it, and as git spells it.
const DECOMPOSED: &str = "u\u{308}ber/re\u{301}sume\u{301}.txt";
const COMPOSED: &str = "\u{fc}ber/r\u{e9}sum\u{e9}.txt";

/// Whether the file system kept the name decomposed. One that normalizes names itself
/// would make these tests pointless.
fn keeps_decomposed_names(test: &TestRepo) -> bool {
    std::fs::read_dir(test.root())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .any(|entry| entry.file_name().to_str() == Some("u\u{308}ber"))
}

fn numbered(count: usize) -> String {
    (1..=count).map(|line| format!("line {line}\n")).collect()
}

#[test]
fn a_decomposed_path_from_the_file_system_means_the_file_git_knows_composed() {
    let test = TestRepo::with_commit();
    test.write(DECOMPOSED, numbered(30));
    if !keeps_decomposed_names(&test) {
        return;
    }

    // Git reports the composed name.
    let status = test.repo.status().unwrap();
    assert_eq!(status.entries.len(), 1);
    assert_eq!(status.entries[0].path, Path::new(COMPOSED));
    assert_ne!(Path::new(DECOMPOSED), Path::new(COMPOSED));

    // The path the file system gives is recognised as the same file.
    assert_eq!(
        test.repo.relative_path(test.path(DECOMPOSED)).unwrap(),
        Path::new(COMPOSED)
    );
    assert_eq!(
        test.repo.relative_path(DECOMPOSED).unwrap(),
        Path::new(COMPOSED)
    );

    // Staging it by that path really stages it.
    test.repo.stage(&[DECOMPOSED]).unwrap();
    let status = test.repo.status().unwrap();
    assert_eq!(status.entry(COMPOSED).unwrap().index, FileState::Added);
    assert_eq!(
        test.repo.index_text(DECOMPOSED).unwrap(),
        Some(numbered(30))
    );

    test.repo.unstage(&[test.path(DECOMPOSED)]).unwrap();
    assert!(
        test.repo
            .status()
            .unwrap()
            .entry(COMPOSED)
            .unwrap()
            .untracked
    );

    test.repo.stage(&[test.path(DECOMPOSED)]).unwrap();
    test.repo
        .commit("add a file with accents", CommitOptions::default())
        .unwrap();
    assert!(test.repo.status().unwrap().is_clean());
    assert_eq!(test.repo.head_text(DECOMPOSED).unwrap(), Some(numbered(30)));
    assert_eq!(
        test.repo
            .log(10, Some(Path::new(DECOMPOSED)))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn diffs_hunks_restore_and_permalinks_take_the_decomposed_path() {
    let test = TestRepo::with_commit();
    test.write(DECOMPOSED, numbered(30));
    if !keeps_decomposed_names(&test) {
        return;
    }
    test.commit_all("base");
    let changed = numbered(30)
        .replace("line 2\n", "line two\n")
        .replace("line 29\n", "line twenty-nine\n");
    test.write(DECOMPOSED, &changed);

    for path in [PathBuf::from(DECOMPOSED), test.path(DECOMPOSED)] {
        let diff = test
            .repo
            .diff_file(&path, DiffTarget::IndexToWorktree)
            .unwrap()
            .expect("the file differs");
        assert_eq!(diff.path, Path::new(COMPOSED));
        assert_eq!(diff.hunks.len(), 2);
    }

    let diff = test
        .repo
        .diff_file(DECOMPOSED, DiffTarget::HeadToWorktree)
        .unwrap()
        .unwrap();
    test.repo.apply_hunk(&diff, 1, HunkAction::Stage).unwrap();
    assert_eq!(
        test.repo.index_text(DECOMPOSED).unwrap().unwrap(),
        numbered(30).replace("line 29\n", "line twenty-nine\n")
    );

    let outcome = test.repo.restore(&[DECOMPOSED]).unwrap();
    assert_eq!(outcome.restored, [PathBuf::from(COMPOSED)]);
    assert_eq!(
        test.read(DECOMPOSED),
        numbered(30).replace("line 29\n", "line twenty-nine\n")
    );

    test.git(&["remote", "add", "origin", "git@github.com:owner/repo.git"]);
    let link = test.repo.permalink(test.path(DECOMPOSED), 1, 2).unwrap();
    assert!(
        link.ends_with("/%C3%BCber/r%C3%A9sum%C3%A9.txt#L1-L2"),
        "{link}"
    );
}

#[test]
fn an_untracked_file_with_a_decomposed_name_is_diffed_and_staged_by_its_hunk() {
    let test = TestRepo::with_commit();
    test.write(DECOMPOSED, "new\n");
    if !keeps_decomposed_names(&test) {
        return;
    }
    let diff = test
        .repo
        .diff_file(test.path(DECOMPOSED), DiffTarget::HeadToWorktree)
        .unwrap()
        .expect("the untracked file is found");
    assert_eq!(diff.path, Path::new(COMPOSED));
    assert_eq!(diff.additions(), 1);
    test.repo.apply_hunk(&diff, 0, HunkAction::Stage).unwrap();
    assert!(test
        .repo
        .status()
        .unwrap()
        .entry(COMPOSED)
        .unwrap()
        .is_fully_staged());
}

#[test]
fn names_are_left_alone_when_git_does_not_compose_them() {
    let test = TestRepo::with_commit();
    test.git(&["config", "core.precomposeUnicode", "false"]);
    test.write(DECOMPOSED, "content\n");
    if !keeps_decomposed_names(&test) {
        return;
    }
    // The setting is read when the repository is opened.
    let repo = Repo::discover(test.root()).unwrap();

    let status = repo.status().unwrap();
    assert_eq!(status.entries[0].path, Path::new(DECOMPOSED));
    assert_eq!(
        repo.relative_path(test.path(DECOMPOSED)).unwrap(),
        Path::new(DECOMPOSED)
    );
    repo.stage(&[DECOMPOSED]).unwrap();
    assert_eq!(
        repo.status().unwrap().entry(DECOMPOSED).unwrap().index,
        FileState::Added
    );
}
