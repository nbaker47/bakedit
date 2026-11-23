//! The watcher against the real file system. Events take a moment to arrive, so every
//! expectation polls with a deadline, and nothing here waits forever.

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use bakedit_project::{FsBatch, WatchError, Watcher};
use common::{wait_until, Project};

/// How long an expected event may take.
const TIMEOUT: Duration = Duration::from_secs(15);

/// How long to listen when nothing is expected to arrive.
const QUIET: Duration = Duration::from_millis(600);

/// The operating system limits how many directories one process may watch at once, so
/// the tests take turns.
fn one_at_a_time() -> MutexGuard<'static, ()> {
    static TURN: Mutex<()> = Mutex::new(());
    TURN.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Watched {
    watcher: Watcher,
    wakes: Arc<AtomicUsize>,
    /// Every batch received so far.
    batches: Vec<FsBatch>,
}

impl Watched {
    fn start(root: &Path) -> Watched {
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&wakes);
        let watcher = Watcher::new(
            root,
            Box::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            }),
        )
        .expect("the watcher starts");
        let mut watched = Watched {
            watcher,
            wakes,
            batches: Vec::new(),
        };
        // Changes made while the project was being set up may still trickle in.
        watched.listen(Duration::from_millis(300));
        watched.batches.clear();
        watched
    }

    /// Receive batches for `duration`.
    fn listen(&mut self, duration: Duration) {
        let deadline = Instant::now() + duration;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match self.watcher.events().recv_timeout(left) {
                Ok(batch) => self.batches.push(batch),
                Err(_) => break,
            }
        }
    }

    /// Receive batches until `done` holds for everything received, or the timeout
    /// passes. Returns whether it held.
    fn wait_for(&mut self, mut done: impl FnMut(&[FsBatch]) -> bool) -> bool {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if done(&self.batches) {
                return true;
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            match self.watcher.events().recv_timeout(left) {
                Ok(batch) => self.batches.push(batch),
                Err(_) => return done(&self.batches),
            }
        }
    }

    fn wait_for_path(&mut self, path: &Path) -> bool {
        self.wait_for(|batches| {
            batches
                .iter()
                .any(|batch| batch.paths.iter().any(|p| p == path))
        })
    }

    fn paths(&self) -> BTreeSet<PathBuf> {
        self.batches
            .iter()
            .flat_map(|batch| batch.paths.iter().cloned())
            .collect()
    }

    fn forget(&mut self) {
        self.batches.clear();
    }
}

#[test]
fn created_changed_renamed_and_removed_files_are_reported() {
    let _turn = one_at_a_time();
    let project = Project::with_files(&["existing.txt"]);
    let mut watched = Watched::start(project.root());

    let created = project.write("created.txt", "one");
    assert!(
        watched.wait_for_path(&created),
        "creation: {:?}",
        watched.batches
    );

    watched.forget();
    project.write("existing.txt", "changed contents");
    assert!(
        watched.wait_for_path(&project.path("existing.txt")),
        "modification: {:?}",
        watched.batches
    );

    watched.forget();
    fs::rename(&created, project.path("renamed.txt")).unwrap();
    assert!(
        watched.wait_for(|batches| {
            let seen: BTreeSet<&PathBuf> = batches.iter().flat_map(|batch| &batch.paths).collect();
            seen.contains(&created) && seen.contains(&project.path("renamed.txt"))
        }),
        "a rename reports the old and the new path: {:?}",
        watched.batches
    );

    watched.forget();
    project.remove("renamed.txt");
    assert!(
        watched.wait_for_path(&project.path("renamed.txt")),
        "removal: {:?}",
        watched.batches
    );

    // Paths are absolute, under the root as it was given, and nothing set the flags.
    for batch in &watched.batches {
        assert!(!batch.git_changed && !batch.rescan, "{batch:?}");
        assert!(batch
            .paths
            .iter()
            .all(|path| path.starts_with(project.root())));
    }
}

