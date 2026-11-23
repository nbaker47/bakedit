//! `Repo::status` against real repositories in every state the git panel has to show.

mod common;

use std::path::{Path, PathBuf};

use bakedit_git::{
    ConflictKind, DisplayStatus, FileState, Repo, RepoState, StageState, Status, StatusEntry,
};
use common::{git_in, isolate_git, TestRepo};

fn entry<'a>(status: &'a Status, path: &str) -> &'a StatusEntry {
    status
        .entry(path)
        .unwrap_or_else(|| panic!("no entry for {path} in {:#?}", status.entries))
}

fn paths(status: &Status) -> Vec<String> {
    status
        .entries
        .iter()
        .map(|entry| entry.path.display().to_string())
        .collect()
}

/// A repository where `main` and `other` both changed `conflict.txt`.
fn diverged() -> TestRepo {
    let test = TestRepo::new();
    test.write("conflict.txt", "one\ntwo\nthree\n");
    test.write("ours-only.txt", "ours\n");
    test.commit_all("base");
    test.git(&["checkout", "--quiet", "-b", "other"]);
    test.write("conflict.txt", "one\nTWO from other\nthree\n");
    test.write("theirs-only.txt", "theirs\n");
    test.commit_all("other change");
    test.git(&["checkout", "--quiet", "main"]);
    test.write("conflict.txt", "one\nTWO from main\nthree\n");
    test.commit_all("main change");
    test
}

#[test]
fn a_repository_without_commits() {
    let test = TestRepo::new();
    let status = test.repo.status().unwrap();
    assert_eq!(status.branch.as_deref(), Some("main"));
    assert_eq!(status.head, None);
    assert_eq!(status.head_sha, None);
    assert_eq!(status.upstream, None);
    assert_eq!((status.ahead, status.behind), (0, 0));
    assert_eq!(status.state, None);
    assert!(status.is_clean());

    test.write("first.txt", "hello\n");
    test.write("staged.txt", "staged\n");
    test.git(&["add", "staged.txt"]);
    let status = test.repo.status().unwrap();
    assert_eq!(paths(&status), ["staged.txt", "first.txt"]);
    let staged = entry(&status, "staged.txt");
    assert_eq!(staged.index, FileState::Added);
    assert_eq!(staged.stage_state(), StageState::Staged);
    assert_eq!(staged.display_status(), DisplayStatus::Added);
    assert!(entry(&status, "first.txt").untracked);
}

#[test]
fn a_clean_repository_reports_its_head() {
    let test = TestRepo::with_commit();
    let status = test.repo.status().unwrap();
    let head = test.head();
    assert_eq!(status.branch.as_deref(), Some("main"));
    assert_eq!(status.head_sha.as_deref(), Some(head.as_str()));
    assert_eq!(status.head.as_deref(), Some(&head[..7]));
    assert!(status.is_clean());
    assert_eq!(status.counts().total, 0);
}

#[test]
fn a_detached_head_has_a_commit_but_no_branch() {
    let test = TestRepo::with_commit();
    test.git(&["checkout", "--quiet", "--detach"]);
    let status = test.repo.status().unwrap();
    assert_eq!(status.branch, None);
    assert_eq!(status.head_sha, Some(test.head()));
    assert_eq!(status.upstream, None);
    assert_eq!(test.repo.current_branch().unwrap(), None);
}

