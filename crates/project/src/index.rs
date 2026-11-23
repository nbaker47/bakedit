//! The list of project files behind the file finder.
//!
//! [`FileIndex`] walks the project on a background thread and keeps the result as an
//! immutable [`IndexSnapshot`]. Callers never wait for the walk: [`FileIndex::search`]
//! and every query method work on the most recent snapshot, which is empty until the
//! first walk finishes ([`FileIndex::is_ready`]).
//!
//! The index does not watch the file system. Hand it the paths of each
//! [`crate::FsBatch`] with [`FileIndex::update`] (or call [`FileIndex::rebuild`] on a
//! rescan) and it brings the list up to date in the background. Whenever the list
//! changes, [`FileIndex::generation`] goes up and the wake callback is called, which is
//! the cue to run an open finder's query again.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use ignore::{DirEntry, ParallelVisitor, ParallelVisitorBuilder, WalkState};

use crate::fuzzy::{best_candidates, Candidate, FileMatch, Query, Scorer};
use crate::ignore_rules::{
    display_relative, has_hidden_component, is_ignore_file, is_own_temp_file, resolve,
    walk_builder, IgnoreRules,
};

/// More changed paths than this in one [`FileIndex::update`] and the index walks the
/// whole project again instead of looking at each path.
const MAX_INCREMENTAL_PATHS: usize = 2_000;

/// The fewest files worth giving a search thread of their own.
const FILES_PER_SEARCH_THREAD: usize = 8_192;

/// The most threads one search uses.
const MAX_SEARCH_THREADS: usize = 8;

/// How a [`FileIndex`] decides which files to list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IndexOptions {
    /// List files that `.gitignore`, `.ignore`, `.git/info/exclude` or the global
    /// gitignore exclude. Off by default. [`crate::ALWAYS_HIDDEN`] names are never listed.
    pub include_ignored: bool,
}

/// An immutable, sorted list of file paths relative to the project root.
///
/// Paths are stored as the text a finder shows: `/` separators, and names that are not
/// valid UTF-8 converted lossily (their real path is kept on the side, see
/// [`IndexSnapshot::path`]). The order is plain byte order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IndexSnapshot {
    /// Every path, back to back.
    text: String,
    /// Where each path ends in `text`.
    ends: Vec<u32>,
    /// The real relative path of the entries whose displayed text is lossy.
    raw: HashMap<String, PathBuf>,
}

