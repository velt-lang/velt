use super::super::monitor::*;
use super::*;
use std::collections::HashSet;

fn idle_task() -> Arc<Task> {
    super::super::task::make(async {}, 0, true).0
}

#[test]
fn a_worker_thread_recurses_as_deep_as_the_main_thread() {
    // Recurses until 5 MiB of stack are in use (measured, whatever a frame's size): past Rust's
    // 2 MiB default for a spawned thread, within the 8 MiB the main thread has.
    fn deep(top: usize, need: usize) -> usize {
        let frame = std::hint::black_box([1u8; 1024]);
        let here = frame.as_ptr() as usize;
        if top.abs_diff(here) >= need {
            return 1;
        }
        deep(top, need) + std::hint::black_box(frame)[0] as usize
    }
    let frames = worker_thread()
        .spawn(|| {
            let top = std::hint::black_box([0u8; 1]).as_ptr() as usize;
            deep(top, 5 << 20)
        })
        .unwrap()
        .join()
        .unwrap();
    assert!(frames > 1);
}

#[test]
fn tasks_handed_back_go_through_the_inbox_and_wake_the_worker() {
    // The monitor's rescue hands tasks back to a worker that may have parked meanwhile: they
    // must be where its park check looks, and it must be woken.
    let w = new_shared();
    w.parked.store(true, Ordering::SeqCst);
    send_back(&w, vec![idle_task(), idle_task()]);
    assert_eq!(w.inbox_len.load(Ordering::SeqCst), 2);
    assert!(!w.parked.load(Ordering::SeqCst), "the worker was not woken");
    let mut r = lock(&w.reactor);
    let Reactor { poll, events } = &mut *r;
    poll.poll(events, Some(Duration::ZERO)).expect("poll");
    assert!(events.iter().any(|e| e.token() == WAKE), "no wake event");
}

#[test]
fn a_woken_worker_is_not_stalled() {
    // Woken from its park, a worker the OS has not run yet still has its old progress tick:
    // the wake restarts it, so the monitor does not take its tasks.
    let w = new_shared();
    w.parked.store(true, Ordering::SeqCst);
    let old = u64::MAX;
    w.progress.store(old, Ordering::Relaxed);
    let before = TICKS.load(Ordering::Relaxed);
    wake(&w);
    let progress = w.progress.load(Ordering::Relaxed);
    assert!(progress >= before && progress <= TICKS.load(Ordering::Relaxed));
}

#[test]
fn a_worker_with_queued_tasks_does_not_sleep_in_its_turn() {
    let w = new_shared();
    assert_eq!(park_timeout(&w), None);
    lock(&w.queue).push_next(idle_task());
    assert_eq!(park_timeout(&w), Some(Duration::ZERO));
}

/// Runs `f` on this thread as worker `index` of its own (not one of the runtime's workers, so
/// no other worker can take what it queues).
fn as_worker<R>(index: usize, f: impl FnOnce(&Local) -> R) -> R {
    struct Leave;
    impl Drop for Leave {
        fn drop(&mut self) {
            LOCAL.with(|l| l.set(std::ptr::null()));
        }
    }
    let me: &'static Shared = Box::leak(Box::new(new_shared()));
    let local = Local::new(index, me);
    LOCAL.with(|l| l.set(&local));
    let _leave = Leave;
    f(&local)
}

#[test]
fn a_task_woken_by_a_task_runs_next_on_the_wakers_worker() {
    // A waiter whose home is worker 1, woken by a task that worker 0 polls: worker 0 runs it
    // next, ahead of the tasks already queued, instead of sending it to worker 1 (a reply
    // reaching the request that waits for it), and it keeps its home. Checked where the wake
    // places it: where it then runs depends on the other workers, which may take work from
    // worker 0 as designed.
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let (waiter, _joined) = super::super::task::make(
        async move {
            let _ = rx.await;
        },
        1,
        false,
    );
    assert!(matches!(waiter.run(), Ran::Waiting));
    as_worker(0, |local| {
        let (input, made) = (idle_task(), idle_task());
        lock(&local.me.queue).extend_io([input.clone()]);
        lock(&local.me.queue).push(made.clone(), Instant::now);
        let (waker, _joined) = super::super::task::make(
            async move {
                let _ = tx.send(());
            },
            0,
            false,
        );
        poll_task(local.me, local, waker, 0, || &[]);
        let mut q = lock(&local.me.queue);
        let order: Vec<_> = std::iter::from_fn(|| q.pop(Instant::now)).collect();
        let is = |t: &Arc<Task>| {
            [("waiter", &waiter), ("input", &input), ("made", &made)]
                .into_iter()
                .find(|(_, u)| Arc::ptr_eq(t, u))
                .map_or("other", |(name, _)| name)
        };
        let order: Vec<_> = order.iter().map(is).collect();
        assert_eq!(order, ["waiter", "input", "made"], "not run next here");
        assert_eq!(local.me.inbox_len.load(Ordering::SeqCst), 0);
    });
    assert_eq!(waiter.home.load(Ordering::Relaxed), 1, "its home moved");
}

