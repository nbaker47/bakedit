//! Project-wide text search and replace.
//!
//! [`search()`] walks the project on a pool of background threads and streams what it
//! finds through a channel: one [`SearchEvent::File`] per file with matches, then a
//! single [`SearchEvent::Done`]. [`replace_all`] runs the same search and rewrites the
//! files that match.
//!
//! Both are line oriented: a match never spans lines, `^` and `$` are the start and end
//! of a line, and a pattern that could only match across lines is refused. Files with a
//! NUL byte are treated as binary and skipped. Symbolic links are not followed, so a
//! file is never searched (or rewritten) twice through two names.

use std::borrow::Cow;
use std::fs;
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use grep_matcher::{Captures, LineTerminator, Matcher};
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkMatch};
use ignore::{DirEntry, ParallelVisitor, ParallelVisitorBuilder, WalkState};

use crate::fileops::write_atomic_checked;
use crate::ignore_rules::{normalize, walk_builder, EntryFilter};

/// The most characters of a line a [`LineMatch`] carries.
pub const MAX_LINE_CHARS: usize = 400;

/// How many characters before its first match a clamped line starts.
const LEADING_CONTEXT: usize = 80;

/// The default for [`SearchOptions::max_results`].
pub const DEFAULT_MAX_RESULTS: usize = 10_000;

/// Lines longer than this many bytes are not searched; the file is skipped from there.
const MAX_LINE_BYTES: usize = 64 << 20;

/// What to search for and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchOptions {
    /// The text or, with [`SearchOptions::regex`], the regular expression to find.
    pub pattern: String,
    /// Treat `pattern` as a regular expression (Rust `regex` syntax) instead of text.
    pub regex: bool,
    /// Match case exactly. Off means every letter matches both of its cases.
    pub case_sensitive: bool,
    /// Only match where the pattern is not part of a longer word.
    pub whole_word: bool,
    /// Globs of the files to search; empty means all of them. See below for the syntax.
    pub include: Vec<String>,
    /// Globs of files and directories to leave out.
    pub exclude: Vec<String>,
    /// Also search files that `.gitignore`, `.ignore` and friends exclude.
    pub include_ignored: bool,
    /// Stop after this many matches. [`SearchEvent::Done`] says whether that happened.
    /// [`replace_all`] ignores it.
    pub max_results: usize,
}

impl Default for SearchOptions {
    fn default() -> SearchOptions {
        SearchOptions {
            pattern: String::new(),
            regex: false,
            case_sensitive: false,
            whole_word: false,
            include: Vec::new(),
            exclude: Vec::new(),
            include_ignored: false,
            max_results: DEFAULT_MAX_RESULTS,
        }
    }
}

impl SearchOptions {
    /// A case-insensitive plain-text search for `pattern` with the default limits.
    ///
    /// # Globs
    ///
    /// `include` and `exclude` take globs that are matched against paths relative to
    /// the project root, the way VS Code's "files to include" does:
    ///
    /// * a glob without a `/` matches a file or directory of that name at any depth
    ///   (`*.rs`, `Cargo.toml`, `node_modules`);
    /// * a glob with a `/` is anchored at the root (`src/*.rs`, `crates/core`);
    /// * a glob that matches a directory covers everything inside it;
    /// * `*` and `?` do not cross a `/`, `**` does (`**/tests/**`), and `{a,b}` and
    ///   `[abc]` work as usual.
    pub fn new(pattern: impl Into<String>) -> SearchOptions {
        SearchOptions {
            pattern: pattern.into(),
            ..SearchOptions::default()
        }
    }
}

/// Why a search could not start.
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    /// The pattern is empty.
    #[error("the search pattern is empty")]
    EmptyPattern,
    /// The pattern is not a valid regular expression. The message says where.
    #[error("invalid pattern: {0}")]
    InvalidPattern(String),
    /// The pattern contains a line break and could only match across lines.
    #[error("the pattern contains a line break; search works one line at a time")]
    MultiLine,
    /// One of the include or exclude globs is malformed.
    #[error("invalid glob {glob:?}: {message}")]
    InvalidGlob {
        /// The glob as the caller wrote it.
        glob: String,
        /// What is wrong with it.
        message: String,
    },
    /// The background thread could not be started.
    #[error("could not start the search: {0}")]
    Thread(#[source] io::Error),
}

/// One matching line of a file, or one window of a very long line.
///
/// `line` is the text of the line without its line break. A line longer than
/// [`MAX_LINE_CHARS`] is cut down to a window of that many characters that starts a
/// little before its first match, and `clamp_offset` says how many characters were cut
/// from the front; matches further along such a line come as additional `LineMatch`es
/// with the same `line_number`. The position of a match in the real line is therefore
/// always `clamp_offset + range.start`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineMatch {
    /// 1-based line number in the file.
    pub line_number: u32,
    /// The line's text, at most [`MAX_LINE_CHARS`] characters. Bytes that are not valid
    /// UTF-8 show as U+FFFD.
    pub line: String,
    /// How many characters of the real line come before `line`. 0 unless clamped.
    pub clamp_offset: usize,
    /// The `char` range of every match inside `line`, in order. A pattern that can match
    /// nothing (`^`, `x*`) yields empty ranges. A match that runs past a clamped window
    /// is cut at its end.
    pub ranges: Vec<Range<usize>>,
}

/// What a running search reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchEvent {
    /// A file with matches. Each file is reported once; files arrive in no particular
    /// order, the lines within one in ascending order.
    File {
        /// The file, relative to the search root.
        path: PathBuf,
        /// Its matching lines, in ascending order. Never empty.
        matches: Vec<LineMatch>,
    },
    /// The search is over (finished, hit its limit, or was cancelled). Always the last
    /// event.
    Done {
        /// How many files had matches.
        files: usize,
        /// How many matches were reported in total.
        matches: usize,
        /// Whether the search stopped at [`SearchOptions::max_results`] with more to find.
        truncated: bool,
        /// How long the search ran.
        elapsed: Duration,
    },
}

/// A search in progress. Dropping it cancels the search.
#[derive(Debug)]
pub struct SearchHandle {
    events: Receiver<SearchEvent>,
    cancel: Arc<AtomicBool>,
}

impl SearchHandle {
    /// The channel the results arrive on. It ends (disconnects) after
    /// [`SearchEvent::Done`].
    pub fn events(&self) -> &Receiver<SearchEvent> {
        &self.events
    }

