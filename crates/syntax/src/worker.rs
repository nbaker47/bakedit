//! Highlighting off the UI thread.
//!
//! The editor hands the worker a snapshot of a document after each edit and keeps drawing
//! with the highlights it already has. When the worker has an answer it puts it on a channel
//! and pokes the UI. Typing faster than the worker can keep up does not queue work: a new
//! snapshot of a document replaces the one still waiting and cancels the one being
//! highlighted.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender};

use crate::brackets::{bracket_marks, BracketMark};
use crate::error::SyntaxError;
use crate::highlight::Highlighter;
use crate::language::Language;
use crate::lines::{LineBreaks, LineHighlights};

/// The highlights of one version of one document.
#[derive(Debug, Clone)]
pub struct SyntaxResult {
    /// The document the request named.
    pub doc_id: u64,
    /// The version the request named. Compare it with the document's current version: an
    /// older result is still the best available until a newer one arrives, but its offsets
    /// are those of the older text.
    pub version: u64,
    /// The highlights, by line.
    pub lines: Arc<LineHighlights>,
    /// Every bracket and its nesting depth, in text order. Empty for a language without a
    /// grammar, where there is no telling a bracket from a bracket in a string.
    pub brackets: Arc<Vec<BracketMark>>,
}

/// One document waiting to be highlighted.
struct Job {
    version: u64,
    language: &'static Language,
    text: String,
}

/// What the UI thread and the worker thread share.
#[derive(Default)]
struct Queue {
    /// Documents with a job waiting, oldest request first.
    order: VecDeque<u64>,
    /// The waiting job of each document in `order`.
    waiting: HashMap<u64, Job>,
    /// The document being highlighted right now and the flag that stops it.
    running: Option<(u64, Arc<AtomicBool>)>,
    shutting_down: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    wake_worker: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        // The queue is plain data that every holder leaves consistent, so a panic elsewhere
        // does not make it unusable.
        self.queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// A background thread that highlights documents.
///
/// Create one for the whole application. [`request`](SyntaxWorker::request) never blocks on
/// highlighting; results come out of [`results`](SyntaxWorker::results) in the order they
/// were finished. Dropping the worker cancels what it is doing and joins the thread.
pub struct SyntaxWorker {
    shared: Arc<Shared>,
    results: Receiver<SyntaxResult>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for SyntaxWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyntaxWorker").finish_non_exhaustive()
    }
}

impl SyntaxWorker {
    /// Start the worker thread.
    ///
    /// `wake` is called on the worker thread each time a result has been put on the channel,
    /// so the UI loop can be made to look. It must not block for long and must not call back
    /// into the worker. Lines are counted the default way ([`LineBreaks::Unicode`]).
    pub fn new(wake: Box<dyn Fn() + Send + Sync>) -> SyntaxWorker {
        SyntaxWorker::with_line_breaks(wake, LineBreaks::default())
    }