impl IndexSnapshot {
    /// A snapshot of the given relative paths (with `/` separators), sorted and with
    /// duplicates removed. Mostly useful for tests; a [`FileIndex`] builds its own.
    pub fn from_paths<I, S>(paths: I) -> IndexSnapshot
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let found = paths
            .into_iter()
            .map(|path| Found {
                display: path.into(),
                raw: None,
            })
            .collect();
        IndexSnapshot::from_found(found)
    }

    fn from_found(mut found: Vec<Found>) -> IndexSnapshot {
        found.sort_unstable_by(|a, b| a.display.cmp(&b.display));
        found.dedup_by(|later, earlier| later.display == earlier.display);
        let mut snapshot = IndexSnapshot {
            text: String::with_capacity(found.iter().map(|entry| entry.display.len()).sum()),
            ends: Vec::with_capacity(found.len()),
            raw: HashMap::new(),
        };
        for entry in found {
            snapshot.push(&entry.display, entry.raw);
        }
        snapshot
    }

    /// Append a path. The caller keeps the paths sorted and unique. Paths beyond 4 GiB
    /// of total text are dropped rather than corrupting the offsets.
    fn push(&mut self, display: &str, raw: Option<PathBuf>) {
        let Ok(end) = u32::try_from(self.text.len() + display.len()) else {
            log::warn!("file index is full, dropping {display}");
            return;
        };
        self.text.push_str(display);
        self.ends.push(end);
        if let Some(raw) = raw {
            self.raw.insert(display.to_string(), raw);
        }
    }

    /// The number of files.
    pub fn len(&self) -> usize {
        self.ends.len()
    }

    /// Whether there are no files.
    pub fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }

    /// The displayed path of the file at `index`, or `None` past the end.
    pub fn get(&self, index: usize) -> Option<&str> {
        let end = *self.ends.get(index)? as usize;
        let start = match index {
            0 => 0,
            _ => self.ends[index - 1] as usize,
        };
        self.text.get(start..end)
    }

    /// Every displayed path, in order.
    pub fn iter(&self) -> impl Iterator<Item = &str> + '_ {
        (0..self.len()).filter_map(|index| self.get(index))
    }

    /// Whether the displayed path `relative` is in the list.
    pub fn contains(&self, relative: &str) -> bool {
        let index = self.partition_point(|entry| entry < relative);
        self.get(index) == Some(relative)
    }

    /// The real relative path behind a displayed one. They only differ for file names
    /// that are not valid UTF-8.
    pub fn path(&self, display: &str) -> PathBuf {
        match self.raw.get(display) {
            Some(raw) => raw.clone(),
            None => PathBuf::from(display),
        }
    }

    /// Fuzzy-search the list. This is [`FileIndex::search`] without the index around
    /// it: `recent` holds displayed relative paths, most recent first, and is trusted
    /// as given (a recent path that is not in the list is still searched and returned).
    ///
    /// See [`FileIndex::search`] for the matching rules.
    pub fn search(&self, query: &str, limit: usize, recent: &[String]) -> Vec<FileMatch> {
        let query = Query::new(query);
        if query.is_empty() {
            return recent
                .iter()
                .take(limit)
                .map(|path| FileMatch {
                    path: self.path(path),
                    score: 0,
                    indices: Vec::new(),
                })
                .collect();
        }

        let mut ranks: HashMap<&str, usize> = HashMap::new();
        for (rank, path) in recent.iter().enumerate() {
            ranks.entry(path.as_str()).or_insert(rank);
        }
        let mut candidates = self.score_all(&query, &ranks, limit);
        let mut scorer = Scorer::new();
        for (path, rank) in &ranks {
            if !self.contains(path) {
                if let Some(score) = scorer.score(&query, path) {
                    candidates.push(Candidate {
                        path,
                        score,
                        recent_rank: *rank,
                    });
                }
            }
        }

        best_candidates(candidates, limit)
            .into_iter()
            .map(|candidate| {
                let indices = scorer
                    .score_with_indices(&query, candidate.path)
                    .map(|(_, indices)| indices)
                    .unwrap_or_default();
                FileMatch {
                    path: self.path(candidate.path),
                    score: candidate.score,
                    indices,
                }
            })
            .collect()
    }

    /// Score every file, on several threads when the list is long. Each thread already
    /// narrows its share down to its best `limit`.
    fn score_all<'a>(
        &'a self,
        query: &Query,
        ranks: &HashMap<&str, usize>,
        limit: usize,
    ) -> Vec<Candidate<'a>> {
        let len = self.len();
        let available = thread::available_parallelism().map_or(1, usize::from);
        let threads = (len / FILES_PER_SEARCH_THREAD).clamp(1, available.min(MAX_SEARCH_THREADS));
        if threads == 1 {
            return self.score_range(0..len, query, ranks, limit);
        }
        let chunk = len.div_ceil(threads);
        thread::scope(|scope| {
            let workers: Vec<_> = (0..threads)
                .map(|worker| {
                    let range = worker * chunk..((worker + 1) * chunk).min(len);
                    scope.spawn(move || self.score_range(range, query, ranks, limit))
                })
                .collect();
            workers
                .into_iter()
                .flat_map(|worker| worker.join().unwrap_or_default())
                .collect()
        })
    }

    fn score_range<'a>(
        &'a self,
        range: Range<usize>,
        query: &Query,
        ranks: &HashMap<&str, usize>,
        limit: usize,
    ) -> Vec<Candidate<'a>> {
        let mut scorer = Scorer::new();
        let mut candidates = Vec::new();
        for index in range {
            let Some(path) = self.get(index) else {
                continue;
            };
            if let Some(score) = scorer.score(query, path) {
                candidates.push(Candidate {
                    path,
                    score,
                    recent_rank: ranks.get(path).copied().unwrap_or(usize::MAX),
                });
            }
        }
        best_candidates(candidates, limit)
    }

    /// The number of leading entries for which `before` holds. `before` must be true for
    /// a prefix of the sorted list and false for the rest.
    fn partition_point(&self, before: impl Fn(&str) -> bool) -> usize {
        let (mut low, mut high) = (0, self.len());
        while low < high {
            let middle = low + (high - low) / 2;
            match self.get(middle) {
                Some(entry) if before(entry) => low = middle + 1,
                _ => high = middle,
            }
        }
        low
    }

    /// The indexes of the entries below the directory prefix `prefix` (which ends in `/`).
    fn range_below(&self, prefix: &str) -> Range<usize> {
        let start = self.partition_point(|entry| entry < prefix);
        let end = self.partition_point(|entry| entry < prefix || entry.starts_with(prefix));
        start..end
    }

    /// This snapshot with `changes` applied. Additions win over removals.
    fn with_changes(&self, changes: Changes) -> IndexSnapshot {
        let mut removed: Vec<Range<usize>> = changes
            .removed_below
            .iter()
            .map(|prefix| self.range_below(prefix))
            .chain(changes.removed.iter().map(|path| {
                let index = self.partition_point(|entry| entry < path.as_str());
                match self.get(index) {
                    Some(entry) if entry == path => index..index + 1,
                    _ => index..index,
                }
            }))
            .filter(|range| !range.is_empty())
            .collect();
        removed.sort_unstable_by_key(|range| range.start);

        let mut snapshot = IndexSnapshot {
            text: String::with_capacity(self.text.len()),
            ends: Vec::with_capacity(self.len() + changes.added.len()),
            raw: HashMap::new(),
        };
        let mut added = changes.added.into_iter().peekable();
        let mut removed = removed.into_iter().peekable();
        for (index, old) in self.iter().enumerate() {
            while let Some((path, _)) = added.peek() {
                if path.as_str() >= old {
                    break;
                }
                if let Some((path, raw)) = added.next() {
                    snapshot.push(&path, raw);
                }
            }
            while removed.peek().is_some_and(|range| range.end <= index) {
                removed.next();
            }
            if added.peek().is_some_and(|(path, _)| path == old) {
                if let Some((path, raw)) = added.next() {
                    snapshot.push(&path, raw);
                }
                continue;
            }
            if removed.peek().is_some_and(|range| range.contains(&index)) {
                continue;
            }
            snapshot.push(old, self.raw.get(old).cloned());
        }
        for (path, raw) in added {
            snapshot.push(&path, raw);
        }
        snapshot
    }
}

