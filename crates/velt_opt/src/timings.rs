//! Per-pass time accounting for `velt build --timings`.
//!
//! Passes run function by function and round by round, so a pass's time is the sum of all its
//! runs. Measuring costs two clock reads per pass run (well under a millisecond on large
//! programs), so the driver always collects it and only prints it on request.

use std::time::{Duration, Instant};

/// Accumulated wall-clock time per pass, in the order the passes first ran.
#[derive(Clone, Debug, Default)]
pub struct PassTimings {
    totals: Vec<(&'static str, Duration)>,
}

impl PassTimings {
    /// No time recorded yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Run `f`, adding its time to pass `name`.
    pub(crate) fn time<T>(&mut self, name: &'static str, f: impl FnOnce() -> T) -> T {
        let start = Instant::now();
        let r = f();
        let d = start.elapsed();
        match self.totals.iter_mut().find(|(n, _)| *n == name) {
            Some((_, total)) => *total += d,
            None => self.totals.push((name, d)),
        }
        r
    }

    /// `(pass, total time)` in first-run order.
    pub fn entries(&self) -> &[(&'static str, Duration)] {
        &self.totals
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_runs_of_the_same_pass_in_first_run_order() {
        let mut t = PassTimings::new();
        assert_eq!(t.time("a", || 1), 1);
        t.time("b", || ());
        t.time("a", || ());
        let names: Vec<_> = t.entries().iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ["a", "b"]);
    }
}