    /// Stop the search. Returns at once; the workers notice within one line of the file
    /// they are in, and [`SearchEvent::Done`] still follows with the totals so far.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Whether [`SearchHandle::cancel`] has been called.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

impl Drop for SearchHandle {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Start searching the directory `root` and return the handle the results stream from.
///
/// The pattern and the globs are checked before anything starts, so a bad regular
/// expression is an immediate [`SearchError`]. The search itself runs on background
/// threads and never blocks the caller.
pub fn search(root: &Path, options: SearchOptions) -> Result<SearchHandle, SearchError> {
    start_search(root, options, None)
}

/// Like [`search()`], and `wake` is called (on a search thread) after every event is
/// put on the channel. Keep it short: it is meant to poke the app's event loop.
pub fn search_with_wake(
    root: &Path,
    options: SearchOptions,
    wake: Box<dyn Fn() + Send + Sync>,
) -> Result<SearchHandle, SearchError> {
    start_search(root, options, Some(wake))
}

fn start_search(
    root: &Path,
    options: SearchOptions,
    wake: Option<Box<dyn Fn() + Send + Sync>>,
) -> Result<SearchHandle, SearchError> {
    let matcher = build_matcher(&options)?;
    let filter = Arc::new(PathFilter::new(&options.include, &options.exclude)?);
    let (events, receiver) = crossbeam_channel::unbounded();
    let cancel = Arc::new(AtomicBool::new(false));
    let context = SearchContext {
        root: normalize(root),
        matcher,
        filter,
        cancel: Arc::clone(&cancel),
        max_results: options.max_results,
        matches: AtomicUsize::new(0),
        files: AtomicUsize::new(0),
        truncated: AtomicBool::new(false),
        events,
        wake,
    };
    let include_ignored = options.include_ignored;
    thread::Builder::new()
        .name("bakedit-search".to_string())
        .spawn(move || {
            let started = Instant::now();
            let mut builder = SearchVisitorBuilder { context: &context };
            walk_builder(
                &context.root,
                &context.root,
                include_ignored,
                Some(context.filter.walk_filter()),
            )
            .build_parallel()
            .visit(&mut builder);
            context.send(SearchEvent::Done {
                files: context.files.load(Ordering::Relaxed),
                matches: context.matches.load(Ordering::Relaxed),
                truncated: context.truncated.load(Ordering::Relaxed),
                elapsed: started.elapsed(),
            });
        })
        .map_err(SearchError::Thread)?;
    Ok(SearchHandle {
        events: receiver,
        cancel,
    })
}

fn build_matcher(options: &SearchOptions) -> Result<RegexMatcher, SearchError> {
    if options.pattern.is_empty() {
        return Err(SearchError::EmptyPattern);
    }
    RegexMatcherBuilder::new()
        .case_insensitive(!options.case_sensitive)
        .word(options.whole_word)
        .fixed_strings(!options.regex)
        // `^` and `$` are line anchors, and they treat `\r\n` like `\n`.
        .multi_line(true)
        .crlf(true)
        .build(&options.pattern)
        .map_err(|error| match error.kind() {
            grep_regex::ErrorKind::NotAllowed(_) => SearchError::MultiLine,
            _ => SearchError::InvalidPattern(error.to_string()),
        })
}

fn build_searcher() -> Searcher {
    SearcherBuilder::new()
        .line_terminator(LineTerminator::crlf())
        .line_number(true)
        .binary_detection(BinaryDetection::quit(0))
        .heap_limit(Some(MAX_LINE_BYTES))
        .build()
}

/// The include and exclude globs of a search.
struct PathFilter {
    include: Option<GlobSet>,
    exclude: Option<GlobSet>,
}

impl PathFilter {
    fn new(include: &[String], exclude: &[String]) -> Result<PathFilter, SearchError> {
        Ok(PathFilter {
            include: compile_globs(include)?,
            exclude: compile_globs(exclude)?,
        })
    }

    /// Whether a file at this root-relative path is one the search should look at.
    fn wants_file(&self, relative: &Path) -> bool {
        self.include
            .as_ref()
            .is_none_or(|include| include.is_match(relative))
            && !self.is_excluded(relative)
    }

    fn is_excluded(&self, relative: &Path) -> bool {
        self.exclude
            .as_ref()
            .is_some_and(|exclude| exclude.is_match(relative))
    }

    /// A walk filter that prunes excluded files and whole excluded directories.
    fn walk_filter(self: &Arc<Self>) -> EntryFilter {
        let filter = Arc::clone(self);
        Arc::new(move |relative, _is_dir| !filter.is_excluded(relative))
    }
}

/// Compile user globs into one set, or `None` when there are none.
fn compile_globs(globs: &[String]) -> Result<Option<GlobSet>, SearchError> {
    let mut builder = GlobSetBuilder::new();
    let mut any = false;
    for glob in globs {
        for pattern in expand_glob(glob) {
            let compiled = GlobBuilder::new(&pattern)
                .literal_separator(true)
                .build()
                .map_err(|error| SearchError::InvalidGlob {
                    glob: glob.clone(),
                    message: error.kind().to_string(),
                })?;
            builder.add(compiled);
            any = true;
        }
    }
    if !any {
        return Ok(None);
    }
    builder
        .build()
        .map(Some)
        .map_err(|error| SearchError::InvalidGlob {
            glob: error.glob().unwrap_or_default().to_string(),
            message: error.kind().to_string(),
        })
}

/// Turn one user glob into the patterns that implement its meaning: the entry itself and
/// everything inside it, at any depth unless the glob names a path.
fn expand_glob(glob: &str) -> Vec<String> {
    let glob = glob.trim();
    let glob = glob.strip_prefix("./").unwrap_or(glob);
    let rooted = glob.starts_with('/');
    let glob = glob.trim_matches('/');
    if glob.is_empty() {
        return Vec::new();
    }
    if rooted || glob.contains('/') {
        vec![glob.to_string(), format!("{glob}/**")]
    } else {
        vec![format!("**/{glob}"), format!("**/{glob}/**")]
    }
}

/// Everything the search threads share.
struct SearchContext {
    root: PathBuf,
    matcher: RegexMatcher,
    filter: Arc<PathFilter>,
    cancel: Arc<AtomicBool>,
    max_results: usize,
    /// Matches reported so far. Never more than `max_results`.
    matches: AtomicUsize,
    files: AtomicUsize,
    truncated: AtomicBool,
    events: Sender<SearchEvent>,
    wake: Option<Box<dyn Fn() + Send + Sync>>,
}

impl SearchContext {
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn stopping(&self) -> bool {
        self.cancelled() || self.truncated.load(Ordering::Relaxed)
    }

    /// How many more matches fit under the limit right now.
    fn room(&self) -> usize {
        self.max_results
            .saturating_sub(self.matches.load(Ordering::Relaxed))
    }

