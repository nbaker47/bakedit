//! Fetch, pull and push against a bare repository in the temp directory, and the limits
//! on how long a git process may keep the caller waiting.

mod common;

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use bakedit_git::{CommitOptions, GitError, RepoState};
use common::{git_in, TestRepo};

/// Makes one commit in another clone of `remote` and pushes it.
fn commit_elsewhere(test: &TestRepo, remote: &Path, clone: &str, file: &str, content: &str) {
    let other = test.sandbox().join(clone);
    if !other.exists() {
        test.clone_of(remote, clone);
    }
    git_in(&other, &["pull", "--quiet", "--ff-only"]);
    fs::write(other.join(file), content).unwrap();
    git_in(&other, &["add", "--all"]);
    git_in(
        &other,
        &["commit", "--quiet", "--message", "change from elsewhere"],
    );
    git_in(&other, &["push", "--quiet"]);
}

fn remote_head(remote: &Path, branch: &str) -> String {
    git_in(remote, &["rev-parse", branch]).trim().to_string()
}

fn process_is_alive(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid])
        .output()
        .is_ok_and(|output| output.status.success())
}

#[test]
fn fetch_without_remotes_does_nothing() {
    let test = TestRepo::with_commit();
    assert_eq!(test.repo.fetch().unwrap(), "");
}

#[test]
fn fetch_brings_in_remote_commits_without_touching_the_work_tree() {
    let test = TestRepo::with_commit();
    let remote = test.add_origin();
    commit_elsewhere(&test, &remote, "elsewhere", "remote.txt", "remote\n");

    let output = test.repo.fetch().unwrap();
    assert!(output.contains("origin/main"), "{output}");
    let status = test.repo.status().unwrap();
    assert_eq!((status.ahead, status.behind), (0, 1));
    assert!(!test.exists("remote.txt"));

    // Nothing new: nothing to say.
    assert_eq!(test.repo.fetch().unwrap(), "");
}

#[test]
fn pull_fast_forwards() {
    let test = TestRepo::with_commit();
    let remote = test.add_origin();
    commit_elsewhere(&test, &remote, "elsewhere", "remote.txt", "remote\n");

    let output = test.repo.pull(false).unwrap();
    assert!(output.contains("remote.txt"), "{output}");
    assert_eq!(test.read("remote.txt"), "remote\n");
    assert_eq!(test.head(), remote_head(&remote, "main"));
    let status = test.repo.status().unwrap();
    assert_eq!((status.ahead, status.behind), (0, 0));

    let output = test.repo.pull(false).unwrap();
    assert!(output.contains("Already up to date"), "{output}");
}

#[test]
fn pull_with_rebase_replays_local_commits_on_top() {
    let test = TestRepo::with_commit();
    let remote = test.add_origin();
    commit_elsewhere(&test, &remote, "elsewhere", "remote.txt", "remote\n");
    test.write("local.txt", "local\n");
    test.commit_all("local commit");

    test.repo.pull(true).unwrap();
    assert_eq!(
        test.git(&["rev-parse", "HEAD~1"]).trim(),
        remote_head(&remote, "main")
    );
    let parents = test.git(&["show", "--no-patch", "--format=%P", "HEAD"]);
    assert_eq!(parents.split_whitespace().count(), 1, "no merge commit");
    assert!(test.exists("remote.txt") && test.exists("local.txt"));
    let status = test.repo.status().unwrap();
    assert_eq!((status.ahead, status.behind, status.state), (1, 0, None));
}

#[test]
fn pull_merges_when_the_configuration_says_so_and_never_opens_an_editor() {
    let test = TestRepo::with_commit();
    let remote = test.add_origin();
    test.git(&["config", "pull.rebase", "false"]);
    // An editor that would hang forever if git ever started it.
    test.git(&["config", "core.editor", "sleep 600"]);
    commit_elsewhere(&test, &remote, "elsewhere", "remote.txt", "remote\n");
    test.write("local.txt", "local\n");
    test.commit_all("local commit");

    test.repo.pull(false).unwrap();
    let parents = test.git(&["show", "--no-patch", "--format=%P", "HEAD"]);
    assert_eq!(parents.split_whitespace().count(), 2, "a merge commit");
    assert!(test.exists("remote.txt") && test.exists("local.txt"));
}

