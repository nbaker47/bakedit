//! Branches, checkout, the commit log and file contents at `HEAD` and in the index.

mod common;

use std::path::Path;

use bakedit_git::{Branch, GitError, Repo};
use common::{git_in, TestRepo};

fn branch<'a>(branches: &'a [Branch], name: &str) -> &'a Branch {
    branches
        .iter()
        .find(|branch| branch.name == name)
        .unwrap_or_else(|| panic!("no branch {name} in {branches:#?}"))
}

fn branch_names(branches: &[Branch]) -> Vec<&str> {
    branches.iter().map(|branch| branch.name.as_str()).collect()
}

/// Commits with a fixed date so the order of branches and commits is known.
fn commit_at(test: &TestRepo, seconds: i64, message: &str) -> String {
    let date = format!("{seconds} +0000");
    test.git(&["add", "--all"]);
    let output = std::process::Command::new("git")
        .current_dir(test.root())
        .args(["commit", "--quiet", "--allow-empty", "--message", message])
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    test.head()
}

#[test]
fn no_branches_before_the_first_commit() {
    let test = TestRepo::new();
    assert!(test.repo.branches().unwrap().is_empty());
    assert_eq!(
        test.repo.current_branch().unwrap(),
        Some("main".to_string())
    );
    assert!(test.repo.log(10, None).unwrap().is_empty());
}

#[test]
fn local_branches_come_first_newest_commit_first() {
    let test = TestRepo::new();
    test.write("a.txt", "a\n");
    let first = commit_at(&test, 1_700_000_000, "first on main");
    test.git(&["checkout", "--quiet", "-b", "old-topic"]);
    test.write("b.txt", "b\n");
    let old = commit_at(&test, 1_700_000_100, "old topic work");
    test.git(&["checkout", "--quiet", "-b", "feat/new-topic", "main"]);
    test.write("c.txt", "c\n");
    let new = commit_at(&test, 1_700_000_200, "new topic work");

    let branches = test.repo.branches().unwrap();
    assert_eq!(
        branch_names(&branches),
        ["feat/new-topic", "old-topic", "main"]
    );
    assert!(branches.iter().all(|branch| !branch.remote));

    let current = branch(&branches, "feat/new-topic");
    assert!(current.current);
    assert_eq!(current.sha, new);
    assert_eq!(current.last_commit_time, 1_700_000_200);
    assert_eq!(current.subject, "new topic work");
    assert_eq!(current.upstream, None);
    assert_eq!(current.worktree.as_deref(), Some(test.root()));

    let main = branch(&branches, "main");
    assert!(!main.current);
    assert_eq!(main.sha, first);
    assert_eq!(main.last_commit_time, 1_700_000_000);
    assert_eq!(main.worktree, None);
    assert_eq!(branch(&branches, "old-topic").sha, old);
}

#[test]
fn remote_branches_follow_and_aliases_are_left_out() {
    let test = TestRepo::with_commit();
    let remote = test.add_origin();
    test.git(&["checkout", "--quiet", "-b", "topic"]);
    test.write("topic.txt", "topic\n");
    test.commit_all("topic work");
    test.git(&["push", "--quiet", "--set-upstream", "origin", "topic"]);
    // What a clone has: origin/HEAD pointing at origin/main.
    test.git(&["remote", "set-head", "origin", "main"]);
    assert!(remote.exists());

    let branches = test.repo.branches().unwrap();
    let (local, remotes): (Vec<&Branch>, Vec<&Branch>) =
        branches.iter().partition(|branch| !branch.remote);
    let names = |list: &[&Branch]| -> Vec<String> {
        let mut names: Vec<String> = list.iter().map(|branch| branch.name.clone()).collect();
        names.sort();
        names
    };
    assert_eq!(names(&local), ["main", "topic"]);
    assert_eq!(names(&remotes), ["origin/main", "origin/topic"]);
    // All local branches come before the first remote one.
    let first_remote = branches.iter().position(|branch| branch.remote).unwrap();
    assert!(branches[first_remote..].iter().all(|branch| branch.remote));

    assert_eq!(
        branch(&branches, "topic").upstream.as_deref(),
        Some("origin/topic")
    );
    assert_eq!(
        branch(&branches, "main").upstream.as_deref(),
        Some("origin/main")
    );
    let remote_topic = branch(&branches, "origin/topic");
    assert!(!remote_topic.current);
    assert_eq!(remote_topic.sha, test.head());
    assert_eq!(remote_topic.upstream, None);
}