    /// Claim room for `wanted` more matches. Returns how many fit under the limit; when
    /// that is fewer than asked for, the search is marked truncated.
    fn claim(&self, wanted: usize) -> usize {
        let mut current = self.matches.load(Ordering::Relaxed);
        loop {
            let granted = wanted.min(self.max_results.saturating_sub(current));
            match self.matches.compare_exchange_weak(
                current,
                current + granted,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    if granted < wanted {
                        self.truncated.store(true, Ordering::Relaxed);
                    }
                    return granted;
                }
                Err(now) => current = now,
            }
        }
    }

    fn send(&self, event: SearchEvent) {
        if self.events.send(event).is_ok() {
            if let Some(wake) = &self.wake {
                wake();
            }
        }
    }
}

struct SearchVisitorBuilder<'a> {
    context: &'a SearchContext,
}

impl<'a> ParallelVisitorBuilder<'a> for SearchVisitorBuilder<'a> {
    fn build(&mut self) -> Box<dyn ParallelVisitor + 'a> {
        Box::new(SearchVisitor {
            context: self.context,
            searcher: build_searcher(),
        })
    }
}

struct SearchVisitor<'a> {
    context: &'a SearchContext,
    searcher: Searcher,
}

impl ParallelVisitor for SearchVisitor<'_> {
    fn visit(&mut self, entry: Result<DirEntry, ignore::Error>) -> WalkState {
        let context = self.context;
        if context.stopping() {
            return WalkState::Quit;
        }
        let Some(relative) = searchable_file(&context.root, &context.filter, entry) else {
            return WalkState::Continue;
        };
        let mut sink = MatchSink {
            context,
            matches: Vec::new(),
            found: 0,
            binary: false,
        };
        let path = context.root.join(&relative);
        if let Err(error) = self
            .searcher
            .search_path(&context.matcher, &path, &mut sink)
        {
            log::debug!("could not search {}: {error}", path.display());
        }
        // Only now is it known that the file is text, so only now do its matches count
        // against the limit.
        if !sink.binary && sink.found > 0 {
            let granted = context.claim(sink.found);
            keep_first_matches(&mut sink.matches, granted);
            if !sink.matches.is_empty() {
                context.files.fetch_add(1, Ordering::Relaxed);
                context.send(SearchEvent::File {
                    path: relative,
                    matches: sink.matches,
                });
            }
        }
        if context.stopping() {
            WalkState::Quit
        } else {
            WalkState::Continue
        }
    }
}

/// The root-relative path of a walk entry if it is a regular file the filter wants.
fn searchable_file(
    root: &Path,
    filter: &PathFilter,
    entry: Result<DirEntry, ignore::Error>,
) -> Option<PathBuf> {
    let entry = entry.ok()?;
    if !entry.file_type()?.is_file() {
        return None;
    }
    let relative = entry.path().strip_prefix(root).ok()?;
    filter.wants_file(relative).then(|| relative.to_path_buf())
}

/// Collects the matching lines of one file.
struct MatchSink<'a> {
    context: &'a SearchContext,
    matches: Vec<LineMatch>,
    /// The number of matches in `matches`.
    found: usize,
    binary: bool,
}

impl Sink for MatchSink<'_> {
    type Error = io::Error;

    fn matched(&mut self, _searcher: &Searcher, found: &SinkMatch<'_>) -> Result<bool, io::Error> {
        if self.context.stopping() {
            return Ok(false);
        }
        let line = strip_line_break(found.bytes());
        let line_number = u32::try_from(found.line_number().unwrap_or(0)).unwrap_or(u32::MAX);
        for window in line_windows(line, &match_spans(&self.context.matcher, line), line_number) {
            self.found += window.ranges.len();
            self.matches.push(window);
        }
        // Once this file alone holds more than the search has room for, the rest of it
        // could not be reported anyway.
        Ok(self.found <= self.context.room())
    }

    fn binary_data(&mut self, _searcher: &Searcher, _offset: u64) -> Result<bool, io::Error> {
        self.binary = true;
        Ok(false)
    }
}

/// Cut a file's matches down to the first `keep`, dropping the lines left with none.
fn keep_first_matches(matches: &mut Vec<LineMatch>, mut keep: usize) {
    let mut lines = 0;
    for line in matches.iter_mut() {
        if keep == 0 {
            break;
        }
        line.ranges.truncate(keep);
        keep -= line.ranges.len();
        lines += 1;
    }
    matches.truncate(lines);
}

/// `line` without its trailing `\n` or `\r\n`.
fn strip_line_break(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// The byte range of every match in one line (given without its line break).
fn match_spans(matcher: &RegexMatcher, line: &[u8]) -> Vec<Range<usize>> {
    let mut spans = Vec::new();
    // The matcher's error type cannot occur.
    let _ = matcher.find_iter(line, |found| {
        spans.push(found.start()..found.end());
        true
    });
    spans
}

/// A line as text, plus what is needed to carry byte offsets over when the bytes were
/// not valid UTF-8 and had to be repaired.
struct LineText<'a> {
    text: Cow<'a, str>,
    /// The length of the original line in bytes.
    raw_len: usize,
    /// For a repaired line: where each stretch of the original bytes starts in the
    /// original and in `text`, and whether it is an invalid stretch (shown as U+FFFD).
    stretches: Vec<(usize, usize, bool)>,
}

