//! The Velt backend's monitor thread: it does the duties of a worker that makes no progress
//! (see [`super::worker`]), and starts spawned tasks that wait behind a sibling that computes.
//!
//! Tasks spawned together that compute (`spawn` in a loop, then await them all) would wait for
//! the rescue of a stalled worker: the first runs on without returning, its siblings wait behind it, and each
//! rescue every millisecond or so moved half of them, so they started about 1.3 ms apart (Apple
//! M4, 4 tasks: 0, 2.3, 3.6 and 4.9 ms), where threads started by Rust's `thread::scope` begin
//! within tens of microseconds. So a worker that starts the first poll of a spawned task with
//! other tasks never polled waiting behind it says so (`Shared::watch`, see [`watch`]), and the monitor
//! checks it every [`FAST_TICK`] while it does: once that one poll has lasted a fast tick, the
//! waiting tasks never polled go to sleeping workers, one each, which start them at once
//! ([`start_fresh`]). A task never polled has no wake pending and has not used its worker's
//! reactor yet (a socket handed to it follows it, as with any task moved), and polls shorter
//! than a fast tick (a burst of connections, or of a broadcast's flushers, each starting with a
//! read or a write) never move anything; the monitor checks that often only while such a poll
//! runs and a worker sleeps.
//!
//! A watch is published only while another worker sleeps (never with one worker), and wakes the
//! monitor from its whole-tick nap ([`MonitorSleep::hurry`]) only until [`HURRY_LIMIT`] such
//! wakes in a row moved nothing; a pass that moves tasks allows them again. A program spawning
//! small bursts back to back (`spawn` twice, then await both, 100k times) thus wakes the monitor
//! a few times, not at every burst, and the next tick finds a long poll anyway: tasks behind it
//! start within a tick of it at worst.

use super::queue::RunQueue;
use super::task::Task;
use super::worker::{
    lock, sleeping_helper_of, start_spare, started, wake, workers, Reactor, Shared, WAKE,
};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// How often the monitor checks a worker whose first poll of a spawned task has tasks never
/// polled waiting behind it ([`start_fresh`]): the shortest such poll that hands them to sleeping
/// workers.
pub(super) const FAST_TICK: Duration = Duration::from_micros(50);
/// `Shared::watch`: the monitor has seen this poll once.
pub(super) const SEEN: u64 = 1 << 31;
/// `Shared::watch`: the tasks queued before this poll.
pub(super) const WATCHED: u64 = SEEN - 1;

/// Wakes of the monitor by a watch in a row that moved nothing, after which watches stop waking
/// it until a pass moves tasks (see the module docs).
pub(super) const HURRY_LIMIT: u32 = 4;

/// Publishes that this worker (`me`, one of the workers started `ws`) starts the first poll of a
/// spawned task (its poll number `polls`) with `behind` tasks in its second queue, some never
/// polled, and has the monitor check it soon: whether it did. Not while no other worker sleeps
/// (none could start them; never with one worker).
pub(super) fn watch(
    me: &Shared,
    ws: &[Shared],
    polls: u64,
    behind: usize,
    monitor: &MonitorSleep,
) -> bool {
    // Relaxed: a worker that parks just after this check only misses a quicker start (the
    // stall rescue still moves the tasks).
    if !ws
        .iter()
        .any(|w| !std::ptr::eq(w, me) && w.parked.load(Ordering::Relaxed))
    {
        return false;
    }
    let behind = u64::try_from(behind).unwrap_or(WATCHED).min(WATCHED);
    let word = (polls & 0xffff_ffff) << 32 | behind;
    // Hurries stopped: no handshake with `fast` (the next tick finds the watch).
    if monitor.calm.load(Ordering::Relaxed) {
        me.watch.store(word, Ordering::Release);
    } else {
        me.watch.store(word, Ordering::SeqCst);
        monitor.hurry();
    }
    true
}

/// The monitor's clock: ticks of [`TICK`] while some worker is awake.
pub(super) static TICKS: AtomicU64 = AtomicU64::new(0);
/// The monitor's tick.
pub(super) const TICK: Duration = Duration::from_millis(1);
/// Ticks without a new task poll after which the monitor does a worker's duties (see the module
/// docs): one full tick at least, which ordinary polls never take.
pub(super) const STALL_TICKS: u64 = 2;

