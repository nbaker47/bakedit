//! `Repo::permalink` against real repositories. No network: a remote is only a URL in
//! the configuration.

mod common;

use bakedit_git::GitError;
use common::TestRepo;

/// A repository with one commit and `origin` pointing at `url`.
fn with_origin(url: &str) -> TestRepo {
    let test = TestRepo::with_commit();
    test.write("src/main.rs", "fn main() {}\n");
    test.commit_all("main");
    test.git(&["remote", "add", "origin", url]);
    test
}

#[test]
fn github_gitlab_and_bitbucket_remotes() {
    for (url, expected) in [
        (
            "git@github.com:owner/repo.git",
            "https://github.com/owner/repo/blob/{sha}/src/main.rs#L10-L20",
        ),
        (
            "https://github.com/owner/repo",
            "https://github.com/owner/repo/blob/{sha}/src/main.rs#L10-L20",
        ),
        (
            "ssh://git@github.com:22/owner/repo.git",
            "https://github.com/owner/repo/blob/{sha}/src/main.rs#L10-L20",
        ),
        (
            "git@gitlab.com:group/subgroup/project.git",
            "https://gitlab.com/group/subgroup/project/-/blob/{sha}/src/main.rs#L10-20",
        ),
        (
            "https://user:secret@bitbucket.org/team/project.git",
            "https://bitbucket.org/team/project/src/{sha}/src/main.rs#lines-10:20",
        ),
        (
            "ssh://git@git.example.com:7999/team/project.git",
            "https://git.example.com/team/project/blob/{sha}/src/main.rs#L10-L20",
        ),
    ] {
        let test = with_origin(url);
        let link = test.repo.permalink("src/main.rs", 10, 20).unwrap();
        assert_eq!(link, expected.replace("{sha}", &test.head()), "{url}");
    }
}

#[test]
fn single_lines_absolute_paths_and_names_that_need_encoding() {
    let test = with_origin("git@github.com:owner/repo.git");
    test.write("docs/my notes [draft].txt", "notes\n");
    test.write("docs/README.md", "# docs\n");
    test.commit_all("docs");
    let sha = test.head();

    assert_eq!(
        test.repo.permalink("src/main.rs", 7, 7).unwrap(),
        format!("https://github.com/owner/repo/blob/{sha}/src/main.rs#L7")
    );
    assert_eq!(
        test.repo.permalink(test.path("src/main.rs"), 3, 1).unwrap(),
        format!("https://github.com/owner/repo/blob/{sha}/src/main.rs#L1-L3")
    );
    assert_eq!(
        test.repo
            .permalink("docs/my notes [draft].txt", 1, 1)
            .unwrap(),
        format!("https://github.com/owner/repo/blob/{sha}/docs/my%20notes%20%5Bdraft%5D.txt#L1")
    );
    assert_eq!(
        test.repo.permalink("docs/README.md", 1, 2).unwrap(),
        format!("https://github.com/owner/repo/blob/{sha}/docs/README.md?plain=1#L1-L2")
    );
}

#[test]
fn the_link_names_the_commit_that_is_checked_out() {
    let test = with_origin("git@github.com:owner/repo.git");
    let first = test.head();
    test.write("src/main.rs", "fn main() { println!(); }\n");
    test.commit_all("second");
    let second = test.head();
    assert_ne!(first, second);

    let link = test.repo.permalink("src/main.rs", 1, 1).unwrap();
    assert!(link.contains(&second) && !link.contains(&first));
    // Uncommitted edits do not change it.
    test.write("src/main.rs", "fn main() { unsaved(); }\n");
    assert_eq!(test.repo.permalink("src/main.rs", 1, 1).unwrap(), link);

    // A detached HEAD links to its own commit, through origin.
    test.git(&["checkout", "--quiet", "--force", "--detach", &first]);
    let link = test.repo.permalink("src/main.rs", 1, 1).unwrap();
    assert_eq!(
        link,
        format!("https://github.com/owner/repo/blob/{first}/src/main.rs#L1")
    );
}