#[test]
fn the_monitors_turn_for_a_stalled_worker_leaves_it_its_wake_up() {
    // Woken as it parks, then taken off its core before its own poll, a worker counts as
    // stalled, and the monitor turns its reactor for it. The wake-up event must still reach the
    // worker's own poll: `parked` is cleared already, so the next push (here the monitor handing
    // back a pinned task) posts no other, and the worker would sleep on with tasks in its inbox.
    let w = new_shared();
    w.parked.store(true, Ordering::SeqCst);
    send_back(&w, vec![idle_task()]);
    turn_for(&w, 0);
    send_back(&w, vec![idle_task()]);
    assert_eq!(w.inbox_len.load(Ordering::SeqCst), 2);
    let mut r = lock(&w.reactor);
    let Reactor { poll, events } = &mut *r;
    poll.poll(events, Some(Duration::ZERO)).expect("poll");
    assert!(
        events.iter().any(|e| e.token() == WAKE),
        "the monitor took the worker's wake-up"
    );
}

#[test]
fn a_worker_that_does_not_park_after_all_wakes_the_monitor() {
    // The monitor sleeps once it sees every worker parked. A worker that marked itself parked
    // and then found tasks in its inbox runs them: the monitor must watch it again (a task
    // that runs on would hold back the ones behind it with no rescue).
    let monitor = MonitorSleep::new();
    monitor.asleep.store(true, Ordering::SeqCst);
    let w = new_shared();
    lock(&w.inbox).push_back(idle_task());
    w.inbox_len.store(1, Ordering::SeqCst);
    assert!(!may_park(&w, &monitor));
    assert!(!w.parked.load(Ordering::SeqCst));
    assert!(*lock(&monitor.woken), "the monitor sleeps on");
    // With nothing in its inbox, the worker parks and the monitor sleeps on.
    let monitor = MonitorSleep::new();
    monitor.asleep.store(true, Ordering::SeqCst);
    let w = new_shared();
    assert!(may_park(&w, &monitor));
    assert!(w.parked.load(Ordering::SeqCst));
    assert!(!*lock(&monitor.woken));
}

#[test]
fn the_first_poll_of_a_spawned_task_with_fresh_tasks_behind_it_is_watched() {
    // A spawn burst: the first task's first poll publishes how many tasks were queued behind
    // it (the monitor hands the fresh ones to sleeping workers if the poll lasts), and clears
    // it once the poll returns; a task polled before is not watched.
    as_worker(0, |local| {
        let me: &'static Shared = local.me;
        let seen = Arc::new(AtomicU64::new(u64::MAX));
        let (first, _j) = {
            let seen = seen.clone();
            super::super::task::make(
                async move { seen.store(me.watch.load(Ordering::SeqCst), Ordering::SeqCst) },
                0,
                false,
            )
        };
        lock(&me.queue).push(first, Instant::now);
        lock(&me.queue).push(idle_task(), Instant::now);
        lock(&me.queue).push(idle_task(), Instant::now);
        let (task, _, behind) = next_task(me);
        assert_eq!(behind, 2);
        // Watched only while another worker sleeps (one that could start them).
        let other: &'static [Shared] = Box::leak(Box::new([new_shared()]));
        other[0].parked.store(true, Ordering::SeqCst);
        poll_task(me, local, task.expect("a task"), behind, || other);
        let watch = seen.load(Ordering::SeqCst);
        assert_eq!(watch & WATCHED, 2, "not watched during its poll");
        assert_eq!(watch & SEEN, 0);
        assert_eq!(me.watch.load(Ordering::SeqCst), 0, "still watched after it");
        // Polled once already: not watched, whatever waits behind it.
        while next_task(me).0.is_some() {}
        let (polled, _j) = super::super::task::make(std::future::pending::<()>(), 0, false);
        assert!(matches!(polled.run(), Ran::Waiting));
        lock(&me.queue).push(polled, Instant::now);
        lock(&me.queue).push(idle_task(), Instant::now);
        assert_eq!(next_task(me).2, 0);
        // No other worker sleeps (one worker, or all busy): not published.
        while next_task(me).0.is_some() {}
        let seen = Arc::new(AtomicU64::new(u64::MAX));
        let (first, _j) = {
            let seen = seen.clone();
            super::super::task::make(
                async move { seen.store(me.watch.load(Ordering::SeqCst), Ordering::SeqCst) },
                0,
                false,
            )
        };
        lock(&me.queue).push(first, Instant::now);
        lock(&me.queue).push(idle_task(), Instant::now);
        let (task, _, behind) = next_task(me);
        assert_eq!(behind, 1);
        other[0].parked.store(false, Ordering::SeqCst);
        poll_task(me, local, task.expect("a task"), behind, || other);
        assert_eq!(
            seen.load(Ordering::SeqCst),
            0,
            "watched with no worker to help"
        );
    });
}

