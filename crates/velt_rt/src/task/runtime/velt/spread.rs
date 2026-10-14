//! When the Velt backend uses another worker, and when it gives it back.
//!
//! More workers only pay when the work is bigger than the workers running it: on a core that
//! keeps up, spreading adds cross-thread wakes and splits the writes it could batch (echo on 10
//! workers took twice the CPU of one worker for less throughput). So a helper is a trial. The
//! runtime measures the rate of work for [`WINDOW`] without the helper, then for [`WINDOW`] with
//! it, and keeps the helper only if the rate rose by at least half of what one more worker could
//! add at best (`(n + 1) / n` for `n` workers running). Otherwise the helper is sent back: its
//! tasks move home to the worker it helped on their next wake (sockets follow, see `io`), and no
//! trial starts for a while, longer after each failed one. CPU-bound work passes every trial
//! and grows to every worker.
//!
//! The rate of work is bytes moved through the backend's sockets plus task polls: polls alone
//! would reward spreading a broadcast, whose split writes take more polls for the same bytes,
//! and bytes alone would see no work in a program that only computes.

use super::worker::workers;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The share of the most one more worker could add (`1 / n` for `n` running) a trial must gain.
const SHARE: f64 = 0.3;
/// How long each half of a trial runs.
const WINDOW: Duration = Duration::from_millis(20);
/// Pause after a failed trial (doubled after each further failure, up to [`MAX_PAUSE`]).
const PAUSE: Duration = Duration::from_millis(500);
const MAX_PAUSE: Duration = Duration::from_secs(4);

/// No redirect.
pub(super) const NONE: usize = usize::MAX;

/// Per worker, what the policy reads (published by the worker): its work so far, and where its
/// tasks go when it was sent back.
pub(super) struct Counters {
    pub(super) work: AtomicU64,
    pub(super) redirect: AtomicUsize,
}

impl Counters {
    pub(super) fn new() -> Counters {
        Counters {
            work: AtomicU64::new(0),
            redirect: AtomicUsize::new(NONE),
        }
    }
}

enum Phase {
    Idle,
    /// The rate without the helper (`each`: every worker's work at the start).
    Measuring {
        origin: usize,
        started: Instant,
        work: u64,
        each: Vec<u64>,
    },
    /// The rate with it.
    Trying {
        origin: usize,
        helper: usize,
        started: Instant,
        work: u64,
        base: f64,
        running: usize,
    },
}

struct State {
    phase: Phase,
    paused_until: Option<Instant>,
    failures: u32,
}

static STATE: Mutex<State> = Mutex::new(State {
    phase: Phase::Idle,
    paused_until: None,
    failures: 0,
});

fn total() -> u64 {
    workers()
        .iter()
        .map(|w| w.counters.work.load(Ordering::Relaxed))
        .sum()
}

/// What a busy worker should do now.
pub(super) enum Step {
    /// Nothing (no trial of its own, or one still running).
    Wait,
    /// Take a helper now: the rate without it is measured.
    Help,
}

/// Called by `origin` while it is busy with tasks waiting: starts a trial (measuring first),
/// moves a trial on, or ends it.
pub(super) fn busy(origin: usize, now: Instant) -> Step {
    let mut s = STATE.lock().unwrap_or_else(|e| e.into_inner());
    match &s.phase {
        Phase::Idle => {
            if s.paused_until.is_none_or(|t| now >= t) {
                s.phase = Phase::Measuring {
                    origin,
                    started: now,
                    work: total(),
                    each: each(),
                };
            }
            Step::Wait
        }
        // A measurement its worker stopped driving (it is no longer busy): this one takes over.
        Phase::Measuring {
            origin: o, started, ..
        } if *o != origin && now.duration_since(*started) >= 3 * WINDOW => {
            s.phase = Phase::Measuring {
                origin,
                started: now,
                work: total(),
                each: each(),
            };
            Step::Wait
        }
        Phase::Measuring {
            origin: o,
            started,
            work,
            each: before,
        } if *o == origin => {
            if now.duration_since(*started) < WINDOW {
                return Step::Wait;
            }
            let base = rate(*work, *started, now);
            // Workers that did work while it was measured.
            let running = each()
                .iter()
                .zip(before)
                .filter(|(now, then)| now > then)
                .count()
                .max(1);
            // Started by `helping`, with the helper's index.
            s.phase = Phase::Trying {
                origin,
                helper: NONE,
                started: now,
                work: total(),
                base,
                running,
            };
            Step::Help
        }
        // Any busy worker ends a trial whose time is up (its own worker may have gone quiet).
        Phase::Trying { .. } => {
            judge(&mut s, now);
            Step::Wait
        }
        _ => Step::Wait,
    }
}

/// The helper `busy` asked for is `helper` (or none was asleep: the trial is dropped).
pub(super) fn helping(origin: usize, helper: Option<usize>) {
    let mut s = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if let Phase::Trying {
        origin: o,
        helper: h,
        ..
    } = &mut s.phase
    {
        if *o == origin {
            match helper {
                Some(i) => {
                    workers()[i]
                        .counters
                        .redirect
                        .store(NONE, Ordering::Release);
                    *h = i;
                }
                None => s.phase = Phase::Idle,
            }
        }
    }
}

fn rate(work: u64, since: Instant, now: Instant) -> f64 {
    total().saturating_sub(work) as f64 / now.duration_since(since).as_secs_f64().max(1e-6)
}

/// Every worker's work so far.
fn each() -> Vec<u64> {
    workers()
        .iter()
        .map(|w| w.counters.work.load(Ordering::Relaxed))
        .collect()
}

/// Ends a trial once its second window is over: the helper stays if the rate rose by half of
/// the most one more worker could add, else it is sent back.
fn judge(s: &mut State, now: Instant) {
    let Phase::Trying {
        origin,
        helper,
        started,
        work,
        base,
        running,
    } = s.phase
    else {
        return;
    };
    if now.duration_since(started) < WINDOW || helper == NONE {
        return;
    }
    let r = rate(work, started, now);
    let gained = r >= base * (1.0 + SHARE / running as f64);
    s.phase = Phase::Idle;
    if gained {
        s.failures = 0;
        s.paused_until = None;
        return;
    }
    workers()[helper]
        .counters
        .redirect
        .store(origin, Ordering::Release);
    let pause = PAUSE.saturating_mul(1 << s.failures.min(8)).min(MAX_PAUSE);
    s.failures += 1;
    s.paused_until = Some(now + pause);
}

/// `origin` is mostly idle now: a trial of its own ends without a verdict, and a helper it had
/// is sent back (its load fell; no pause, it was not the trial that failed).
pub(super) fn idle(origin: usize) {
    let mut s = STATE.lock().unwrap_or_else(|e| e.into_inner());
    match s.phase {
        Phase::Measuring { origin: o, .. } if o == origin => s.phase = Phase::Idle,
        Phase::Trying {
            origin: o, helper, ..
        } if o == origin => {
            if helper != NONE {
                workers()[helper]
                    .counters
                    .redirect
                    .store(origin, Ordering::Release);
            }
            s.phase = Phase::Idle;
        }
        _ => {}
    }
}