#[test]
fn new_directories_and_what_is_put_in_them_are_reported() {
    let _turn = one_at_a_time();
    let project = Project::new();
    let mut watched = Watched::start(project.root());

    let deep = project.write("a/b/c/deep.txt", "x");
    assert!(watched.wait_for_path(&deep), "{:?}", watched.batches);

    watched.forget();
    let later = project.write("a/b/c/later.txt", "y");
    assert!(watched.wait_for_path(&later), "{:?}", watched.batches);

    watched.forget();
    project.remove("a");
    assert!(
        watched.wait_for(|batches| batches
            .iter()
            .flat_map(|batch| &batch.paths)
            .any(|path| path.starts_with(project.path("a")))),
        "{:?}",
        watched.batches
    );
}

#[test]
fn a_burst_of_writes_is_one_path_in_few_batches() {
    let _turn = one_at_a_time();
    let project = Project::new();
    let mut watched = Watched::start(project.root());

    let busy = project.path("busy.txt");
    for round in 0..40 {
        fs::write(&busy, format!("round {round}")).unwrap();
    }
    assert!(watched.wait_for_path(&busy));
    watched.listen(QUIET);

    for batch in &watched.batches {
        let unique: BTreeSet<&PathBuf> = batch.paths.iter().collect();
        assert_eq!(
            unique.len(),
            batch.paths.len(),
            "no path twice in a batch: {batch:?}"
        );
        let mut sorted = batch.paths.clone();
        sorted.sort();
        assert_eq!(sorted, batch.paths, "paths are sorted");
        assert!(!batch.is_empty(), "empty batches are not sent");
    }
    assert!(
        watched.batches.len() <= 5,
        "40 writes in a row arrived as {} batches",
        watched.batches.len()
    );
    assert_eq!(watched.paths(), BTreeSet::from([busy]));
}

#[test]
fn changes_inside_ignored_directories_are_dropped() {
    let _turn = one_at_a_time();
    let project = Project::new();
    project.write(".gitignore", "target/\nnode_modules/\n");
    project.mkdir("target/debug");
    let mut watched = Watched::start(project.root());

    // A build: lots of churn where nobody is looking.
    for n in 0..200 {
        project.write(&format!("target/debug/deps/unit{n}.o"), "object code");
    }
    project.write("node_modules/pkg/index.js", "module.exports = 1");
    project.remove("target/debug/deps/unit0.o");
    // Something real afterwards proves the watcher was listening the whole time.
    let marker = project.write("src/main.rs", "fn main() {}");
    assert!(watched.wait_for_path(&marker), "{:?}", watched.batches);
    watched.listen(QUIET);

    for path in watched.paths() {
        let relative = path.strip_prefix(project.root()).unwrap();
        let depth = relative.components().count();
        let in_ignored = relative.starts_with("target") || relative.starts_with("node_modules");
        assert!(
            !in_ignored || depth == 1,
            "{} is inside an ignored directory",
            relative.display()
        );
    }
    assert!(watched.batches.iter().all(|batch| !batch.rescan));
}

#[test]
fn an_ignored_directory_that_is_open_in_the_tree_is_reported() {
    let _turn = one_at_a_time();
    let project = Project::new();
    project.write(".gitignore", "dist/\n");
    project.mkdir("dist/assets");
    let mut watched = Watched::start(project.root());
    assert_eq!(watched.watcher.root(), project.root());

    // Closed: nothing inside it is reported.
    project.write("dist/one.js", "1");
    let marker = project.write("marker-one.txt", "x");
    assert!(watched.wait_for_path(&marker));
    watched.listen(QUIET);
    assert_eq!(watched.paths(), BTreeSet::from([marker]));

    // Open: its direct children are, deeper ones still are not.
    watched.forget();
    watched.watcher.watch_ignored_dirs(&[project.path("dist")]);
    let direct = project.write("dist/two.js", "2");
    project.write("dist/assets/deep.png", "3");
    assert!(watched.wait_for_path(&direct), "{:?}", watched.batches);
    watched.listen(QUIET);
    assert!(!watched
        .paths()
        .contains(&project.path("dist/assets/deep.png")));

    // Closed again.
    watched.forget();
    watched.watcher.watch_ignored_dirs::<PathBuf>(&[]);
    project.write("dist/three.js", "3");
    let marker = project.write("marker-two.txt", "x");
    assert!(watched.wait_for_path(&marker));
    watched.listen(QUIET);
    assert_eq!(watched.paths(), BTreeSet::from([marker]));
}

