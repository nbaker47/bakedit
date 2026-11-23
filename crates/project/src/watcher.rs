//! File-system watching: raw events in, tidy batches out.
//!
//! [`Watcher`] watches the project directory recursively (FSEvents on macOS, inotify on
//! Linux) and turns the stream of raw events into [`FsBatch`]es:
//!
//! * events are **debounced**: everything that happens within about 75 ms of the first
//!   event becomes one batch, with each path listed once;
//! * changes **inside ignored directories** (`target/`, `node_modules/`) are dropped, so a
//!   build does not flood the UI. An ignored entry that sits in a visible directory is
//!   still reported, because the tree shows it, and so is what happens directly inside
//!   an ignored directory the app says it has open ([`Watcher::watch_ignored_dirs`]);
//! * [`crate::ALWAYS_HIDDEN`] names are dropped;
//! * changes **inside the git directory** are not listed; they set
//!   [`FsBatch::git_changed`] when they touch `HEAD`, the index, a ref or the state of a
//!   merge or rebase, which is the cue to refresh git status. This also works in a linked
//!   worktree and when the project is a subdirectory of its repository: the repository's
//!   git directory is found and watched too.
//!
//! The typical consumer does, for each batch: `tree.refresh(&batch.paths)`,
//! `index.update(&batch.paths)`, reload open buffers whose file is listed, refresh git
//! status if `git_changed`; and on `rescan`, `tree.refresh_all()` and `index.rebuild()`.

use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{select, Receiver, Sender};
use notify::event::{EventKind, ModifyKind};
use notify::{EventKindMask, RecommendedWatcher, RecursiveMode, Watcher as _};

use crate::ignore_rules::{
    has_hidden_component, is_ignore_file, is_own_temp_file, normalize, resolve, IgnoreRules,
};

/// How long events are collected after the first one before a batch is sent.
pub const DEBOUNCE: Duration = Duration::from_millis(75);

/// A batch with more distinct paths than this is sent as a rescan instead.
pub const MAX_BATCH_PATHS: usize = 4_096;

/// How long the operating system may hold events back to deliver them together.
const BACKEND_LATENCY: Duration = Duration::from_millis(20);

/// The file-system changes of one short stretch of time.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FsBatch {
    /// The paths that were created, changed, renamed or removed: absolute, starting with
    /// the watched root as it was given, sorted, each listed once. What happened to a
    /// path is not recorded; look at the disk. Empty when `rescan` is set.
    ///
    /// Nothing inside `.git` is listed, with one exception: `.git/info/exclude`, because
    /// a change to it changes which files are ignored, just like a `.gitignore`.
    pub paths: Vec<PathBuf>,
    /// Something in the git directory changed that affects status: `HEAD`, the index, a
    /// branch or tag, `packed-refs`, the config, `info/exclude`, or the state of a merge,
    /// rebase, cherry-pick, revert or bisect.
    pub git_changed: bool,
    /// Events were lost (the backend overflowed or reported an error), the root itself
    /// was moved or removed, or too much changed at once. Treat everything as changed.
    pub rescan: bool,
}

impl FsBatch {
    /// Whether the batch says nothing at all. Such batches are never sent.
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty() && !self.git_changed && !self.rescan
    }
}

