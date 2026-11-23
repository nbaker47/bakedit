//! Staging, unstaging, discarding and committing against real repositories.

mod common;

use std::path::{Path, PathBuf};

use bakedit_git::{CommitOptions, FileState, GitError, RepoState, StageState, Status};
use common::TestRepo;

fn stage_state(status: &Status, path: &str) -> StageState {
    status
        .entry(path)
        .unwrap_or_else(|| panic!("no entry for {path}"))
        .stage_state()
}

fn names(paths: &[PathBuf]) -> Vec<String> {
    let mut names: Vec<String> = paths
        .iter()
        .map(|path| path.display().to_string())
        .collect();
    names.sort();
    names
}

/// A repository in the middle of a merge with one conflicted file (`conflict.txt`) and
/// one cleanly merged, already staged file (`theirs-only.txt`).
fn merging() -> TestRepo {
    let test = TestRepo::new();
    test.write("conflict.txt", "one\ntwo\nthree\n");
    test.write("plain.txt", "plain\n");
    test.commit_all("base");
    test.git(&["checkout", "--quiet", "-b", "other"]);
    test.write("conflict.txt", "one\nTWO from other\nthree\n");
    test.write("theirs-only.txt", "theirs\n");
    test.commit_all("other change");
    test.git(&["checkout", "--quiet", "main"]);
    test.write("conflict.txt", "one\nTWO from main\nthree\n");
    test.commit_all("main change");
    assert!(test.try_git(&["merge", "other"]).is_err());
    test
}

#[test]
fn stage_and_unstage_single_files() {
    let test = TestRepo::with_commit();
    test.write("README.md", "# changed\n");
    test.write("new.txt", "new\n");
    test.write("other.txt", "other\n");

    test.repo.stage(&["README.md", "new.txt"]).unwrap();
    let status = test.repo.status().unwrap();
    assert_eq!(stage_state(&status, "README.md"), StageState::Staged);
    assert_eq!(stage_state(&status, "new.txt"), StageState::Staged);
    assert_eq!(stage_state(&status, "other.txt"), StageState::Unstaged);

    test.repo.unstage(&["README.md"]).unwrap();
    let status = test.repo.status().unwrap();
    assert_eq!(stage_state(&status, "README.md"), StageState::Unstaged);
    assert_eq!(stage_state(&status, "new.txt"), StageState::Staged);
    // The work tree is not touched by either.
    assert_eq!(test.read("README.md"), "# changed\n");
}