#[test]
fn always_hidden_names_and_own_temp_files_are_dropped() {
    let _turn = one_at_a_time();
    let project = Project::new();
    project.mkdir("docs");
    let mut watched = Watched::start(project.root());

    project.write(".DS_Store", "finder droppings");
    project.write("docs/.DS_Store", "more of them");
    project.write("docs/.bakedit-tmp-1-2-3", "half-written");
    let marker = project.write("docs/real.md", "# real");
    assert!(watched.wait_for_path(&marker));
    watched.listen(QUIET);
    assert_eq!(watched.paths(), BTreeSet::from([marker]));
}

#[test]
fn git_state_changes_set_the_flag_and_list_no_paths() {
    let _turn = one_at_a_time();
    let project = Project::new();
    project.write(".git/HEAD", "ref: refs/heads/main\n");
    project.write(".git/index", "index v1");
    project.mkdir(".git/objects/ab");
    project.mkdir(".git/refs/heads");
    let mut watched = Watched::start(project.root());

    // Object churn and lock files are not state.
    project.write(".git/objects/ab/cdef", "blob");
    project.write(".git/index.lock", "");
    project.remove(".git/index.lock");
    watched.listen(QUIET);
    assert!(
        watched.batches.is_empty(),
        "git noise produced {:?}",
        watched.batches
    );

    // Staging rewrites the index.
    project.write(".git/index", "index v2");
    assert!(watched.wait_for(|batches| batches.iter().any(|batch| batch.git_changed)));
    assert!(
        watched.paths().is_empty(),
        "git paths are not listed: {:?}",
        watched.batches
    );

    // A commit moves a branch.
    watched.forget();
    project.write(".git/refs/heads/main", "0123456789abcdef\n");
    assert!(watched.wait_for(|batches| batches.iter().any(|batch| batch.git_changed)));

    // Switching branches rewrites HEAD.
    watched.forget();
    project.write(".git/HEAD", "ref: refs/heads/other\n");
    assert!(watched.wait_for(|batches| batches.iter().any(|batch| batch.git_changed)));
    assert!(watched.paths().is_empty());

    // An ordinary file change does not set the flag.
    watched.forget();
    let plain = project.write("notes.txt", "plain");
    assert!(watched.wait_for_path(&plain));
    assert!(watched.batches.iter().all(|batch| !batch.git_changed));
}

#[test]
fn a_linked_worktree_sees_its_git_state_change() {
    let _turn = one_at_a_time();
    let home = Project::new();
    // The main checkout's git directory, with a worktree registered in it.
    home.write("main/.git/HEAD", "ref: refs/heads/main\n");
    home.mkdir("main/.git/refs/heads");
    home.write(
        "main/.git/worktrees/feature/HEAD",
        "ref: refs/heads/feature\n",
    );
    home.write("main/.git/worktrees/feature/index", "index v1");
    home.write("main/.git/worktrees/feature/commondir", "../..\n");
    // The worktree itself, somewhere else, pointing back with a `.git` file.
    home.write(
        "worktrees/feature/.git",
        format!(
            "gitdir: {}\n",
            home.path("main/.git/worktrees/feature").display()
        ),
    );
    home.write("worktrees/feature/src/lib.rs", "");
    let mut watched = Watched::start(&home.path("worktrees/feature"));

    // The worktree's own index lives outside the watched directory.
    home.write("main/.git/worktrees/feature/index", "index v2");
    assert!(
        watched.wait_for(|batches| batches.iter().any(|batch| batch.git_changed)),
        "the worktree's index changed: {:?}",
        watched.batches
    );
    assert!(watched.paths().is_empty());

    // Its branch lives in the shared refs.
    watched.forget();
    home.write("main/.git/refs/heads/feature", "0123456789abcdef\n");
    assert!(
        watched.wait_for(|batches| batches.iter().any(|batch| batch.git_changed)),
        "the worktree's branch moved: {:?}",
        watched.batches
    );

    // The main checkout's own HEAD and index are not this worktree's business.
    watched.forget();
    home.write("main/.git/HEAD", "ref: refs/heads/other\n");
    home.write("main/.git/index", "the main checkout's index");
    watched.listen(QUIET);
    assert!(watched.batches.is_empty(), "{:?}", watched.batches);

    // Ordinary files in the worktree still come through.
    let file = home.write("worktrees/feature/src/new.rs", "");
    assert!(watched.wait_for_path(&file));
}