/// Why a watcher could not be started.
#[derive(Debug, thiserror::Error)]
pub enum WatchError {
    /// The root does not exist or is not a directory.
    #[error("{} is not a directory", .0.display())]
    NotADirectory(PathBuf),
    /// The operating system refused the watch.
    #[error("could not watch {}: {message}", .path.display())]
    Backend {
        /// The directory that could not be watched.
        path: PathBuf,
        /// What the system said.
        message: String,
    },
    /// The debounce thread could not be started.
    #[error("could not start the watcher thread: {0}")]
    Thread(#[source] io::Error),
}

/// Watches one project directory. Dropping it stops the watch and its thread.
pub struct Watcher {
    root: PathBuf,
    events: Receiver<FsBatch>,
    /// Ignored directories whose direct children are reported all the same.
    open_ignored: Arc<Mutex<HashSet<PathBuf>>>,
    // Declared in the order they must be dropped: first the backend (no more raw
    // events), then the stop signal, and `Drop` joins the thread last.
    backend: Option<RecommendedWatcher>,
    stop: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Watcher {
    /// Start watching the directory `root` and everything below it.
    ///
    /// `wake` is called (on the watcher's thread) each time a batch has been put on the
    /// channel; keep it short, it is meant to poke the app's event loop. Changes made
    /// before this call returns may or may not be reported.
    ///
    /// A relative `root` is made absolute against the current directory; reported paths
    /// start with that root exactly as written, symbolic links included.
    pub fn new(root: &Path, wake: Box<dyn Fn() + Send + Sync>) -> Result<Watcher, WatchError> {
        let root = normalize(root);
        if !fs::metadata(&root).is_ok_and(|meta| meta.is_dir()) {
            return Err(WatchError::NotADirectory(root));
        }

        let (raw_sender, raw_events) = crossbeam_channel::unbounded();
        let config = notify::Config::default()
            .with_event_kinds(EventKindMask::CORE)
            .with_fsevent_latency(BACKEND_LATENCY);
        let handler = move |event: notify::Result<notify::Event>| {
            let _ = raw_sender.send(event);
        };
        let mut backend = RecommendedWatcher::new(handler, config)
            .map_err(|error| backend_error(&root, &error))?;
        backend
            .watch(&root, RecursiveMode::Recursive)
            .map_err(|error| backend_error(&root, &error))?;

        let git = GitDirs::discover(&root);
        if let Some(git) = &git {
            for dir in git.dirs_outside(&root) {
                // Without this watch the project still works; only `git_changed` is lost.
                if let Err(error) = backend.watch(&dir, RecursiveMode::Recursive) {
                    log::warn!("could not watch git directory {}: {error}", dir.display());
                }
            }
        }

        let (events_sender, events) = crossbeam_channel::unbounded();
        let (stop, stop_signal) = crossbeam_channel::bounded::<()>(0);
        let open_ignored = Arc::new(Mutex::new(HashSet::new()));
        let mut classifier = Classifier {
            rules: IgnoreRules::new(&root),
            root: root.clone(),
            git,
            open_ignored: Arc::clone(&open_ignored),
        };
        let thread = thread::Builder::new()
            .name("bakedit-watcher".to_string())
            .spawn(move || {
                debounce_loop(
                    &mut classifier,
                    &raw_events,
                    &stop_signal,
                    &events_sender,
                    &*wake,
                );
            })
            .map_err(WatchError::Thread)?;

        Ok(Watcher {
            root,
            events,
            open_ignored,
            backend: Some(backend),
            stop: Some(stop),
            thread: Some(thread),
        })
    }

    /// The channel the batches arrive on. Batches queue up until they are read.
    pub fn events(&self) -> &Receiver<FsBatch> {
        &self.events
    }

    /// The watched directory, absolute, as every reported path starts.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Also report what is created, changed or removed directly inside these ignored
    /// directories (absolute, or relative to the root). The list replaces the previous
    /// one; an empty list goes back to dropping everything inside ignored directories.
    ///
    /// This is for ignored directories the user has opened in the tree, which would
    /// otherwise show stale contents: pass
    /// [`FileTree::expanded_ignored_dirs`](crate::FileTree::expanded_ignored_dirs)
    /// whenever that list changes. Only direct children count, so a build that churns
    /// deep inside `target/` stays quiet unless those deep directories are open too.
    pub fn watch_ignored_dirs<P: AsRef<Path>>(&self, dirs: &[P]) {
        let dirs = dirs
            .iter()
            .map(|dir| resolve(&self.root, dir.as_ref()))
            .collect();
        *self
            .open_ignored
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = dirs;
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.backend = None;
        self.stop = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn backend_error(path: &Path, error: &notify::Error) -> WatchError {
    WatchError::Backend {
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}

/// Collect raw events into batches until the watcher is dropped.
fn debounce_loop(
    classifier: &mut Classifier,
    raw_events: &Receiver<notify::Result<notify::Event>>,
    stop: &Receiver<()>,
    batches: &Sender<FsBatch>,
    wake: &(dyn Fn() + Send + Sync),
) {
    loop {
        let mut pending = Pending::default();
        // Wait for the first event of the next batch.
        select! {
            recv(raw_events) -> event => match event {
                Ok(event) => classifier.add(&mut pending, event),
                Err(_) => return,
            },
            recv(stop) -> _ => return,
        }
        // Then take everything else that arrives within the debounce window.
        let deadline = Instant::now() + DEBOUNCE;
        loop {
            select! {
                recv(raw_events) -> event => match event {
                    Ok(event) => classifier.add(&mut pending, event),
                    Err(_) => return,
                },
                recv(stop) -> _ => return,
                recv(crossbeam_channel::at(deadline)) -> _ => break,
            }
        }
        let batch = classifier.finish(pending);
        if batch.is_empty() {
            continue;
        }
        if batches.send(batch).is_err() {
            return;
        }
        wake();
    }
}

/// What has been seen since the last batch was sent.
#[derive(Default)]
struct Pending {
    paths: BTreeSet<PathBuf>,
    git_changed: bool,
    rescan: bool,
    /// An ignore file changed, so the rules have to be read again.
    rules_changed: bool,
}

/// Decides what each raw event means for the project.
struct Classifier {
    root: PathBuf,
    git: Option<GitDirs>,
    rules: IgnoreRules,
    /// Ignored directories whose direct children are reported all the same.
    open_ignored: Arc<Mutex<HashSet<PathBuf>>>,
}

impl Classifier {
    fn add(&mut self, pending: &mut Pending, event: notify::Result<notify::Event>) {
        let event = match event {
            Ok(event) => event,
            Err(error) => {
                log::warn!("file watcher error: {error}");
                pending.rescan = true;
                return;
            }
        };
        if event.need_rescan() {
            pending.rescan = true;
        }
        let root_gone = matches!(
            event.kind,
            EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_))
        ) || event.info() == Some("root changed");
        for path in &event.paths {
            self.add_path(pending, path, root_gone);
        }
    }

    fn add_path(&mut self, pending: &mut Pending, path: &Path, removal_or_rename: bool) {
        if self.git.is_none() && path.strip_prefix(&self.root).is_ok_and(starts_with_dot_git) {
            // A repository appeared after the watcher started (`git init`).
            self.git = GitDirs::discover(&self.root);
        }
        if let Some(change) = self.git.as_ref().and_then(|git| git.classify(path)) {
            match change {
                GitChange::Noise => {}
                GitChange::State => pending.git_changed = true,
                GitChange::IgnoreRules => {
                    pending.git_changed = true;
                    pending.rules_changed = true;
                    // Listed so that the tree and the index reload their rules too.
                    if path.starts_with(&self.root) {
                        pending.paths.insert(path.to_path_buf());
                    }
                }
            }
            return;
        }

        let Ok(relative) = path.strip_prefix(&self.root) else {
            return;
        };
        if relative.as_os_str().is_empty() {
            // The root itself: only its disappearance matters.
            pending.rescan |= removal_or_rename;
            return;
        }
        if has_hidden_component(relative) || is_own_temp_file(relative) {
            return;
        }
        if self.rules.inside_ignored_dir(relative) && !self.in_open_ignored_dir(path) {
            return;
        }
        pending.rules_changed |= is_ignore_file(path);
        pending.paths.insert(path.to_path_buf());
    }

    /// Whether `path` sits directly inside an ignored directory the app has open.
    fn in_open_ignored_dir(&self, path: &Path) -> bool {
        let open = self
            .open_ignored
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        !open.is_empty() && path.parent().is_some_and(|parent| open.contains(parent))
    }

    /// Turn what was collected into the batch to send.
    fn finish(&mut self, pending: Pending) -> FsBatch {
        let rescan = pending.rescan || pending.paths.len() > MAX_BATCH_PATHS;
        if pending.rules_changed || rescan {
            self.rules.reload();
        }
        FsBatch {
            paths: if rescan {
                Vec::new()
            } else {
                pending.paths.into_iter().collect()
            },
            git_changed: pending.git_changed,
            rescan,
        }
    }
}

fn starts_with_dot_git(relative: &Path) -> bool {
    relative
        .components()
        .next()
        .is_some_and(|first| first.as_os_str() == ".git")
}

/// What a change inside a git directory means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitChange {
    /// Objects, logs, lock files, hooks: nothing the UI shows.
    Noise,
    /// `HEAD`, the index, refs, merge or rebase state: git status may differ now.
    State,
    /// `info/exclude`: which files are ignored may differ now.
    IgnoreRules,
}

/// Where a project's repository keeps its data.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GitDirs {
    /// The directory with this checkout's `HEAD` and index. For a linked worktree that
    /// is `<main>/.git/worktrees/<name>`.
    git_dir: PathBuf,
    /// The directory with the refs, shared by all worktrees. The same as `git_dir`
    /// except in a linked worktree.
    common_dir: PathBuf,
}

impl GitDirs {
    /// Find the repository `root` belongs to by looking for `.git` in it and the
    /// directories above it, following a `.git` file (worktree, submodule) to where it
    /// points.
    fn discover(root: &Path) -> Option<GitDirs> {
        for dir in root.ancestors() {
            let dot_git = dir.join(".git");
            let Ok(meta) = fs::metadata(&dot_git) else {
                continue;
            };
            if meta.is_dir() {
                return Some(GitDirs {
                    git_dir: dot_git.clone(),
                    common_dir: dot_git,
                });
            }
            let text = fs::read_to_string(&dot_git).ok()?;
            let target = text
                .lines()
                .find_map(|line| line.strip_prefix("gitdir:"))?
                .trim();
            let git_dir = normalize(&dir.join(target));
            let common_dir = match fs::read_to_string(git_dir.join("commondir")) {
                Ok(common) => normalize(&git_dir.join(common.trim())),
                Err(_) => git_dir.clone(),
            };
            return Some(GitDirs {
                git_dir,
                common_dir,
            });
        }
        None
    }

