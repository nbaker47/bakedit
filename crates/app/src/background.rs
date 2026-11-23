//! Work off the UI thread: waking the main loop, and handing a result back to it.
//!
//! Nothing a view does may block drawing, so anything slow (a git command, a search, loading
//! a big file) runs on another thread. Two small pieces make that uniform:
//!
//! * [`Waker`] tells the main loop "something is ready, come and look". It is cheap to
//!   clone and may be used from any thread. Many wakes before the loop gets round to
//!   looking are delivered as one.
//! * [`Task`] is a value being computed on another thread. The view that started it keeps
//!   the task and asks it for the value from [`View::tick`](crate::view::View::tick), which
//!   the loop calls after every wake.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver, TryRecvError};

type Notify = Box<dyn Fn() + Send + Sync>;

struct WakerInner {
    /// A wake has been sent and the loop has not picked it up yet.
    pending: AtomicBool,
    /// The earliest time somebody asked to be ticked at.
    deadline: Mutex<Option<Instant>>,
    notify: Notify,
}

/// Wakes the main loop from any thread.
#[derive(Clone)]
pub struct Waker {
    inner: Arc<WakerInner>,
}

impl std::fmt::Debug for Waker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Waker").field("pending", &self.inner.pending.load(Ordering::SeqCst)).finish()
    }
}

impl Waker {
    /// A waker that calls `notify` to get the loop's attention. `notify` runs on whichever
    /// thread wakes, so it should do no more than send a message.
    pub fn new(notify: impl Fn() + Send + Sync + 'static) -> Waker {
        Waker { inner: Arc::new(WakerInner { pending: AtomicBool::new(false), deadline: Mutex::new(None), notify: Box::new(notify) }) }
    }

    /// A waker with no loop behind it, for tests and for views drawn on their own. Wakes are
    /// still recorded, so [`Waker::take_woken`] works.
    pub fn detached() -> Waker {
        Waker::new(|| {})
    }

    /// Ask the loop to tick every view soon. Calling it again before the loop has looked
    /// costs nothing and notifies nobody.
    pub fn wake(&self) {
        if !self.inner.pending.swap(true, Ordering::SeqCst) {
            (self.inner.notify)();
        }
    }

    /// Ask the loop to tick every view at `when` (or soon after). The earliest request
    /// wins; once it has fired, ask again for the next one.
    pub fn wake_at(&self, when: Instant) {
        {
            let mut deadline = self.inner.deadline.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            match *deadline {
                Some(existing) if existing <= when => return,
                _ => *deadline = Some(when),
            }
        }
        // The loop may be asleep until a later deadline: have it look at the new one.
        self.wake();
    }

    /// [`Waker::wake_at`] counted from now.
    pub fn wake_after(&self, delay: Duration) {
        self.wake_at(Instant::now() + delay);
    }

    /// For the loop: whether a wake arrived since the last call. Clears it, so a wake that
    /// comes in while the loop is looking notifies again.
    pub fn take_woken(&self) -> bool {
        self.inner.pending.swap(false, Ordering::SeqCst)
    }

    /// For the loop: the time it must wake up at, if a view asked for one.
    pub fn deadline(&self) -> Option<Instant> {
        *self.inner.deadline.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// For the loop: whether the requested time has come. Clears the request when it has.
    pub fn take_due(&self, now: Instant) -> bool {
        let mut deadline = self.inner.deadline.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        match *deadline {
            Some(when) if when <= now => {
                *deadline = None;
                true
            }
            _ => false,
        }
    }
}

/// Wakes the loop when dropped, so a worker that panics still gets the loop to notice that
/// its task will never finish.
struct WakeOnDrop(Waker);

impl Drop for WakeOnDrop {
    fn drop(&mut self) {
        self.0.wake();
    }
}

/// A value being worked out on another thread.
///
/// Keep the task and call [`Task::poll`] from `tick`: it returns the value exactly once,
/// when the work is done. Dropping the task does not stop the work; its result is simply
/// thrown away.
#[derive(Debug)]
pub struct Task<T> {
    receiver: Receiver<T>,
    finished: bool,
}

impl<T: Send + 'static> Task<T> {
    /// Run `work` on a new thread and wake the loop when it is done.
    pub fn spawn(waker: &Waker, work: impl FnOnce() -> T + Send + 'static) -> Task<T> {
        let (sender, receiver) = bounded(1);
        let wake = WakeOnDrop(waker.clone());
        let spawned = std::thread::Builder::new().name("bakedit-task".into()).spawn(move || {
            let wake = wake;
            let _ = sender.send(work());
            drop(wake);
        });
        if let Err(err) = spawned {
            // The closure (and with it the sender) was dropped: the task reads as finished
            // without a value, which is how every caller already has to treat a lost worker.
            log::error!("could not start a background thread: {err}");
        }
        Task { receiver, finished: false }
    }