#[test]
fn pull_of_diverged_branches_without_a_configured_way_to_join_them_is_refused() {
    let test = TestRepo::with_commit();
    let remote = test.add_origin();
    commit_elsewhere(&test, &remote, "elsewhere", "remote.txt", "remote\n");
    test.write("local.txt", "local\n");
    test.commit_all("local commit");
    let before = test.head();

    let error = test.repo.pull(false).unwrap_err();
    let output = error.git_output().unwrap();
    assert!(output.contains("divergent branches"), "{output}");
    assert_eq!(test.head(), before);
    assert_eq!(test.repo.status().unwrap().state, None);
    // Asking for a rebase is one way to settle it.
    test.repo.pull(true).unwrap();
    assert!(test.exists("remote.txt") && test.exists("local.txt"));
}

#[test]
fn pull_that_conflicts_fails_and_leaves_the_conflict_to_resolve() {
    let test = TestRepo::with_commit();
    let remote = test.add_origin();
    test.git(&["config", "pull.rebase", "false"]);
    commit_elsewhere(&test, &remote, "elsewhere", "README.md", "# theirs\n");
    test.write("README.md", "# ours\n");
    test.commit_all("our change");

    let error = test.repo.pull(false).unwrap_err();
    assert_eq!(error.exit_code(), Some(1));
    assert!(error
        .to_string()
        .starts_with("git pull failed (exit code 1)"));
    assert!(error.git_output().unwrap().contains("CONFLICT"));
    match error {
        // Everything git said, on either of its output streams, is the error's text.
        GitError::Failed {
            command,
            stdout,
            stderr,
            ..
        } => {
            assert_eq!(command, "pull");
            assert_eq!(stdout, "");
            assert!(stderr.contains("CONFLICT"), "{stderr}");
            assert!(stderr.contains("Automatic merge failed"), "{stderr}");
        }
        other => panic!("unexpected {other:?}"),
    }
    let status = test.repo.status().unwrap();
    assert_eq!(status.state, Some(RepoState::Merging));
    assert!(status.entry("README.md").unwrap().conflicted);

    // Resolving and committing concludes it.
    test.write("README.md", "# both\n");
    test.repo.stage(&["README.md"]).unwrap();
    test.repo.commit("merge", CommitOptions::default()).unwrap();
    assert_eq!(test.repo.status().unwrap().state, None);
}

#[test]
fn pull_with_rebase_that_conflicts_leaves_a_rebase_in_progress() {
    let test = TestRepo::with_commit();
    let remote = test.add_origin();
    commit_elsewhere(&test, &remote, "elsewhere", "README.md", "# theirs\n");
    test.write("README.md", "# ours\n");
    test.commit_all("our change");

    assert!(matches!(test.repo.pull(true), Err(GitError::Failed { .. })));
    let status = test.repo.status().unwrap();
    assert_eq!(status.state, Some(RepoState::Rebasing));
    assert_eq!(status.conflicts().len(), 1);
    test.git(&["rebase", "--abort"]);
}