#[test]
fn the_upstream_remote_of_the_branch_wins_over_origin() {
    let test = with_origin("git@github.com:me/fork.git");
    test.git(&[
        "remote",
        "add",
        "company",
        "git@gitlab.example.com:team/project.git",
    ]);
    let sha = test.head();

    // No upstream: origin.
    assert_eq!(
        test.repo.permalink("src/main.rs", 1, 1).unwrap(),
        format!("https://github.com/me/fork/blob/{sha}/src/main.rs#L1")
    );
    // The branch tracks a branch of the other remote.
    test.git(&["config", "branch.main.remote", "company"]);
    test.git(&["config", "branch.main.merge", "refs/heads/main"]);
    assert_eq!(
        test.repo.permalink("src/main.rs", 1, 1).unwrap(),
        format!("https://gitlab.example.com/team/project/-/blob/{sha}/src/main.rs#L1")
    );
    // Tracking a local branch says nothing about a site: origin again.
    test.git(&["config", "branch.main.remote", "."]);
    assert_eq!(
        test.repo.permalink("src/main.rs", 1, 1).unwrap(),
        format!("https://github.com/me/fork/blob/{sha}/src/main.rs#L1")
    );
    // So does a remote that no longer exists.
    test.git(&["config", "branch.main.remote", "removed"]);
    assert_eq!(
        test.repo.permalink("src/main.rs", 1, 1).unwrap(),
        format!("https://github.com/me/fork/blob/{sha}/src/main.rs#L1")
    );
}

#[test]
fn without_origin_the_only_remote_is_used() {
    let test = TestRepo::with_commit();
    assert!(matches!(
        test.repo.permalink("README.md", 1, 1),
        Err(GitError::NoRemote)
    ));

    test.git(&["remote", "add", "github", "git@github.com:owner/repo.git"]);
    let sha = test.head();
    assert_eq!(
        test.repo.permalink("README.md", 2, 2).unwrap(),
        format!("https://github.com/owner/repo/blob/{sha}/README.md?plain=1#L2")
    );

    // Two remotes and neither is origin: no way to choose.
    test.git(&["remote", "add", "mirror", "git@gitlab.com:owner/repo.git"]);
    assert!(matches!(
        test.repo.permalink("README.md", 1, 1),
        Err(GitError::NoRemote)
    ));
}

#[test]
fn url_rewrites_from_the_configuration_are_applied() {
    let test = with_origin("gh:owner/repo.git");
    test.git(&["config", "url.git@github.com:.insteadOf", "gh:"]);
    let sha = test.head();
    assert_eq!(
        test.repo.permalink("src/main.rs", 5, 6).unwrap(),
        format!("https://github.com/owner/repo/blob/{sha}/src/main.rs#L5-L6")
    );
}

#[test]
fn what_cannot_be_linked_is_a_typed_error() {
    // No commit to point at.
    let empty = TestRepo::new();
    empty.git(&["remote", "add", "origin", "git@github.com:owner/repo.git"]);
    assert!(matches!(
        empty.repo.permalink("file.txt", 1, 1),
        Err(GitError::NoCommits)
    ));

    // A remote that is a directory on this machine has no web site.
    let test = TestRepo::with_commit();
    let remote = test.add_origin();
    match test.repo.permalink("README.md", 1, 1) {
        Err(GitError::UnsupportedRemoteUrl(url)) => {
            assert_eq!(url, remote.to_str().unwrap())
        }
        other => panic!("unexpected {other:?}"),
    }

    // A file outside the work tree.
    let test = with_origin("git@github.com:owner/repo.git");
    assert!(matches!(
        test.repo
            .permalink(test.sandbox().join("elsewhere.rs"), 1, 1),
        Err(GitError::PathOutsideRepo(_))
    ));
}