/// The monitor's sleep while every worker sleeps, until a worker wakes it ([`MonitorSleep::kick`]).
pub(super) struct MonitorSleep {
    pub(super) asleep: AtomicBool,
    pub(super) woken: Mutex<bool>,
    pub(super) cv: Condvar,
    /// Checking watched workers every [`FAST_TICK`]: a new watch needs no [`hurry`](Self::hurry).
    pub(super) fast: AtomicBool,
    /// A worker started a watched poll while the monitor napped for a whole tick.
    pub(super) hurry: AtomicBool,
    /// [`HURRY_LIMIT`] hurries in a row moved nothing: watches do not hurry (module docs).
    pub(super) calm: AtomicBool,
    pub(super) nap: Mutex<()>,
    pub(super) nap_cv: Condvar,
}

pub(super) static MONITOR: MonitorSleep = MonitorSleep::new();

impl MonitorSleep {
    pub(super) const fn new() -> MonitorSleep {
        MonitorSleep {
            asleep: AtomicBool::new(false),
            woken: Mutex::new(false),
            cv: Condvar::new(),
            fast: AtomicBool::new(false),
            hurry: AtomicBool::new(false),
            calm: AtomicBool::new(false),
            nap: Mutex::new(()),
            nap_cv: Condvar::new(),
        }
    }

    /// Cuts the monitor's nap short unless it checks watched workers often already (a worker
    /// published a watch, see the module docs). SeqCst: ordered after the caller's store to its
    /// watch, which the monitor reads after storing `fast` (each sees the other's store).
    pub(super) fn hurry(&self) {
        if !self.fast.load(Ordering::SeqCst) && !self.hurry.swap(true, Ordering::SeqCst) {
            let _nap = lock(&self.nap);
            self.nap_cv.notify_one();
        }
    }

    /// Sleeps for `d` or until a worker hurries it: whether one did.
    #[cfg(not(windows))]
    pub(super) fn nap(&self, d: Duration) -> bool {
        let nap = lock(&self.nap);
        let _ = self
            .nap_cv
            .wait_timeout_while(nap, d, |_| !self.hurry.load(Ordering::SeqCst));
        self.hurry.swap(false, Ordering::SeqCst)
    }

    /// Sleeps for `d` or until a worker hurries it: whether one did. On Windows a condition
    /// variable's timeout rounds up to the system timer's tick (about 15 ms) where
    /// `thread::sleep` uses a high-resolution timer (1 ms takes about 1.5 ms, 50 µs about
    /// 0.6 ms): sleeps in slices of a fast tick at most, looking for a hurry between them.
    #[cfg(windows)]
    pub(super) fn nap(&self, d: Duration) -> bool {
        let end = Instant::now() + d;
        while !self.hurry.load(Ordering::SeqCst) {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            std::thread::sleep(left.min(FAST_TICK));
        }
        self.hurry.swap(false, Ordering::SeqCst)
    }

    /// Wakes the monitor if it sleeps (a worker woke up: its polls need watching).
    pub(super) fn kick(&self) {
        // SeqCst: ordered after the caller's store to `parked`, which the monitor checks after
        // its own store here (on ARM a relaxed load could come first and both would miss the
        // other).
        if self.asleep.load(Ordering::SeqCst) {
            *lock(&self.woken) = true;
            self.cv.notify_one();
        }
    }

    /// Sleeps while every worker of `ws` is parked, until a worker kicks.
    pub(super) fn sleep_while_parked(&self, ws: &[Shared]) {
        let all_parked = || ws.iter().all(|w| w.parked.load(Ordering::SeqCst));
        let mut woken = lock(&self.woken);
        self.asleep.store(true, Ordering::SeqCst);
        // A worker that woke between the caller's check and here kicks after this store.
        if all_parked() {
            while !*woken {
                woken = self.cv.wait(woken).unwrap_or_else(|e| e.into_inner());
            }
        }
        *woken = false;
        self.asleep.store(false, Ordering::SeqCst);
    }
}