/// Run git in `dir`, cut off from the user's own configuration. Returns whether it worked,
/// which it does not when git is not installed.
fn git(dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Wait for a batch with `git_changed`, and check that no git-internal path is listed.
fn expect_git_change(watched: &mut Watched, what: &str) {
    assert!(
        watched.wait_for(|batches| batches.iter().any(|batch| batch.git_changed)),
        "{what} should set git_changed: {:?}",
        watched.batches
    );
    watched.listen(QUIET);
    for path in watched.paths() {
        assert!(
            !path.components().any(|part| part.as_os_str() == ".git"),
            "{what} listed {}",
            path.display()
        );
    }
    watched.forget();
}

#[test]
fn real_git_commands_set_the_flag() {
    let _turn = one_at_a_time();
    let project = Project::new();
    let root = project.root();
    if !git(root, &["init", "--quiet", "--initial-branch=main"]) {
        // No git on this machine: the hand-made layouts above still cover the logic.
        return;
    }
    project.write("tracked.txt", "one\n");
    assert!(git(root, &["add", "."]));
    assert!(git(root, &["commit", "--quiet", "-m", "first"]));
    let mut watched = Watched::start(root);

    project.write("tracked.txt", "two\n");
    assert!(watched.wait_for_path(&project.path("tracked.txt")));
    watched.listen(QUIET);
    assert!(
        watched.batches.iter().all(|batch| !batch.git_changed),
        "editing a file is not a git change: {:?}",
        watched.batches
    );
    watched.forget();

    assert!(git(root, &["add", "tracked.txt"]));
    expect_git_change(&mut watched, "git add");
    assert!(git(root, &["commit", "--quiet", "-m", "second"]));
    expect_git_change(&mut watched, "git commit");
    assert!(git(root, &["checkout", "--quiet", "-b", "feature"]));
    expect_git_change(&mut watched, "git checkout -b");
    assert!(git(root, &["stash", "list"]));
    assert!(git(root, &["status", "--porcelain"]));
    assert!(git(root, &["tag", "v1"]));
    expect_git_change(&mut watched, "git tag");
}

#[test]
fn real_git_worktrees_set_the_flag() {
    let _turn = one_at_a_time();
    let home = Project::new();
    let main = home.mkdir("main");
    if !git(&main, &["init", "--quiet", "--initial-branch=main"]) {
        return;
    }
    home.write("main/tracked.txt", "one\n");
    assert!(git(&main, &["add", "."]));
    assert!(git(&main, &["commit", "--quiet", "-m", "first"]));
    let worktree = home.path("linked");
    assert!(git(
        &main,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "feature",
            worktree.to_str().unwrap()
        ]
    ));
    let mut watched = Watched::start(&worktree);

    home.write("linked/tracked.txt", "two\n");
    assert!(watched.wait_for_path(&worktree.join("tracked.txt")));
    watched.listen(QUIET);
    watched.forget();

    // The worktree's index and branch both live in the main checkout's git directory.
    assert!(git(&worktree, &["add", "tracked.txt"]));
    expect_git_change(&mut watched, "git add in a worktree");
    assert!(git(&worktree, &["commit", "--quiet", "-m", "second"]));
    expect_git_change(&mut watched, "git commit in a worktree");

    // A commit in the main checkout moves a branch too, which a worktree may show
    // (ahead/behind), so it counts.
    home.write("main/other.txt", "x\n");
    assert!(git(&main, &["add", "."]));
    assert!(git(&main, &["commit", "--quiet", "-m", "third"]));
    expect_git_change(&mut watched, "a commit in the main checkout");
}