#[test]
fn staged_unstaged_and_partially_staged_changes() {
    let test = TestRepo::new();
    for name in [
        "staged.txt",
        "unstaged.txt",
        "partial.txt",
        "deleted.txt",
        "gone.txt",
    ] {
        test.write(name, "original\n");
    }
    test.commit_all("files");

    test.write("staged.txt", "changed\n");
    test.git(&["add", "staged.txt"]);
    test.write("unstaged.txt", "changed\n");
    test.write("partial.txt", "staged part\n");
    test.git(&["add", "partial.txt"]);
    test.write("partial.txt", "staged part\nand more\n");
    test.remove("deleted.txt");
    test.git(&["rm", "--quiet", "gone.txt"]);
    test.write("new.txt", "new\n");

    let status = test.repo.status().unwrap();
    let summary = |path: &str| {
        let entry = entry(&status, path);
        (
            entry.index,
            entry.worktree,
            entry.stage_state(),
            entry.display_status(),
        )
    };
    use DisplayStatus as D;
    use FileState as F;
    use StageState as S;
    assert_eq!(
        summary("staged.txt"),
        (F::Modified, F::Unmodified, S::Staged, D::Modified)
    );
    assert_eq!(
        summary("unstaged.txt"),
        (F::Unmodified, F::Modified, S::Unstaged, D::Modified)
    );
    assert_eq!(
        summary("partial.txt"),
        (F::Modified, F::Modified, S::PartiallyStaged, D::Modified)
    );
    assert_eq!(
        summary("deleted.txt"),
        (F::Unmodified, F::Deleted, S::Unstaged, D::Deleted)
    );
    assert_eq!(
        summary("gone.txt"),
        (F::Deleted, F::Unmodified, S::Staged, D::Deleted)
    );
    assert_eq!(
        summary("new.txt"),
        (F::Unmodified, F::Unmodified, S::Unstaged, D::Untracked)
    );

    let counts = status.counts();
    assert_eq!(
        (
            counts.staged,
            counts.unstaged,
            counts.untracked,
            counts.conflicted,
            counts.total
        ),
        (3, 3, 1, 0, 6)
    );
    assert_eq!(status.staged().len(), 3);
    assert_eq!(status.unstaged().len(), 4);
}

#[test]
fn a_staged_rename_knows_where_it_came_from() {
    let test = TestRepo::new();
    test.write("old name.txt", "line 1\nline 2\nline 3\nline 4\nline 5\n");
    test.commit_all("file");
    test.git(&["mv", "old name.txt", "new name.txt"]);

    let status = test.repo.status().unwrap();
    assert_eq!(paths(&status), ["new name.txt"]);
    let renamed = entry(&status, "new name.txt");
    assert_eq!(
        renamed.orig_path.as_deref(),
        Some(Path::new("old name.txt"))
    );
    assert_eq!(renamed.index, FileState::Renamed);
    assert_eq!(renamed.worktree, FileState::Unmodified);
    assert_eq!(renamed.display_status(), DisplayStatus::Renamed);
    assert!(renamed.is_fully_staged());

    // Edited after the move: still a rename, now only partly staged.
    test.write(
        "new name.txt",
        "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\n",
    );
    let status = test.repo.status().unwrap();
    let renamed = entry(&status, "new name.txt");
    assert_eq!(renamed.worktree, FileState::Modified);
    assert_eq!(renamed.stage_state(), StageState::PartiallyStaged);
    assert_eq!(renamed.display_status(), DisplayStatus::Renamed);
}

#[test]
fn untracked_directories_are_listed_file_by_file_and_ignored_files_are_left_out() {
    let test = TestRepo::with_commit();
    test.write(".gitignore", "*.log\nbuild/\n");
    test.write("new/deep/a.txt", "a\n");
    test.write("new/deep/b.txt", "b\n");
    test.write("new/c.txt", "c\n");
    test.write("debug.log", "ignored\n");
    test.write("build/output.bin", "ignored\n");

    let status = test.repo.status().unwrap();
    assert_eq!(
        paths(&status),
        [
            ".gitignore",
            "new/c.txt",
            "new/deep/a.txt",
            "new/deep/b.txt"
        ]
    );
    assert!(status.entries.iter().all(|entry| entry.untracked));
    assert_eq!(status.untracked().len(), 4);
}