pub(super) fn run() {
    let mut next_tick = Instant::now() + TICK;
    let mut last_pass = Instant::now();
    // Hurried wakes in a row that moved nothing.
    let mut idle_hurries: u32 = 0;
    loop {
        let ws = workers();
        if ws.iter().all(|w| w.parked.load(Ordering::SeqCst)) {
            MONITOR.sleep_while_parked(ws);
            next_tick = Instant::now() + TICK;
            continue;
        }
        let up = &ws[..started()];
        // Slow mode published before the last look at the watches: a watch stored after it
        // sees `fast` false and hurries (see `MonitorSleep::hurry`).
        let mut fast = watching(up);
        MONITOR.fast.store(fast, Ordering::SeqCst);
        if !fast && watching(up) {
            fast = true;
            MONITOR.fast.store(true, Ordering::SeqCst);
        }
        let until_tick = next_tick.saturating_duration_since(Instant::now());
        let hurried = MONITOR.nap(if fast {
            until_tick.min(FAST_TICK)
        } else {
            until_tick
        });
        if hurried {
            idle_hurries = idle_hurries.saturating_add(1);
            if idle_hurries >= HURRY_LIMIT {
                MONITOR.calm.store(true, Ordering::Relaxed);
            }
        }
        let now = Instant::now();
        // Passes a fast tick apart at least: a poll seen twice has lasted that long.
        if now.duration_since(last_pass) >= FAST_TICK {
            last_pass = now;
            if start_fresh(up) > 0 {
                idle_hurries = 0;
                MONITOR.calm.store(false, Ordering::Relaxed);
            }
        }
        if now < next_tick {
            continue;
        }
        next_tick = now + TICK;
        let now = TICKS.fetch_add(1, Ordering::Relaxed) + 1;
        for (i, w) in ws.iter().enumerate().take(started()) {
            if stalled(w, now) {
                rescue(i, now);
            }
        }
    }
}

/// Whether one of the workers `ws` (those started) runs a watched poll that sleeping ones could
/// help with.
pub(super) fn watching(ws: &[Shared]) -> bool {
    let watched = ws
        .iter()
        .any(|w| w.watch.load(Ordering::SeqCst) != 0 && !w.parked.load(Ordering::SeqCst));
    watched && ws.iter().any(|w| w.parked.load(Ordering::SeqCst))
}

/// The monitor's pass over the watches of the workers `ws` (those started), passes a
/// [`FAST_TICK`] apart at least: marks a watched poll seen, and when it was seen at the last
/// pass already (the same poll, on for a fast tick), hands the tasks never polled that waited
/// behind it to sleeping workers, one each (see the module docs). How many it moved.
pub(super) fn start_fresh(ws: &[Shared]) -> usize {
    let mut moved = 0;
    for (i, w) in ws.iter().enumerate() {
        let watch = w.watch.load(Ordering::SeqCst);
        if watch == 0 || w.parked.load(Ordering::SeqCst) {
            continue;
        }
        if watch & SEEN == 0 {
            let _ =
                w.watch
                    .compare_exchange(watch, watch | SEEN, Ordering::SeqCst, Ordering::Relaxed);
            continue;
        }
        let helpers = (0..ws.len())
            .filter(|&j| j != i && ws[j].parked.load(Ordering::SeqCst))
            .count();
        if helpers == 0 {
            continue;
        }
        // Not waiting on the worker: one that holds its queue is between polls.
        let Ok(mut queue) = w.queue.try_lock() else {
            continue;
        };
        // Still the same poll: done with it either way.
        if w.watch
            .compare_exchange(watch, 0, Ordering::SeqCst, Ordering::Relaxed)
            .is_err()
        {
            continue;
        }
        let within = usize::try_from(watch & WATCHED).unwrap_or(usize::MAX);
        let (_moving, fresh) = take_fresh(&mut queue, within, helpers);
        drop(queue);
        moved += fresh.len();
        for task in fresh {
            match sleeping_helper_of(ws, i) {
                Some(h) => hand_over(&ws[h], h, vec![task]),
                None => send_back(w, vec![task]),
            }
        }
    }
    moved
}

/// Takes up to `max` fresh tasks from the first `within` of `queue`'s second queue
/// (`RunQueue::take_fresh`), with the guard that counts them as pending until they reach an
/// inbox: taken first, so that no moment leaves them in no queue and uncounted (the worker may
/// park meanwhile, the last one to, and the process would end with them never run).
pub(super) fn take_fresh(
    queue: &mut RunQueue,
    within: usize,
    max: usize,
) -> (crate::task::quiet::Moving, Vec<Arc<Task>>) {
    let moving = crate::task::quiet::Moving::new();
    (moving, queue.take_fresh(within, max))
}

/// Awake and no task poll started for [`STALL_TICKS`] ticks up to tick `now`.
pub(super) fn stalled(w: &Shared, now: u64) -> bool {
    !w.parked.load(Ordering::SeqCst)
        && now.saturating_sub(w.progress.load(Ordering::Relaxed)) >= STALL_TICKS
}

/// Does the duties of worker `i`, stalled: a turn of its reactor (I/O and due timers), then its
/// inbox (the tasks that woke) and half of its queue to another worker, which becomes their
/// home. Pinned tasks stay.
pub(super) fn rescue(i: usize, now: u64) {
    rescue_of(workers(), started(), i, now, start_spare);
}