    /// As [`SyntaxWorker::new`], counting lines the way `breaks` says. Use the same setting
    /// the text buffer uses, or line numbers will disagree in files with unusual line ends.
    pub fn with_line_breaks(wake: Box<dyn Fn() + Send + Sync>, breaks: LineBreaks) -> SyntaxWorker {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue::default()),
            wake_worker: Condvar::new(),
        });
        let (sender, results) = crossbeam_channel::unbounded();
        let thread = {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("bakedit-syntax".to_owned())
                .spawn(move || run(&shared, &sender, wake.as_ref(), breaks))
                .ok()
        };
        SyntaxWorker {
            shared,
            results,
            thread,
        }
    }

    /// Ask for `text` to be highlighted as `language`.
    ///
    /// `doc_id` names the document and `version` this snapshot of it; both come back on the
    /// result. If the document already has a request waiting, this one takes its place, and
    /// if the document is being highlighted right now that run is cancelled: only the newest
    /// snapshot of a document is ever worth finishing. Requests for different documents do
    /// not affect each other and are served in the order they first arrived.
    ///
    /// Returns at once. If the worker thread could not be started the request is dropped and
    /// no result ever arrives.
    pub fn request(&self, doc_id: u64, version: u64, language: &'static Language, text: String) {
        let mut queue = self.shared.lock();
        if queue.shutting_down {
            return;
        }
        if queue
            .waiting
            .insert(
                doc_id,
                Job {
                    version,
                    language,
                    text,
                },
            )
            .is_none()
        {
            queue.order.push_back(doc_id);
        }
        if let Some((running, cancel)) = &queue.running {
            if *running == doc_id {
                cancel.store(true, Ordering::Relaxed);
            }
        }
        drop(queue);
        self.shared.wake_worker.notify_one();
    }

    /// Forget a document: drop its waiting request and cancel it if it is being highlighted.
    /// Call it when a document is closed. A result finished just before this may still be
    /// on the channel.
    pub fn forget(&self, doc_id: u64) {
        let mut queue = self.shared.lock();
        if queue.waiting.remove(&doc_id).is_some() {
            queue.order.retain(|waiting| *waiting != doc_id);
        }
        if let Some((running, cancel)) = &queue.running {
            if *running == doc_id {
                cancel.store(true, Ordering::Relaxed);
            }
        }
    }

    /// Where results arrive. Poll it with `try_recv` when the wake callback has fired; one
    /// wake may stand for several results.
    pub fn results(&self) -> &Receiver<SyntaxResult> {
        &self.results
    }
}

impl Drop for SyntaxWorker {
    fn drop(&mut self) {
        {
            let mut queue = self.shared.lock();
            queue.shutting_down = true;
            queue.waiting.clear();
            queue.order.clear();
            if let Some((_, cancel)) = &queue.running {
                cancel.store(true, Ordering::Relaxed);
            }
        }
        self.shared.wake_worker.notify_all();
        if let Some(thread) = self.thread.take() {
            // The thread only panics if a grammar's scanner does; there is nothing to do
            // about that here.
            let _ = thread.join();
        }
    }
}

/// The worker thread: take the oldest waiting document, highlight it, report, repeat.
fn run(
    shared: &Shared,
    results: &Sender<SyntaxResult>,
    wake: &(dyn Fn() + Send + Sync),
    breaks: LineBreaks,
) {
    let mut highlighter = Highlighter::new();
    while let Some((doc_id, job, cancel)) = next_job(shared) {
        let outcome = highlight_job(&mut highlighter, &job, &cancel, breaks);
        let mut queue = shared.lock();
        queue.running = None;
        // A cancelled run was superseded or forgotten: its answer is not wanted.
        let superseded = cancel.load(Ordering::Relaxed) || queue.shutting_down;
        drop(queue);
        let Some((lines, brackets)) = outcome.filter(|_| !superseded) else {
            continue;
        };
        let result = SyntaxResult {
            doc_id,
            version: job.version,
            lines: Arc::new(lines),
            brackets: Arc::new(brackets),
        };
        if results.send(result).is_err() {
            return;
        }
        wake();
    }
}

/// Block until a document is waiting and claim it. `None` means the worker is shutting down.
fn next_job(shared: &Shared) -> Option<(u64, Job, Arc<AtomicBool>)> {
    let mut queue = shared.lock();
    loop {
        if queue.shutting_down {
            return None;
        }
        if let Some(doc_id) = queue.order.pop_front() {
            if let Some(job) = queue.waiting.remove(&doc_id) {
                let cancel = Arc::new(AtomicBool::new(false));
                queue.running = Some((doc_id, Arc::clone(&cancel)));
                return Some((doc_id, job, cancel));
            }
            continue;
        }
        queue = shared
            .wake_worker
            .wait(queue)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
    }
}