/// One file a walk found.
struct Found {
    display: String,
    /// The real relative path, when `display` could not represent it exactly.
    raw: Option<PathBuf>,
}

impl Found {
    fn new(relative: &Path) -> Found {
        Found {
            display: display_relative(relative),
            raw: relative.to_str().is_none().then(|| relative.to_path_buf()),
        }
    }
}

/// What a batch of file-system changes does to the list.
#[derive(Default)]
struct Changes {
    /// Displayed paths that are no longer files.
    removed: BTreeSet<String>,
    /// Directory prefixes (ending in `/`) whose whole contents go.
    removed_below: Vec<String>,
    /// Files to list, with their real path when the text is lossy.
    added: BTreeMap<String, Option<PathBuf>>,
}

enum Command {
    Rebuild,
    Update(Vec<PathBuf>),
    IncludeIgnored(bool),
}

enum Outcome {
    Unchanged,
    Changed(IndexSnapshot),
    Rebuild,
}

struct State {
    snapshot: Arc<IndexSnapshot>,
    generation: u64,
    ready: bool,
    include_ignored: bool,
    /// Commands handed to the worker.
    submitted: u64,
    /// Commands the worker has fully dealt with.
    completed: u64,
}

struct Shared {
    root: PathBuf,
    state: Mutex<State>,
    progress: Condvar,
    stop: AtomicBool,
    wake: Option<Box<dyn Fn() + Send + Sync>>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn stopping(&self) -> bool {
        self.stop.load(AtomicOrdering::Relaxed)
    }

    /// Make `snapshot` the current list. The generation only moves when the list
    /// differs from the one before, or on the very first publish.
    fn publish(&self, snapshot: IndexSnapshot) {
        let mut state = self.lock();
        let first = !state.ready;
        let changed = first || *state.snapshot != snapshot;
        if changed {
            state.snapshot = Arc::new(snapshot);
            state.generation += 1;
        }
        state.ready = true;
        drop(state);
        self.progress.notify_all();
        if changed {
            if let Some(wake) = &self.wake {
                wake();
            }
        }
    }

    fn mark_completed(&self, commands: u64) {
        self.lock().completed = commands;
        self.progress.notify_all();
    }
}