/// [`rescue`] among the workers `ws`, of which the first `up` have started, with `spare` to
/// start another. The tasks move only when a worker can take them: one asleep, one not stalled
/// itself, or a spare that starts. With none (one worker and every spare started, or every
/// other worker stalled too) they stay where they are: sent back to the same worker, or on to
/// another stalled one, they would only be moved again at every tick, hundreds of thousands
/// of them under the inbox lock the worker needs (#1673).
pub(super) fn rescue_of(
    ws: &[Shared],
    up: usize,
    i: usize,
    now: u64,
    spare: impl FnOnce() -> Option<usize>,
) {
    let w = &ws[i];
    turn_for(w, i);
    lock(&w.timers).fire(Instant::now());
    // Not waiting on the worker: one that holds its queue is running.
    let waiting =
        w.inbox_len.load(Ordering::Acquire) > 0 || w.queue.try_lock().is_ok_and(|q| q.len() > 0);
    if !waiting {
        return;
    }
    let Some(helper) = helper_for(&ws[..up], i, now).or_else(spare) else {
        return;
    };
    // The tasks taken below are in no queue until they reach one again: counted meanwhile, as
    // the worker may count as idle (woken, not yet running).
    let _moving = crate::task::quiet::Moving::new();
    let mut moved: Vec<Arc<Task>> = Vec::new();
    if w.inbox_len.load(Ordering::Acquire) > 0 {
        let mut inbox = lock(&w.inbox);
        moved.extend(inbox.drain(..));
        w.inbox_len.store(0, Ordering::SeqCst);
    }
    if let Ok(mut queue) = w.queue.try_lock() {
        moved.extend(queue.take_half_up());
    }
    let (pinned, moved): (Vec<_>, Vec<_>) = moved.into_iter().partition(|t| t.pinned);
    // Back through the inbox with a wake: the worker may have parked since the drain above.
    if !pinned.is_empty() {
        send_back(w, pinned);
    }
    if !moved.is_empty() {
        hand_over(&ws[helper], helper, moved);
    }
}

/// The worker among `ws` (those started) that takes the tasks of worker `i`, stalled at tick
/// `now`: the sleeping one that did least, else the awake one not stalled with the shortest
/// queue. None when every other worker is stalled too.
fn helper_for(ws: &[Shared], i: usize, now: u64) -> Option<usize> {
    sleeping_helper_of(ws, i).or_else(|| {
        (0..ws.len())
            .filter(|&j| j != i && !stalled(&ws[j], now))
            .min_by_key(|&j| ws[j].queued.load(Ordering::Relaxed))
    })
}

/// Makes worker `index` (`w`) the home of `tasks`, queues them in its inbox and wakes it.
pub(super) fn hand_over(w: &Shared, index: usize, tasks: Vec<Arc<Task>>) {
    {
        let mut inbox = lock(&w.inbox);
        for task in tasks {
            task.home.store(index, Ordering::Relaxed);
            inbox.push_back(task);
        }
        w.inbox_len.store(inbox.len(), Ordering::SeqCst);
    }
    wake(w);
}

/// A non-blocking turn of the reactor of worker `i` (`w`), stalled, for it: the events of its
/// sockets. Its own wake-up event is posted again for it: a worker woken as it parks and then
/// taken off its core before its own poll counts as stalled, and with the event taken here its
/// poll would sleep on with tasks in its inbox, the wake spent (`parked` already cleared, so
/// later pushes and the monitor's hand-back post none).
pub(super) fn turn_for(w: &Shared, i: usize) {
    let Ok(mut r) = w.reactor.try_lock() else {
        return;
    };
    let Reactor { poll, events } = &mut *r;
    if poll.poll(events, Some(Duration::ZERO)).is_err() {
        events.clear();
    }
    let mut woken = false;
    for ev in events.iter() {
        if ev.token() == WAKE {
            woken = true;
        } else {
            super::io::dispatch(i, ev);
        }
    }
    drop(r);
    if woken {
        let _ = w.waker.wake();
    }
}

/// Returns tasks the monitor took from `w` to its inbox and wakes it.
pub(super) fn send_back(w: &Shared, tasks: Vec<Arc<Task>>) {
    {
        let mut inbox = lock(&w.inbox);
        inbox.extend(tasks);
        w.inbox_len.store(inbox.len(), Ordering::SeqCst);
    }
    wake(w);
}
