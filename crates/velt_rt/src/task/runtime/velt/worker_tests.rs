use super::*;

fn idle_task() -> Arc<Task> {
    super::super::task::make(async {}, 0, true).0
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
        poll_task(local.me, local, waker);
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