#[test]
fn a_watched_poll_seen_twice_hands_the_fresh_tasks_to_sleeping_workers() {
    let ws: &'static [Shared] = Box::leak(
        (0..4)
            .map(|_| new_shared())
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    );
    for w in &ws[1..] {
        w.parked.store(true, Ordering::SeqCst);
    }
    let polled = idle_task();
    assert!(matches!(polled.run(), Ran::Done));
    let (a, b) = (
        super::super::task::make(async {}, 0, false).0,
        super::super::task::make(async {}, 0, false).0,
    );
    let pinned = idle_task();
    for t in [a.clone(), polled.clone(), pinned.clone(), b.clone()] {
        lock(&ws[0].queue).push(t, Instant::now);
    }
    let watch = 7 << 32 | 4;
    ws[0].watch.store(watch, Ordering::SeqCst);
    assert!(watching(ws));
    // Seen once: the poll may end any moment; nothing moves.
    start_fresh(ws);
    assert_eq!(ws[0].watch.load(Ordering::SeqCst), watch | SEEN);
    assert_eq!(lock(&ws[0].queue).len(), 4);
    // Seen again: the same poll has lasted; the fresh tasks start on sleeping workers, one each.
    assert_eq!(start_fresh(ws), 2);
    assert_eq!(
        crate::task::quiet::moving_here(),
        0,
        "still counted as moving"
    );
    assert_eq!(ws[0].watch.load(Ordering::SeqCst), 0);
    assert_eq!(
        lock(&ws[0].queue).len(),
        2,
        "the polled and the pinned task stay"
    );
    let homes: HashSet<usize> = [&a, &b]
        .iter()
        .map(|t| t.home.load(Ordering::Relaxed))
        .collect();
    assert_eq!(homes.len(), 2, "not one each");
    for h in homes {
        assert!(h != 0);
        assert_eq!(ws[h].inbox_len.load(Ordering::SeqCst), 1);
        assert!(!ws[h].parked.load(Ordering::SeqCst), "not woken");
    }
    assert!(!watching(ws));
}

#[test]
fn tasks_taken_to_start_elsewhere_count_as_pending_until_they_reach_an_inbox() {
    // Out of their worker's queue and in no inbox yet: the guard that counts them as pending
    // comes with them (else the worker, parking last, could end the process meanwhile).
    let mut q = super::super::queue::RunQueue::new(Instant::now());
    for _ in 0..3 {
        q.push(super::super::task::make(async {}, 0, false).0, Instant::now);
    }
    assert_eq!(crate::task::quiet::moving_here(), 0);
    let (moving, fresh) = take_fresh(&mut q, 3, 2);
    assert_eq!(fresh.len(), 2);
    assert_eq!(q.len(), 1);
    assert_eq!(
        crate::task::quiet::moving_here(),
        1,
        "taken and not counted"
    );
    drop(moving);
    assert_eq!(crate::task::quiet::moving_here(), 0);
}

#[test]
fn a_watch_hurries_the_monitor_only_while_it_naps_for_a_whole_tick() {
    let monitor = MonitorSleep::new();
    let ws: &'static [Shared] = Box::leak(Box::new([new_shared(), new_shared()]));
    let w = &ws[0];
    // No other worker sleeps: nothing published, no hurry.
    assert!(!watch(w, ws, 1, 3, &monitor));
    assert_eq!(w.watch.load(Ordering::SeqCst), 0);
    assert!(!monitor.hurry.load(Ordering::SeqCst));
    ws[1].parked.store(true, Ordering::SeqCst);
    assert!(watch(w, ws, 1, 3, &monitor));
    assert_eq!(w.watch.load(Ordering::SeqCst), 1 << 32 | 3);
    assert!(monitor.hurry.load(Ordering::SeqCst));
    // Hurried: the nap returns at once (it would otherwise last a minute).
    let start = Instant::now();
    assert!(monitor.nap(Duration::from_secs(60)));
    assert!(start.elapsed() < Duration::from_secs(30));
    assert!(!monitor.hurry.load(Ordering::SeqCst));
    // Hurries stopped (several in a row moved nothing): published, no hurry.
    monitor.calm.store(true, Ordering::SeqCst);
    assert!(watch(w, ws, 2, 3, &monitor));
    assert_eq!(w.watch.load(Ordering::SeqCst), 2 << 32 | 3);
    assert!(!monitor.hurry.load(Ordering::SeqCst));
    monitor.calm.store(false, Ordering::SeqCst);
    // Checking often already: no hurry.
    monitor.fast.store(true, Ordering::SeqCst);
    assert!(watch(w, ws, 4, 3, &monitor));
    assert!(!monitor.hurry.load(Ordering::SeqCst));
    // Not hurried: the nap lasts its time and says so.
    assert!(!monitor.nap(Duration::from_millis(1)));
}
