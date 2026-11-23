//! Time source for undo grouping.
//!
//! The buffer merges consecutive typed characters into one undo step while they arrive
//! close together. It asks a [`Clock`] for the time instead of reading the system clock
//! directly, so tests can drive time by hand.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// A monotonic clock that reports milliseconds since an arbitrary fixed point.
pub trait Clock: Send + Sync {
    /// Milliseconds elapsed since the clock's own epoch. Must never go backwards.
    fn now_ms(&self) -> u64;
}

/// The real clock: milliseconds since this value was created, from [`Instant`].
#[derive(Debug, Clone, Copy)]
pub struct SystemClock {
    start: Instant,
}

impl SystemClock {
    /// A clock whose epoch is now.
    pub fn new() -> SystemClock {
        SystemClock {
            start: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> SystemClock {
        SystemClock::new()
    }
}

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// A clock that only moves when told to, for deterministic tests.
#[derive(Debug, Default)]
pub struct ManualClock {
    now: AtomicU64,
}

impl ManualClock {
    /// A clock that reads `start_ms` until it is moved.
    pub fn new(start_ms: u64) -> ManualClock {
        ManualClock {
            now: AtomicU64::new(start_ms),
        }
    }

    /// Jump to an absolute time. Moving backwards is allowed but makes every elapsed
    /// interval read as zero.
    pub fn set(&self, ms: u64) {
        self.now.store(ms, Ordering::Relaxed);
    }

    /// Move forward by `ms` milliseconds (saturating).
    pub fn advance(&self, ms: u64) {
        let now = self.now.load(Ordering::Relaxed);
        self.now.store(now.saturating_add(ms), Ordering::Relaxed);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.now.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clock_moves_only_when_told() {
        let clock = ManualClock::new(10);
        assert_eq!(clock.now_ms(), 10);
        clock.advance(5);
        assert_eq!(clock.now_ms(), 15);
        clock.set(3);
        assert_eq!(clock.now_ms(), 3);
        clock.advance(u64::MAX);
        assert_eq!(clock.now_ms(), u64::MAX);
    }

    #[test]
    fn system_clock_does_not_go_backwards() {
        let clock = SystemClock::new();
        let a = clock.now_ms();
        let b = clock.now_ms();
        assert!(b >= a);
    }
}