#[test]
fn a_detached_head_flags_no_branch() {
    let test = TestRepo::with_commit();
    test.git(&["checkout", "--quiet", "--detach"]);
    let branches = test.repo.branches().unwrap();
    assert_eq!(branch_names(&branches), ["main"]);
    assert!(!branches[0].current);
}

#[test]
fn branches_checked_out_in_other_work_trees_say_where() {
    let test = TestRepo::with_commit();
    let linked = test.sandbox().join("linked tree");
    test.git(&[
        "worktree",
        "add",
        "--quiet",
        "-b",
        "elsewhere",
        linked.to_str().unwrap(),
    ]);
    let linked = linked.canonicalize().unwrap();

    let branches = test.repo.branches().unwrap();
    let elsewhere = branch(&branches, "elsewhere");
    assert!(!elsewhere.current);
    assert_eq!(elsewhere.worktree.as_deref(), Some(linked.as_path()));

    // Seen from the linked tree the roles are swapped.
    let from_linked = Repo::discover(&linked).unwrap().branches().unwrap();
    assert!(branch(&from_linked, "elsewhere").current);
    assert!(!branch(&from_linked, "main").current);
    assert_eq!(
        branch(&from_linked, "main").worktree.as_deref(),
        Some(test.root())
    );

    // Git refuses to check a branch out twice.
    match test.repo.checkout("elsewhere") {
        Err(error @ GitError::Failed { .. }) => {
            let output = error.git_output().unwrap();
            assert!(output.contains("linked tree"), "{output}");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn create_branch_with_and_without_switching() {
    let test = TestRepo::with_commit();
    test.write("README.md", "# uncommitted\n");

    test.repo.create_branch("stay-here", false).unwrap();
    assert_eq!(test.repo.current_branch().unwrap().as_deref(), Some("main"));
    test.repo.create_branch("feat/go-there", true).unwrap();
    assert_eq!(
        test.repo.current_branch().unwrap().as_deref(),
        Some("feat/go-there")
    );
    // Uncommitted work comes along.
    assert_eq!(test.read("README.md"), "# uncommitted\n");

    let branches = test.repo.branches().unwrap();
    let mut names = branch_names(&branches);
    names.sort();
    assert_eq!(names, ["feat/go-there", "main", "stay-here"]);
    assert!(branches.iter().all(|branch| branch.sha == test.head()));
}

#[test]
fn bad_branch_names_are_rejected() {
    let test = TestRepo::with_commit();
    for name in ["", "-f", "--orphan"] {
        assert!(
            matches!(
                test.repo.create_branch(name, true),
                Err(GitError::InvalidBranchName(_))
            ),
            "{name:?}"
        );
        assert!(
            matches!(
                test.repo.checkout(name),
                Err(GitError::InvalidBranchName(_))
            ),
            "{name:?}"
        );
    }
    // Names git itself refuses come back with its explanation.
    for name in ["has space", "double..dot", "main"] {
        for switch in [false, true] {
            match test.repo.create_branch(name, switch) {
                Err(GitError::Failed { code, .. }) => assert_eq!(code, Some(128), "{name:?}"),
                other => panic!("unexpected {other:?} for {name:?}"),
            }
        }
    }
    assert_eq!(test.repo.current_branch().unwrap().as_deref(), Some("main"));
    assert_eq!(test.repo.branches().unwrap().len(), 1);
}

#[test]
fn renaming_the_unborn_branch() {
    let test = TestRepo::new();
    test.repo.create_branch("trunk", true).unwrap();
    assert_eq!(
        test.repo.current_branch().unwrap().as_deref(),
        Some("trunk")
    );
    // Without switching there is nothing to branch from yet.
    assert!(matches!(
        test.repo.create_branch("other", false),
        Err(GitError::Failed { .. })
    ));
}

#[test]
fn checkout_switches_between_local_branches() {
    let test = TestRepo::with_commit();
    test.repo.create_branch("topic", true).unwrap();
    test.write("topic.txt", "topic\n");
    test.commit_all("topic work");

    test.repo.checkout("main").unwrap();
    assert_eq!(test.repo.current_branch().unwrap().as_deref(), Some("main"));
    assert!(!test.exists("topic.txt"));
    test.repo.checkout("topic").unwrap();
    assert!(test.exists("topic.txt"));

    // A file called like the branch does not confuse it.
    test.write("main", "a file named main\n");
    test.repo.checkout("main").unwrap();
    assert_eq!(test.repo.current_branch().unwrap().as_deref(), Some("main"));
}

#[test]
fn checkout_refuses_to_overwrite_local_changes() {
    let test = TestRepo::with_commit();
    test.repo.create_branch("topic", true).unwrap();
    test.write("README.md", "# topic version\n");
    test.commit_all("topic readme");
    test.repo.checkout("main").unwrap();
    test.write("README.md", "# uncommitted work\n");

    match test.repo.checkout("topic") {
        Err(error @ GitError::Failed { .. }) => {
            assert!(error.git_output().unwrap().contains("would be overwritten"));
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(test.repo.current_branch().unwrap().as_deref(), Some("main"));
    assert_eq!(test.read("README.md"), "# uncommitted work\n");
}

#[test]
fn checking_out_a_remote_branch_creates_a_tracking_branch() {
    let test = TestRepo::with_commit();
    let remote = test.add_origin();
    // A branch that only exists on the remote.
    let other = test.clone_of(&remote, "elsewhere");
    git_in(&other, &["checkout", "--quiet", "-b", "feat/remote-only"]);
    std::fs::write(other.join("remote.txt"), "remote\n").unwrap();
    git_in(&other, &["add", "--all"]);
    git_in(&other, &["commit", "--quiet", "--message", "remote work"]);
    git_in(
        &other,
        &[
            "push",
            "--quiet",
            "--set-upstream",
            "origin",
            "feat/remote-only",
        ],
    );
    test.repo.fetch().unwrap();

    test.repo.checkout("origin/feat/remote-only").unwrap();
    assert_eq!(
        test.repo.current_branch().unwrap().as_deref(),
        Some("feat/remote-only")
    );
    assert_eq!(test.read("remote.txt"), "remote\n");
    let status = test.repo.status().unwrap();
    assert_eq!(status.upstream.as_deref(), Some("origin/feat/remote-only"));

    // Asking again later switches to the local branch that now exists, keeping its
    // own commits.
    test.write("local.txt", "local\n");
    test.commit_all("local work");
    let local_head = test.head();
    test.repo.checkout("main").unwrap();
    test.repo.checkout("origin/feat/remote-only").unwrap();
    assert_eq!(
        test.repo.current_branch().unwrap().as_deref(),
        Some("feat/remote-only")
    );
    assert_eq!(test.head(), local_head);
}

#[test]
fn checking_out_a_commit_or_a_tag_detaches_head() {
    let test = TestRepo::with_commit();
    let first = test.head();
    test.git(&["tag", "v1"]);
    test.write("more.txt", "more\n");
    test.commit_all("more");

    test.repo.checkout("v1").unwrap();
    assert_eq!(test.repo.current_branch().unwrap(), None);
    assert_eq!(test.head(), first);
    test.repo.checkout("main").unwrap();
    test.repo.checkout(&first).unwrap();
    assert_eq!(test.repo.current_branch().unwrap(), None);

    assert!(matches!(
        test.repo.checkout("no-such-branch"),
        Err(GitError::Failed { .. })
    ));
}

#[test]
fn log_lists_the_newest_commits_first() {
    let test = TestRepo::new();
    let mut shas = Vec::new();
    for number in 0..5 {
        test.write("file.txt", format!("{number}\n"));
        shas.push(commit_at(
            &test,
            1_700_000_000 + number * 60,
            &format!("commit {number}"),
        ));
    }

    let log = test.repo.log(3, None).unwrap();
    assert_eq!(log.len(), 3);
    let subjects: Vec<&str> = log.iter().map(|commit| commit.subject.as_str()).collect();
    assert_eq!(subjects, ["commit 4", "commit 3", "commit 2"]);

    let newest = &log[0];
    assert_eq!(newest.sha, shas[4]);
    assert!(newest.sha.starts_with(&newest.short));
    assert!(newest.short.len() >= 7 && newest.short.len() < 40);
    assert_eq!(newest.author, "Test Author");
    assert_eq!(newest.time, 1_700_000_240);
    assert!(
        newest.relative_time.ends_with("ago"),
        "{}",
        newest.relative_time
    );

    assert_eq!(test.repo.log(100, None).unwrap().len(), 5);
    assert!(test.repo.log(0, None).unwrap().is_empty());
}

#[test]
fn log_is_not_disturbed_by_signature_checks_or_another_output_encoding() {
    let test = TestRepo::new();
    test.sign_commits_with_a_stand_in();
    test.write("a.txt", "a\n");
    test.commit_all("erster Commit mit \u{fc}");
    test.write("a.txt", "b\n");
    test.commit_all("zweiter Commit mit \u{df}");
    // What a user may have set for their own `git log`.
    test.git(&["config", "log.showSignature", "true"]);
    test.git(&["config", "i18n.logOutputEncoding", "ISO-8859-1"]);
    test.git(&["config", "log.decorate", "full"]);
    test.git(&["config", "format.pretty", "fuller"]);
    test.git(&["config", "log.date", "iso"]);

    let log = test.repo.log(10, None).unwrap();
    let subjects: Vec<&str> = log.iter().map(|commit| commit.subject.as_str()).collect();
    assert_eq!(
        subjects,
        ["zweiter Commit mit \u{df}", "erster Commit mit \u{fc}"]
    );
    assert_eq!(log[0].sha, test.head());
    assert!(log.iter().all(|commit| commit.sha.len() == 40));
    assert!(log.iter().all(|commit| commit.author == "Test Author"));
    assert_eq!(
        test.repo.head_commit_message().unwrap().as_deref(),
        Some("zweiter Commit mit \u{df}")
    );
    let branches = test.repo.branches().unwrap();
    assert_eq!(branches[0].subject, "zweiter Commit mit \u{df}");
}

#[test]
fn log_of_a_path_follows_renames() {
    let test = TestRepo::new();
    test.write("old.txt", "line 1\nline 2\nline 3\nline 4\n");
    test.write("unrelated.txt", "x\n");
    test.commit_all("add old");
    test.write("unrelated.txt", "y\n");
    test.commit_all("touch something else");
    std::fs::create_dir(test.path("dir with space")).unwrap();
    test.git(&["mv", "old.txt", "dir with space/new name.txt"]);
    test.commit_all("move it");
    test.write(
        "dir with space/new name.txt",
        "line 1\nline 2\nline 3\nline 4\nline 5\n",
    );
    test.commit_all("edit it");

    let subjects = |path: &Path| -> Vec<String> {
        test.repo
            .log(50, Some(path))
            .unwrap()
            .into_iter()
            .map(|commit| commit.subject)
            .collect()
    };
    assert_eq!(
        subjects(Path::new("dir with space/new name.txt")),
        ["edit it", "move it", "add old"]
    );
    // An absolute path means the same file.
    assert_eq!(
        subjects(&test.path("dir with space/new name.txt")),
        ["edit it", "move it", "add old"]
    );
    // A directory lists what changed below it.
    assert_eq!(
        subjects(Path::new("dir with space")),
        ["edit it", "move it"]
    );
    assert_eq!(
        subjects(Path::new("unrelated.txt")),
        ["touch something else", "add old"]
    );
    assert!(subjects(Path::new("never-existed.txt")).is_empty());
    // The top of the work tree is the whole history.
    assert_eq!(subjects(test.root()).len(), 4);
    assert_eq!(
        test.repo
            .log(1, Some(Path::new("unrelated.txt")))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn head_text_and_index_text_follow_commit_and_stage() {
    let test = TestRepo::new();
    test.write("src/lib.rs", "committed\n");
    test.commit_all("add");
    test.write("src/lib.rs", "staged\n");
    test.repo.stage(&["src/lib.rs"]).unwrap();
    test.write("src/lib.rs", "work tree\n");

    let head = |path: &str| test.repo.head_text(path).unwrap();
    let index = |path: &str| test.repo.index_text(path).unwrap();
    assert_eq!(head("src/lib.rs").as_deref(), Some("committed\n"));
    assert_eq!(index("src/lib.rs").as_deref(), Some("staged\n"));
    assert_eq!(
        test.repo
            .head_text(test.path("src/lib.rs"))
            .unwrap()
            .as_deref(),
        Some("committed\n")
    );

    // A new file is nowhere, then in the index, never at HEAD.
    test.write("new.txt", "new\n");
    assert_eq!(head("new.txt"), None);
    assert_eq!(index("new.txt"), None);
    test.repo.stage(&["new.txt"]).unwrap();
    assert_eq!(head("new.txt"), None);
    assert_eq!(index("new.txt").as_deref(), Some("new\n"));

    // A staged deletion is at HEAD and no longer in the index.
    test.git(&["rm", "--quiet", "--cached", "--force", "src/lib.rs"]);
    assert_eq!(head("src/lib.rs").as_deref(), Some("committed\n"));
    assert_eq!(index("src/lib.rs"), None);

    // Directories and the top of the work tree are not files.
    assert_eq!(head("src"), None);
    assert_eq!(index("src"), None);
    assert_eq!(head(""), None);
    assert_eq!(head("no/such/file.txt"), None);
}

#[test]
fn contents_before_the_first_commit() {
    let test = TestRepo::new();
    test.write("a.txt", "a\n");
    assert_eq!(test.repo.head_text("a.txt").unwrap(), None);
    assert_eq!(test.repo.index_text("a.txt").unwrap(), None);
    test.repo.stage_all().unwrap();
    assert_eq!(test.repo.head_text("a.txt").unwrap(), None);
    assert_eq!(
        test.repo.index_text("a.txt").unwrap().as_deref(),
        Some("a\n")
    );
}

#[test]
fn contents_of_files_with_unusual_names_and_bytes() {
    let test = TestRepo::new();
    let names = [
        "dir with space/\u{fc} \u{f1}.txt",
        "new\nline.txt",
        "1:colon.txt",
        "[id].tsx",
        "caret^{tree}.txt",
    ];
    for name in names {
        test.write(name, format!("content of {name}\n"));
    }
    test.write("latin1.txt", b"caf\xe9\n");
    test.write("binary.bin", b"\x00\x01\x02\xff");
    test.write("empty.txt", "");
    test.write("no-newline.txt", "no newline");
    test.write("crlf.txt", "one\r\ntwo\r\n");
    test.commit_all("odd files");

    for name in names {
        assert_eq!(
            test.repo.head_text(name).unwrap(),
            Some(format!("content of {name}\n")),
            "{name:?}"
        );
        assert_eq!(
            test.repo.index_text(name).unwrap(),
            Some(format!("content of {name}\n")),
            "{name:?}"
        );
    }
    assert_eq!(
        test.repo.head_bytes("latin1.txt").unwrap().as_deref(),
        Some(&b"caf\xe9\n"[..])
    );
    assert_eq!(
        test.repo.head_text("latin1.txt").unwrap().as_deref(),
        Some("caf\u{fffd}\n")
    );
    assert_eq!(
        test.repo.index_bytes("binary.bin").unwrap().as_deref(),
        Some(&b"\x00\x01\x02\xff"[..])
    );
    assert_eq!(
        test.repo.head_text("empty.txt").unwrap().as_deref(),
        Some("")
    );
    assert_eq!(
        test.repo.head_text("no-newline.txt").unwrap().as_deref(),
        Some("no newline")
    );
    assert_eq!(
        test.repo.head_text("crlf.txt").unwrap().as_deref(),
        Some("one\r\ntwo\r\n")
    );
}

#[test]
fn contents_come_back_the_way_a_checkout_writes_them() {
    let test = TestRepo::new();
    test.write("text.txt", "one\ntwo\n");
    test.commit_all("stored with line feeds");
    // From now on git checks text files out with CRLF line endings.
    test.write(".gitattributes", "*.txt text eol=crlf\n");

    assert_eq!(
        test.repo.head_text("text.txt").unwrap().as_deref(),
        Some("one\r\ntwo\r\n")
    );
    assert_eq!(
        test.repo.index_text("text.txt").unwrap().as_deref(),
        Some("one\r\ntwo\r\n")
    );
}

#[test]
fn a_conflicted_file_has_no_single_staged_version() {
    let test = TestRepo::new();
    test.write("conflict.txt", "base\n");
    test.commit_all("base");
    test.git(&["checkout", "--quiet", "-b", "other"]);
    test.write("conflict.txt", "theirs\n");
    test.commit_all("theirs");
    test.git(&["checkout", "--quiet", "main"]);
    test.write("conflict.txt", "ours\n");
    test.commit_all("ours");
    assert!(test.try_git(&["merge", "other"]).is_err());

    assert_eq!(test.repo.index_text("conflict.txt").unwrap(), None);
    assert_eq!(
        test.repo.head_text("conflict.txt").unwrap().as_deref(),
        Some("ours\n")
    );
}

#[test]
fn a_submodule_is_not_a_file() {
    let test = TestRepo::with_commit();
    let library = test.sandbox().join("library");
    common::init_repository(&library);
    std::fs::write(library.join("lib.txt"), "library\n").unwrap();
    git_in(&library, &["add", "--all"]);
    git_in(&library, &["commit", "--quiet", "--message", "library"]);
    test.git(&[
        "-c",
        "protocol.file.allow=always",
        "submodule",
        "--quiet",
        "add",
        library.to_str().unwrap(),
        "vendor/library",
    ]);
    test.commit_all("add the library");

    assert_eq!(test.repo.head_text("vendor/library").unwrap(), None);
    assert_eq!(test.repo.index_text("vendor/library").unwrap(), None);
    assert!(test.repo.head_text(".gitmodules").unwrap().is_some());
}