/// Highlight one job. `None` means it was cancelled. Any other failure still gives an
/// answer, an empty one, so the UI is not left waiting for a result that cannot come.
fn highlight_job(
    highlighter: &mut Highlighter,
    job: &Job,
    cancel: &AtomicBool,
    breaks: LineBreaks,
) -> Option<(LineHighlights, Vec<BracketMark>)> {
    let spans = match highlighter.highlight(job.language, &job.text, Some(cancel)) {
        Ok(spans) => spans,
        Err(SyntaxError::Cancelled) => return None,
        Err(_) => Vec::new(),
    };
    let lines = LineHighlights::with_line_breaks(&job.text, &spans, breaks);
    let brackets = if job.language.has_grammar() {
        bracket_marks(&job.text, &spans)
    } else {
        Vec::new()
    };
    Some((lines, brackets))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::language::{language_by_id, plain_text};
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    const TIMEOUT: Duration = Duration::from_secs(60);

    fn rust() -> &'static Language {
        language_by_id("rust").unwrap()
    }

    fn quiet_worker() -> SyntaxWorker {
        SyntaxWorker::new(Box::new(|| {}))
    }

    /// A Rust source long enough that highlighting it takes a moment.
    fn long_source(functions: usize) -> String {
        (0..functions)
            .map(|i| format!("fn function_{i}(value: u32) -> u32 {{\n    let doubled = value * 2;\n    doubled + {i}\n}}\n\n"))
            .collect()
    }

    #[test]
    fn a_request_comes_back_with_lines_and_brackets() {
        let worker = quiet_worker();
        worker.request(
            7,
            3,
            rust(),
            "fn main() {\n    let x = (1, [2]);\n}\n".to_owned(),
        );
        let result = worker.results().recv_timeout(TIMEOUT).unwrap();
        assert_eq!((result.doc_id, result.version), (7, 3));
        assert_eq!(result.lines.line_count(), 4);
        let first: Vec<&str> = result
            .lines
            .spans_for_line(0)
            .iter()
            .map(|(_, capture)| capture.name())
            .collect();
        assert_eq!(first[..2], ["keyword", "function"]);
        assert_eq!(result.brackets.len(), 8);
        assert!(result.brackets.iter().all(|mark| mark.matched));
        assert_eq!(result.brackets.iter().map(|mark| mark.depth).max(), Some(2));
    }

    #[test]
    fn the_wake_callback_fires_once_per_result() {
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&wakes);
        let worker = SyntaxWorker::new(Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        for doc_id in 0..5 {
            worker.request(doc_id, 1, rust(), format!("fn f{doc_id}() {{}}\n"));
        }
        let mut seen: Vec<u64> = (0..5)
            .map(|_| worker.results().recv_timeout(TIMEOUT).unwrap().doc_id)
            .collect();
        // Different documents are served in the order they were asked for.
        assert_eq!(seen, [0, 1, 2, 3, 4]);
        seen.clear();
        drop(worker);
        assert_eq!(wakes.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn a_newer_request_replaces_one_still_waiting() {
        let worker = quiet_worker();
        // Document 1 keeps the worker busy while document 2 is asked for three times over.
        worker.request(1, 1, rust(), long_source(4000));
        worker.request(2, 1, rust(), "fn a() {}\n".to_owned());
        worker.request(2, 2, rust(), "fn b() {}\n".to_owned());
        worker.request(2, 3, rust(), "fn c() {}\nfn d() {}\n".to_owned());
        let mut versions_of_two = Vec::new();
        let mut saw_one = false;
        while !saw_one || versions_of_two.is_empty() {
            let result = worker.results().recv_timeout(TIMEOUT).unwrap();
            match result.doc_id {
                1 => saw_one = true,
                2 => versions_of_two.push((result.version, result.lines.line_count())),
                other => panic!("unexpected document {other}"),
            }
        }
        // Only the last snapshot of document 2 was ever highlighted.
        assert_eq!(versions_of_two, [(3, 3)]);
        assert!(worker
            .results()
            .recv_timeout(Duration::from_millis(200))
            .is_err());
    }

    #[test]
    fn a_newer_request_cancels_the_one_running() {
        let worker = quiet_worker();
        let slow = long_source(60_000);
        worker.request(9, 1, rust(), slow);
        // Wait until the worker has picked the slow job up, then supersede it.
        let deadline = std::time::Instant::now() + TIMEOUT;
        while worker.shared.lock().running.is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "the worker never started"
            );
            std::thread::yield_now();
        }
        worker.request(9, 2, rust(), "fn quick() {}\n".to_owned());
        let result = worker.results().recv_timeout(TIMEOUT).unwrap();
        assert_eq!(
            (result.doc_id, result.version),
            (9, 2),
            "the stale version must not be reported"
        );
        assert_eq!(result.lines.line_count(), 2);
        assert!(worker
            .results()
            .recv_timeout(Duration::from_millis(200))
            .is_err());
    }

    #[test]
    fn forgetting_a_document_drops_its_request() {
        let worker = quiet_worker();
        worker.request(1, 1, rust(), long_source(4000));
        worker.request(2, 1, rust(), "fn gone() {}\n".to_owned());
        worker.forget(2);
        worker.request(3, 1, rust(), "fn kept() {}\n".to_owned());
        let mut seen = Vec::new();
        while seen.len() < 2 {
            seen.push(worker.results().recv_timeout(TIMEOUT).unwrap().doc_id);
        }
        assert_eq!(seen, [1, 3]);
        assert!(worker
            .results()
            .recv_timeout(Duration::from_millis(200))
            .is_err());
        // Forgetting something unknown is fine.
        worker.forget(99);
    }

    #[test]
    fn a_language_without_a_grammar_gets_an_empty_answer() {
        let worker = quiet_worker();
        worker.request(4, 1, plain_text(), "some (text)\nmore\n".to_owned());
        let result = worker.results().recv_timeout(TIMEOUT).unwrap();
        assert_eq!(result.lines.line_count(), 3);
        assert!(result.lines.is_empty());
        assert!(result.brackets.is_empty());
    }

    #[test]
    fn line_break_mode_reaches_the_results() {
        let text = "fn a() {}\u{c}fn b() {}\n";
        let unicode = quiet_worker();
        unicode.request(1, 1, rust(), text.to_owned());
        assert_eq!(
            unicode
                .results()
                .recv_timeout(TIMEOUT)
                .unwrap()
                .lines
                .line_count(),
            3
        );
        let lf = SyntaxWorker::with_line_breaks(Box::new(|| {}), LineBreaks::Lf);
        lf.request(1, 1, rust(), text.to_owned());
        assert_eq!(
            lf.results()
                .recv_timeout(TIMEOUT)
                .unwrap()
                .lines
                .line_count(),
            2
        );
    }

    #[test]
    fn dropping_the_worker_stops_the_thread_promptly() {
        let worker = quiet_worker();
        let results = worker.results().clone();
        worker.request(1, 1, rust(), long_source(60_000));
        for doc_id in 2..50 {
            worker.request(doc_id, 1, rust(), long_source(2000));
        }
        let started = std::time::Instant::now();
        drop(worker);
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "drop waited for the queue to drain"
        );
        // The sender is gone with the thread: the channel reports disconnection, not a
        // result for every queued document.
        let mut delivered = 0;
        loop {
            match results.recv_timeout(TIMEOUT) {
                Ok(_) => delivered += 1,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    panic!("the thread is still alive")
                }
            }
        }
        assert!(delivered < 5, "{delivered} results arrived after the drop");
    }

    #[test]
    fn many_workers_can_run_at_once() {
        let workers: Vec<SyntaxWorker> = (0..4).map(|_| quiet_worker()).collect();
        for (i, worker) in workers.iter().enumerate() {
            worker.request(i as u64, 1, rust(), long_source(200));
        }
        for (i, worker) in workers.iter().enumerate() {
            let result = worker.results().recv_timeout(TIMEOUT).unwrap();
            assert_eq!(result.doc_id, i as u64);
            assert!(!result.lines.is_empty());
        }
    }
}