#[test]
fn unusual_file_names_survive() {
    let test = TestRepo::with_commit();
    let names = [
        "dir with space/\u{fc} \u{f1}.txt",
        "ta\tb.txt",
        "new\nline.txt",
        "quo\"te.txt",
        "back\\slash.txt",
        "\u{65e5}\u{672c}\u{8a9e}.txt",
        "[id].tsx",
        "star*.txt",
        ":colon.txt",
        "-dash.txt",
    ];
    for name in names {
        test.write(name, "content\n");
    }
    let status = test.repo.status().unwrap();
    for name in names {
        assert!(entry(&status, name).untracked, "{name:?}");
    }
    assert_eq!(status.entries.len(), names.len());

    // The same names once tracked and modified.
    test.commit_all("odd names");
    for name in names {
        test.write(name, "changed\n");
    }
    let status = test.repo.status().unwrap();
    for name in names {
        assert_eq!(
            entry(&status, name).worktree,
            FileState::Modified,
            "{name:?}"
        );
    }
    assert_eq!(status.entries.len(), names.len());
}

#[test]
fn a_file_name_that_is_not_utf8_survives() {
    use std::os::unix::ffi::OsStrExt;
    let test = TestRepo::with_commit();
    let name = std::ffi::OsStr::from_bytes(b"latin1-caf\xe9.txt");
    // Some file systems (APFS) refuse names that are not UTF-8; nothing to test there.
    if std::fs::write(test.root().join(name), "content\n").is_err() {
        return;
    }
    let status = test.repo.status().unwrap();
    assert_eq!(status.entries.len(), 1);
    assert_eq!(status.entries[0].path, PathBuf::from(name));
    test.repo.stage(&[PathBuf::from(name)]).unwrap();
    let status = test.repo.status().unwrap();
    assert_eq!(status.entries[0].index, FileState::Added);
}

#[test]
fn a_merge_conflict_lists_conflicted_files_and_the_merge_in_progress() {
    let test = diverged();
    assert!(test.try_git(&["merge", "other"]).is_err());

    let status = test.repo.status().unwrap();
    assert_eq!(status.state, Some(RepoState::Merging));
    assert_eq!(status.branch.as_deref(), Some("main"));

    let conflict = entry(&status, "conflict.txt");
    assert!(conflict.conflicted);
    assert_eq!(conflict.conflict, Some(ConflictKind::BothModified));
    assert_eq!(conflict.display_status(), DisplayStatus::Conflict);
    assert_eq!(conflict.stage_state(), StageState::Unstaged);

    // What merged cleanly is already staged.
    let clean = entry(&status, "theirs-only.txt");
    assert_eq!(clean.index, FileState::Added);
    assert!(!clean.conflicted);

    let counts = status.counts();
    assert_eq!((counts.conflicted, counts.staged, counts.total), (1, 1, 2));
    assert_eq!(status.conflicts().len(), 1);
}

#[test]
fn conflict_kinds_of_a_merge() {
    let test = TestRepo::new();
    test.write("deleted-by-them.txt", "keep\n");
    test.write("deleted-by-us.txt", "keep\n");
    test.commit_all("base");
    test.git(&["checkout", "--quiet", "-b", "other"]);
    test.git(&["rm", "--quiet", "deleted-by-them.txt"]);
    test.write("deleted-by-us.txt", "changed by them\n");
    test.write("both-added.txt", "added by other\n");
    test.commit_all("other");
    test.git(&["checkout", "--quiet", "main"]);
    test.write("deleted-by-them.txt", "changed by us\n");
    test.git(&["rm", "--quiet", "deleted-by-us.txt"]);
    test.write("both-added.txt", "added by main\n");
    test.commit_all("main");
    assert!(test.try_git(&["merge", "other"]).is_err());

    let status = test.repo.status().unwrap();
    let kind = |path: &str| entry(&status, path).conflict;
    assert_eq!(kind("both-added.txt"), Some(ConflictKind::BothAdded));
    assert_eq!(
        kind("deleted-by-them.txt"),
        Some(ConflictKind::DeletedByThem)
    );
    assert_eq!(kind("deleted-by-us.txt"), Some(ConflictKind::DeletedByUs));
    assert_eq!(status.conflicts().len(), 3);
}

