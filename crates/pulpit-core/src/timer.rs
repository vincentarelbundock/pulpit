use std::time::{Duration, Instant};

/// Elapsed-time tracker with an optional target duration.
///
/// The timer never reads the clock itself: callers pass `now`. That keeps
/// every timer test deterministic and lets a display event, reload or
/// reconciliation pass through the state machine without disturbing time.
#[derive(Debug, Clone, PartialEq)]
pub struct Timer {
    elapsed: Duration,
    running_since: Option<Instant>,
    pub target: Option<Duration>,
}

impl Default for Timer {
    fn default() -> Self {
        Self {
            elapsed: Duration::ZERO,
            running_since: None,
            target: None,
        }
    }
}

impl Timer {
    pub fn new(target: Option<Duration>) -> Self {
        Self {
            target,
            ..Self::default()
        }
    }

    pub fn is_running(&self) -> bool {
        self.running_since.is_some()
    }

    pub fn elapsed(&self, now: Instant) -> Duration {
        match self.running_since {
            Some(started) => self.elapsed + now.saturating_duration_since(started),
            None => self.elapsed,
        }
    }

    pub fn start(&mut self, now: Instant) {
        if self.running_since.is_none() {
            self.running_since = Some(now);
        }
    }

    pub fn pause(&mut self, now: Instant) {
        if let Some(started) = self.running_since.take() {
            self.elapsed += now.saturating_duration_since(started);
        }
    }

    pub fn toggle(&mut self, now: Instant) {
        if self.is_running() {
            self.pause(now);
        } else {
            self.start(now);
        }
    }

    /// Reset elapsed time. The running/paused state is preserved so that a
    /// reset during a talk does not silently stop the clock.
    pub fn reset(&mut self, now: Instant) {
        self.elapsed = Duration::ZERO;
        if self.running_since.is_some() {
            self.running_since = Some(now);
        }
    }

    /// Remaining time against the target, negative once overrun.
    pub fn remaining(&self, now: Instant) -> Option<i64> {
        let target = self.target?;
        Some(target.as_secs() as i64 - self.elapsed(now).as_secs() as i64)
    }

    pub fn is_overrun(&self, now: Instant) -> bool {
        self.remaining(now).is_some_and(|r| r < 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn accumulates_only_while_running() {
        let start = t0();
        let mut timer = Timer::default();
        assert_eq!(
            timer.elapsed(start + Duration::from_secs(10)),
            Duration::ZERO
        );

        timer.start(start);
        assert_eq!(
            timer.elapsed(start + Duration::from_secs(5)),
            Duration::from_secs(5)
        );

        timer.pause(start + Duration::from_secs(5));
        assert_eq!(
            timer.elapsed(start + Duration::from_secs(60)),
            Duration::from_secs(5)
        );

        timer.start(start + Duration::from_secs(60));
        assert_eq!(
            timer.elapsed(start + Duration::from_secs(62)),
            Duration::from_secs(7)
        );
    }

    #[test]
    fn reset_preserves_running_state() {
        let start = t0();
        let mut timer = Timer::default();
        timer.start(start);
        timer.reset(start + Duration::from_secs(30));
        assert!(timer.is_running());
        assert_eq!(
            timer.elapsed(start + Duration::from_secs(31)),
            Duration::from_secs(1)
        );
    }

    #[test]
    fn overrun_is_reported() {
        let start = t0();
        let mut timer = Timer::new(Some(Duration::from_secs(60)));
        timer.start(start);
        assert_eq!(timer.remaining(start + Duration::from_secs(10)), Some(50));
        assert!(!timer.is_overrun(start + Duration::from_secs(10)));
        assert!(timer.is_overrun(start + Duration::from_secs(61)));
    }
}
