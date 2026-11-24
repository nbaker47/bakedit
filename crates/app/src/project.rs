//! The project: everything the views share about the directory that is open.
//!
//! One `Project` is owned by the shell and handed to every view by reference (`cx.project`).
//! It holds
//!
//! * the root directory;
//! * the git repository, if the root is in one: the latest status, a queue for commands
//!   that change the repository ([`Project::git_job`]), and the committed and staged text
//!   of files for the editor's gutter ([`Project::git_base`]);
//! * the file index behind the file finder and the watcher that keeps it, the tree and the
//!   open editors up to date;
//! * the [`Waker`] that background work uses to get the main loop's attention;
//! * what is remembered between runs: recent files and search history;
//! * the one syntax highlighting thread every editor uses.
//!
//! Nothing here blocks the UI thread. Finding the repository, reading its status, walking
//! the tree and every git command run on other threads; the main loop calls
//! [`Project::poll`] after each wake to collect what they produced. Views only ever see
//! `&Project`, so everything they may call takes `&self`.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bakedit_git::{Branch, GitError, Repo, RepoState, Status, StatusEntry};
use bakedit_project::{FileIndex, FileMatch, IndexOptions, Watcher};
use bakedit_syntax::{Language, LineBreaks, SyntaxResult, SyntaxWorker};
use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::background::{Task, Waker};
use crate::view::FsChange;

/// The shortest time between two status reads, so a burst of file changes (a build, an
/// agent rewriting files) does not run git back to back.
const STATUS_GAP: Duration = Duration::from_millis(120);
/// How often a directory that is not in a repository is asked again whether it is in one.
const DISCOVER_GAP: Duration = Duration::from_secs(2);
/// How many files' committed and staged text is kept.
const BASE_CACHE_LIMIT: usize = 128;
/// How many recent files and search queries are kept.
const HISTORY_LIMIT: usize = 100;

/// How deep below a non-repo project root to scan for nested git repositories
/// (3 covers `~/Code/<repo>` as well as `~/Code/worktrees/<repo>/<lane>`).
const NESTED_REPO_DEPTH: usize = 3;

/// A git repository found at or below the project root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredRepo {
    pub repo: Repo,
    /// Top of the work tree spelled the way the project root is spelled.
    pub top: PathBuf,
    /// Path relative to the project root (e.g. `starbase`, `worktrees/bakedit/feat`),
    /// or the directory name when the repository is at or above the project root.
    pub label: String,
}

/// Which base the git panel and project diff compare the work tree against.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DiffBase {
    /// Staged, unstaged, and untracked changes against `HEAD` (default).
    #[default]
    Uncommitted,
    /// All changes in the current branch and work tree since the merge-base with this ref
    /// (for example `origin/main` or `main`).
    Branch(String),
    /// The changes introduced by a single historical commit.
    Commit { sha: String, label: String },
}

/// Git facts for the status bar.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GitSummary {
    /// The branch, or a short commit id when detached, with the operation in progress
    /// ("main (rebasing)") when there is one.
    pub branch: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    /// Number of changed files.
    pub changes: usize,
}

/// What a file looks like in git: the text of the last commit and the text that is staged.
/// `None` means the file is not there (a new file has no committed text).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GitBase {
    pub head: Option<Arc<str>>,
    pub index: Option<Arc<str>>,
}

/// Which search box a remembered query belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SearchKind {
    /// Find in the file.
    Buffer,
    /// Find in the project.
    Project,
}

/// Something that happened in the background, for the main loop to act on.
#[derive(Debug, Clone, PartialEq)]
pub enum ProjectEvent {
    /// Files changed on disk: tell the views.
    Fs(FsChange),
    /// The repository, its status, or what is committed or staged changed.
    Git,
    /// A [`Project::git_job`] finished. `result` is the line for the status bar: what was
    /// done (may be empty), or why it failed.
    JobFinished { label: String, result: Result<String, String> },
    /// Something the user should be told.
    Notice { text: String, error: bool },
}

type JobWork = Box<dyn FnOnce(&Repo) -> Result<String, GitError> + Send>;

struct Job {
    label: String,
    repo: Repo,
    work: JobWork,
    reply: Sender<Result<String, String>>,
}

enum StatusCmd {
    Refresh,
    SelectRepo(PathBuf),
}

enum ReadRequest {
    Base { repo: Repo, path: PathBuf, relative: PathBuf, epoch: u64 },
    Branches { repo: Repo, epoch: u64 },
}

enum GitMsg {
    /// The set of repositories at or nested under the project root.
    Repos(Vec<DiscoveredRepo>),
    /// The active repository was found, switched, or is gone. `top` is the top of its
    /// work tree spelled the way the project root is spelled.
    Discovered { repo: Option<Repo>, top: Option<PathBuf> },
    Status { repo_root: PathBuf, result: Result<Status, String> },
    JobFinished { label: String, result: Result<String, String> },
    Base { path: PathBuf, epoch: u64, base: GitBase },
    Branches { repo_root: PathBuf, epoch: u64, branches: Result<Vec<Branch>, String> },
}

/// Counts status reads asked for and done, so tests can wait for the last one.
#[derive(Default)]
struct StatusProgress {
    requested: AtomicU64,
    served: AtomicU64,
}

struct BaseEntry {
    value: Option<Arc<GitBase>>,
    /// The git epoch the value was read at, or `None` while the first read is under way.
    epoch: Option<u64>,
    loading: bool,
    /// When it was last asked for, to decide what to forget.
    used: u64,
}

#[derive(Default)]
struct BranchCache {
    value: Option<Arc<Vec<Branch>>>,
    epoch: Option<u64>,
    loading: bool,
    version: u64,
}

struct GitLink {
    events: Sender<GitMsg>,
    status_requests: Sender<StatusCmd>,
    progress: Arc<StatusProgress>,
    jobs: OnceCell<Sender<Job>>,
    reads: OnceCell<Sender<ReadRequest>>,
}

pub struct Project {
    root: PathBuf,
    waker: Waker,

    // Git. `link` is `None` for a detached project.
    link: Option<GitLink>,
    git_events: Receiver<GitMsg>,
    repos: Vec<DiscoveredRepo>,
    repos_version: u64,
    repo: Option<Repo>,
    git_top: Option<PathBuf>,
    diff_base: DiffBase,
    status: Option<Arc<Status>>,
    /// Entry index by path relative to the top of the work tree.
    status_index: HashMap<PathBuf, usize>,
    git_generation: u64,
    /// Goes up when HEAD or the index may have changed: cached texts and branches from an
    /// earlier epoch are out of date.
    epoch: Cell<u64>,
    running_jobs: RefCell<VecDeque<String>>,
    reads_in_flight: Cell<usize>,
    bases: RefCell<HashMap<PathBuf, BaseEntry>>,
    base_clock: Cell<u64>,
    branches: RefCell<BranchCache>,

    // Files.
    index: Option<FileIndex>,
    watcher: Option<Watcher>,

    // Remembered between runs.
    recent: RefCell<Vec<PathBuf>>,
    searches: RefCell<HashMap<SearchKind, Vec<String>>>,