/// The project's file list, kept up to date on a background thread. See the
/// [module documentation](self).
pub struct FileIndex {
    shared: Arc<Shared>,
    commands: Option<Sender<Command>>,
    worker: Option<JoinHandle<()>>,
}

impl FileIndex {
    /// Start indexing the directory `root`. Returns immediately; the first walk runs in
    /// the background and the index is empty until [`FileIndex::is_ready`].
    ///
    /// A relative `root` is made absolute against the current directory.
    pub fn new(root: impl AsRef<Path>, options: IndexOptions) -> FileIndex {
        FileIndex::start(root.as_ref(), options, None)
    }

    /// Like [`FileIndex::new`], and `wake` is called (on the index thread) every time
    /// the file list changes, including when the first walk completes. Keep it short: it
    /// is meant to poke the app's event loop.
    pub fn with_wake(
        root: impl AsRef<Path>,
        options: IndexOptions,
        wake: Box<dyn Fn() + Send + Sync>,
    ) -> FileIndex {
        FileIndex::start(root.as_ref(), options, Some(wake))
    }

    fn start(
        root: &Path,
        options: IndexOptions,
        wake: Option<Box<dyn Fn() + Send + Sync>>,
    ) -> FileIndex {
        let shared = Arc::new(Shared {
            root: crate::ignore_rules::normalize(root),
            state: Mutex::new(State {
                snapshot: Arc::new(IndexSnapshot::default()),
                generation: 0,
                ready: false,
                include_ignored: options.include_ignored,
                submitted: 0,
                completed: 0,
            }),
            progress: Condvar::new(),
            stop: AtomicBool::new(false),
            wake,
        });
        let (commands, receiver) = crossbeam_channel::unbounded();
        let worker_shared = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("bakedit-file-index".to_string())
            .spawn(move || run_worker(&worker_shared, &receiver))
            .map_err(|error| log::error!("could not start the file index thread: {error}"))
            .ok();
        FileIndex {
            shared,
            commands: Some(commands),
            worker,
        }
    }

    /// The project root, absolute.
    pub fn root(&self) -> &Path {
        &self.shared.root
    }

    /// Whether the first walk has completed. Until then the list is empty.
    pub fn is_ready(&self) -> bool {
        self.shared.lock().ready
    }

    /// Whether a walk or an update is running or queued, i.e. the list may be about to
    /// change. Handy for a "indexing…" hint.
    pub fn is_scanning(&self) -> bool {
        let state = self.shared.lock();
        !state.ready || state.completed < state.submitted
    }

    /// A counter that goes up every time the file list changes. Starts at 0 and becomes
    /// 1 when the first walk completes.
    pub fn generation(&self) -> u64 {
        self.shared.lock().generation
    }

    /// The number of files currently listed.
    pub fn len(&self) -> usize {
        self.shared.lock().snapshot.len()
    }

    /// Whether no files are listed (always the case before the first walk completes).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The current file list. Cheap: the snapshot is shared, not copied.
    pub fn snapshot(&self) -> Arc<IndexSnapshot> {
        Arc::clone(&self.shared.lock().snapshot)
    }

    /// Whether ignored files are listed.
    pub fn include_ignored(&self) -> bool {
        self.shared.lock().include_ignored
    }

    /// Choose whether ignored files are listed. A change walks the project again in the
    /// background.
    pub fn set_include_ignored(&self, include: bool) {
        let changed = {
            let mut state = self.shared.lock();
            let changed = state.include_ignored != include;
            state.include_ignored = include;
            changed
        };
        if changed {
            self.send(Command::IncludeIgnored(include));
        }
    }

    /// Walk the whole project again in the background. Use it when the watcher reports a
    /// rescan. Searches keep using the old list until the new one is ready.
    pub fn rebuild(&self) {
        self.send(Command::Rebuild);
    }

    /// Bring the list up to date after `paths` (absolute, or relative to the root)
    /// changed on disk. Returns immediately; the work happens in the background.
    ///
    /// Each path is looked at as it is now: a file is added (unless ignored), a path
    /// that is gone is removed together with everything below it, a directory is walked
    /// again. A change to an ignore file, or a very large batch, walks the whole project.
    pub fn update<P: AsRef<Path>>(&self, paths: &[P]) {
        if paths.is_empty() {
            return;
        }
        let paths = paths
            .iter()
            .map(|path| path.as_ref().to_path_buf())
            .collect();
        self.send(Command::Update(paths));
    }