#[test]
fn a_changed_gitignore_takes_effect_for_later_events() {
    let _turn = one_at_a_time();
    let project = Project::new();
    project.mkdir("logs");
    let mut watched = Watched::start(project.root());

    let before = project.write("logs/before.log", "x");
    assert!(watched.wait_for_path(&before));

    // The ignore file itself is reported, so the tree and the index can reload too.
    let gitignore = project.write(".gitignore", "logs/\n");
    assert!(watched.wait_for_path(&gitignore));

    watched.listen(QUIET);
    watched.forget();
    project.write("logs/after.log", "x");
    let marker = project.write("marker.txt", "x");
    assert!(watched.wait_for_path(&marker));
    watched.listen(QUIET);
    assert!(
        !watched.paths().contains(&project.path("logs/after.log")),
        "{:?}",
        watched.batches
    );
}

#[test]
fn the_wake_callback_fires_once_per_batch() {
    let _turn = one_at_a_time();
    let project = Project::new();
    let mut watched = Watched::start(project.root());
    let baseline = watched.wakes.load(Ordering::SeqCst);

    let first = project.write("first.txt", "1");
    assert!(watched.wait_for_path(&first));
    watched.listen(QUIET);
    let second = project.write("second.txt", "2");
    assert!(watched.wait_for_path(&second));
    watched.listen(QUIET);

    let batches = watched.batches.len();
    assert!(
        batches >= 2,
        "two changes well apart are at least two batches"
    );
    assert!(wait_until(TIMEOUT, || {
        watched.wakes.load(Ordering::SeqCst) - baseline == batches
    }));
}

#[test]
fn moving_the_root_away_asks_for_a_rescan() {
    let _turn = one_at_a_time();
    let home = Project::new();
    home.write("project/file.txt", "x");
    let mut watched = Watched::start(&home.path("project"));

    fs::rename(home.path("project"), home.path("elsewhere")).unwrap();
    assert!(
        watched.wait_for(|batches| batches.iter().any(|batch| batch.rescan)),
        "{:?}",
        watched.batches
    );
    let rescan = watched.batches.iter().find(|batch| batch.rescan).unwrap();
    assert!(rescan.paths.is_empty(), "a rescan carries no paths");
}

#[test]
fn only_directories_can_be_watched() {
    let _turn = one_at_a_time();
    let project = Project::with_files(&["file.txt"]);
    assert!(matches!(
        Watcher::new(&project.path("missing"), Box::new(|| {})),
        Err(WatchError::NotADirectory(path)) if path == project.path("missing")
    ));
    assert!(matches!(
        Watcher::new(&project.path("file.txt"), Box::new(|| {})),
        Err(WatchError::NotADirectory(_))
    ));
}

#[test]
fn dropping_a_watcher_stops_it_quickly() {
    let _turn = one_at_a_time();
    let project = Project::new();
    let started = Instant::now();
    for round in 0..5 {
        let watcher = Watcher::new(project.root(), Box::new(|| {})).unwrap();
        project.write(&format!("round{round}.txt"), "x");
        let events = watcher.events().clone();
        drop(watcher);
        // With the watcher gone its channel ends rather than hanging.
        let drained = Instant::now();
        while events.recv_timeout(TIMEOUT).is_ok() {}
        assert!(drained.elapsed() < Duration::from_secs(5));
    }
    assert!(started.elapsed() < Duration::from_secs(20));
}

#[cfg(unix)]
#[test]
fn paths_keep_the_spelling_of_the_root_through_a_symlink() {
    let _turn = one_at_a_time();
    let home = Project::new();
    home.mkdir("real");
    let link = home.symlink("real", "link");
    let mut watched = Watched::start(&link);

    home.write("real/via-link.txt", "x");
    assert!(
        watched.wait_for_path(&link.join("via-link.txt")),
        "reported under the link, as the root was given: {:?}",
        watched.batches
    );
}
