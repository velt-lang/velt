//! When the Velt backend uses another worker: when a worker's input waits too long in its queue.
//!
//! Work stays on the worker that made it (see [`super::worker`]): on a core that keeps up,
//! spreading only adds cross-thread wakes and splits the writes it could batch (#717). So a
//! worker measures how long its tasks wait. Every few polls, and when it runs out of work, it
//! notes its queue's length; at the end of each [`WINDOW`] it estimates the delay of its queue
//! as the mean length noted times the mean time of a task poll (wall time while awake). From [`TARGET`] on it offers
//! half of its queue to a sleeping worker, which takes the tasks over (their sockets follow, see
//! `io`); the halves keep measuring, so a load spreads until no worker's tasks wait that long.
//! Wall time, not CPU time: tasks of a worker the OS took off its core wait as well; but not
//! the time the worker slept, which would make a worker that idles between bursts look like
//! one whose polls are long.
//!
//! The mean, not the shortest queue: requests that come in bursts empty the queue between
//! bursts, and still wait in it during each (JSON on 256 connections: two workers whose
//! shortest queue was always empty, p50 0.8 ms at 200k requests/s; spread by the mean over
//! every core, 0.4 ms, against 0.55 ms with tokio's work stealing).
//!
//! What counts is input (the first queue, see `queue`): the work tasks made waits behind it on
//! purpose, so that output batches (a broadcast's flushers), and spreading a broadcast moved
//! its flushers' wakes across threads for fewer deliveries (1000 connections: half the
//! deliveries at twice the CPU). The whole queue counts once polls are long ([`HEAVY`]): tasks
//! that compute gain from every core.
//!
//! An earlier policy kept a helper only if it raised the rate of work on a 20 ms trial. On a
//! loaded machine the trials failed at random and left a saturated worker alone with all its
//! connections (JSON on 512 connections: p99 1.8 s; spread by delay: 1.1 ms, tokio's work
//! stealing 1.2 ms).

use std::cell::Cell;
use std::time::{Duration, Instant};

/// How long each measurement runs.
const WINDOW: Duration = Duration::from_millis(10);
/// The delay from which a worker takes on a helper.
const TARGET: Duration = Duration::from_micros(200);
/// Mean time per task poll from which all waiting tasks count, not only input.
const HEAVY: Duration = Duration::from_micros(20);

/// One worker's current measurement.
pub(super) struct Window {
    started: Cell<Instant>,
    /// The worker's task polls when it started.
    polls: Cell<u64>,
    /// The input queue's and the whole queue's lengths noted in it, summed, and how often.
    input: Cell<usize>,
    all: Cell<usize>,
    noted: Cell<usize>,
    /// Time the worker slept in it.
    slept: Cell<Duration>,
}

impl Window {
    pub(super) fn new(now: Instant) -> Window {
        Window {
            started: Cell::new(now),
            polls: Cell::new(0),
            input: Cell::new(0),
            all: Cell::new(0),
            noted: Cell::new(0),
            slept: Cell::new(Duration::ZERO),
        }
    }

    /// Notes the queue's length, input and all.
    pub(super) fn saw(&self, input: usize, len: usize) {
        self.input.set(self.input.get().saturating_add(input));
        self.all.set(self.all.get().saturating_add(len));
        self.noted.set(self.noted.get() + 1);
    }

    /// Counts time the worker slept.
    pub(super) fn slept(&self, d: Duration) {
        self.slept.set(self.slept.get() + d);
    }

    /// Once the window is over: whether the queue's delay reached [`TARGET`] (the next window
    /// starts). `None` while it runs.
    pub(super) fn end(&self, now: Instant, polls: u64) -> Option<bool> {
        let wall = now.duration_since(self.started.get());
        if wall < WINDOW {
            return None;
        }
        let ran = polls.saturating_sub(self.polls.get()).max(1);
        let awake = wall.saturating_sub(self.slept.get());
        let per_poll = awake / u32::try_from(ran).unwrap_or(u32::MAX);
        let sum = if per_poll >= HEAVY {
            self.all.get()
        } else {
            self.input.get()
        };
        let waiting = sum / self.noted.get().max(1);
        self.started.set(now);
        self.polls.set(polls);
        self.input.set(0);
        self.all.set(0);
        self.noted.set(0);
        self.slept.set(Duration::ZERO);
        Some(per_poll.saturating_mul(u32::try_from(waiting).unwrap_or(u32::MAX)) >= TARGET)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_queue_that_is_mostly_empty_is_not_overloaded() {
        let t = Instant::now();
        let w = Window::new(t);
        // 10 ms over 10 000 polls: 1 µs a poll; 300 waiting once, then nothing: 100 µs.
        w.saw(300, 300);
        w.saw(0, 0);
        w.saw(0, 0);
        assert_eq!(w.end(t + WINDOW, 10_000), Some(false));
    }

    #[test]
    fn a_queue_long_on_average_is_overloaded() {
        let t = Instant::now();
        let w = Window::new(t);
        // 1 µs a poll; input 400 waiting on average, emptied once in a while: 400 µs.
        w.saw(600, 700);
        w.saw(0, 0);
        w.saw(600, 700);
        assert_eq!(w.end(t + WINDOW, 10_000), Some(true));
        // The next window starts empty.
        assert_eq!(w.end(t + WINDOW + WINDOW / 2, 20_000), None);
        assert_eq!(w.end(t + 2 * WINDOW, 20_000), Some(false));
    }

    #[test]
    fn a_short_queue_of_long_polls_is_overloaded() {
        let t = Instant::now();
        let w = Window::new(t);
        // 4 waiting behind polls of 1 ms each (none of them input).
        w.saw(0, 4);
        assert_eq!(w.end(t + WINDOW, 10), Some(true));
    }

    #[test]
    fn sleeping_does_not_make_polls_look_long() {
        let t = Instant::now();
        let w = Window::new(t);
        // 100 polls in 1 ms awake (10 µs each), 9 ms asleep; 5000 flushers wait behind no input.
        w.slept(WINDOW - Duration::from_millis(1));
        w.saw(0, 5000);
        assert_eq!(w.end(t + WINDOW, 100), Some(false));
    }

    #[test]
    fn output_held_back_behind_input_is_not_overloaded() {
        let t = Instant::now();
        let w = Window::new(t);
        // 5000 short flushers wait behind little input.
        w.saw(1, 5000);
        w.saw(0, 4000);
        assert_eq!(w.end(t + WINDOW, 10_000), Some(false));
    }
}