    fn send(&self, command: Command) {
        let Some(commands) = &self.commands else {
            return;
        };
        let mut state = self.shared.lock();
        if commands.send(command).is_ok() {
            state.submitted += 1;
        }
    }

    /// Block until the first walk has completed, or `timeout` has passed. Returns
    /// whether the index is ready. For tests and command-line use; a UI should poll
    /// [`FileIndex::is_ready`] or use the wake callback instead.
    pub fn wait_ready(&self, timeout: Duration) -> bool {
        self.wait(timeout, |state| state.ready)
    }

    /// Block until everything requested so far ([`FileIndex::update`],
    /// [`FileIndex::rebuild`], the first walk) has been applied, or `timeout` has passed.
    /// Returns whether the index caught up.
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let target = self.shared.lock().submitted;
        self.wait(timeout, |state| state.ready && state.completed >= target)
    }

    fn wait(&self, timeout: Duration, done: impl Fn(&State) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self.shared.lock();
        loop {
            if done(&state) {
                return true;
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            if self.worker.as_ref().is_none_or(JoinHandle::is_finished) {
                return done(&state);
            }
            state = self
                .shared
                .progress
                .wait_timeout(state, left.min(Duration::from_millis(100)))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Fuzzy-search the file list for the file finder. Returns at most `limit` matches,
    /// best first.
    ///
    /// `query` is the path part of what the user typed ([`crate::parse_query`] strips a
    /// `:line:col` suffix). Its whitespace-separated words all have to match, smart-case;
    /// a match in the file name beats a match in the directory part; ties go to recent
    /// files, then the shorter path, then alphabetical order. The rules are spelled out
    /// in the [`crate::fuzzy`] module.
    ///
    /// `recent` lists recently opened files (absolute, or relative to the root), most
    /// recent first; entries outside the project are skipped. An **empty query returns
    /// the recent files in that order**, leaving out the ones that no longer exist. A
    /// recent file that is not in the index (an ignored file that was opened from the
    /// tree, say) is still searched.
    ///
    /// The search runs on the calling thread, with helper threads for long lists, on
    /// the current snapshot: it never waits for indexing, and before the first walk
    /// completes it only knows the recent files.
    pub fn search(&self, query: &str, limit: usize, recent: &[PathBuf]) -> Vec<FileMatch> {
        let snapshot = self.snapshot();
        let mut known: Vec<String> = Vec::new();
        for path in recent {
            let absolute = resolve(&self.shared.root, path);
            let Ok(relative) = absolute.strip_prefix(&self.shared.root) else {
                continue;
            };
            let display = display_relative(relative);
            if display.is_empty() || known.contains(&display) {
                continue;
            }
            if snapshot.contains(&display) || fs::metadata(&absolute).is_ok_and(|m| m.is_file()) {
                known.push(display);
            }
        }
        snapshot.search(query, limit, &known)
    }
}

impl Drop for FileIndex {
    fn drop(&mut self) {
        self.shared.stop.store(true, AtomicOrdering::Relaxed);
        self.commands = None;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn run_worker(shared: &Shared, commands: &Receiver<Command>) {
    let mut rules = IgnoreRules::new(&shared.root);
    let mut include_ignored = shared.lock().include_ignored;
    let mut rebuild = true;
    let mut changed: Vec<PathBuf> = Vec::new();
    let mut received: u64 = 0;

    loop {
        let mut next = if !rebuild && changed.is_empty() {
            shared.mark_completed(received);
            match commands.recv() {
                Ok(command) => Some(command),
                Err(_) => return,
            }
        } else {
            commands.try_recv().ok()
        };
        // Take everything that is already queued, so a burst of updates is one pass.
        while let Some(command) = next {
            received += 1;
            match command {
                Command::Rebuild => rebuild = true,
                Command::Update(paths) => changed.extend(paths),
                Command::IncludeIgnored(include) => {
                    rebuild |= include != include_ignored;
                    include_ignored = include;
                }
            }
            next = commands.try_recv().ok();
        }
        if shared.stopping() {
            return;
        }

        if rebuild {
            rebuild = false;
            changed.clear();
            rules.reload();
            match walk(shared, &shared.root, include_ignored) {
                Some(found) => shared.publish(IndexSnapshot::from_found(found)),
                None => return,
            }
        } else {
            let batch = std::mem::take(&mut changed);
            match apply_changes(shared, &mut rules, include_ignored, batch) {
                Outcome::Unchanged => {}
                Outcome::Changed(snapshot) => shared.publish(snapshot),
                Outcome::Rebuild => rebuild = true,
            }
        }
    }
}

/// Every file below `start`, or `None` if the index is shutting down.
fn walk(shared: &Shared, start: &Path, include_ignored: bool) -> Option<Vec<Found>> {
    let collected = Mutex::new(Vec::new());
    let mut builder = CollectorBuilder {
        shared,
        collected: &collected,
    };
    walk_builder(&shared.root, start, include_ignored, None)
        .build_parallel()
        .visit(&mut builder);
    if shared.stopping() {
        return None;
    }
    Some(
        collected
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner),
    )
}

struct CollectorBuilder<'a> {
    shared: &'a Shared,
    collected: &'a Mutex<Vec<Found>>,
}

impl<'a> ParallelVisitorBuilder<'a> for CollectorBuilder<'a> {
    fn build(&mut self) -> Box<dyn ParallelVisitor + 'a> {
        Box::new(Collector {
            shared: self.shared,
            collected: self.collected,
            found: Vec::new(),
        })
    }
}