#[test]
fn absolute_paths_and_directories_are_accepted() {
    let test = TestRepo::with_commit();
    test.write("src/a.rs", "a\n");
    test.write("src/deep/b.rs", "b\n");
    test.write("docs/c.md", "c\n");

    test.repo.stage(&[test.path("src")]).unwrap();
    let status = test.repo.status().unwrap();
    assert_eq!(stage_state(&status, "src/a.rs"), StageState::Staged);
    assert_eq!(stage_state(&status, "src/deep/b.rs"), StageState::Staged);
    assert_eq!(stage_state(&status, "docs/c.md"), StageState::Unstaged);

    test.repo.unstage(&[test.path("src/deep")]).unwrap();
    let status = test.repo.status().unwrap();
    assert_eq!(stage_state(&status, "src/a.rs"), StageState::Staged);
    assert_eq!(stage_state(&status, "src/deep/b.rs"), StageState::Unstaged);

    match test.repo.stage(&[test.sandbox().join("outside.txt")]) {
        Err(GitError::PathOutsideRepo(_)) => {}
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn staging_a_deletion_and_unstaging_it() {
    let test = TestRepo::with_commit();
    test.remove("README.md");
    test.repo.stage(&["README.md"]).unwrap();
    let status = test.repo.status().unwrap();
    let entry = status.entry("README.md").unwrap();
    assert_eq!(entry.index, FileState::Deleted);
    assert!(entry.is_fully_staged());

    test.repo.unstage(&["README.md"]).unwrap();
    let status = test.repo.status().unwrap();
    let entry = status.entry("README.md").unwrap();
    assert_eq!(entry.index, FileState::Unmodified);
    assert_eq!(entry.worktree, FileState::Deleted);
}

#[test]
fn names_with_pattern_characters_are_taken_literally() {
    let test = TestRepo::new();
    let files = [
        "app/[id].tsx",
        "app/i.tsx",
        "app/d.tsx",
        "star*.txt",
        "starry.txt",
        ":colon.txt",
        "-dash.txt",
        "new\nline.txt",
        "dir with space/\u{fc}.txt",
    ];
    for file in files {
        test.write(file, "one\n");
    }
    test.commit_all("files");
    for file in files {
        test.write(file, "two\n");
    }

    test.repo
        .stage(&[
            "app/[id].tsx",
            "star*.txt",
            ":colon.txt",
            "-dash.txt",
            "new\nline.txt",
        ])
        .unwrap();
    let status = test.repo.status().unwrap();
    let staged: Vec<_> = status
        .staged()
        .iter()
        .map(|entry| entry.path.clone())
        .collect();
    assert_eq!(
        names(&staged),
        [
            "-dash.txt",
            ":colon.txt",
            "app/[id].tsx",
            "new\nline.txt",
            "star*.txt"
        ]
    );

    test.repo.unstage(&["app/[id].tsx", "star*.txt"]).unwrap();
    let status = test.repo.status().unwrap();
    assert_eq!(status.staged().len(), 3);

    // Discarding one of them leaves its look-alikes modified.
    let outcome = test.repo.restore(&["app/[id].tsx", "star*.txt"]).unwrap();
    assert_eq!(names(&outcome.restored), ["app/[id].tsx", "star*.txt"]);
    assert_eq!(test.read("app/[id].tsx"), "one\n");
    assert_eq!(test.read("app/i.tsx"), "two\n");
    assert_eq!(test.read("app/d.tsx"), "two\n");
    assert_eq!(test.read("starry.txt"), "two\n");
}

#[test]
fn empty_path_lists_do_nothing() {
    let test = TestRepo::with_commit();
    test.write("README.md", "# changed\n");
    test.write("new.txt", "new\n");
    let none: [&Path; 0] = [];

    test.repo.stage(&none).unwrap();
    assert!(test.repo.status().unwrap().staged().is_empty());

    test.repo.stage_all().unwrap();
    test.repo.unstage(&none).unwrap();
    assert_eq!(test.repo.status().unwrap().staged().len(), 2);

    test.repo.unstage_all().unwrap();
    let outcome = test.repo.restore(&none).unwrap();
    assert!(outcome.restored.is_empty() && outcome.untracked.is_empty());
    assert_eq!(test.read("README.md"), "# changed\n");
}

#[test]
fn a_path_that_matches_nothing_is_an_error_from_git() {
    let test = TestRepo::with_commit();
    match test.repo.stage(&["no-such-file.txt"]) {
        Err(GitError::Failed {
            command,
            code,
            stderr,
            ..
        }) => {
            assert_eq!(command, "add");
            assert_eq!(code, Some(128));
            assert!(stderr.contains("did not match any files"), "{stderr}");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn stage_all_and_unstage_all() {
    let test = TestRepo::new();
    test.write("tracked.txt", "one\n");
    test.write("deleted.txt", "one\n");
    test.write("moved.txt", "one\ntwo\nthree\nfour\n");
    test.commit_all("files");
    test.write("tracked.txt", "two\n");
    test.remove("deleted.txt");
    test.git(&["mv", "moved.txt", "moved away.txt"]);
    test.git(&["reset", "--quiet"]);
    test.write("new/file.txt", "new\n");

    test.repo.stage_all().unwrap();
    let status = test.repo.status().unwrap();
    assert!(status.entries.iter().all(|entry| entry.is_fully_staged()));
    assert_eq!(status.entries.len(), 4);
    let moved = status.entry("moved away.txt").unwrap();
    assert_eq!(moved.index, FileState::Renamed);
    assert_eq!(moved.orig_path.as_deref(), Some(Path::new("moved.txt")));

    test.repo.unstage_all().unwrap();
    let status = test.repo.status().unwrap();
    assert!(status.staged().is_empty());
    // The rename is undone on both of its paths: a deletion and an untracked file.
    assert_eq!(
        status.entry("moved.txt").unwrap().worktree,
        FileState::Deleted
    );
    assert!(status.entry("moved away.txt").unwrap().untracked);
    assert!(status.entry("new/file.txt").unwrap().untracked);
}

#[test]
fn unstaging_before_the_first_commit() {
    let test = TestRepo::new();
    test.write("a.txt", "a\n");
    test.write("b.txt", "b\n");
    test.repo.stage_all().unwrap();
    assert_eq!(test.repo.status().unwrap().staged().len(), 2);

    test.repo.unstage(&["a.txt"]).unwrap();
    let status = test.repo.status().unwrap();
    assert!(status.entry("a.txt").unwrap().untracked);
    assert_eq!(stage_state(&status, "b.txt"), StageState::Staged);

    test.repo.unstage_all().unwrap();
    let status = test.repo.status().unwrap();
    assert!(status.entries.iter().all(|entry| entry.untracked));
    assert_eq!(status.entries.len(), 2);
    assert_eq!(test.read("b.txt"), "b\n");
}

#[test]
fn unstage_all_keeps_a_merge_and_its_conflicts() {
    let test = merging();
    test.write("plain.txt", "edited during the merge\n");
    test.repo.stage(&["plain.txt"]).unwrap();

    test.repo.unstage_all().unwrap();
    let status = test.repo.status().unwrap();
    assert_eq!(status.state, Some(RepoState::Merging));
    assert!(status.entry("conflict.txt").unwrap().conflicted);
    assert!(status.staged().is_empty());
    assert_eq!(
        status.entry("plain.txt").unwrap().worktree,
        FileState::Modified
    );
}

#[test]
fn staging_a_conflicted_file_resolves_it() {
    let test = merging();
    test.write("conflict.txt", "one\nTWO resolved\nthree\n");
    test.repo.stage(&["conflict.txt"]).unwrap();
    let status = test.repo.status().unwrap();
    let entry = status.entry("conflict.txt").unwrap();
    assert!(!entry.conflicted);
    assert!(entry.is_fully_staged());
    assert!(status.conflicts().is_empty());
    assert_eq!(status.state, Some(RepoState::Merging));
}

#[test]
fn restore_discards_unstaged_changes_and_keeps_staged_ones() {
    let test = TestRepo::new();
    test.write("edited.txt", "original\n");
    test.write("partly.txt", "original\n");
    test.write("deleted.txt", "original\n");
    test.write("untouched.txt", "original\n");
    test.commit_all("files");

    test.write("edited.txt", "edited\n");
    test.write("partly.txt", "staged\n");
    test.repo.stage(&["partly.txt"]).unwrap();
    test.write("partly.txt", "staged\nand not staged\n");
    test.remove("deleted.txt");
    test.write("untouched.txt", "also edited\n");

    let outcome = test
        .repo
        .restore(&["edited.txt", "partly.txt", "deleted.txt"])
        .unwrap();
    assert_eq!(
        names(&outcome.restored),
        ["deleted.txt", "edited.txt", "partly.txt"]
    );
    assert!(outcome.untracked.is_empty() && outcome.conflicted.is_empty());

    assert_eq!(test.read("edited.txt"), "original\n");
    assert_eq!(test.read("deleted.txt"), "original\n");
    // Back to what is staged, not to the last commit.
    assert_eq!(test.read("partly.txt"), "staged\n");
    assert_eq!(test.read("untouched.txt"), "also edited\n");
    let status = test.repo.status().unwrap();
    assert_eq!(stage_state(&status, "partly.txt"), StageState::Staged);
    assert_eq!(status.entries.len(), 2);
}

#[test]
fn restore_never_deletes_files_that_only_exist_in_the_work_tree() {
    let test = TestRepo::with_commit();
    test.write("README.md", "# edited\n");
    test.write("untracked.txt", "precious\n");
    test.write("new dir/inside.txt", "precious too\n");
    test.write("intent.txt", "known to git by name only\n");
    test.git(&["add", "--intent-to-add", "intent.txt"]);
    test.write("staged-new.txt", "staged\n");
    test.repo.stage(&["staged-new.txt"]).unwrap();

    let outcome = test
        .repo
        .restore(&[
            "README.md",
            "untracked.txt",
            "new dir",
            "intent.txt",
            "staged-new.txt",
        ])
        .unwrap();
    assert_eq!(names(&outcome.restored), ["README.md"]);
    assert_eq!(
        names(&outcome.untracked),
        ["intent.txt", "new dir/inside.txt", "untracked.txt"]
    );
    assert!(outcome.conflicted.is_empty());

    assert_eq!(test.read("README.md"), "# test\n");
    assert_eq!(test.read("untracked.txt"), "precious\n");
    assert_eq!(test.read("new dir/inside.txt"), "precious too\n");
    assert_eq!(test.read("intent.txt"), "known to git by name only\n");
    assert_eq!(test.read("staged-new.txt"), "staged\n");
}

#[test]
fn restore_before_the_first_commit() {
    let test = TestRepo::new();
    test.write("staged.txt", "staged\n");
    test.repo.stage(&["staged.txt"]).unwrap();
    test.write("staged.txt", "staged\nand edited\n");
    test.write("untracked.txt", "untracked\n");

    let outcome = test.repo.restore_all().unwrap();
    assert_eq!(names(&outcome.restored), ["staged.txt"]);
    assert_eq!(names(&outcome.untracked), ["untracked.txt"]);
    assert_eq!(test.read("staged.txt"), "staged\n");
    assert_eq!(test.read("untracked.txt"), "untracked\n");
}

#[test]
fn restore_all_covers_the_whole_work_tree() {
    let test = TestRepo::new();
    test.write("a.txt", "original\n");
    test.write("dir/b.txt", "original\n");
    test.commit_all("files");
    test.write("a.txt", "edited\n");
    test.write("dir/b.txt", "edited\n");
    test.write("new.txt", "new\n");

    let outcome = test.repo.restore_all().unwrap();
    assert_eq!(names(&outcome.restored), ["a.txt", "dir/b.txt"]);
    assert_eq!(names(&outcome.untracked), ["new.txt"]);
    assert_eq!(test.read("a.txt"), "original\n");
    assert_eq!(test.read("dir/b.txt"), "original\n");
    assert_eq!(test.read("new.txt"), "new\n");

    // The top of the work tree as a path means the same.
    test.write("a.txt", "edited again\n");
    let outcome = test.repo.restore(&[test.root()]).unwrap();
    assert_eq!(names(&outcome.restored), ["a.txt"]);
    assert_eq!(names(&outcome.untracked), ["new.txt"]);
    assert_eq!(test.read("a.txt"), "original\n");
}

#[test]
fn restore_leaves_conflicted_files_alone() {
    let test = merging();
    let with_markers = test.read("conflict.txt");
    test.write("plain.txt", "edited during the merge\n");

    let outcome = test.repo.restore(&["conflict.txt", "plain.txt"]).unwrap();
    assert_eq!(names(&outcome.restored), ["plain.txt"]);
    assert_eq!(names(&outcome.conflicted), ["conflict.txt"]);
    assert_eq!(test.read("conflict.txt"), with_markers);
    assert_eq!(test.read("plain.txt"), "plain\n");
    assert!(
        test.repo
            .status()
            .unwrap()
            .entry("conflict.txt")
            .unwrap()
            .conflicted
    );
}

#[test]
fn the_first_commit() {
    let test = TestRepo::new();
    test.write("first.txt", "hello\n");
    test.repo.stage_all().unwrap();
    let sha = test
        .repo
        .commit(
            "feat: first commit\n\nWith a body.\n",
            CommitOptions::default(),
        )
        .unwrap();
    assert_eq!(sha, test.head());
    assert_eq!(sha.len(), 40);
    assert_eq!(
        test.repo.head_commit_message().unwrap().as_deref(),
        Some("feat: first commit\n\nWith a body.")
    );
    assert!(test.repo.status().unwrap().is_clean());
}

#[test]
fn committing_on_a_detached_head() {
    let test = TestRepo::with_commit();
    let first = test.head();
    test.git(&["checkout", "--quiet", "--detach"]);
    test.write("a.txt", "a\n");
    test.repo.stage_all().unwrap();
    let sha = test
        .repo
        .commit("on no branch", CommitOptions::default())
        .unwrap();
    assert_eq!(sha, test.head());
    assert_ne!(sha, first);
    assert_eq!(test.repo.current_branch().unwrap(), None);
    assert_eq!(test.git(&["rev-parse", "main"]).trim(), first);
}

#[test]
fn commit_takes_only_what_is_staged() {
    let test = TestRepo::with_commit();
    let before = test.head();
    test.write("staged.txt", "staged\n");
    test.write("left.txt", "left\n");
    test.repo.stage(&["staged.txt"]).unwrap();

    let sha = test
        .repo
        .commit("add the staged file", CommitOptions::default())
        .unwrap();
    assert_ne!(sha, before);
    assert_eq!(test.git(&["rev-parse", "HEAD~1"]).trim(), before);
    let files = test.git(&["show", "--name-only", "--format=", "HEAD"]);
    assert_eq!(files.trim(), "staged.txt");
    let status = test.repo.status().unwrap();
    assert_eq!(status.entries.len(), 1);
    assert!(status.entry("left.txt").unwrap().untracked);
}

#[test]
fn the_message_is_passed_through_unchanged() {
    let test = TestRepo::with_commit();
    test.write("a.txt", "a\n");
    test.repo.stage_all().unwrap();
    let message = "fix: \u{65e5}\u{672c}\u{8a9e} and \"quotes\" and $(not run)\n\n\
                   # a line that starts with a hash is kept\n\
                   - bullet\n\
                   -m looks like an option\n";
    test.repo.commit(message, CommitOptions::default()).unwrap();
    assert_eq!(
        test.repo.head_commit_message().unwrap().unwrap(),
        message.trim_end()
    );
}

#[test]
fn an_empty_message_is_a_typed_error() {
    let test = TestRepo::with_commit();
    test.write("a.txt", "a\n");
    test.repo.stage_all().unwrap();
    let before = test.head();
    for message in ["", "   ", "\n\n\t\n"] {
        match test.repo.commit(message, CommitOptions::default()) {
            Err(GitError::EmptyCommitMessage) => {}
            other => panic!("unexpected {other:?} for {message:?}"),
        }
    }
    let amend = CommitOptions {
        amend: true,
        ..CommitOptions::default()
    };
    assert!(matches!(
        test.repo.commit("", amend),
        Err(GitError::EmptyCommitMessage)
    ));
    assert_eq!(test.head(), before);
}

#[test]
fn nothing_staged_is_a_typed_error() {
    let test = TestRepo::with_commit();
    let before = test.head();
    assert!(matches!(
        test.repo.commit("nothing to see", CommitOptions::default()),
        Err(GitError::NothingStaged)
    ));
    // Unstaged and untracked changes do not count.
    test.write("README.md", "# edited\n");
    test.write("new.txt", "new\n");
    assert!(matches!(
        test.repo.commit("still nothing", CommitOptions::default()),
        Err(GitError::NothingStaged)
    ));
    assert_eq!(test.head(), before);

    // Nor is there anything in a repository that has no commits and no staged files.
    let empty = TestRepo::new();
    assert!(matches!(
        empty
            .repo
            .commit("nothing at all", CommitOptions::default()),
        Err(GitError::NothingStaged)
    ));
    assert_eq!(empty.repo.head_commit_message().unwrap(), None);
}

#[test]
fn amend_replaces_the_last_commit() {
    let test = TestRepo::with_commit();
    test.write("a.txt", "a\n");
    test.repo.stage_all().unwrap();
    let first = test
        .repo
        .commit("first wording", CommitOptions::default())
        .unwrap();
    let parent = test.git(&["rev-parse", "HEAD~1"]);
    let amend = CommitOptions {
        amend: true,
        sign_off: false,
    };

    // Only the message changes: nothing needs to be staged.
    let reworded = test.repo.commit("better wording", amend).unwrap();
    assert_ne!(reworded, first);
    assert_eq!(test.git(&["rev-parse", "HEAD~1"]), parent);
    assert_eq!(
        test.repo.head_commit_message().unwrap().as_deref(),
        Some("better wording")
    );

    // With something staged the amended commit takes it in.
    test.write("b.txt", "b\n");
    test.repo.stage_all().unwrap();
    test.repo.commit("better wording", amend).unwrap();
    assert_eq!(test.git(&["rev-parse", "HEAD~1"]), parent);
    let files = test.git(&["show", "--name-only", "--format=", "HEAD"]);
    assert_eq!(files.trim(), "a.txt\nb.txt");
}

#[test]
fn sign_off_adds_the_trailer() {
    let test = TestRepo::with_commit();
    test.write("a.txt", "a\n");
    test.repo.stage_all().unwrap();
    let options = CommitOptions {
        amend: false,
        sign_off: true,
    };
    test.repo.commit("signed work", options).unwrap();
    assert_eq!(
        test.repo.head_commit_message().unwrap().as_deref(),
        Some("signed work\n\nSigned-off-by: Test Author <author@example.com>")
    );
}

#[test]
fn a_failing_hook_surfaces_its_output() {
    let test = TestRepo::with_commit();
    test.install_hook(
        "pre-commit",
        "#!/bin/sh\necho 'lint: trailing whitespace in a.txt'\necho 'hook failed' >&2\nexit 3\n",
    );
    test.write("a.txt", "a \n");
    test.repo.stage_all().unwrap();
    let before = test.head();

    let error = test
        .repo
        .commit("will be refused", CommitOptions::default())
        .unwrap_err();
    match &error {
        GitError::Failed {
            command,
            code,
            stderr,
            ..
        } => {
            assert_eq!(command, "commit");
            assert_eq!(*code, Some(1));
            assert!(
                stderr.contains("lint: trailing whitespace in a.txt"),
                "{stderr}"
            );
            assert!(stderr.contains("hook failed"), "{stderr}");
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(error.to_string().contains("lint: trailing whitespace"));
    assert_eq!(error.exit_code(), Some(1));
    assert_eq!(test.head(), before);
    // Still staged, ready for another try.
    assert_eq!(test.repo.status().unwrap().staged().len(), 1);
}

#[test]
fn hooks_see_the_message_and_can_rewrite_it() {
    let test = TestRepo::with_commit();
    test.install_hook(
        "commit-msg",
        "#!/bin/sh\nprintf '\\nReviewed-by: the hook\\n' >> \"$1\"\n",
    );
    test.write("a.txt", "a\n");
    test.repo.stage_all().unwrap();
    test.repo
        .commit("subject", CommitOptions::default())
        .unwrap();
    assert_eq!(
        test.repo.head_commit_message().unwrap().as_deref(),
        Some("subject\n\nReviewed-by: the hook")
    );
}

#[test]
fn committing_with_unresolved_conflicts_is_refused_by_git() {
    let test = merging();
    match test.repo.commit("too early", CommitOptions::default()) {
        Err(GitError::Failed {
            command, stderr, ..
        }) => {
            assert_eq!(command, "commit");
            assert!(stderr.contains("unmerged files"), "{stderr}");
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(test.repo.status().unwrap().state, Some(RepoState::Merging));
}

#[test]
fn commit_concludes_a_merge() {
    let test = merging();
    test.write("conflict.txt", "one\nTWO resolved\nthree\n");
    test.repo.stage(&["conflict.txt"]).unwrap();
    let sha = test
        .repo
        .commit("merge other into main", CommitOptions::default())
        .unwrap();
    assert_eq!(sha, test.head());
    let parents = test.git(&["show", "--no-patch", "--format=%P", "HEAD"]);
    assert_eq!(parents.split_whitespace().count(), 2, "a merge commit");
    let status = test.repo.status().unwrap();
    assert_eq!(status.state, None);
    assert!(status.is_clean());
}

#[test]
fn a_merge_that_changes_nothing_can_still_be_concluded() {
    let test = TestRepo::with_commit();
    test.git(&["checkout", "--quiet", "-b", "other"]);
    test.write("README.md", "# theirs\n");
    test.commit_all("their change");
    test.git(&["checkout", "--quiet", "main"]);
    // Keep our side entirely, without committing yet.
    test.git(&["merge", "--strategy=ours", "--no-commit", "other"]);
    assert_eq!(test.repo.status().unwrap().state, Some(RepoState::Merging));

    test.repo
        .commit("merge other, keeping ours", CommitOptions::default())
        .unwrap();
    let parents = test.git(&["show", "--no-patch", "--format=%P", "HEAD"]);
    assert_eq!(parents.split_whitespace().count(), 2);
    assert_eq!(test.read("README.md"), "# test\n");
}