    /// A task whose value is already there.
    pub fn ready(value: T) -> Task<T> {
        let (sender, receiver) = bounded(1);
        let _ = sender.send(value);
        Task { receiver, finished: false }
    }

    /// The value, the first time this is called after the work finished; `None` before
    /// that and ever after.
    pub fn poll(&mut self) -> Option<T> {
        if self.finished {
            return None;
        }
        match self.receiver.try_recv() {
            Ok(value) => {
                self.finished = true;
                Some(value)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.finished = true;
                None
            }
        }
    }

    /// True once [`Task::poll`] has handed out the value, or the worker died without one.
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Block until the value is there or `timeout` has passed. For tests and command line
    /// modes; a view must never wait.
    pub fn wait(mut self, timeout: Duration) -> Option<T> {
        if self.finished {
            return None;
        }
        self.finished = true;
        self.receiver.recv_timeout(timeout).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn counting() -> (Waker, Arc<AtomicUsize>) {
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        (Waker::new(move || { seen.fetch_add(1, Ordering::SeqCst); }), count)
    }

    #[test]
    fn wakes_are_coalesced_until_the_loop_looks() {
        let (waker, count) = counting();
        waker.wake();
        waker.wake();
        waker.clone().wake();
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(waker.take_woken());
        assert!(!waker.take_woken());
        waker.wake();
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn the_earliest_deadline_wins_and_fires_once() {
        let (waker, count) = counting();
        let now = Instant::now();
        waker.wake_at(now + Duration::from_secs(5));
        waker.wake_at(now + Duration::from_secs(9));
        assert_eq!(waker.deadline(), Some(now + Duration::from_secs(5)));
        waker.take_woken();
        waker.wake_at(now + Duration::from_secs(2));
        assert_eq!(waker.deadline(), Some(now + Duration::from_secs(2)));
        assert_eq!(count.load(Ordering::SeqCst), 2, "an earlier deadline gets the loop to look again");
        assert!(!waker.take_due(now + Duration::from_secs(1)));
        assert!(waker.take_due(now + Duration::from_secs(2)));
        assert_eq!(waker.deadline(), None);
        assert!(!waker.take_due(now + Duration::from_secs(60)));
    }

    #[test]
    fn a_task_hands_its_value_over_once_and_wakes() {
        let (waker, count) = counting();
        let mut task = Task::spawn(&waker, || 6 * 7);
        let started = Instant::now();
        let value = loop {
            if let Some(value) = task.poll() {
                break value;
            }
            assert!(started.elapsed() < Duration::from_secs(5), "the task never finished");
            std::thread::sleep(Duration::from_millis(1));
        };
        assert_eq!(value, 42);
        assert!(task.is_finished());
        assert_eq!(task.poll(), None);
        // The wake comes right after the value; give the worker a moment to get there.
        while count.load(Ordering::SeqCst) == 0 {
            assert!(started.elapsed() < Duration::from_secs(5), "the loop was never woken");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn ready_and_wait() {
        let mut ready = Task::ready("done");
        assert_eq!(ready.poll(), Some("done"));
        assert_eq!(ready.poll(), None);
        let slow = Task::spawn(&Waker::detached(), || {
            std::thread::sleep(Duration::from_millis(20));
            7
        });
        assert_eq!(slow.wait(Duration::from_secs(5)), Some(7));
    }

    #[test]
    fn a_worker_that_panics_finishes_the_task_without_a_value() {
        let (waker, count) = counting();
        let mut task: Task<u8> = Task::spawn(&waker, || panic!("deliberate, to test the task"));
        let started = Instant::now();
        while !task.is_finished() {
            assert_eq!(task.poll(), None);
            assert!(started.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(count.load(Ordering::SeqCst) >= 1, "the loop is still told to look");
    }
}