/// Collects the files one walker thread sees and hands them over when the thread ends.
struct Collector<'a> {
    shared: &'a Shared,
    collected: &'a Mutex<Vec<Found>>,
    found: Vec<Found>,
}

impl ParallelVisitor for Collector<'_> {
    fn visit(&mut self, entry: Result<DirEntry, ignore::Error>) -> WalkState {
        if self.shared.stopping() {
            return WalkState::Quit;
        }
        let Ok(entry) = entry else {
            return WalkState::Continue;
        };
        let Some(file_type) = entry.file_type() else {
            return WalkState::Continue;
        };
        // A link to a file is listed like a file; a link to a directory is not followed.
        let is_file = if file_type.is_symlink() {
            fs::metadata(entry.path()).is_ok_and(|meta| meta.is_file())
        } else {
            file_type.is_file()
        };
        if is_file {
            if let Ok(relative) = entry.path().strip_prefix(&self.shared.root) {
                self.found.push(Found::new(relative));
            }
        }
        WalkState::Continue
    }
}

impl Drop for Collector<'_> {
    fn drop(&mut self) {
        self.collected
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .append(&mut self.found);
    }
}

/// What a changed path is now.
enum Kind {
    /// A file, or a link to one.
    File,
    /// A real directory.
    Dir,
    /// Gone, or something that is never listed (a link to a directory, a socket).
    Nothing,
}

fn kind_of(path: &Path) -> Kind {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return Kind::Nothing;
    };
    if meta.is_dir() {
        return Kind::Dir;
    }
    let is_file = if meta.file_type().is_symlink() {
        fs::metadata(path).is_ok_and(|target| target.is_file())
    } else {
        meta.is_file()
    };
    if is_file {
        Kind::File
    } else {
        Kind::Nothing
    }
}