#[test]
fn rebase_cherry_pick_revert_and_bisect_are_reported() {
    let test = diverged();
    let state = || test.repo.status().unwrap().state;
    assert_eq!(state(), None);

    assert!(test.try_git(&["rebase", "other"]).is_err());
    assert_eq!(state(), Some(RepoState::Rebasing));
    // A rebase detaches HEAD while it runs.
    assert_eq!(test.repo.status().unwrap().branch, None);
    test.git(&["rebase", "--abort"]);
    assert_eq!(state(), None);

    assert!(test.try_git(&["cherry-pick", "other"]).is_err());
    assert_eq!(state(), Some(RepoState::CherryPicking));
    test.git(&["cherry-pick", "--abort"]);
    assert_eq!(state(), None);

    // A revert that has not been committed yet leaves the repository reverting.
    test.git(&["revert", "--no-commit", "HEAD"]);
    assert_eq!(state(), Some(RepoState::Reverting));
    test.git(&["revert", "--abort"]);
    assert_eq!(state(), None);

    test.git(&["bisect", "start"]);
    assert_eq!(state(), Some(RepoState::Bisecting));
    test.git(&["bisect", "reset"]);
    assert_eq!(state(), None);
}

#[test]
fn a_cherry_pick_of_several_commits_stays_in_progress_between_its_commits() {
    let test = TestRepo::new();
    test.write("conflict.txt", "base\n");
    test.commit_all("base");
    test.git(&["checkout", "--quiet", "-b", "other"]);
    test.write("conflict.txt", "theirs\n");
    test.commit_all("first to pick");
    test.write("later.txt", "later\n");
    test.commit_all("second to pick");
    test.git(&["checkout", "--quiet", "main"]);
    test.write("conflict.txt", "ours\n");
    test.commit_all("ours");

    assert!(test.try_git(&["cherry-pick", "other~1", "other"]).is_err());
    assert_eq!(
        test.repo.status().unwrap().state,
        Some(RepoState::CherryPicking)
    );

    // The user resolves the first commit by hand and commits it. The second is still
    // waiting: git says "cherry-pick in progress", and so do we.
    test.write("conflict.txt", "resolved\n");
    test.repo.stage(&["conflict.txt"]).unwrap();
    test.repo
        .commit("first, resolved", bakedit_git::CommitOptions::default())
        .unwrap();
    let status = test.repo.status().unwrap();
    assert!(status.is_clean());
    assert_eq!(status.state, Some(RepoState::CherryPicking));

    test.git(&["cherry-pick", "--continue"]);
    assert_eq!(test.repo.status().unwrap().state, None);
    assert!(test.exists("later.txt"));
}

#[test]
fn a_cherry_pick_is_reported_in_a_reftable_repository() {
    isolate_git();
    let sandbox = tempfile::tempdir().unwrap();
    let root = sandbox.path().join("work");
    std::fs::create_dir_all(&root).unwrap();
    let created = common::try_git_in(
        &root,
        &[
            "-c",
            "init.defaultBranch=main",
            "init",
            "--quiet",
            "--ref-format=reftable",
        ],
    );
    // Older gits do not know the reftable format; nothing to test there.
    if created.is_err() {
        return;
    }
    git_in(&root, &["config", "user.name", "Test Author"]);
    git_in(&root, &["config", "user.email", "author@example.com"]);
    git_in(&root, &["config", "commit.gpgsign", "false"]);
    let commit = |content: &str, message: &str| {
        std::fs::write(root.join("file.txt"), content).unwrap();
        git_in(&root, &["add", "--all"]);
        git_in(&root, &["commit", "--quiet", "--message", message]);
    };
    commit("base\n", "base");
    git_in(&root, &["checkout", "--quiet", "-b", "side"]);
    commit("side\n", "side");
    git_in(&root, &["checkout", "--quiet", "main"]);
    commit("main\n", "main");
    assert!(common::try_git_in(&root, &["cherry-pick", "side"]).is_err());

    let repo = Repo::discover(&root).unwrap();
    let status = repo.status().unwrap();
    assert_eq!(status.state, Some(RepoState::CherryPicking));
    assert_eq!(status.conflicts().len(), 1);
}