impl<'a> LineText<'a> {
    fn new(line: &'a [u8]) -> LineText<'a> {
        if let Ok(text) = std::str::from_utf8(line) {
            return LineText {
                text: Cow::Borrowed(text),
                raw_len: line.len(),
                stretches: Vec::new(),
            };
        }
        let mut text = String::with_capacity(line.len());
        let mut stretches = Vec::new();
        let mut raw = 0;
        for chunk in line.utf8_chunks() {
            if !chunk.valid().is_empty() {
                stretches.push((raw, text.len(), false));
                text.push_str(chunk.valid());
                raw += chunk.valid().len();
            }
            if !chunk.invalid().is_empty() {
                stretches.push((raw, text.len(), true));
                text.push(char::REPLACEMENT_CHARACTER);
                raw += chunk.invalid().len();
            }
        }
        LineText {
            text: Cow::Owned(text),
            raw_len: line.len(),
            stretches,
        }
    }

    /// The offset in `text` of the byte offset `raw` of the original line. Offsets that
    /// fall inside a character move to its start, or to its end when `round_up` is set.
    fn offset(&self, raw: usize, round_up: bool) -> usize {
        if raw >= self.raw_len {
            return self.text.len();
        }
        let mut offset = match self.stretches.partition_point(|stretch| stretch.0 <= raw) {
            // A line that was valid UTF-8 to begin with.
            0 => raw,
            after => {
                let (raw_start, text_start, invalid) = self.stretches[after - 1];
                if !invalid {
                    text_start + (raw - raw_start)
                } else if raw == raw_start || !round_up {
                    text_start
                } else {
                    text_start + char::REPLACEMENT_CHARACTER.len_utf8()
                }
            }
        };
        offset = offset.min(self.text.len());
        while !self.text.is_char_boundary(offset) {
            if round_up {
                offset += 1;
            } else {
                offset -= 1;
            }
        }
        offset
    }
}

/// Turn the matches of one line into [`LineMatch`]es: one for a line that fits, one per
/// window of matches for a line longer than [`MAX_LINE_CHARS`].
fn line_windows(line: &[u8], spans: &[Range<usize>], line_number: u32) -> Vec<LineMatch> {
    let line = LineText::new(line);
    let text: &str = &line.text;
    // Byte ranges in `text`, on character boundaries, in ascending order.
    let spans: Vec<Range<usize>> = spans
        .iter()
        .map(|span| {
            let start = line.offset(span.start, false);
            start..line.offset(span.end, true).max(start)
        })
        .collect();
    if spans.is_empty() {
        return Vec::new();
    }

    if text.len() <= MAX_LINE_CHARS || text.chars().nth(MAX_LINE_CHARS).is_none() {
        return vec![LineMatch {
            line_number,
            line: text.to_string(),
            clamp_offset: 0,
            ranges: char_ranges(text, 0, text.len(), &spans),
        }];
    }

    let mut windows = Vec::new();
    // The number of characters before `counted_to`, kept as matches are walked.
    let (mut counted_to, mut chars_before) = (0, 0);
    let mut next = 0;
    while next < spans.len() {
        let first = spans[next].clone();
        chars_before += text[counted_to..first.start].chars().count();
        counted_to = first.start;

        let mut start = step_back(text, first.start, LEADING_CONTEXT);
        let end = match text[start..].char_indices().nth(MAX_LINE_CHARS) {
            Some((offset, _)) => start + offset,
            None => {
                // The line ends inside the window: use the room for more leading context.
                start = step_back(text, text.len(), MAX_LINE_CHARS);
                text.len()
            }
        };
        let clamp_offset = chars_before - text[start..first.start].chars().count();

        let mut last = next;
        while last < spans.len() && (spans[last].start < end || spans[last].end <= end) {
            last += 1;
        }
        // The first match always belongs to its own window, even when it is empty and
        // sits at the very end.
        let last = last.max(next + 1);
        windows.push(LineMatch {
            line_number,
            line: text[start..end].to_string(),
            clamp_offset,
            ranges: char_ranges(text, start, end, &spans[next..last]),
        });
        next = last;
    }
    windows
}

/// The byte offset `count` characters before `from` (or 0 if the text starts sooner).
fn step_back(text: &str, from: usize, count: usize) -> usize {
    if count == 0 {
        return from;
    }
    text[..from]
        .char_indices()
        .rev()
        .nth(count - 1)
        .map_or(0, |(offset, _)| offset)
}

/// The byte ranges `spans` as `char` ranges relative to the window `start..end` of
/// `text`, cut off at the window's end.
fn char_ranges(text: &str, start: usize, end: usize, spans: &[Range<usize>]) -> Vec<Range<usize>> {
    let (mut counted_to, mut chars) = (start, 0);
    let mut char_at = |offset: usize| {
        let offset = offset.clamp(counted_to, end);
        chars += text[counted_to..offset].chars().count();
        counted_to = offset;
        chars
    };
    spans
        .iter()
        .map(|span| {
            let range_start = char_at(span.start);
            range_start..char_at(span.end)
        })
        .collect()
}

/// What [`replace_all`] did with one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaceResult {
    /// The file, relative to the search root.
    pub path: PathBuf,
    /// What happened to it.
    pub outcome: ReplaceOutcome,
}

/// The outcome of a replacement in one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplaceOutcome {
    /// The file was rewritten.
    Replaced {
        /// How many matches were replaced in it.
        replacements: usize,
    },
    /// The file has matches but was left untouched, for the given reason.
    Skipped(SkipReason),
    /// Reading or writing the file failed; it is unchanged. The text is the error.
    Failed(String),
}

/// Why [`replace_all`] left a matching file alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// Something else changed the file between reading it and writing it back.
    Changed,
    /// The file is UTF-16. It can be searched but is not rewritten.
    UnsupportedEncoding,
}

/// Replace every match of `options` under `root` with `replacement` and report what
/// happened to each file that had matches, sorted by path.
///
/// The search is the one [`search()`] runs (same pattern rules, globs and ignore
/// handling), except that [`SearchOptions::max_results`] does not apply: every match is
/// replaced. With [`SearchOptions::regex`] the replacement may refer to capture groups
/// as `$1`, `${1}`, `$name` or `${name}`, with `$$` for a literal dollar; otherwise it is
/// inserted as it is. Line endings and a UTF-8 byte order mark are preserved.
///
/// Each file is rewritten in one step ([`crate::fileops::write_atomic`]): a reader sees
/// the old contents or the new, never a mix. A file that changed on disk between being
/// read and being written back is skipped ([`SkipReason::Changed`]) rather than
/// overwritten.
///
/// This call blocks until every file is done, working on several threads. Run it off
/// the UI thread.
pub fn replace_all(
    root: &Path,
    options: &SearchOptions,
    replacement: &str,
) -> Result<Vec<ReplaceResult>, SearchError> {
    let context = ReplaceContext {
        root: normalize(root),
        matcher: build_matcher(options)?,
        filter: Arc::new(PathFilter::new(&options.include, &options.exclude)?),
        replacement: replacement.as_bytes().to_vec(),
        interpolate: options.regex,
        results: Mutex::new(Vec::new()),
    };
    let mut builder = ReplaceVisitorBuilder { context: &context };
    walk_builder(
        &context.root,
        &context.root,
        options.include_ignored,
        Some(context.filter.walk_filter()),
    )
    .build_parallel()
    .visit(&mut builder);

    let mut results = context
        .results
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner);
    results.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(results)
}

struct ReplaceContext {
    root: PathBuf,
    matcher: RegexMatcher,
    filter: Arc<PathFilter>,
    replacement: Vec<u8>,
    /// Expand `$1`-style references in the replacement.
    interpolate: bool,
    results: Mutex<Vec<ReplaceResult>>,
}

struct ReplaceVisitorBuilder<'a> {
    context: &'a ReplaceContext,
}

impl<'a> ParallelVisitorBuilder<'a> for ReplaceVisitorBuilder<'a> {
    fn build(&mut self) -> Box<dyn ParallelVisitor + 'a> {
        Box::new(ReplaceVisitor {
            context: self.context,
            searcher: build_searcher(),
        })
    }
}

struct ReplaceVisitor<'a> {
    context: &'a ReplaceContext,
    searcher: Searcher,
}