#[test]
fn pull_without_an_upstream_is_an_error_from_git() {
    let test = TestRepo::with_commit();
    test.add_origin();
    test.git(&["checkout", "--quiet", "-b", "untracked-branch"]);
    match test.repo.pull(false) {
        Err(error @ GitError::Failed { .. }) => {
            let output = error.git_output().unwrap();
            assert!(output.contains("no tracking information"), "{output}");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn the_first_push_publishes_the_branch_and_sets_its_upstream() {
    let test = TestRepo::with_commit();
    let remote = test.add_origin();
    test.repo.create_branch("feat/topic", true).unwrap();
    test.write("topic.txt", "topic\n");
    test.commit_all("topic work");
    assert_eq!(test.repo.status().unwrap().upstream, None);

    let output = test.repo.push(false).unwrap();
    assert!(output.contains("feat/topic"), "{output}");
    assert_eq!(remote_head(&remote, "feat/topic"), test.head());
    let status = test.repo.status().unwrap();
    assert_eq!(status.upstream.as_deref(), Some("origin/feat/topic"));
    assert_eq!((status.ahead, status.behind), (0, 0));

    // From now on a plain push and pull work.
    test.write("topic.txt", "more topic\n");
    test.commit_all("more topic work");
    assert_eq!(test.repo.status().unwrap().ahead, 1);
    test.repo.push(false).unwrap();
    assert_eq!(remote_head(&remote, "feat/topic"), test.head());
    assert!(test
        .repo
        .pull(false)
        .unwrap()
        .contains("Already up to date"));
}

#[test]
fn a_push_that_would_lose_commits_is_rejected_unless_forced_with_lease() {
    let test = TestRepo::with_commit();
    let remote = test.add_origin();
    commit_elsewhere(&test, &remote, "elsewhere", "remote.txt", "remote\n");
    test.write("local.txt", "local\n");
    test.commit_all("local commit");

    match test.repo.push(false) {
        Err(error @ GitError::Failed { .. }) => {
            assert_eq!(error.exit_code(), Some(1));
            assert!(error.git_output().unwrap().contains("rejected"));
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_ne!(remote_head(&remote, "main"), test.head());

    // The lease is on what was last fetched: without a fetch the remote has moved
    // behind our back and even the forced push is refused.
    match test.repo.push(true) {
        Err(error @ GitError::Failed { .. }) => {
            assert!(error.git_output().unwrap().contains("stale info"));
        }
        other => panic!("unexpected {other:?}"),
    }

    test.repo.fetch().unwrap();
    let output = test.repo.push(true).unwrap();
    assert!(output.contains("forced update"), "{output}");
    assert_eq!(remote_head(&remote, "main"), test.head());
}

#[test]
fn force_with_lease_also_publishes_a_new_branch() {
    let test = TestRepo::with_commit();
    let remote = test.add_origin();
    test.repo.create_branch("fresh", true).unwrap();
    test.repo.push(true).unwrap();
    assert_eq!(remote_head(&remote, "fresh"), test.head());
    assert_eq!(
        test.repo.status().unwrap().upstream.as_deref(),
        Some("origin/fresh")
    );
}

#[test]
fn publishing_picks_the_push_default_then_origin_then_the_only_remote() {
    let test = TestRepo::with_commit();
    assert!(matches!(test.repo.push(false), Err(GitError::NoRemote)));

    let bare = |name: &str| {
        let path = test.sandbox().join(name);
        fs::create_dir_all(&path).unwrap();
        git_in(&path, &["init", "--quiet", "--bare"]);
        path
    };
    let (company, fork) = (bare("company.git"), bare("fork.git"));

    // The only remote, whatever it is called.
    test.git(&["remote", "add", "company", company.to_str().unwrap()]);
    test.repo.push(false).unwrap();
    assert_eq!(remote_head(&company, "main"), test.head());
    assert_eq!(
        test.repo.status().unwrap().upstream.as_deref(),
        Some("company/main")
    );

    // Two remotes, neither called origin: no way to choose.
    test.git(&["remote", "add", "fork", fork.to_str().unwrap()]);
    test.repo.create_branch("second", true).unwrap();
    assert!(matches!(test.repo.push(false), Err(GitError::NoRemote)));

    // The user's configured default settles it.
    test.git(&["config", "remote.pushDefault", "fork"]);
    test.repo.push(false).unwrap();
    assert_eq!(remote_head(&fork, "second"), test.head());
    assert_eq!(
        test.repo.status().unwrap().upstream.as_deref(),
        Some("fork/second")
    );
}

#[test]
fn pushing_and_pulling_before_the_first_commit_are_errors_from_git() {
    let test = TestRepo::new();
    let remote = test.sandbox().join("origin.git");
    fs::create_dir_all(&remote).unwrap();
    git_in(&remote, &["init", "--quiet", "--bare"]);
    test.git(&["remote", "add", "origin", remote.to_str().unwrap()]);

    // Nothing to push yet, nothing to pull from yet; fetching an empty remote is fine.
    assert!(matches!(
        test.repo.push(false),
        Err(GitError::Failed { .. })
    ));
    assert!(matches!(
        test.repo.pull(false),
        Err(GitError::Failed { .. })
    ));
    assert_eq!(test.repo.fetch().unwrap(), "");
    assert!(test.repo.status().unwrap().is_clean());
}

#[test]
fn pushing_a_detached_head_is_an_error_from_git() {
    let test = TestRepo::with_commit();
    test.add_origin();
    test.git(&["checkout", "--quiet", "--detach"]);
    match test.repo.push(false) {
        Err(error @ GitError::Failed { .. }) => {
            assert!(error
                .git_output()
                .unwrap()
                .contains("not currently on a branch"));
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn a_network_command_that_hangs_is_stopped_with_everything_it_started() {
    let mut test = TestRepo::with_commit();
    test.add_origin();
    let pid_file = test.sandbox().join("hook.pid");
    test.install_hook(
        "pre-push",
        &format!(
            "#!/bin/sh\necho $$ > '{}'\necho 'slow hook started' >&2\nsleep 600\n",
            pid_file.display()
        ),
    );
    test.write("a.txt", "a\n");
    test.commit_all("something to push");
    let remote_before = test.git(&["rev-parse", "origin/main"]);

    test.repo.set_network_timeout(Some(Duration::from_secs(3)));
    let started = Instant::now();
    let error = test.repo.push(false).unwrap_err();
    let elapsed = started.elapsed();

    match &error {
        GitError::TimedOut {
            command,
            limit,
            output,
        } => {
            assert_eq!(command, "push");
            assert_eq!(*limit, Duration::from_secs(3));
            assert!(output.contains("slow hook started"), "{output}");
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(elapsed >= Duration::from_secs(3), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(30), "{elapsed:?}");
    assert_eq!(error.git_output(), Some("slow hook started"));

    // The hook (and with it the sleep it was waiting for) is gone, not orphaned.
    let hook_pid = fs::read_to_string(&pid_file).unwrap();
    let hook_pid = hook_pid.trim();
    let deadline = Instant::now() + Duration::from_secs(10);
    while process_is_alive(hook_pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!process_is_alive(hook_pid), "the hook is still running");
    // Nothing was pushed, and the repository is usable afterwards.
    assert_eq!(test.git(&["rev-parse", "origin/main"]), remote_before);
    assert!(test.repo.status().unwrap().is_clean());
}

#[test]
fn without_a_time_limit_a_slow_command_is_waited_for() {
    let mut test = TestRepo::with_commit();
    let remote = test.add_origin();
    test.install_hook("pre-push", "#!/bin/sh\nsleep 1\n");
    test.write("a.txt", "a\n");
    test.commit_all("something to push");

    test.repo.set_network_timeout(None);
    let started = Instant::now();
    test.repo.push(false).unwrap();
    assert!(started.elapsed() >= Duration::from_secs(1));
    assert_eq!(remote_head(&remote, "main"), test.head());
}

#[test]
fn a_background_job_left_by_a_hook_does_not_keep_the_caller_waiting() {
    let test = TestRepo::with_commit();
    let pid_file = test.sandbox().join("background.pid");
    // The job inherits git's output pipes and keeps them open long after git is done.
    test.install_hook(
        "post-commit",
        &format!(
            "#!/bin/sh\nsleep 600 &\necho $! > '{}'\necho 'hook done'\n",
            pid_file.display()
        ),
    );
    test.write("a.txt", "a\n");
    test.repo.stage_all().unwrap();

    let started = Instant::now();
    let sha = test
        .repo
        .commit("commit with a leaky hook", CommitOptions::default());
    let elapsed = started.elapsed();

    // Clean up the job this test started before judging anything.
    let background_pid = fs::read_to_string(&pid_file).unwrap();
    let background_pid = background_pid.trim().to_string();
    assert!(process_is_alive(&background_pid));
    Command::new("kill").arg(&background_pid).status().unwrap();

    assert_eq!(sha.unwrap(), test.head());
    assert!(elapsed < Duration::from_secs(60), "{elapsed:?}");
}