    /// The git directories that a recursive watch of `root` does not already cover.
    fn dirs_outside(&self, root: &Path) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if !self.common_dir.starts_with(root) {
            dirs.push(self.common_dir.clone());
        }
        if !self.git_dir.starts_with(root) && !self.git_dir.starts_with(&self.common_dir) {
            dirs.push(self.git_dir.clone());
        }
        dirs
    }

    /// What a change to `path` means, or `None` if the path is not in a git directory.
    fn classify(&self, path: &Path) -> Option<GitChange> {
        let shared = self.git_dir == self.common_dir;
        if let Ok(relative) = path.strip_prefix(&self.git_dir) {
            return Some(classify_git_path(relative, true, shared));
        }
        if let Ok(relative) = path.strip_prefix(&self.common_dir) {
            return Some(classify_git_path(relative, false, true));
        }
        None
    }
}

/// Classify a path relative to a git directory. `checkout` says the directory holds a
/// checkout's own state (`HEAD`, index), `common` that it holds the shared state (refs).
fn classify_git_path(relative: &Path, checkout: bool, common: bool) -> GitChange {
    let mut components = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => name.to_str(),
            _ => None,
        });
    let Some(first) = components.next() else {
        return GitChange::Noise;
    };
    let last = relative
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if last.ends_with(".lock") {
        return GitChange::Noise;
    }
    // Refs live in the common directory, but each worktree has a few of its own.
    if first == "refs" {
        return GitChange::State;
    }
    if checkout {
        let is_state = matches!(
            first,
            "HEAD"
                | "index"
                | "MERGE_HEAD"
                | "CHERRY_PICK_HEAD"
                | "REVERT_HEAD"
                | "REBASE_HEAD"
                | "BISECT_LOG"
                | "BISECT_START"
                | "AUTO_MERGE"
                | "rebase-merge"
                | "rebase-apply"
                | "sequencer"
        );
        if is_state {
            return GitChange::State;
        }
    }
    if common {
        if matches!(first, "packed-refs" | "config") {
            return GitChange::State;
        }
        if first == "info" && components.next() == Some("exclude") {
            return GitChange::IgnoreRules;
        }
    }
    GitChange::Noise
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, Flag, RemoveKind};
    use notify::Event;

    fn classify(path: &str, checkout: bool, common: bool) -> GitChange {
        classify_git_path(Path::new(path), checkout, common)
    }

    #[test]
    fn git_state_files_are_recognised() {
        for path in [
            "HEAD",
            "index",
            "MERGE_HEAD",
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "REBASE_HEAD",
            "BISECT_LOG",
            "rebase-merge/done",
            "rebase-apply/next",
            "sequencer/todo",
            "refs/heads/main",
            "refs/remotes/origin/feat/x",
            "refs/tags/v1",
            "refs/stash",
            "packed-refs",
            "config",
        ] {
            assert_eq!(classify(path, true, true), GitChange::State, "{path}");
        }
        assert_eq!(classify("info/exclude", true, true), GitChange::IgnoreRules);
    }

    #[test]
    fn git_noise_is_dropped() {
        for path in [
            "",
            "index.lock",
            "HEAD.lock",
            "refs/heads/main.lock",
            "objects/ab/cdef0123",
            "objects/pack/pack-1.pack",
            "logs/HEAD",
            "logs/refs/heads/main",
            "COMMIT_EDITMSG",
            "FETCH_HEAD",
            "ORIG_HEAD",
            "hooks/pre-commit",
            "info/attributes",
            "worktrees/other/HEAD",
            "modules/sub/HEAD",
        ] {
            assert_eq!(classify(path, true, true), GitChange::Noise, "{path}");
        }
    }

    #[test]
    fn a_worktree_splits_checkout_state_from_shared_state() {
        // The worktree's own directory: HEAD and index, but not the shared files.
        assert_eq!(classify("HEAD", true, false), GitChange::State);
        assert_eq!(classify("index", true, false), GitChange::State);
        assert_eq!(classify("refs/bisect/bad", true, false), GitChange::State);
        assert_eq!(classify("packed-refs", true, false), GitChange::Noise);
        // The common directory: refs, but the main checkout's HEAD is not ours.
        assert_eq!(classify("refs/heads/feat", false, true), GitChange::State);
        assert_eq!(classify("packed-refs", false, true), GitChange::State);
        assert_eq!(
            classify("info/exclude", false, true),
            GitChange::IgnoreRules
        );
        assert_eq!(classify("HEAD", false, true), GitChange::Noise);
        assert_eq!(classify("index", false, true), GitChange::Noise);
    }

    #[test]
    fn a_plain_repository_is_discovered() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("crates/core")).unwrap();

        let git = GitDirs::discover(&root).unwrap();
        assert_eq!(git.git_dir, root.join(".git"));
        assert_eq!(git.common_dir, root.join(".git"));
        assert!(git.dirs_outside(&root).is_empty());
        assert_eq!(
            git.classify(&root.join(".git/HEAD")),
            Some(GitChange::State)
        );
        assert_eq!(
            git.classify(&root.join(".git/objects/aa/bb")),
            Some(GitChange::Noise)
        );
        assert_eq!(git.classify(&root.join("src/main.rs")), None);

        // Opened on a subdirectory: the repository above is found and needs its own watch.
        let sub = root.join("crates/core");
        let git = GitDirs::discover(&sub).unwrap();
        assert_eq!(git.git_dir, root.join(".git"));
        assert_eq!(git.dirs_outside(&sub), [root.join(".git")]);
    }

    #[test]
    fn a_linked_worktree_is_discovered() {
        let dir = tempfile::tempdir().unwrap();
        let main_git = dir.path().join("main/.git");
        let worktree_git = main_git.join("worktrees/feature");
        fs::create_dir_all(&worktree_git).unwrap();
        fs::write(worktree_git.join("commondir"), "../..\n").unwrap();
        let worktree = dir.path().join("worktrees/feature");
        fs::create_dir_all(&worktree).unwrap();
        fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", worktree_git.display()),
        )
        .unwrap();

        let git = GitDirs::discover(&worktree).unwrap();
        assert_eq!(git.git_dir, worktree_git);
        assert_eq!(git.common_dir, main_git);
        // One watch on the common directory covers the worktree's own directory too.
        assert_eq!(git.dirs_outside(&worktree), std::slice::from_ref(&main_git));

        assert_eq!(
            git.classify(&worktree_git.join("HEAD")),
            Some(GitChange::State)
        );
        assert_eq!(
            git.classify(&worktree_git.join("index")),
            Some(GitChange::State)
        );
        assert_eq!(
            git.classify(&main_git.join("refs/heads/feature")),
            Some(GitChange::State)
        );
        // The main checkout's own HEAD and index are someone else's state.
        assert_eq!(git.classify(&main_git.join("HEAD")), Some(GitChange::Noise));
        assert_eq!(
            git.classify(&main_git.join("index")),
            Some(GitChange::Noise)
        );
        assert_eq!(
            git.classify(&main_git.join("worktrees/other/HEAD")),
            Some(GitChange::Noise)
        );
    }

    #[test]
    fn a_relative_gitdir_and_a_missing_repository() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("super/sub");
        fs::create_dir_all(&root).unwrap();
        let modules = dir.path().join("super/.git-data/modules/sub");
        fs::create_dir_all(&modules).unwrap();
        fs::write(root.join(".git"), "gitdir: ../.git-data/modules/sub\n").unwrap();

        let git = GitDirs::discover(&root).unwrap();
        assert_eq!(git.git_dir, modules);
        assert_eq!(git.common_dir, modules);

        // A `.git` file that does not say where the repository is.
        fs::write(root.join(".git"), "nonsense\n").unwrap();
        assert_eq!(GitDirs::discover(&root), None);
    }

    fn classifier(root: &Path) -> Classifier {
        Classifier {
            root: root.to_path_buf(),
            git: GitDirs::discover(root),
            rules: IgnoreRules::new(root),
            open_ignored: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    fn event(kind: EventKind, path: &Path) -> notify::Result<Event> {
        Ok(Event::new(kind).add_path(path.to_path_buf()))
    }

    fn created(path: &Path) -> notify::Result<Event> {
        event(EventKind::Create(CreateKind::File), path)
    }

    #[test]
    fn events_are_filtered_and_deduplicated() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join(".gitignore"), "target/\n*.secret\n").unwrap();
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        let mut classifier = classifier(root);
        let mut pending = Pending::default();

        for path in [
            "src/main.rs",
            "src/main.rs",
            "target/debug/build.log",
            "target/debug",
            "target",
            "api.secret",
            ".DS_Store",
            "docs/.DS_Store",
            "src/.bakedit-tmp-1-2-3",
            ".git/objects/ab/cd",
            ".git/index.lock",
        ] {
            classifier.add(&mut pending, created(&root.join(path)));
        }
        classifier.add(&mut pending, created(Path::new("/somewhere/else")));

        let batch = classifier.finish(pending);
        assert_eq!(
            batch,
            FsBatch {
                // The ignored directory itself and an ignored file in a visible directory
                // are reported; what is inside the ignored directory is not.
                paths: vec![
                    root.join("api.secret"),
                    root.join("src/main.rs"),
                    root.join("target"),
                ],
                git_changed: false,
                rescan: false,
            }
        );
    }

    #[test]
    fn open_ignored_directories_report_their_direct_children() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join(".gitignore"), "target/\n").unwrap();
        fs::create_dir_all(root.join("target/debug/deps")).unwrap();
        let mut classifier = classifier(root);
        let seen = |classifier: &mut Classifier| {
            let mut pending = Pending::default();
            for path in [
                "target/a.txt",
                "target/debug/b.txt",
                "target/debug/deps/c.o",
            ] {
                classifier.add(&mut pending, created(&root.join(path)));
            }
            classifier.finish(pending).paths
        };
        assert!(seen(&mut classifier).is_empty());

        classifier
            .open_ignored
            .lock()
            .unwrap()
            .insert(root.join("target/debug"));
        assert_eq!(seen(&mut classifier), [root.join("target/debug/b.txt")]);

        classifier
            .open_ignored
            .lock()
            .unwrap()
            .insert(root.join("target"));
        assert_eq!(
            seen(&mut classifier),
            [root.join("target/a.txt"), root.join("target/debug/b.txt")]
        );
    }

    #[test]
    fn git_changes_set_the_flag_without_listing_paths() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join(".git/info")).unwrap();
        let mut classifier = classifier(root);

        let mut pending = Pending::default();
        classifier.add(&mut pending, created(&root.join(".git/index")));
        classifier.add(&mut pending, created(&root.join(".git/refs/heads/main")));
        let batch = classifier.finish(pending);
        assert!(batch.git_changed && !batch.rescan);
        assert!(batch.paths.is_empty());

        // The exclude file changes the ignore rules, so it is listed.
        let mut pending = Pending::default();
        classifier.add(&mut pending, created(&root.join(".git/info/exclude")));
        let batch = classifier.finish(pending);
        assert!(batch.git_changed);
        assert_eq!(batch.paths, [root.join(".git/info/exclude")]);
    }

    #[test]
    fn a_repository_created_later_is_noticed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut classifier = classifier(root);
        // Whatever repository the temp directory may sit in, it is not this one yet.
        classifier.git = None;

        fs::create_dir_all(root.join(".git")).unwrap();
        let mut pending = Pending::default();
        classifier.add(&mut pending, created(&root.join(".git/HEAD")));
        let batch = classifier.finish(pending);
        assert!(batch.git_changed);
        assert!(batch.paths.is_empty());
    }

    #[test]
    fn ignore_file_changes_reload_the_rules() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("out")).unwrap();
        let mut classifier = classifier(root);

        let mut pending = Pending::default();
        classifier.add(&mut pending, created(&root.join("out/a.txt")));
        assert_eq!(classifier.finish(pending).paths, [root.join("out/a.txt")]);

        fs::write(root.join(".gitignore"), "out/\n").unwrap();
        let mut pending = Pending::default();
        classifier.add(&mut pending, created(&root.join(".gitignore")));
        assert_eq!(classifier.finish(pending).paths, [root.join(".gitignore")]);

        let mut pending = Pending::default();
        classifier.add(&mut pending, created(&root.join("out/b.txt")));
        assert!(classifier.finish(pending).is_empty());
    }

    #[test]
    fn lost_events_and_a_vanished_root_ask_for_a_rescan() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut classifier = classifier(root);

        let mut pending = Pending::default();
        classifier.add(&mut pending, created(&root.join("a.txt")));
        classifier.add(
            &mut pending,
            Ok(Event::new(EventKind::Other).set_flag(Flag::Rescan)),
        );
        let batch = classifier.finish(pending);
        assert!(batch.rescan);
        assert!(batch.paths.is_empty(), "a rescan carries no paths");

        let mut pending = Pending::default();
        classifier.add(
            &mut pending,
            event(EventKind::Remove(RemoveKind::Folder), root),
        );
        assert!(classifier.finish(pending).rescan);

        let mut pending = Pending::default();
        classifier.add(&mut pending, Err(notify::Error::generic("backend failed")));
        assert!(classifier.finish(pending).rescan);

        // A change of the root's own metadata is nothing.
        let mut pending = Pending::default();
        classifier.add(&mut pending, created(root));
        assert!(classifier.finish(pending).is_empty());
    }

    #[test]
    fn a_flood_becomes_a_rescan() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut classifier = classifier(root);
        let mut pending = Pending::default();
        for n in 0..=MAX_BATCH_PATHS {
            classifier.add(&mut pending, created(&root.join(format!("file{n}.txt"))));
        }
        let batch = classifier.finish(pending);
        assert!(batch.rescan);
        assert!(batch.paths.is_empty());
    }
}