impl ParallelVisitor for ReplaceVisitor<'_> {
    fn visit(&mut self, entry: Result<DirEntry, ignore::Error>) -> WalkState {
        let context = self.context;
        let Some(relative) = searchable_file(&context.root, &context.filter, entry) else {
            return WalkState::Continue;
        };
        let path = context.root.join(&relative);
        if !self.has_matches(&path) {
            return WalkState::Continue;
        }
        let outcome = match replace_in_file(context, &path) {
            Ok(Some(outcome)) => outcome,
            Ok(None) => return WalkState::Continue,
            Err(error) => ReplaceOutcome::Failed(error.to_string()),
        };
        context
            .results
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(ReplaceResult {
                path: relative,
                outcome,
            });
        WalkState::Continue
    }
}

impl ReplaceVisitor<'_> {
    /// Whether the search would report matches for this file: it has a matching line and
    /// is not binary. The whole file is read, exactly as the search does.
    fn has_matches(&mut self, path: &Path) -> bool {
        let mut probe = ProbeSink::default();
        if let Err(error) = self
            .searcher
            .search_path(&self.context.matcher, path, &mut probe)
        {
            log::debug!("could not search {}: {error}", path.display());
            return false;
        }
        probe.matched && !probe.binary
    }
}

#[derive(Default)]
struct ProbeSink {
    matched: bool,
    binary: bool,
}

impl Sink for ProbeSink {
    type Error = io::Error;

    fn matched(&mut self, _searcher: &Searcher, _found: &SinkMatch<'_>) -> Result<bool, io::Error> {
        self.matched = true;
        Ok(true)
    }

    fn binary_data(&mut self, _searcher: &Searcher, _offset: u64) -> Result<bool, io::Error> {
        self.binary = true;
        Ok(false)
    }
}

const UTF8_BOM: &[u8] = b"\xef\xbb\xbf";

/// The new contents of a file, ready to be written back.
struct Rewrite {
    /// What the file looked like when it was read.
    before: fs::Metadata,
    /// How many bytes were read. Differs from `before` if the file changed meanwhile.
    read_len: u64,
    contents: Vec<u8>,
    replacements: usize,
}

/// Rewrite one file. `Ok(None)` means there turned out to be nothing to replace.
fn replace_in_file(context: &ReplaceContext, path: &Path) -> io::Result<Option<ReplaceOutcome>> {
    match prepare_rewrite(context, path)? {
        Prepared::Nothing => Ok(None),
        Prepared::Skip(reason) => Ok(Some(ReplaceOutcome::Skipped(reason))),
        Prepared::Rewrite(rewrite) => commit_rewrite(path, &rewrite).map(Some),
    }
}

enum Prepared {
    /// No matches after all, or the file is binary.
    Nothing,
    Skip(SkipReason),
    Rewrite(Rewrite),
}

/// Read the file and work out its new contents, without writing anything.
fn prepare_rewrite(context: &ReplaceContext, path: &Path) -> io::Result<Prepared> {
    let before = fs::metadata(path)?;
    let original = fs::read(path)?;
    if original.starts_with(b"\xff\xfe") || original.starts_with(b"\xfe\xff") {
        return Ok(Prepared::Skip(SkipReason::UnsupportedEncoding));
    }
    if original.contains(&0) {
        return Ok(Prepared::Nothing);
    }

    let (bom, body) = match original.strip_prefix(UTF8_BOM) {
        Some(body) => (UTF8_BOM, body),
        None => (&[][..], &original[..]),
    };
    let mut contents = Vec::with_capacity(original.len());
    contents.extend_from_slice(bom);
    let mut replacements = 0;
    for line in body.split_inclusive(|byte| *byte == b'\n') {
        let content = strip_line_break(line);
        replacements += replace_in_line(context, content, &mut contents);
        contents.extend_from_slice(&line[content.len()..]);
    }
    if replacements == 0 {
        return Ok(Prepared::Nothing);
    }
    Ok(Prepared::Rewrite(Rewrite {
        before,
        read_len: original.len() as u64,
        contents,
        replacements,
    }))
}

/// Write the new contents, unless the file is no longer the version that was read.
fn commit_rewrite(path: &Path, rewrite: &Rewrite) -> io::Result<ReplaceOutcome> {
    let replaced = write_atomic_checked(path, &rewrite.contents, |target| {
        rewrite.read_len == rewrite.before.len()
            && fs::metadata(target).is_ok_and(|now| same_version(&rewrite.before, &now))
    })?;
    Ok(if replaced {
        ReplaceOutcome::Replaced {
            replacements: rewrite.replacements,
        }
    } else {
        ReplaceOutcome::Skipped(SkipReason::Changed)
    })
}

/// Append `line` to `out` with every match replaced, and return the number of matches.
fn replace_in_line(context: &ReplaceContext, line: &[u8], out: &mut Vec<u8>) -> usize {
    let matcher = &context.matcher;
    let mut count = 0;
    // The matcher's error type cannot occur.
    if context.interpolate {
        let Ok(mut captures) = matcher.new_captures() else {
            out.extend_from_slice(line);
            return 0;
        };
        let _ = matcher.replace_with_captures(line, &mut captures, out, |captures, out| {
            captures.interpolate(
                |name| matcher.capture_index(name),
                line,
                &context.replacement,
                out,
            );
            count += 1;
            true
        });
    } else {
        let _ = matcher.replace(line, out, |_, out| {
            out.extend_from_slice(&context.replacement);
            count += 1;
            true
        });
    }
    count
}

/// Whether two looks at a file saw the same version of it: same size, same modification
/// time and (where the platform can tell) the same file.
fn same_version(before: &fs::Metadata, now: &fs::Metadata) -> bool {
    before.len() == now.len()
        && before.modified().ok() == now.modified().ok()
        && same_inode(before, now)
}

