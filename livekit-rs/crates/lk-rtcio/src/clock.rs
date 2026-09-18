//! Clock injection.
//!
//! Every deadline in the media path comes from here rather than from
//! `Instant::now()`, so the same driver runs against wall time in production
//! and against a virtual clock in the vnet suites.

use std::time::{Duration, Instant};

/// A source of monotonic time.
pub trait Clock {
    /// The current instant.
    fn now(&self) -> Instant;
}

/// Wall-clock time. The production clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// A clock that only moves when it is told to.
///
/// Cloning shares the same underlying instant, so a `ManualClock` handed to a
/// driver and one kept by the test step together.
#[derive(Debug, Clone)]
pub struct ManualClock {
    inner: std::rc::Rc<std::cell::Cell<Instant>>,
}

impl ManualClock {
    /// Start a manual clock at `start`.
    pub fn new(start: Instant) -> Self {
        Self {
            inner: std::rc::Rc::new(std::cell::Cell::new(start)),
        }
    }

    /// Move the clock forward. Time never runs backwards, so `advance` is the
    /// only mutator.
    pub fn advance(&self, by: Duration) {
        self.inner.set(self.inner.get() + by);
    }

    /// Move the clock to `to`, ignoring the call if that would rewind it.
    pub fn advance_to(&self, to: Instant) {
        if to > self.inner.get() {
            self.inner.set(to);
        }
    }
}

impl Default for ManualClock {
    fn default() -> Self {
        Self::new(Instant::now())
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Instant {
        self.inner.get()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn manual_clock_shares_time_across_clones() {
        let a = ManualClock::default();
        let b = a.clone();
        let t0 = a.now();
        a.advance(Duration::from_millis(250));
        assert_eq!(b.now(), t0 + Duration::from_millis(250));
    }

    #[test]
    fn advance_to_never_rewinds() {
        let c = ManualClock::default();
        let t0 = c.now();
        c.advance(Duration::from_secs(5));
        c.advance_to(t0);
        assert_eq!(c.now(), t0 + Duration::from_secs(5));
    }
}