/// Look at each changed path and work out the new list.
fn apply_changes(
    shared: &Shared,
    rules: &mut IgnoreRules,
    include_ignored: bool,
    paths: Vec<PathBuf>,
) -> Outcome {
    if paths.len() > MAX_INCREMENTAL_PATHS {
        return Outcome::Rebuild;
    }
    let mut relative_paths = Vec::with_capacity(paths.len());
    for path in paths {
        let absolute = resolve(&shared.root, &path);
        let Ok(relative) = absolute.strip_prefix(&shared.root) else {
            continue;
        };
        if relative.as_os_str().is_empty() || is_ignore_file(&absolute) {
            return Outcome::Rebuild;
        }
        if has_hidden_component(relative) || is_own_temp_file(relative) {
            continue;
        }
        relative_paths.push(relative.to_path_buf());
    }
    // Sorted by component, so everything inside a directory follows it directly.
    relative_paths.sort();
    relative_paths.dedup();

    let mut changes = Changes::default();
    let mut walked: Option<PathBuf> = None;
    for relative in relative_paths {
        if walked.as_ref().is_some_and(|dir| relative.starts_with(dir)) {
            continue;
        }
        let absolute = shared.root.join(&relative);
        let found = Found::new(&relative);
        // Whatever the path is now, what was listed below it is out of date.
        changes.removed_below.push(format!("{}/", found.display));
        match kind_of(&absolute) {
            Kind::Nothing => {
                changes.removed.insert(found.display);
            }
            Kind::File => {
                if include_ignored || !rules.is_ignored(&relative, false) {
                    changes.added.insert(found.display, found.raw);
                } else {
                    changes.removed.insert(found.display);
                }
            }
            Kind::Dir => {
                changes.removed.insert(found.display);
                if include_ignored || !rules.is_ignored(&relative, true) {
                    let Some(files) = walk(shared, &absolute, include_ignored) else {
                        return Outcome::Unchanged;
                    };
                    for file in files {
                        changes.added.insert(file.display, file.raw);
                    }
                }
                walked = Some(relative);
            }
        }
    }

    let old = shared.lock().snapshot.clone();
    let new = old.with_changes(changes);
    if *old == new {
        Outcome::Unchanged
    } else {
        Outcome::Changed(new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(paths: &[&str]) -> IndexSnapshot {
        IndexSnapshot::from_paths(paths.iter().copied())
    }

    fn listed(snapshot: &IndexSnapshot) -> Vec<&str> {
        snapshot.iter().collect()
    }

    #[test]
    fn snapshots_are_sorted_and_unique() {
        let snapshot = snapshot(&["src/b.rs", "a.txt", "src/a.rs", "a.txt"]);
        assert_eq!(listed(&snapshot), ["a.txt", "src/a.rs", "src/b.rs"]);
        assert_eq!(snapshot.len(), 3);
        assert_eq!(snapshot.get(1), Some("src/a.rs"));
        assert_eq!(snapshot.get(3), None);
        assert!(snapshot.contains("src/b.rs"));
        assert!(!snapshot.contains("src"));
        assert!(!snapshot.contains("src/c.rs"));
        assert!(IndexSnapshot::default().is_empty());
        assert_eq!(IndexSnapshot::default().get(0), None);
    }

    #[test]
    fn directory_ranges_are_found_by_prefix() {
        let snapshot = snapshot(&[
            "a.txt",
            "src-old/x",
            "src/a.rs",
            "src/sub/b.rs",
            "srcs",
            "z",
        ]);
        let range = snapshot.range_below("src/");
        let below: Vec<&str> = range.filter_map(|index| snapshot.get(index)).collect();
        assert_eq!(below, ["src/a.rs", "src/sub/b.rs"]);
        assert!(snapshot.range_below("missing/").is_empty());
        assert!(snapshot.range_below("a.txt/").is_empty());
    }

    fn changes(removed: &[&str], removed_below: &[&str], added: &[&str]) -> Changes {
        Changes {
            removed: removed.iter().map(|path| path.to_string()).collect(),
            removed_below: removed_below.iter().map(|path| path.to_string()).collect(),
            added: added.iter().map(|path| (path.to_string(), None)).collect(),
        }
    }

    #[test]
    fn changes_add_and_remove_in_order() {
        let old = snapshot(&["a", "c", "dir/x", "dir/y", "e"]);
        let new = old.with_changes(changes(&["c", "missing"], &["dir/"], &["b", "f", "0"]));
        assert_eq!(listed(&new), ["0", "a", "b", "e", "f"]);
    }

    #[test]
    fn additions_win_over_removals() {
        let old = snapshot(&["dir/old", "dir/stays", "keep"]);
        // A directory walked again: everything below goes, what was found comes back.
        let new = old.with_changes(changes(&["dir"], &["dir/"], &["dir/new", "dir/stays"]));
        assert_eq!(listed(&new), ["dir/new", "dir/stays", "keep"]);
        // A path both removed and added stays.
        let same = old.with_changes(changes(&["keep"], &[], &["keep"]));
        assert_eq!(same, old);
    }

    #[test]
    fn overlapping_removals_are_handled() {
        let old = snapshot(&["a/b/c", "a/b/d", "a/e", "f"]);
        let new = old.with_changes(changes(&["a/e"], &["a/b/", "a/"], &[]));
        assert_eq!(listed(&new), ["f"]);
        let empty = IndexSnapshot::default().with_changes(changes(&["x"], &["y/"], &["z"]));
        assert_eq!(listed(&empty), ["z"]);
    }

    #[test]
    fn an_empty_query_returns_the_recent_files_in_order() {
        let snapshot = snapshot(&["a.rs", "b.rs", "c.rs"]);
        let recent = vec!["c.rs".to_string(), "a.rs".to_string()];
        let matches = snapshot.search("", 10, &recent);
        let paths: Vec<_> = matches.iter().map(|m| m.path.clone()).collect();
        assert_eq!(paths, [PathBuf::from("c.rs"), PathBuf::from("a.rs")]);
        assert!(matches.iter().all(|m| m.score == 0 && m.indices.is_empty()));
        assert_eq!(snapshot.search("  ", 1, &recent).len(), 1);
        assert!(snapshot.search("", 10, &[]).is_empty());
    }

    fn found(snapshot: &IndexSnapshot, query: &str, recent: &[&str]) -> Vec<String> {
        let recent: Vec<String> = recent.iter().map(|path| path.to_string()).collect();
        snapshot
            .search(query, 50, &recent)
            .into_iter()
            .map(|m| m.path.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn results_are_ranked_and_limited() {
        let snapshot = snapshot(&[
            "src/main.rs",
            "src/main_loop.rs",
            "src/domain.rs",
            "main/readme.md",
            "docs/unrelated.md",
        ]);
        assert_eq!(
            found(&snapshot, "main", &[]),
            [
                "src/main.rs",
                "src/main_loop.rs",
                "src/domain.rs",
                "main/readme.md"
            ]
        );
        assert_eq!(snapshot.search("main", 2, &[]).len(), 2);
        assert!(snapshot.search("main", 0, &[]).is_empty());
        assert!(found(&snapshot, "zzzz", &[]).is_empty());
    }

    #[test]
    fn equal_scores_fall_back_to_length_then_alphabet() {
        let snapshot = snapshot(&["b/mod.rs", "a/mod.rs", "deeper/dir/mod.rs", "c/mod.rs"]);
        assert_eq!(
            found(&snapshot, "mod.rs", &[]),
            ["a/mod.rs", "b/mod.rs", "c/mod.rs", "deeper/dir/mod.rs"]
        );
    }

    #[test]
    fn recent_files_win_ties() {
        let snapshot = snapshot(&["a/mod.rs", "b/mod.rs", "c/mod.rs"]);
        assert_eq!(
            found(&snapshot, "mod", &["c/mod.rs", "b/mod.rs"]),
            ["c/mod.rs", "b/mod.rs", "a/mod.rs"]
        );
        // Recency never beats a better match.
        let snapshot = self::snapshot(&["keys.rs", "monkeys.rs"]);
        assert_eq!(
            found(&snapshot, "keys", &["monkeys.rs"]),
            ["keys.rs", "monkeys.rs"]
        );
    }

    #[test]
    fn recent_files_outside_the_list_are_still_searched() {
        let snapshot = snapshot(&["src/env.rs"]);
        assert_eq!(found(&snapshot, "env", &[".env"]), ["src/env.rs", ".env"]);
        assert_eq!(found(&snapshot, ".env", &[".env"]), [".env"]);
        assert_eq!(found(&snapshot, "zzz", &[".env"]), Vec::<String>::new());
    }

    #[test]
    fn matches_carry_highlight_positions() {
        let snapshot = snapshot(&["src/main.rs"]);
        let matches = snapshot.search("main", 10, &[]);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].path, PathBuf::from("src/main.rs"));
        assert_eq!(matches[0].indices, vec![4, 5, 6, 7]);
        assert!(matches[0].score >= crate::fuzzy::NAME_BONUS);
    }

    #[test]
    fn long_lists_search_the_same_on_many_threads() {
        let paths: Vec<String> = (0..40_000)
            .map(|n| format!("pkg{}/module{}/file{}.rs", n % 97, n % 13, n))
            .chain(["pkg0/needle_in_haystack.rs".to_string()])
            .collect();
        let snapshot = IndexSnapshot::from_paths(paths);
        let matches = snapshot.search("needlehay", 5, &[]);
        assert_eq!(matches[0].path, PathBuf::from("pkg0/needle_in_haystack.rs"));

        // The parallel search agrees with a plain single-threaded pass.
        let query = Query::new("file77");
        let ranks = HashMap::new();
        let parallel = best_candidates(snapshot.score_all(&query, &ranks, 20), 20);
        let serial = snapshot.score_range(0..snapshot.len(), &query, &ranks, 20);
        assert_eq!(parallel, serial);
        assert_eq!(parallel.len(), 20);
    }
}