    // Syntax highlighting.
    syntax: OnceCell<SyntaxWorker>,
    syntax_results: RefCell<HashMap<u64, SyntaxResult>>,
    next_doc_id: Cell<u64>,

    notices: RefCell<Vec<ProjectEvent>>,
}

fn describe(error: &GitError) -> String {
    let text = match error.git_output() {
        Some(output) if !output.trim().is_empty() => output.to_string(),
        _ => error.to_string(),
    };
    // One line for the status bar: git's explanations often span several.
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The top of the work tree, spelled the way the project root is spelled (git reports it
/// with symbolic links resolved, the project root is kept as the user gave it).
fn top_in_project_spelling(root: &Path, repo: &Repo) -> PathBuf {
    if root.starts_with(repo.root()) || repo.root().starts_with(root) {
        return repo.root().to_path_buf();
    }
    let Ok(canonical) = root.canonicalize() else { return repo.root().to_path_buf() };
    // Nested repo below a symlinked project root (e.g. macOS `/var/folders` -> `/private/var/folders`).
    if let Ok(below) = repo.root().strip_prefix(&canonical) {
        return root.join(below);
    }
    let Ok(below) = canonical.strip_prefix(repo.root()) else { return repo.root().to_path_buf() };
    let mut top = root.to_path_buf();
    for _ in below.components() {
        if !top.pop() {
            return repo.root().to_path_buf();
        }
    }
    top
}

fn repo_label(root: &Path, top: &Path) -> String {
    if let Ok(rel) = top.strip_prefix(root) {
        if !rel.as_os_str().is_empty() {
            return rel.to_string_lossy().into_owned();
        }
    }
    top.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| top.display().to_string())
}

fn discover_all(root: &Path) -> Vec<DiscoveredRepo> {
    if let Some(repo) = Repo::discover(root) {
        let top = top_in_project_spelling(root, &repo);
        let label = repo_label(root, &top);
        return vec![DiscoveredRepo { repo, top, label }];
    }
    Repo::discover_nested(root, NESTED_REPO_DEPTH)
        .into_iter()
        .map(|repo| {
            let top = top_in_project_spelling(root, &repo);
            let label = repo_label(root, &top);
            DiscoveredRepo { repo, top, label }
        })
        .collect()
}

fn status_thread(root: PathBuf, requests: Receiver<StatusCmd>, events: Sender<GitMsg>, progress: Arc<StatusProgress>, waker: Waker) {
    let mut repo: Option<Repo> = None;
    let mut repos: Vec<DiscoveredRepo> = Vec::new();
    let mut last_discover: Option<Instant> = None;
    while let Ok(first) = requests.recv() {
        let mut pending_select = match first {
            StatusCmd::SelectRepo(path) => Some(path),
            StatusCmd::Refresh => None,
        };
        while let Ok(cmd) = requests.try_recv() {
            if let StatusCmd::SelectRepo(path) = cmd {
                pending_select = Some(path);
            }
        }
        let target = progress.requested.load(Ordering::SeqCst);

        if let Some(wanted) = pending_select {
            let chosen = repos.iter().find(|r| r.repo.root() == wanted || r.top == wanted).map(|r| r.repo.clone()).or_else(|| Repo::discover(&wanted));
            let top = chosen.as_ref().map(|r| top_in_project_spelling(&root, r));
            repo = chosen.clone();
            if events.send(GitMsg::Discovered { repo: chosen, top }).is_err() {
                return;
            }
        } else if repo.is_none() && last_discover.is_none_or(|at| at.elapsed() >= DISCOVER_GAP) {
            last_discover = Some(Instant::now());
            repos = discover_all(&root);
            if events.send(GitMsg::Repos(repos.clone())).is_err() {
                return;
            }
            if let Some(first_repo) = repos.first() {
                repo = Some(first_repo.repo.clone());
                if events.send(GitMsg::Discovered { repo: Some(first_repo.repo.clone()), top: Some(first_repo.top.clone()) }).is_err() {
                    return;
                }
            } else if events.send(GitMsg::Discovered { repo: None, top: None }).is_err() {
                return;
            }
        }

        if let Some(found) = &repo {
            let repo_root = found.root().to_path_buf();
            let result = found.status().map_err(|error| describe(&error));
            let failed = result.is_err();
            if events.send(GitMsg::Status { repo_root: repo_root.clone(), result }).is_err() {
                return;
            }
            // A status that fails usually means the repository went away: look again.
            if failed && Repo::discover(&repo_root).is_none() {
                repo = None;
                repos.retain(|r| r.repo.root() != repo_root);
                last_discover = Some(Instant::now());
                if events.send(GitMsg::Repos(repos.clone())).is_err() {
                    return;
                }
                if events.send(GitMsg::Discovered { repo: None, top: None }).is_err() {
                    return;
                }
            }
        }
        progress.served.store(target, Ordering::SeqCst);
        waker.wake();
        std::thread::sleep(STATUS_GAP);
    }
}

fn jobs_thread(jobs: Receiver<Job>, events: Sender<GitMsg>, status_requests: Sender<StatusCmd>, progress: Arc<StatusProgress>, waker: Waker) {
    for job in jobs {
        // A view's closure that panics must not take the queue down with it: every later
        // job would fail and this one would show as running for ever.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (job.work)(&job.repo)));
        let result = match outcome {
            Ok(result) => result.map_err(|error| describe(&error)),
            Err(_) => Err("the git command stopped unexpectedly (see the log)".to_string()),
        };
        let _ = job.reply.send(result.clone());
        if events.send(GitMsg::JobFinished { label: job.label, result }).is_err() {
            return;
        }
        progress.requested.fetch_add(1, Ordering::SeqCst);
        let _ = status_requests.send(StatusCmd::Refresh);
        waker.wake();
    }
}

fn reads_thread(requests: Receiver<ReadRequest>, events: Sender<GitMsg>, waker: Waker) {
    for request in requests {
        let message = match request {
            ReadRequest::Base { repo, path, relative, epoch } => {
                let text = |result: Result<Option<String>, GitError>| result.ok().flatten().map(Arc::<str>::from);
                let base = GitBase { head: text(repo.head_text(&relative)), index: text(repo.index_text(&relative)) };
                GitMsg::Base { path, epoch, base }
            }
            ReadRequest::Branches { repo, epoch } => {
                GitMsg::Branches { repo_root: repo.root().to_path_buf(), epoch, branches: repo.branches().map_err(|error| describe(&error)) }
            }
        };
        if events.send(message).is_err() {
            return;
        }
        waker.wake();
    }
}

fn spawn_named(name: &str, work: impl FnOnce() + Send + 'static) {
    if let Err(err) = std::thread::Builder::new().name(name.into()).spawn(work) {
        log::error!("could not start the {name} thread: {err}");
    }
}

impl Project {
    /// Open the directory `root`: start finding its repository, reading the status,
    /// indexing its files and watching it. Returns at once; results arrive through
    /// [`Project::poll`], and `waker` is woken whenever there is something to collect.
    pub fn open(root: impl Into<PathBuf>, waker: Waker) -> Project {
        let root = root.into();
        let (events, git_events) = unbounded();
        let (status_requests, status_inbox) = unbounded();
        let progress = Arc::new(StatusProgress::default());
        {
            let (root, events, progress, waker) = (root.clone(), events.clone(), progress.clone(), waker.clone());
            spawn_named("bakedit-git-status", move || status_thread(root, status_inbox, events, progress, waker));
        }
        let link = GitLink { events, status_requests, progress, jobs: OnceCell::new(), reads: OnceCell::new() };

        let index_waker = waker.clone();
        let index = FileIndex::with_wake(&root, IndexOptions::default(), Box::new(move || index_waker.wake()));
        let watch_waker = waker.clone();
        let mut project = Project::assemble(root, waker, Some(link), git_events, Some(index));
        match Watcher::new(&project.root, Box::new(move || watch_waker.wake())) {
            Ok(watcher) => project.watcher = Some(watcher),
            Err(err) => project.notice(format!("Files are not being watched for changes: {err}"), true),
        }
        project.refresh_git();
        project
    }

    /// A project that reads nothing from disk and starts no threads: no repository, no
    /// file index, no watcher. For tests and for views drawn on their own.
    pub fn detached(root: impl Into<PathBuf>) -> Project {
        let (_, git_events) = unbounded();
        Project::assemble(root.into(), Waker::detached(), None, git_events, None)
    }

    fn assemble(root: PathBuf, waker: Waker, link: Option<GitLink>, git_events: Receiver<GitMsg>, index: Option<FileIndex>) -> Project {
        Project {
            root,
            waker,
            link,
            git_events,
            repos: Vec::new(),
            repos_version: 0,
            repo: None,
            git_top: None,
            diff_base: DiffBase::Uncommitted,
            status: None,
            status_index: HashMap::new(),
            git_generation: 0,
            epoch: Cell::new(0),
            running_jobs: RefCell::new(VecDeque::new()),
            reads_in_flight: Cell::new(0),
            bases: RefCell::new(HashMap::new()),
            base_clock: Cell::new(0),
            branches: RefCell::new(BranchCache::default()),
            index,
            watcher: None,
            recent: RefCell::new(Vec::new()),
            searches: RefCell::new(HashMap::new()),
            syntax: OnceCell::new(),
            syntax_results: RefCell::new(HashMap::new()),
            next_doc_id: Cell::new(1),
            notices: RefCell::new(Vec::new()),
        }
    }

    /// The project directory, as the user gave it (absolute, symbolic links kept).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Wakes the main loop. Clone it into any thread a view starts.
    pub fn waker(&self) -> &Waker {
        &self.waker
    }

    /// Run `work` on another thread; the loop is woken when it is done, and the view
    /// collects the value with [`Task::poll`] in its `tick`.
    pub fn spawn<T: Send + 'static>(&self, work: impl FnOnce() -> T + Send + 'static) -> Task<T> {
        Task::spawn(&self.waker, work)
    }

    fn notice(&self, text: String, error: bool) {
        self.notices.borrow_mut().push(ProjectEvent::Notice { text, error });
    }

    // ---- git ---------------------------------------------------------------------------

    /// The repository the project is in (or the currently selected nested repository).
    /// `None` until it has been found, and when there is none. Clone it into a
    /// [`Project::spawn`] to read diffs, logs and the like.
    pub fn repo(&self) -> Option<&Repo> {
        self.repo.as_ref()
    }

    /// All git repositories discovered at or below the project root.
    pub fn repos(&self) -> &[DiscoveredRepo] {
        &self.repos
    }

    /// The deepest discovered git repository that contains `path`, if any.
    pub fn repo_root_for_path(&self, path: &Path) -> Option<PathBuf> {
        self.repos
            .iter()
            .filter(|r| path.starts_with(&r.top) || path.starts_with(r.repo.root()))
            .max_by_key(|r| r.top.as_os_str().len().max(r.repo.root().as_os_str().len()))
            .map(|r| r.repo.root().to_path_buf())
    }

    /// Changes whenever [`Project::repos`] changes.
    pub fn repos_version(&self) -> u64 {
        self.repos_version
    }

    /// Display name of the active repository when it is nested inside a non-repo project
    /// root (like `~/Code/starbase` when `~/Code` is open) or when multiple repositories
    /// exist in the project. `None` for a single repository at or above the project root.
    pub fn active_repo_label(&self) -> Option<&str> {
        let active = self.repo.as_ref()?;
        let found = self.repos.iter().find(|r| r.repo.root() == active.root())?;
        if self.repos.len() > 1 || (found.top.starts_with(&self.root) && found.top != self.root) {
            Some(found.label.as_str())
        } else {
            None
        }
    }

    /// Switch the active git repository to the repository at `repo_root` (a path from
    /// [`Project::repos`] or any path inside a repository). Clears cached status, bases
    /// and branches and triggers a fresh status read in the background.
    pub fn select_repo(&mut self, repo_root: &Path) {
        let matched = self.repos.iter().find(|r| r.repo.root() == repo_root || r.top == repo_root).cloned();
        let next_repo = matched.as_ref().map(|r| r.repo.clone());
        let next_top = matched.as_ref().map(|r| r.top.clone());
        if self.repo.as_ref().map(Repo::root) == next_repo.as_ref().map(Repo::root) && next_repo.is_some() {
            return;
        }
        self.repo = next_repo;
        self.git_top = next_top;
        self.diff_base = DiffBase::Uncommitted;
        self.status = None;
        self.status_index.clear();
        self.bases.borrow_mut().clear();
        {
            let mut cache = self.branches.borrow_mut();
            cache.value = None;
            cache.epoch = None;
            cache.loading = false;
            cache.version += 1;
        }
        self.epoch.set(self.epoch.get() + 1);
        self.git_generation += 1;
        if let Some(link) = &self.link {
            link.progress.requested.fetch_add(1, Ordering::SeqCst);
            let _ = link.status_requests.send(StatusCmd::SelectRepo(repo_root.to_path_buf()));
        }
    }

    /// What the git panel and project diff compare the work tree against.
    pub fn diff_base(&self) -> &DiffBase {
        &self.diff_base
    }

    /// Change the diff base and bump `git_generation` so the git panel and diff view reload.
    pub fn set_diff_base(&mut self, base: DiffBase) {
        if self.diff_base == base {
            return;
        }
        self.diff_base = base;
        self.git_generation += 1;
    }

    /// Toggle between uncommitted changes (`HEAD`) and a branch diff against the repo's
    /// default base ref (`origin/main`, `main`, etc.). Returns a short status description.
    pub fn toggle_branch_diff(&mut self) -> String {
        match &self.diff_base {
            DiffBase::Branch(_) | DiffBase::Commit { .. } => {
                self.set_diff_base(DiffBase::Uncommitted);
                "Diff: uncommitted changes".to_string()
            }
            DiffBase::Uncommitted => {
                let base = self.repo.as_ref().and_then(Repo::default_base_ref).unwrap_or_else(|| "main".to_string());
                self.set_diff_base(DiffBase::Branch(base.clone()));
                format!("Diff: branch vs {base}")
            }
        }
    }

    /// The latest status: branch, upstream, ahead and behind, and every changed file.
    /// `None` until the first read has finished, and outside a repository.
    pub fn status(&self) -> Option<&Arc<Status>> {
        self.status.as_ref()
    }

    /// Goes up whenever something git-related a view may be showing could have changed: a
    /// new status, a commit, a change to what is staged. Compare it in `tick` to know when
    /// to rebuild.
    pub fn git_generation(&self) -> u64 {
        self.git_generation
    }

    /// The branch line of the status bar.
    pub fn git_summary(&self) -> GitSummary {
        let Some(status) = &self.status else { return GitSummary::default() };
        let name = status.branch.clone().or_else(|| status.head.clone());
        let state = status.state.map(|state| match state {
            RepoState::Merging => "merging",
            RepoState::Rebasing => "rebasing",
            RepoState::CherryPicking => "cherry-picking",
            RepoState::Reverting => "reverting",
            RepoState::Bisecting => "bisecting",
        });
        let branch = match (name, state) {
            (Some(name), Some(state)) => Some(format!("{name} ({state})")),
            (name, _) => name,
        };
        let branch = match (self.active_repo_label(), branch) {
            (Some(repo), Some(branch)) => Some(format!("{repo} / {branch}")),
            (Some(repo), None) => Some(repo.to_string()),
            (None, branch) => branch,
        };
        GitSummary {
            branch,
            ahead: u32::try_from(status.ahead).unwrap_or(u32::MAX),
            behind: u32::try_from(status.behind).unwrap_or(u32::MAX),
            changes: status.entries.len(),
        }
    }

    /// `path` (absolute, spelled like the project root) as git names it: relative to the
    /// top of the work tree. `None` outside the repository. Does not touch the disk.
    pub fn git_path(&self, path: &Path) -> Option<PathBuf> {
        let repo = self.repo.as_ref()?;
        let top = self.git_top.as_deref()?;
        let below = path.strip_prefix(top).or_else(|_| path.strip_prefix(repo.root())).ok()?;
        repo.relative_path(below).ok()
    }

    /// The top of the active git work tree, spelled like the project root.
    pub fn git_top(&self) -> Option<&Path> {
        self.git_top.as_deref()
    }

    /// The absolute path, spelled like the project root, of a path git reported.
    pub fn abs_path(&self, git_path: &Path) -> Option<PathBuf> {
        Some(self.git_top.as_ref()?.join(git_path))
    }

    /// The status entry of a file (absolute path), if git has anything to say about it.
    pub fn status_of(&self, path: &Path) -> Option<&StatusEntry> {
        let relative = self.git_path(path)?;
        let index = *self.status_index.get(&relative)?;
        self.status.as_ref()?.entries.get(index)
    }

    /// Read the status again soon. Call it when something changed that the file watcher
    /// cannot see; changes on disk and finished git jobs refresh it by themselves.
    pub fn refresh_git(&self) {
        if let Some(link) = &self.link {
            link.progress.requested.fetch_add(1, Ordering::SeqCst);
            let _ = link.status_requests.send(StatusCmd::Refresh);
        }
    }

    /// HEAD or the index may have moved: what was read from them is out of date.
    fn git_moved(&mut self) {
        self.epoch.set(self.epoch.get() + 1);
        self.git_generation += 1;
        self.refresh_git();
    }

    /// Run something that changes the repository (stage, commit, checkout, push ...) off
    /// the UI thread. Jobs run one at a time in the order given, because git allows only
    /// one writer of the index.
    ///
    /// `label` names the job for the status bar ("Commit", "Push main"): while it runs the
    /// bar shows "label…", a failure shows "label failed: why". `work` returns the line to
    /// show on success, or an empty string for nothing. Afterwards the status is read again
    /// and the views see it through [`Project::status`]. The returned task resolves to the
    /// same outcome, for a view that needs to know (to clear a commit message, say).
    pub fn git_job<F>(&self, label: impl Into<String>, work: F) -> Task<Result<String, String>>
    where
        F: FnOnce(&Repo) -> Result<String, GitError> + Send + 'static,
    {
        let label = label.into();
        let (Some(repo), Some(link)) = (self.repo.clone(), &self.link) else {
            let result = Err("this folder is not in a git repository".to_string());
            self.notices.borrow_mut().push(ProjectEvent::JobFinished { label, result: result.clone() });
            self.waker.wake();
            return Task::ready(result);
        };
        let sender = link.jobs.get_or_init(|| {
            let (sender, receiver) = unbounded();
            let (events, requests, progress, waker) =
                (link.events.clone(), link.status_requests.clone(), link.progress.clone(), self.waker.clone());
            spawn_named("bakedit-git-jobs", move || jobs_thread(receiver, events, requests, progress, waker));
            sender
        });
        let (reply, outcome) = crossbeam_channel::bounded(1);
        self.running_jobs.borrow_mut().push_back(label.clone());
        if sender.send(Job { label: label.clone(), repo, work: Box::new(work), reply }).is_err() {
            self.running_jobs.borrow_mut().pop_back();
            let result = Err("the git worker is not running".to_string());
            self.notices.borrow_mut().push(ProjectEvent::JobFinished { label, result: result.clone() });
            self.waker.wake();
            return Task::ready(result);
        }
        // The worker answers on `outcome` and wakes the loop right after.
        Task::from_channel(outcome)
    }

    /// The label of the git job that is running (or next in line), for "label…" in the
    /// status bar.
    pub fn busy(&self) -> Option<String> {
        self.running_jobs.borrow().front().cloned()
    }

    fn read_request(&self, request: ReadRequest) -> bool {
        let Some(link) = &self.link else { return false };
        let sender = link.reads.get_or_init(|| {
            let (sender, receiver) = unbounded();
            let (events, waker) = (link.events.clone(), self.waker.clone());
            spawn_named("bakedit-git-read", move || reads_thread(receiver, events, waker));
            sender
        });
        let sent = sender.send(request).is_ok();
        if sent {
            self.reads_in_flight.set(self.reads_in_flight.get() + 1);
        }
        sent
    }

    /// The committed and the staged text of a file (absolute path), for the gutter's
    /// change marks. Call it from `tick` or `render`: it is a map lookup. `None` until the
    /// text has been read (the loop is woken when it has) and for files outside the
    /// repository. After a commit or a change to what is staged the old value is returned
    /// until the new one has been read; compare the `Arc`s to notice the change.
    pub fn git_base(&self, path: &Path) -> Option<Arc<GitBase>> {
        let repo = self.repo.as_ref()?;
        let epoch = self.epoch.get();
        let clock = self.base_clock.get() + 1;
        self.base_clock.set(clock);
        let mut bases = self.bases.borrow_mut();
        if !bases.contains_key(path) {
            if bases.len() >= BASE_CACHE_LIMIT {
                if let Some(oldest) = bases.iter().min_by_key(|(_, entry)| entry.used).map(|(path, _)| path.clone()) {
                    bases.remove(&oldest);
                }
            }
            bases.insert(path.to_path_buf(), BaseEntry { value: None, epoch: None, loading: false, used: clock });
        }
        let entry = bases.get_mut(path)?;
        entry.used = clock;
        if entry.epoch != Some(epoch) && !entry.loading {
            let Some(relative) = self.git_path(path) else {
                // Outside the work tree: there is nothing to compare with, and never will be.
                entry.epoch = Some(epoch);
                return None;
            };
            entry.loading = self.read_request(ReadRequest::Base { repo: repo.clone(), path: path.to_path_buf(), relative, epoch });
        }
        entry.value.clone()
    }

    /// Local and remote branches, newest commit first. `None` until they have been read
    /// (the loop is woken when they have); [`Project::branches_version`] moves when the
    /// answer changes.
    pub fn branches(&self) -> Option<Arc<Vec<Branch>>> {
        let repo = self.repo.as_ref()?;
        let epoch = self.epoch.get();
        let mut cache = self.branches.borrow_mut();
        if cache.epoch != Some(epoch) && !cache.loading {
            cache.loading = self.read_request(ReadRequest::Branches { repo: repo.clone(), epoch });
        }
        cache.value.clone()
    }

    /// Changes whenever [`Project::branches`] has a new answer.
    pub fn branches_version(&self) -> u64 {
        self.branches.borrow().version
    }

    // ---- files -------------------------------------------------------------------------

    /// The file index, for anything [`Project::find_files`] does not cover. `None` for a
    /// detached project.
    pub fn index(&self) -> Option<&FileIndex> {
        self.index.as_ref()
    }

    /// Fuzzy-find files by path, best first; an empty query lists the recent files. Paths
    /// in the result are relative to the root. Never waits for indexing.
    pub fn find_files(&self, query: &str, limit: usize) -> Vec<FileMatch> {
        match &self.index {
            Some(index) => index.search(query, limit, &self.recent.borrow()),
            None => Vec::new(),
        }
    }

    /// Changes whenever the file list does.
    pub fn index_generation(&self) -> u64 {
        self.index.as_ref().map(FileIndex::generation).unwrap_or(0)
    }

    /// The first walk of the tree has not finished: the file list is still growing.
    pub fn is_indexing(&self) -> bool {
        self.index.as_ref().is_some_and(|index| !index.is_ready())
    }

    /// Ignored directories that are open in the file tree: the watcher reports what
    /// happens directly inside them too (it skips ignored directories otherwise). Replaces
    /// the previous list.
    pub fn watch_ignored_dirs<P: AsRef<Path>>(&self, dirs: &[P]) {
        if let Some(watcher) = &self.watcher {
            watcher.watch_ignored_dirs(dirs);
        }
    }

    /// A view created, renamed or removed these files itself: bring the index and the
    /// status up to date without waiting for the watcher.
    pub fn paths_changed(&self, paths: &[PathBuf]) {
        if let Some(index) = &self.index {
            index.update(paths);
        }
        self.refresh_git();
    }

    // ---- remembered between runs ---------------------------------------------------------

    /// Files opened lately, most recent first.
    pub fn recent_files(&self) -> Vec<PathBuf> {
        self.recent.borrow().clone()
    }

    /// Move `path` to the front of the recent files.
    pub fn touch_recent(&self, path: &Path) {
        let mut recent = self.recent.borrow_mut();
        recent.retain(|existing| existing != path);
        recent.insert(0, path.to_path_buf());
        recent.truncate(HISTORY_LIMIT);
    }

    /// Replace the recent files (when a session is restored).
    pub fn set_recent_files(&self, files: Vec<PathBuf>) {
        *self.recent.borrow_mut() = files;
    }

    /// What was searched for lately in that search box, most recent first.
    pub fn search_history(&self, kind: SearchKind) -> Vec<String> {
        self.searches.borrow().get(&kind).cloned().unwrap_or_default()
    }

    /// Put `query` at the front of that search box's history. Empty queries are not kept.
    pub fn remember_search(&self, kind: SearchKind, query: &str) {
        if query.is_empty() {
            return;
        }
        let mut searches = self.searches.borrow_mut();
        let list = searches.entry(kind).or_default();
        list.retain(|existing| existing != query);
        list.insert(0, query.to_string());
        list.truncate(HISTORY_LIMIT);
    }

    /// Replace a search box's history (when a session is restored).
    pub fn set_search_history(&self, kind: SearchKind, queries: Vec<String>) {
        self.searches.borrow_mut().insert(kind, queries);
    }

    // ---- syntax highlighting -------------------------------------------------------------

    /// A number no other document has, to name a document in the calls below.
    pub fn next_doc_id(&self) -> u64 {
        let id = self.next_doc_id.get();
        self.next_doc_id.set(id + 1);
        id
    }

    fn syntax_worker(&self) -> &SyntaxWorker {
        self.syntax.get_or_init(|| {
            let waker = self.waker.clone();
            // The text buffer counts '\n' only; the highlighter must count lines the same way.
            SyntaxWorker::with_line_breaks(Box::new(move || waker.wake()), LineBreaks::Lf)
        })
    }

    /// Highlight `text` (version `version` of document `doc_id`) on the highlighting
    /// thread. A newer request for the same document replaces one still waiting. The loop
    /// is woken when the result is there; fetch it with [`Project::syntax`].
    pub fn request_syntax(&self, doc_id: u64, version: u64, language: &'static Language, text: String) {
        self.syntax_worker().request(doc_id, version, language, text);
    }

    /// The newest highlights for a document, whatever version they are of: compare
    /// `version` with the document's. Lines are split at '\n' only.
    pub fn syntax(&self, doc_id: u64) -> Option<SyntaxResult> {
        self.collect_syntax();
        self.syntax_results.borrow().get(&doc_id).cloned()
    }

    /// The document was closed: drop its highlights and any request still waiting.
    pub fn forget_syntax(&self, doc_id: u64) {
        if let Some(worker) = self.syntax.get() {
            worker.forget(doc_id);
        }
        self.syntax_results.borrow_mut().remove(&doc_id);
    }

    fn collect_syntax(&self) {
        let Some(worker) = self.syntax.get() else { return };
        let mut results = self.syntax_results.borrow_mut();
        while let Ok(result) = worker.results().try_recv() {
            let newer = results.get(&result.doc_id).is_none_or(|existing| existing.version <= result.version);
            if newer {
                results.insert(result.doc_id, result);
            }
        }
    }

    // ---- the main loop's side ------------------------------------------------------------

    /// Collect what the background threads produced since the last call. The main loop
    /// calls this after every wake and acts on the events: file changes go to the views,
    /// a git change refreshes the status bar, finished jobs and notices become messages.
    pub fn poll(&mut self) -> Vec<ProjectEvent> {
        let mut events = std::mem::take(&mut *self.notices.borrow_mut());
        self.poll_watcher(&mut events);
        let mut git_changed = false;
        while let Ok(message) = self.git_events.try_recv() {
            git_changed |= self.apply_git(message, &mut events);
        }
        if git_changed {
            self.git_generation += 1;
            events.push(ProjectEvent::Git);
        }
        self.collect_syntax();
        events
    }

    fn poll_watcher(&mut self, events: &mut Vec<ProjectEvent>) {
        let Some(watcher) = &self.watcher else { return };
        let mut change = FsChange::default();
        let mut git_moved = false;
        while let Ok(batch) = watcher.events().try_recv() {
            change.rescan |= batch.rescan;
            change.paths.extend(batch.paths);
            git_moved |= batch.git_changed;
        }
        if change.rescan {
            change.paths.clear();
        } else {
            change.paths.sort();
            change.paths.dedup();
        }
        if !change.is_empty() {
            if let Some(index) = &self.index {
                if change.rescan {
                    index.rebuild();
                } else {
                    index.update(&change.paths);
                }
            }
            self.refresh_git();
            events.push(ProjectEvent::Fs(change));
        }
        if git_moved {
            self.git_moved();
            events.push(ProjectEvent::Git);
        }
    }

    /// Take in one message from a git thread. Returns true if views may need to redraw.
    fn apply_git(&mut self, message: GitMsg, events: &mut Vec<ProjectEvent>) -> bool {
        match message {
            GitMsg::Repos(repos) => {
                if self.repos == repos {
                    return false;
                }
                self.repos = repos;
                self.repos_version += 1;
                true
            }
            GitMsg::Discovered { repo, top } => {
                let changed = self.repo != repo;
                if changed {
                    self.status = None;
                    self.status_index.clear();
                    self.bases.borrow_mut().clear();
                    let mut cache = self.branches.borrow_mut();
                    cache.value = None;
                    cache.epoch = None;
                    cache.loading = false;
                    cache.version += 1;
                }
                self.repo = repo;
                self.git_top = top;
                changed
            }
            GitMsg::Status { repo_root, result: Ok(status) } => {
                if self.repo.as_ref().map(Repo::root) != Some(repo_root.as_path()) {
                    return false;
                }
                if self.status.as_deref() == Some(&status) {
                    return false;
                }
                self.status_index = status.entries.iter().enumerate().map(|(index, entry)| (entry.path.clone(), index)).collect();
                self.status = Some(Arc::new(status));
                true
            }
            GitMsg::Status { result: Err(reason), .. } => {
                log::warn!("git status failed: {reason}");
                false
            }
            GitMsg::JobFinished { label, result } => {
                let mut running = self.running_jobs.borrow_mut();
                if let Some(position) = running.iter().position(|running| *running == label) {
                    running.remove(position);
                }
                drop(running);
                // What the job changed is exactly what cached texts and branches are about.
                self.epoch.set(self.epoch.get() + 1);
                events.push(ProjectEvent::JobFinished { label, result });
                true
            }
            GitMsg::Base { path, epoch, base } => {
                self.reads_in_flight.set(self.reads_in_flight.get().saturating_sub(1));
                let mut bases = self.bases.borrow_mut();
                let Some(entry) = bases.get_mut(&path) else { return false };
                entry.loading = false;
                entry.epoch = Some(epoch);
                if entry.value.as_deref() == Some(&base) {
                    return false;
                }
                entry.value = Some(Arc::new(base));
                true
            }
            GitMsg::Branches { repo_root, epoch, branches } => {
                self.reads_in_flight.set(self.reads_in_flight.get().saturating_sub(1));
                if self.repo.as_ref().map(Repo::root) != Some(repo_root.as_path()) {
                    return false;
                }
                let mut cache = self.branches.borrow_mut();
                cache.loading = false;
                cache.epoch = Some(epoch);
                match branches {
                    Ok(branches) => {
                        if cache.value.as_deref() != Some(&branches) {
                            cache.value = Some(Arc::new(branches));
                            cache.version += 1;
                        }
                    }
                    Err(reason) => log::warn!("listing branches failed: {reason}"),
                }
                // The picker that shows them asks for the version itself.
                false
            }
        }
    }

    /// Whether every background thread has finished what it was asked to do.
    fn is_idle(&self) -> bool {
        let status_done = self
            .link
            .as_ref()
            .is_none_or(|link| link.progress.served.load(Ordering::SeqCst) >= link.progress.requested.load(Ordering::SeqCst));
        let index_done = self.index.as_ref().is_none_or(|index| index.is_ready() && !index.is_scanning());
        status_done && index_done && self.running_jobs.borrow().is_empty() && self.reads_in_flight.get() == 0
    }

    /// Wait until the background work asked for so far is done (or `timeout` has passed)
    /// and return everything [`Project::poll`] collected meanwhile. For tests: the app
    /// itself never waits.
    pub fn settle(&mut self, timeout: Duration) -> Vec<ProjectEvent> {
        let started = Instant::now();
        let mut events = Vec::new();
        loop {
            let idle = self.is_idle();
            events.extend(self.poll());
            if idle || started.elapsed() >= timeout {
                return events;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::Path;
    use std::process::{Command, Stdio};

    /// Run git in a throwaway repository with a fixed identity and none of the user's
    /// configuration, hooks or prompts.
    pub fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.invalid", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?} failed");
    }

    /// A temp directory holding a repository with one commit: `a.txt` ("one\ntwo\n") and
    /// `src/lib.rs`.
    pub fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("a temp dir");
        git(dir.path(), &["init", "--quiet"]);
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn answer() -> u32 {\n    42\n}\n").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "--quiet", "-m", "first"]);
        dir
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{git, repo};
    use super::*;

    const WAIT: Duration = Duration::from_secs(20);

    fn open(dir: &Path) -> Project {
        let mut project = Project::open(dir, Waker::detached());
        project.settle(WAIT);
        project
    }

    /// Poll until `done` says so; fails the test if it never does.
    fn until(project: &mut Project, mut done: impl FnMut(&Project, &[ProjectEvent]) -> bool) -> Vec<ProjectEvent> {
        let started = Instant::now();
        let mut seen = Vec::new();
        loop {
            seen.extend(project.poll());
            if done(project, &seen) {
                return seen;
            }
            assert!(started.elapsed() < WAIT, "timed out; events so far: {seen:?}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_detached_project_is_inert() {
        let mut project = Project::detached("/nowhere/at/all");
        assert_eq!(project.root(), Path::new("/nowhere/at/all"));
        assert!(project.repo().is_none() && project.status().is_none() && project.index().is_none());
        assert_eq!(project.find_files("a", 10), Vec::new());
        assert_eq!(project.git_summary(), GitSummary::default());
        assert_eq!(project.git_base(Path::new("/nowhere/at/all/a.rs")), None);
        assert_eq!(project.branches(), None);
        project.refresh_git();
        project.paths_changed(&[PathBuf::from("/nowhere/at/all/a.rs")]);
        let mut task = project.git_job("Commit", |_| Ok(String::new()));
        assert!(task.poll().unwrap().unwrap_err().contains("not in a git repository"));
        let events = project.settle(Duration::from_secs(1));
        assert!(matches!(&events[..], [ProjectEvent::JobFinished { label, result: Err(_) }] if label == "Commit"), "{events:?}");
        assert_eq!(project.busy(), None);
    }

    #[test]
    fn finds_the_repository_and_reads_its_status() {
        let dir = repo();
        std::fs::write(dir.path().join("a.txt"), "one\nchanged\n").unwrap();
        std::fs::write(dir.path().join("new.txt"), "x").unwrap();
        let project = open(dir.path());
        let status = project.status().expect("the status was read");
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert_eq!(status.entries.len(), 2);
        let summary = project.git_summary();
        assert_eq!((summary.branch.as_deref(), summary.changes, summary.ahead, summary.behind), (Some("main"), 2, 0, 0));
        // Paths are matched in the project's own spelling, which on macOS is not the one
        // git reports for a temp directory (/var against /private/var).
        let entry = project.status_of(&dir.path().join("a.txt")).expect("a.txt is modified");
        assert_eq!(entry.path, Path::new("a.txt"));
        assert!(project.status_of(&dir.path().join("src/lib.rs")).is_none());
        assert_eq!(project.git_path(&dir.path().join("src/lib.rs")), Some(PathBuf::from("src/lib.rs")));
        assert_eq!(project.abs_path(Path::new("src/lib.rs")), Some(dir.path().join("src/lib.rs")));
        assert_eq!(project.git_path(Path::new("/somewhere/else.rs")), None);
        assert!(project.git_generation() > 0);
    }

    #[test]
    fn a_project_below_the_top_of_its_repository() {
        let dir = repo();
        std::fs::write(dir.path().join("src/lib.rs"), "changed\n").unwrap();
        let project = open(&dir.path().join("src"));
        assert_eq!(project.root(), dir.path().join("src"));
        let entry = project.status_of(&dir.path().join("src/lib.rs")).expect("matched through the project's spelling");
        assert_eq!(entry.path, Path::new("src/lib.rs"));
        assert_eq!(project.abs_path(Path::new("a.txt")), Some(dir.path().join("a.txt")));
    }

    #[test]
    fn a_folder_outside_git_has_no_repository_and_still_indexes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.md"), "hello").unwrap();
        let project = open(dir.path());
        assert!(project.repo().is_none());
        assert!(project.status().is_none());
        assert!(!project.is_indexing());
        let found = project.find_files("notes", 10);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, Path::new("notes.md"));
    }

    #[test]
    fn a_parent_folder_discovers_nested_repos_and_switches_between_them() {
        let parent = tempfile::tempdir().unwrap();
        let alpha = parent.path().join("alpha");
        let beta = parent.path().join("beta");
        std::fs::create_dir_all(&alpha).unwrap();
        std::fs::create_dir_all(&beta).unwrap();
        for (dir, branch, file) in [(&alpha, "main", "a.txt"), (&beta, "dev", "b.txt")] {
            git(dir, &["init", "-b", branch]);
            git(dir, &["config", "user.name", "Test"]);
            git(dir, &["config", "user.email", "test@example.com"]);
            std::fs::write(dir.join(file), "initial\n").unwrap();
            git(dir, &["add", "."]);
            git(dir, &["commit", "-m", "first"]);
        }
        std::fs::write(beta.join("b.txt"), "edited\n").unwrap();

        let mut project = open(parent.path());
        let labels: Vec<&str> = project.repos().iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, vec!["alpha", "beta"]);
        assert_eq!(project.active_repo_label(), Some("alpha"));
        assert_eq!(project.git_summary().branch.as_deref(), Some("alpha / main"));
        assert_eq!(project.git_summary().changes, 0);

        let beta_root = project.repos()[1].repo.root().to_path_buf();
        project.select_repo(&beta_root);
        project.settle(WAIT);
        assert_eq!(project.active_repo_label(), Some("beta"));
        assert_eq!(project.git_summary().branch.as_deref(), Some("beta / dev"));
        assert_eq!(project.git_summary().changes, 1);
        assert_eq!(project.abs_path(Path::new("b.txt")), Some(beta.join("b.txt")));
        assert_eq!(project.repo_root_for_path(&alpha.join("a.txt")), Some(project.repos()[0].repo.root().to_path_buf()));
        assert_eq!(project.repo_root_for_path(&beta.join("b.txt")), Some(beta_root));
        assert_eq!(project.repo_root_for_path(parent.path()), None);
    }

    #[test]
    fn jobs_run_in_order_and_refresh_the_status() {
        let dir = repo();
        std::fs::write(dir.path().join("a.txt"), "one\nchanged\n").unwrap();
        let mut project = open(dir.path());
        assert!(project.status().unwrap().staged().is_empty());

        let mut staged = project.git_job("Stage a.txt", |repo| repo.stage(&["a.txt"]).map(|()| "Staged a.txt".to_string()));
        let committed = project.git_job("Commit", |repo| {
            repo.commit("second", bakedit_git::CommitOptions::default()).map(|sha| format!("Committed {}", &sha[..7]))
        });
        assert_eq!(project.busy().as_deref(), Some("Stage a.txt"));

        let events = until(&mut project, |project, seen| {
            seen.iter().filter(|event| matches!(event, ProjectEvent::JobFinished { .. })).count() == 2
                && project.status().is_some_and(|status| status.is_clean())
        });
        let finished: Vec<&ProjectEvent> = events.iter().filter(|event| matches!(event, ProjectEvent::JobFinished { .. })).collect();
        assert_eq!(finished[0], &ProjectEvent::JobFinished { label: "Stage a.txt".into(), result: Ok("Staged a.txt".into()) });
        assert!(matches!(finished[1], ProjectEvent::JobFinished { label, result: Ok(message) } if label == "Commit" && message.starts_with("Committed ")));
        assert!(events.contains(&ProjectEvent::Git));
        assert_eq!(project.busy(), None);
        // The views that started the jobs hear the same outcome.
        let started = Instant::now();
        let outcome = loop {
            if let Some(outcome) = staged.poll() {
                break outcome;
            }
            assert!(started.elapsed() < WAIT);
            std::thread::sleep(Duration::from_millis(2));
        };
        assert_eq!(outcome, Ok("Staged a.txt".to_string()));
        assert!(committed.wait(WAIT).unwrap().is_ok());
    }

    #[test]
    fn a_failed_job_reports_why_in_one_line() {
        let dir = repo();
        let mut project = open(dir.path());
        let task = project.git_job("Commit", |repo| repo.commit("nothing to commit", bakedit_git::CommitOptions::default()));
        let events = until(&mut project, |_, seen| seen.iter().any(|event| matches!(event, ProjectEvent::JobFinished { .. })));
        let failure = events
            .iter()
            .find_map(|event| match event {
                ProjectEvent::JobFinished { label, result: Err(reason) } => Some((label.clone(), reason.clone())),
                _ => None,
            })
            .expect("the commit failed");
        assert_eq!(failure.0, "Commit");
        assert!(failure.1.contains("nothing is staged"), "{failure:?}");
        assert!(!failure.1.contains('\n'));
        assert!(task.wait(WAIT).unwrap().is_err());
    }

    #[test]
    fn changes_on_disk_reach_the_views_the_index_and_the_status() {
        let dir = repo();
        let mut project = open(dir.path());
        assert!(project.status().unwrap().is_clean());
        let before = project.git_generation();
        let created = dir.path().join("src/extra.rs");
        std::fs::write(&created, "// new\n").unwrap();

        let events = until(&mut project, |project, seen| {
            let told = seen.iter().any(|event| matches!(event, ProjectEvent::Fs(change) if change.touches(&created)));
            told && project.status().is_some_and(|status| status.entries.len() == 1) && !project.find_files("extra", 5).is_empty()
        });
        assert!(events.contains(&ProjectEvent::Git), "{events:?}");
        assert!(project.git_generation() > before);
        assert_eq!(project.status_of(&created).map(|entry| entry.untracked), Some(true));
    }

    #[test]
    fn a_commit_made_elsewhere_is_noticed() {
        let dir = repo();
        let mut project = open(dir.path());
        let first = project.status().unwrap().head_sha.clone();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\nthree\n").unwrap();
        git(dir.path(), &["commit", "--quiet", "-am", "outside"]);
        until(&mut project, |project, _| {
            project.status().is_some_and(|status| status.head_sha != first && status.is_clean())
        });
    }

    #[test]
    fn committed_and_staged_text_for_the_gutter() {
        let dir = repo();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "one\ntwo\nstaged\n").unwrap();
        git(dir.path(), &["add", "a.txt"]);
        std::fs::write(&path, "one\ntwo\nstaged\nunstaged\n").unwrap();
        let mut project = open(dir.path());

        assert_eq!(project.git_base(&path), None, "asked for, not read yet");
        until(&mut project, |project, _| project.git_base(&path).is_some());
        let base = project.git_base(&path).unwrap();
        assert_eq!(base.head.as_deref(), Some("one\ntwo\n"));
        assert_eq!(base.index.as_deref(), Some("one\ntwo\nstaged\n"));
        assert!(Arc::ptr_eq(&base, &project.git_base(&path).unwrap()), "the same value until git changes");

        // A file git has never seen has neither text; one outside the repository has no answer.
        let untracked = dir.path().join("fresh.txt");
        std::fs::write(&untracked, "x").unwrap();
        until(&mut project, |project, _| project.git_base(&untracked).is_some());
        assert_eq!(*project.git_base(&untracked).unwrap(), GitBase::default());
        assert_eq!(project.git_base(Path::new("/etc/hosts")), None);

        // Committing moves HEAD: the old value stays until the new one has been read.
        let task = project.git_job("Commit", |repo| repo.commit("staged", bakedit_git::CommitOptions::default()));
        until(&mut project, |project, _| {
            project.git_base(&path).is_some_and(|now| now.head.as_deref() == Some("one\ntwo\nstaged\n"))
        });
        assert!(!Arc::ptr_eq(&base, &project.git_base(&path).unwrap()));
        assert!(task.wait(WAIT).unwrap().is_ok());
    }

    #[test]
    fn branches_arrive_and_follow_the_repository() {
        let dir = repo();
        git(dir.path(), &["branch", "feature"]);
        let mut project = open(dir.path());
        assert_eq!(project.branches(), None);
        let version = project.branches_version();
        until(&mut project, |project, _| project.branches().is_some());
        assert!(project.branches_version() > version);
        let names: Vec<String> = project.branches().unwrap().iter().map(|branch| branch.name.clone()).collect();
        assert!(names.contains(&"main".to_string()) && names.contains(&"feature".to_string()), "{names:?}");

        let task = project.git_job("Switch to feature", |repo| repo.checkout("feature").map(|()| String::new()));
        until(&mut project, |project, _| {
            project.status().is_some_and(|status| status.branch.as_deref() == Some("feature"))
                && project.branches().is_some_and(|branches| branches.iter().any(|branch| branch.current && branch.name == "feature"))
        });
        assert_eq!(project.git_summary().branch.as_deref(), Some("feature"));
        assert!(task.wait(WAIT).unwrap().is_ok());
    }

    #[test]
    fn recent_files_and_search_history() {
        let project = Project::detached("/p");
        project.touch_recent(Path::new("/p/a.rs"));
        project.touch_recent(Path::new("/p/b.rs"));
        project.touch_recent(Path::new("/p/a.rs"));
        assert_eq!(project.recent_files(), vec![PathBuf::from("/p/a.rs"), PathBuf::from("/p/b.rs")]);
        for i in 0..300 {
            project.touch_recent(Path::new(&format!("/p/{i}")));
        }
        assert_eq!(project.recent_files().len(), HISTORY_LIMIT);
        project.set_recent_files(vec![PathBuf::from("/p/z.rs")]);
        assert_eq!(project.recent_files(), vec![PathBuf::from("/p/z.rs")]);

        project.remember_search(SearchKind::Buffer, "needle");
        project.remember_search(SearchKind::Buffer, "");
        project.remember_search(SearchKind::Project, "haystack");
        project.remember_search(SearchKind::Buffer, "pin");
        project.remember_search(SearchKind::Buffer, "needle");
        assert_eq!(project.search_history(SearchKind::Buffer), vec!["needle".to_string(), "pin".to_string()]);
        assert_eq!(project.search_history(SearchKind::Project), vec!["haystack".to_string()]);
    }

    #[test]
    fn recent_files_come_first_in_the_finder() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["alpha.rs", "beta.rs", "gamma.rs"] {
            std::fs::write(dir.path().join(name), "").unwrap();
        }
        let project = open(dir.path());
        assert_eq!(project.find_files("", 10), Vec::new(), "nothing recent yet");
        project.touch_recent(&dir.path().join("gamma.rs"));
        project.touch_recent(&dir.path().join("alpha.rs"));
        let listed: Vec<PathBuf> = project.find_files("", 10).into_iter().map(|found| found.path).collect();
        assert_eq!(listed, vec![PathBuf::from("alpha.rs"), PathBuf::from("gamma.rs")]);
        assert!(project.index_generation() > 0);
    }

    #[test]
    fn highlights_come_back_for_the_document_that_asked() {
        let mut project = Project::detached("/p");
        let (first, second) = (project.next_doc_id(), project.next_doc_id());
        assert_ne!(first, second);
        assert!(project.syntax(first).is_none());
        let rust = bakedit_syntax::language_by_id("rust").unwrap();
        project.request_syntax(first, 1, rust, "fn main() {}\nlet x = 1;\n".to_string());
        project.request_syntax(second, 7, rust, "struct S;\n".to_string());
        until(&mut project, |project, _| project.syntax(first).is_some() && project.syntax(second).is_some());
        let result = project.syntax(first).unwrap();
        assert_eq!((result.doc_id, result.version), (first, 1));
        assert!(!result.lines.spans_for_line(0).is_empty(), "`fn` is a keyword");
        assert_eq!(result.lines.line_count(), 3, "lines are counted at line feeds, like the buffer");
        assert_eq!(project.syntax(second).unwrap().version, 7);
        project.forget_syntax(first);
        assert!(project.syntax(first).is_none());
    }
}