#[test]
fn ahead_and_behind_an_upstream() {
    let test = TestRepo::with_commit();
    let remote = test.add_origin();
    let status = test.repo.status().unwrap();
    assert_eq!(status.upstream.as_deref(), Some("origin/main"));
    assert!(!status.upstream_gone);
    assert_eq!((status.ahead, status.behind), (0, 0));

    // Two commits arrive on the remote from another clone, three are made here.
    let other = test.clone_of(&remote, "other-clone");
    for number in 1..=2 {
        std::fs::write(other.join(format!("remote-{number}.txt")), "remote\n").unwrap();
        git_in(&other, &["add", "--all"]);
        git_in(&other, &["commit", "--quiet", "--message", "remote commit"]);
    }
    git_in(&other, &["push", "--quiet"]);
    for number in 1..=3 {
        test.write(&format!("local-{number}.txt"), "local\n");
        test.commit_all("local commit");
    }
    // Not fetched yet: the remote's new commits are unknown.
    let status = test.repo.status().unwrap();
    assert_eq!((status.ahead, status.behind), (3, 0));

    test.git(&["fetch", "--quiet"]);
    let status = test.repo.status().unwrap();
    assert_eq!((status.ahead, status.behind), (3, 2));
    assert_eq!(status.upstream.as_deref(), Some("origin/main"));
}

#[test]
fn an_upstream_that_was_deleted_is_gone() {
    let test = TestRepo::with_commit();
    test.add_origin();
    test.git(&["checkout", "--quiet", "-b", "topic"]);
    test.git(&["push", "--quiet", "--set-upstream", "origin", "topic"]);
    test.git(&["push", "--quiet", "origin", "--delete", "topic"]);

    let status = test.repo.status().unwrap();
    assert_eq!(status.branch.as_deref(), Some("topic"));
    assert_eq!(status.upstream.as_deref(), Some("origin/topic"));
    assert!(status.upstream_gone);
    assert_eq!((status.ahead, status.behind), (0, 0));
}

#[test]
fn status_works_in_a_linked_work_tree() {
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
    std::fs::write(linked.join("README.md"), "# changed in the linked tree\n").unwrap();
    std::fs::write(linked.join("only-here.txt"), "new\n").unwrap();

    let repo = Repo::discover(&linked).unwrap();
    let status = repo.status().unwrap();
    assert_eq!(status.branch.as_deref(), Some("feature"));
    assert_eq!(paths(&status), ["README.md", "only-here.txt"]);
    assert_eq!(entry(&status, "README.md").worktree, FileState::Modified);

    // A merge in the linked tree is its state alone.
    test.write("README.md", "# changed on main\n");
    test.commit_all("main change");
    git_in(&linked, &["add", "--all"]);
    git_in(
        &linked,
        &["commit", "--quiet", "--message", "feature change"],
    );
    assert!(common::try_git_in(&linked, &["merge", "main"]).is_err());
    assert_eq!(repo.status().unwrap().state, Some(RepoState::Merging));
    assert_eq!(test.repo.status().unwrap().state, None);
    assert!(test.repo.status().unwrap().is_clean());
}

#[test]
fn status_does_not_wait_for_the_index_lock() {
    let test = TestRepo::with_commit();
    test.write("README.md", "# changed\n");
    // Another git command is "running": it holds the index lock.
    let lock = test.path(".git/index.lock");
    std::fs::write(&lock, "").unwrap();
    let status = test.repo.status().unwrap();
    assert_eq!(paths(&status), ["README.md"]);
    assert!(lock.exists(), "the other command's lock is left alone");
}