#[cfg(unix)]
fn same_inode(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

#[cfg(not(unix))]
fn same_inode(_: &fs::Metadata, _: &fs::Metadata) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(pattern: &str) -> SearchOptions {
        SearchOptions::new(pattern)
    }

    #[test]
    fn default_options_match_the_brief() {
        let options = SearchOptions::default();
        assert_eq!(options.max_results, 10_000);
        assert!(!options.regex && !options.case_sensitive && !options.whole_word);
        assert!(options.include.is_empty() && options.exclude.is_empty());
        assert!(!options.include_ignored);
        assert_eq!(SearchOptions::new("x").pattern, "x");
    }

    #[test]
    fn bad_patterns_are_typed_errors() {
        assert!(matches!(
            build_matcher(&options("")),
            Err(SearchError::EmptyPattern)
        ));
        let mut regex = options("(unclosed");
        regex.regex = true;
        assert!(matches!(
            build_matcher(&regex),
            Err(SearchError::InvalidPattern(_))
        ));
        // The same text is fine as a literal.
        assert!(build_matcher(&options("(unclosed")).is_ok());
        assert!(matches!(
            build_matcher(&options("two\nlines")),
            Err(SearchError::MultiLine)
        ));
        let mut newline = options(r"a\nb");
        newline.regex = true;
        assert!(matches!(
            build_matcher(&newline),
            Err(SearchError::MultiLine)
        ));
    }

    #[test]
    fn bad_globs_are_typed_errors() {
        let error = PathFilter::new(&["src/[".to_string()], &[]).err().unwrap();
        assert!(matches!(error, SearchError::InvalidGlob { ref glob, .. } if glob == "src/["));
        assert!(PathFilter::new(&[], &["{a,b".to_string()]).is_err());
    }

    fn filter(include: &[&str], exclude: &[&str]) -> PathFilter {
        let owned = |globs: &[&str]| {
            globs
                .iter()
                .map(|glob| glob.to_string())
                .collect::<Vec<_>>()
        };
        PathFilter::new(&owned(include), &owned(exclude)).unwrap()
    }

    #[test]
    fn globs_without_a_slash_match_at_any_depth() {
        let rust = filter(&["*.rs"], &[]);
        assert!(rust.wants_file(Path::new("main.rs")));
        assert!(rust.wants_file(Path::new("crates/core/src/lib.rs")));
        assert!(!rust.wants_file(Path::new("main.rs.bak")));
        assert!(!rust.wants_file(Path::new("src/main.py")));

        let named = filter(&["Cargo.toml"], &[]);
        assert!(named.wants_file(Path::new("Cargo.toml")));
        assert!(named.wants_file(Path::new("crates/core/Cargo.toml")));
        assert!(!named.wants_file(Path::new("crates/core/NotCargo.toml")));
    }

    #[test]
    fn globs_cover_what_is_inside_a_matching_directory() {
        let tests = filter(&["tests"], &[]);
        assert!(tests.wants_file(Path::new("tests/a.rs")));
        assert!(tests.wants_file(Path::new("crates/core/tests/deep/b.rs")));
        assert!(!tests.wants_file(Path::new("src/tests.rs")));

        let anchored = filter(&["crates/core"], &[]);
        assert!(anchored.wants_file(Path::new("crates/core/src/lib.rs")));
        assert!(!anchored.wants_file(Path::new("vendor/crates/core/src/lib.rs")));
        assert!(!anchored.wants_file(Path::new("crates/corelib/x.rs")));
    }

    #[test]
    fn globs_with_a_slash_are_anchored_and_stars_stop_at_slashes() {
        let direct = filter(&["src/*.rs"], &[]);
        assert!(direct.wants_file(Path::new("src/main.rs")));
        assert!(!direct.wants_file(Path::new("src/sub/mod.rs")));
        assert!(!direct.wants_file(Path::new("other/src/main.rs")));

        let deep = filter(&["src/**/*.rs", "/README.md", "./docs/"], &[]);
        assert!(deep.wants_file(Path::new("src/sub/mod.rs")));
        assert!(deep.wants_file(Path::new("src/main.rs")));
        assert!(deep.wants_file(Path::new("README.md")));
        assert!(!deep.wants_file(Path::new("pkg/README.md")));
        assert!(deep.wants_file(Path::new("docs/guide/intro.md")));
    }

    #[test]
    fn exclude_wins_and_empty_globs_are_ignored() {
        let filter = filter(&["*.rs", "  ", ""], &["generated", "*_test.rs"]);
        assert!(filter.wants_file(Path::new("src/main.rs")));
        assert!(!filter.wants_file(Path::new("src/generated/api.rs")));
        assert!(!filter.wants_file(Path::new("src/main_test.rs")));
        assert!(filter.is_excluded(Path::new("src/generated")));
        assert!(!filter.is_excluded(Path::new("src")));

        let everything = self::filter(&[], &[]);
        assert!(everything.wants_file(Path::new("anything/at.all")));
        assert!(expand_glob(" / ").is_empty());
    }

    /// A single match span.
    fn span(range: Range<usize>) -> Vec<Range<usize>> {
        std::iter::once(range).collect()
    }

    fn spans(text: &str, needle: &str) -> Vec<Range<usize>> {
        text.match_indices(needle)
            .map(|(start, found)| start..start + found.len())
            .collect()
    }

    #[test]
    fn short_lines_are_reported_whole() {
        let line = "let x = foo(foo);";
        let windows = line_windows(line.as_bytes(), &spans(line, "foo"), 7);
        assert_eq!(
            windows,
            [LineMatch {
                line_number: 7,
                line: line.to_string(),
                clamp_offset: 0,
                ranges: vec![8..11, 12..15],
            }]
        );
        assert!(line_windows(line.as_bytes(), &[], 7).is_empty());
    }

    #[test]
    fn ranges_are_char_offsets() {
        let line = "héllo wörld → wörld";
        let windows = line_windows(line.as_bytes(), &spans(line, "wörld"), 1);
        assert_eq!(windows[0].ranges, vec![6..11, 14..19]);
        let highlighted: Vec<String> = windows[0]
            .ranges
            .iter()
            .map(|range| line.chars().skip(range.start).take(range.len()).collect())
            .collect();
        assert_eq!(highlighted, ["wörld", "wörld"]);
    }

    #[test]
    fn empty_matches_are_kept() {
        let line = "abc";
        let windows = line_windows(line.as_bytes(), &[0..0, 3..3], 1);
        assert_eq!(windows[0].ranges, vec![0..0, 3..3]);
    }

    #[test]
    fn a_line_at_the_limit_is_not_clamped() {
        let line = "x".repeat(MAX_LINE_CHARS - 1) + "é";
        assert_eq!(line.chars().count(), MAX_LINE_CHARS);
        let windows = line_windows(line.as_bytes(), &spans(&line, "é"), 1);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].clamp_offset, 0);
        assert_eq!(windows[0].line, line);
        assert_eq!(windows[0].ranges, vec![MAX_LINE_CHARS - 1..MAX_LINE_CHARS]);
    }

    /// Check a clamped window against the line it was cut from.
    fn assert_window(line: &str, window: &LineMatch, needle: &str) {
        let chars: Vec<char> = line.chars().collect();
        assert!(window.line.chars().count() <= MAX_LINE_CHARS);
        let expected: String = chars[window.clamp_offset..]
            .iter()
            .take(window.line.chars().count())
            .collect();
        assert_eq!(window.line, expected, "the window is a slice of the line");
        for range in &window.ranges {
            let found: String = window
                .line
                .chars()
                .skip(range.start)
                .take(range.len())
                .collect();
            assert_eq!(found, needle);
        }
    }

    #[test]
    fn long_lines_are_clamped_around_the_first_match() {
        let line = format!("{}needle{}", "a".repeat(1000), "b".repeat(1000));
        let windows = line_windows(line.as_bytes(), &spans(&line, "needle"), 3);
        assert_eq!(windows.len(), 1);
        let window = &windows[0];
        assert_eq!(window.line.chars().count(), MAX_LINE_CHARS);
        assert_eq!(window.clamp_offset, 1000 - LEADING_CONTEXT);
        assert_eq!(window.ranges, vec![LEADING_CONTEXT..LEADING_CONTEXT + 6]);
        assert_eq!(window.clamp_offset + window.ranges[0].start, 1000);
        assert_window(&line, window, "needle");
    }

    #[test]
    fn a_match_near_the_start_or_end_keeps_a_full_window() {
        let line = format!("ab needle{}", "c".repeat(2000));
        let windows = line_windows(line.as_bytes(), &spans(&line, "needle"), 1);
        assert_eq!(windows[0].clamp_offset, 0);
        assert_eq!(windows[0].ranges, vec![3..9]);
        assert_eq!(windows[0].line.chars().count(), MAX_LINE_CHARS);

        let line = format!("{}needle", "c".repeat(2000));
        let windows = line_windows(line.as_bytes(), &spans(&line, "needle"), 1);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].line.chars().count(), MAX_LINE_CHARS);
        assert_eq!(windows[0].clamp_offset, 2006 - MAX_LINE_CHARS);
        assert!(windows[0].line.ends_with("needle"));
        assert_window(&line, &windows[0], "needle");
    }

    #[test]
    fn matches_far_apart_on_one_line_get_their_own_windows() {
        let line = format!(
            "{}needle{}needle needle{}needle",
            "é".repeat(600),
            "x".repeat(900),
            "y".repeat(1500)
        );
        let all = spans(&line, "needle");
        assert_eq!(all.len(), 4);
        let windows = line_windows(line.as_bytes(), &all, 9);
        assert_eq!(windows.len(), 3);
        assert_eq!(
            windows
                .iter()
                .map(|window| window.ranges.len())
                .collect::<Vec<_>>(),
            [1, 2, 1]
        );
        let chars: Vec<char> = line.chars().collect();
        let mut positions = Vec::new();
        for window in &windows {
            assert_eq!(window.line_number, 9);
            assert_window(&line, window, "needle");
            for range in &window.ranges {
                positions.push(window.clamp_offset + range.start);
            }
        }
        // Every match is reported exactly once, at its real position.
        let expected: Vec<usize> = (0..chars.len())
            .filter(|start| chars[*start..].starts_with(&['n', 'e', 'e', 'd', 'l', 'e']))
            .collect();
        assert_eq!(positions, expected);
    }

    #[test]
    fn a_match_longer_than_the_window_is_cut_at_its_end() {
        let line = "z".repeat(3000);
        let windows = line_windows(line.as_bytes(), &span(100..2900), 1);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].clamp_offset, 100 - LEADING_CONTEXT);
        assert_eq!(windows[0].ranges, vec![LEADING_CONTEXT..MAX_LINE_CHARS]);
    }

    #[test]
    fn an_empty_match_at_the_end_of_a_long_line_is_reported() {
        let line = "q".repeat(1000);
        let windows = line_windows(line.as_bytes(), &[0..0, 1000..1000], 1);
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].ranges, vec![0..0]);
        assert_eq!(windows[1].clamp_offset + windows[1].ranges[0].start, 1000);
        assert!(windows[1].ranges[0].is_empty());
    }

    #[test]
    fn invalid_utf8_is_repaired_and_offsets_follow() {
        // "ab" + two invalid bytes + "cd needle": each invalid byte shows as one U+FFFD.
        let line = b"ab\xff\xfecd needle";
        let windows = line_windows(line, &span(7..13), 1);
        assert_eq!(windows[0].line, "ab\u{fffd}\u{fffd}cd needle");
        assert_eq!(windows[0].ranges, vec![7..13]);
        // A match on the invalid bytes themselves covers their replacement characters.
        let windows = line_windows(line, &span(2..4), 1);
        assert_eq!(windows[0].ranges, vec![2..4]);
        // A truncated multi-byte sequence is a single replacement character.
        let line = b"x\xe2\x82y needle";
        let windows = line_windows(line, &span(5..11), 1);
        assert_eq!(windows[0].line, "x\u{fffd}y needle");
        assert_eq!(windows[0].ranges, vec![4..10]);
    }

    #[test]
    fn line_breaks_are_stripped() {
        assert_eq!(strip_line_break(b"abc\n"), b"abc");
        assert_eq!(strip_line_break(b"abc\r\n"), b"abc");
        assert_eq!(strip_line_break(b"abc"), b"abc");
        assert_eq!(strip_line_break(b"\n"), b"");
        assert_eq!(strip_line_break(b"a\rb\n"), b"a\rb");
    }

    #[test]
    fn the_limit_hands_out_exactly_max_results() {
        let (events, _receiver) = crossbeam_channel::unbounded();
        let context = SearchContext {
            root: PathBuf::new(),
            matcher: build_matcher(&options("x")).unwrap(),
            filter: Arc::new(filter(&[], &[])),
            cancel: Arc::new(AtomicBool::new(false)),
            max_results: 5,
            matches: AtomicUsize::new(0),
            files: AtomicUsize::new(0),
            truncated: AtomicBool::new(false),
            events,
            wake: None,
        };
        assert_eq!(context.room(), 5);
        assert_eq!(context.claim(3), 3);
        assert!(!context.stopping());
        assert_eq!(context.room(), 2);
        assert_eq!(context.claim(2), 2);
        // Exactly at the limit is not truncated: nothing was turned away yet.
        assert!(!context.stopping());
        assert_eq!(context.room(), 0);
        assert_eq!(context.claim(1), 0);
        assert!(context.stopping());
        assert_eq!(context.claim(4), 0);
        assert_eq!(context.matches.load(Ordering::Relaxed), 5);
    }

    #[test]
    fn a_partial_claim_is_split_fairly_between_racing_files() {
        let (events, _receiver) = crossbeam_channel::unbounded();
        let context = SearchContext {
            root: PathBuf::new(),
            matcher: build_matcher(&options("x")).unwrap(),
            filter: Arc::new(filter(&[], &[])),
            cancel: Arc::new(AtomicBool::new(false)),
            max_results: 1_000,
            matches: AtomicUsize::new(0),
            files: AtomicUsize::new(0),
            truncated: AtomicBool::new(false),
            events,
            wake: None,
        };
        // However the claims interleave, exactly `max_results` are granted in total.
        let granted: usize = thread::scope(|scope| {
            let workers: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| (0..100).map(|_| context.claim(3)).sum::<usize>()))
                .collect();
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .sum()
        });
        assert_eq!(granted, 1_000);
        assert_eq!(context.matches.load(Ordering::Relaxed), 1_000);
        assert!(context.stopping());
    }

    #[test]
    fn a_partial_claim_keeps_the_first_matches() {
        // Each line is given as the start offsets of its one-character matches.
        let line = |starts: &[usize]| LineMatch {
            line_number: 1,
            line: String::new(),
            clamp_offset: 0,
            ranges: starts.iter().map(|start| *start..start + 1).collect(),
        };
        let all = vec![line(&[0, 2]), line(&[4]), line(&[6, 8])];
        let kept = |keep: usize| {
            let mut matches = all.clone();
            keep_first_matches(&mut matches, keep);
            matches
        };
        assert_eq!(kept(4), [line(&[0, 2]), line(&[4]), line(&[6])]);
        assert_eq!(kept(3), [line(&[0, 2]), line(&[4])]);
        assert_eq!(kept(1), [line(&[0])]);
        assert!(kept(0).is_empty());
        assert_eq!(kept(99), all);
    }

    #[test]
    fn a_file_that_changed_since_it_was_read_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.txt");
        fs::write(&path, "port = 8080\n").unwrap();
        let context = ReplaceContext {
            root: dir.path().to_path_buf(),
            matcher: build_matcher(&options("8080")).unwrap(),
            filter: Arc::new(filter(&[], &[])),
            replacement: b"9090".to_vec(),
            interpolate: false,
            results: Mutex::new(Vec::new()),
        };
        let Prepared::Rewrite(rewrite) = prepare_rewrite(&context, &path).unwrap() else {
            panic!("the file has a match");
        };
        assert_eq!(rewrite.contents, b"port = 9090\n");
        assert_eq!(rewrite.replacements, 1);

        // Someone else saves the file between the read and the write.
        fs::write(&path, "port = 8080\nhost = example.org\n").unwrap();
        assert_eq!(
            commit_rewrite(&path, &rewrite).unwrap(),
            ReplaceOutcome::Skipped(SkipReason::Changed)
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "port = 8080\nhost = example.org\n",
            "the other program's version is kept"
        );
        let left_behind: Vec<_> = fs::read_dir(dir.path()).unwrap().flatten().collect();
        assert_eq!(left_behind.len(), 1, "the temporary file is cleaned up");

        // Even a change that keeps the size is noticed, through the modification time.
        fs::write(&path, "port = 8080\n").unwrap();
        let Prepared::Rewrite(rewrite) = prepare_rewrite(&context, &path).unwrap() else {
            panic!("the file has a match");
        };
        let later = fs::metadata(&path).unwrap().modified().unwrap() + Duration::from_secs(2);
        fs::write(&path, "port = 8081\n").unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert_eq!(
            commit_rewrite(&path, &rewrite).unwrap(),
            ReplaceOutcome::Skipped(SkipReason::Changed)
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "port = 8081\n");

        // Left alone, a rewrite goes through.
        fs::write(&path, "port = 8080\n").unwrap();
        let Prepared::Rewrite(rewrite) = prepare_rewrite(&context, &path).unwrap() else {
            panic!("the file has a match");
        };
        assert_eq!(
            commit_rewrite(&path, &rewrite).unwrap(),
            ReplaceOutcome::Replaced { replacements: 1 }
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "port = 9090\n");
    }

    #[test]
    fn files_that_turn_out_to_have_nothing_to_replace_are_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let context = ReplaceContext {
            root: dir.path().to_path_buf(),
            matcher: build_matcher(&options("needle")).unwrap(),
            filter: Arc::new(filter(&[], &[])),
            replacement: b"pin".to_vec(),
            interpolate: false,
            results: Mutex::new(Vec::new()),
        };
        let plain = dir.path().join("plain.txt");
        fs::write(&plain, "nothing here\n").unwrap();
        assert!(matches!(
            prepare_rewrite(&context, &plain).unwrap(),
            Prepared::Nothing
        ));
        let binary = dir.path().join("binary.bin");
        fs::write(&binary, b"needle\x00").unwrap();
        assert!(matches!(
            prepare_rewrite(&context, &binary).unwrap(),
            Prepared::Nothing
        ));
        let wide = dir.path().join("wide.txt");
        fs::write(&wide, b"\xfe\xff\x00n").unwrap();
        assert!(matches!(
            prepare_rewrite(&context, &wide).unwrap(),
            Prepared::Skip(SkipReason::UnsupportedEncoding)
        ));
        assert!(prepare_rewrite(&context, &dir.path().join("missing.txt")).is_err());
    }

    fn replaced(options: &SearchOptions, replacement: &str, line: &str) -> (String, usize) {
        let context = ReplaceContext {
            root: PathBuf::new(),
            matcher: build_matcher(options).unwrap(),
            filter: Arc::new(filter(&[], &[])),
            replacement: replacement.as_bytes().to_vec(),
            interpolate: options.regex,
            results: Mutex::new(Vec::new()),
        };
        let mut out = Vec::new();
        let count = replace_in_line(&context, line.as_bytes(), &mut out);
        (String::from_utf8(out).unwrap(), count)
    }

    #[test]
    fn literal_replacement_is_inserted_as_it_is() {
        let (line, count) = replaced(&options("foo"), "$1 bar", "foo FOO food");
        assert_eq!(line, "$1 bar $1 bar $1 bard");
        assert_eq!(count, 3);
        let (line, count) = replaced(&options("zzz"), "x", "nothing here");
        assert_eq!((line.as_str(), count), ("nothing here", 0));
    }

    #[test]
    fn regex_replacement_expands_capture_groups() {
        let mut swap = options(r"(\w+)=(?P<value>\w+)");
        swap.regex = true;
        swap.case_sensitive = true;
        let (line, count) = replaced(&swap, "${value}=$1 $$", "a=1, b=2");
        assert_eq!(line, "1=a $, 2=b $");
        assert_eq!(count, 2);

        let mut comment = options("^");
        comment.regex = true;
        assert_eq!(
            replaced(&comment, "// ", "let x;"),
            ("// let x;".to_string(), 1)
        );
    }

    #[test]
    fn whole_word_and_case_apply_to_replacement_too() {
        let mut word = options("cat");
        word.whole_word = true;
        word.case_sensitive = true;
        let (line, count) = replaced(&word, "dog", "cat concat Cat cat.");
        assert_eq!(line, "dog concat Cat dog.");
        assert_eq!(count, 2);
    }
}
