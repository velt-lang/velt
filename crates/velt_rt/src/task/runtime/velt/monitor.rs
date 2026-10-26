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

use super::task::Task;
use super::worker::{
    lock, sleeping_helper, sleeping_helper_of, start_spare, started, wake, workers, Reactor,
    Shared, WAKE,
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

/// Publishes that this worker starts the first poll of a spawned task (its poll number `polls`)
/// with `behind` tasks in its second queue, some never polled, and has the monitor check it soon.
pub(super) fn watch(me: &Shared, polls: u64, behind: usize, monitor: &MonitorSleep) {
    let behind = u64::try_from(behind).unwrap_or(WATCHED).min(WATCHED);
    me.watch
        .store((polls & 0xffff_ffff) << 32 | behind, Ordering::SeqCst);
    monitor.hurry();
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

    /// Sleeps for `d` or until a worker hurries it.
    pub(super) fn nap(&self, d: Duration) {
        let nap = lock(&self.nap);
        let _ = self
            .nap_cv
            .wait_timeout_while(nap, d, |_| !self.hurry.load(Ordering::SeqCst));
        self.hurry.store(false, Ordering::SeqCst);
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
    // Its ticks time the rescues: the user-interactive class also keeps its naps short.
    crate::task::runtime::worker_thread_started();
    let mut next_tick = Instant::now() + TICK;
    let mut last_pass = Instant::now();
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
        MONITOR.nap(if fast {
            until_tick.min(FAST_TICK)
        } else {
            until_tick
        });
        let now = Instant::now();
        // Passes a fast tick apart at least: a poll seen twice has lasted that long.
        if now.duration_since(last_pass) >= FAST_TICK {
            last_pass = now;
            start_fresh(up);
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
/// behind it to sleeping workers, one each (see the module docs).
pub(super) fn start_fresh(ws: &[Shared]) {
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
        let fresh = queue.take_fresh(within, helpers);
        drop(queue);
        // In no queue until they reach an inbox (the worker may count as idle meanwhile).
        let _moving = crate::task::quiet::Moving::new();
        for task in fresh {
            match sleeping_helper_of(ws, i) {
                Some(h) => hand_over(&ws[h], h, vec![task]),
                None => send_back(w, vec![task]),
            }
        }
    }
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
    let ws = workers();
    let w = &ws[i];
    turn_for(w, i);
    lock(&w.timers).fire(Instant::now());
    // The tasks taken below are in no queue until they reach one again: counted meanwhile, as
    // the worker may count as idle (woken, not yet running).
    let _moving = crate::task::quiet::Moving::new();
    let mut moved: Vec<Arc<Task>> = Vec::new();
    if w.inbox_len.load(Ordering::Acquire) > 0 {
        let mut inbox = lock(&w.inbox);
        moved.extend(inbox.drain(..));
        w.inbox_len.store(0, Ordering::SeqCst);
    }
    // Not waiting on the worker: one that holds its queue is running.
    if let Ok(mut queue) = w.queue.try_lock() {
        moved.extend(queue.take_half_up());
    }
    if moved.is_empty() {
        return;
    }
    let (pinned, moved): (Vec<_>, Vec<_>) = moved.into_iter().partition(|t| t.pinned);
    // Back through the inbox with a wake: the worker may have parked since the drain above.
    if !pinned.is_empty() {
        send_back(w, pinned);
    }
    let others = || (0..started()).filter(|&j| j != i);
    let least_queued = |j: &usize| ws[*j].queued.load(Ordering::Relaxed);
    let helper = sleeping_helper(i)
        .or_else(|| {
            others()
                .filter(|&j| !stalled(&ws[j], now))
                .min_by_key(least_queued)
        })
        .or_else(start_spare)
        .or_else(|| others().min_by_key(least_queued));
    let Some(helper) = helper else {
        send_back(w, moved);
        return;
    };
    if moved.is_empty() {
        return;
    }
    hand_over(&ws[helper], helper, moved);
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
