use super::*;
use std::sync::Mutex as StdMutex;

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
}

/// Await the turns of `timers` (handles) concurrently, polled in the order given; returns the
/// order (indices into `timers`) in which they got their turns.
async fn turns(timers: &[u64], polled: &[usize]) -> Vec<usize> {
    let log = Arc::new(StdMutex::new(Vec::new()));
    let futs = polled.iter().map(|&i| {
        let log = log.clone();
        // SAFETY: the handles outlive the turns.
        let t = unsafe { Turn::new(timers[i] as *mut Handle, None) };
        async move {
            t.await;
            log.lock().unwrap().push(i);
        }
    });
    futures_util::future::join_all(futs).await;
    let order = log.lock().unwrap().clone();
    order
}

/// The next timer, taking its turn (the loop moves on; it stays queued until taken out).
fn step(o: &mut Order, clock: &mut dyn FnMut() -> u64) -> Option<Key> {
    let p = o.peek_cached(None, true, clock);
    let (k, _) = p.next?;
    o.commit(&p, None);
    Some(k)
}

/// The order a timers pass runs `timers` (delay, start) in when every one is due by `now`
/// (one task, Node's lists): indices into `timers`.
fn pass(timers: &[(u64, u64)], now: u64) -> Vec<usize> {
    let mut o = Order::new();
    let keys: Vec<Key> = timers
        .iter()
        .map(|&(ms, start)| o.insert_at(ms, start, None))
        .collect();
    let mut ran = Vec::new();
    let mut clock = || now;
    while let Some(k) = step(&mut o, &mut clock) {
        ran.push(keys.iter().position(|x| *x == k).unwrap());
        assert!(o.take(k));
    }
    assert!(o.is_empty());
    ran
}

/// Node's rule: one list per delay, lists by (expiry, list id), each list's due timers in
/// insertion order before the next list.
#[test]
fn a_pass_runs_node_lists_in_expiry_then_id_order() {
    // `A 10, B 12, busy(5), C 10`, due together: A and C share the 10 list.
    assert_eq!(pass(&[(10, 0), (12, 0), (10, 5)], 45), [0, 2, 1]);
    // `X 4, A 5, busy(3), B 4`.
    assert_eq!(pass(&[(4, 0), (5, 0), (4, 3)], 43), [0, 2, 1]);
    // Equal expiry: the list created first.
    assert_eq!(pass(&[(5, 3), (8, 0), (5, 3)], 20), [0, 2, 1]);
}

/// A partly due list waits for its next timer with a new id: behind a list of the same expiry
/// that was created before the pass.
#[test]
fn a_partly_due_list_gets_a_new_id() {
    let mut o = Order::new();
    let a = o.insert_at(10, 0, None); // 10 list, expiry 10, id 1
    let b = o.insert_at(10, 8, None); // due 18
    let c = o.insert_at(13, 5, None); // 13 list, expiry 18, id 2
    let mut now = 12;
    let mut clock = || now;
    assert_eq!(step(&mut o, &mut clock), Some(a));
    assert!(o.take(a));
    // Nothing else is due by 12; the 10 list now waits for 18 with id 3, after the 13 list.
    assert_eq!(step(&mut o, &mut clock), None);
    now = 30;
    let mut clock = || now;
    assert_eq!(step(&mut o, &mut clock), Some(c));
    assert!(o.take(c));
    assert_eq!(step(&mut o, &mut clock), Some(b));
    assert!(o.take(b));
    assert!(o.is_empty());
}

/// A pass reads `now` once: a timer due later is left for the next pass, even if the clock has
/// moved on meanwhile.
#[test]
fn a_pass_keeps_its_now() {
    let mut o = Order::new();
    let a = o.insert_at(5, 0, None);
    let b = o.insert_at(6, 0, None);
    let mut reads = vec![20, 5];
    let mut clock = || reads.pop().unwrap();
    assert_eq!(step(&mut o, &mut clock), Some(a));
    assert!(o.take(a));
    // The pass at 5 has nothing more: `b` is due at 6, by the next pass (read 20).
    assert_eq!(step(&mut o, &mut clock), Some(b));
    assert!(o.take(b));
    assert!(o.is_empty());
}

// One test: the order is process-wide.
#[test]
fn turns_follow_the_order() {
    rt().block_on(async {
        // Equal deadlines (#1328), polled in reverse.
        let eq: Vec<u64> = (0..5).map(|_| velt_rt_timer_new(3)).collect();
        assert_eq!(turns(&eq, &[4, 3, 2, 1, 0]).await, [0, 1, 2, 3, 4]);

        // By deadline first; a cleared timer does not hold the others.
        let hs = [
            velt_rt_timer_new(6),
            velt_rt_timer_new(2),
            velt_rt_timer_new(4),
            velt_rt_timer_new(2),
            velt_rt_timer_new(1),
        ];
        // SAFETY: a live handle.
        unsafe { velt_rt_timer_clear(hs[4]) };
        assert_eq!(turns(&hs, &[0, 2, 3, 1]).await, [1, 3, 2, 0]);
        // A cleared timer's turn is ready at once.
        assert_eq!(turns(&hs, &[4]).await, [4]);

        // An interval's next run goes behind the timers queued before it: `b` is created before
        // `a` is queued again with the same delay, so `b` (an earlier or equal due time, created
        // first) takes its turn first.
        let a = velt_rt_timer_new(0);
        let b = velt_rt_timer_new(2);
        // SAFETY: a live handle.
        unsafe { velt_rt_timer_again(a, 2) };
        let hs2 = [a, b];
        assert_eq!(turns(&hs2, &[1, 0]).await, [1, 0]);
        // A timer that took its turn is ready again at once.
        assert_eq!(turns(&hs2, &[0]).await, [0]);
        // SAFETY: live handles, not used again.
        unsafe {
            for h in eq.into_iter().chain(hs).chain(hs2) {
                velt_rt_timer_free(h);
            }
        }
        assert!(ORDER.lock().is_empty());

        // A timer whose task is busy (in a poll) does not hold back a later one of another
        // task; it takes its turn once that task is idle again.
        let task = super::super::gate::Gate::new_busy();
        let (key, at) = ORDER.lock().insert(0, Some(task.clone()), None);
        let first = Box::into_raw(Box::new(Handle {
            key: Some(key),
            at,
            task: Some(task.clone()),
            waker: None,
            ran: None,
        })) as u64;
        let second = velt_rt_timer_new(0);
        assert_eq!(turns(&[first, second], &[1]).await, [1]);
        task.leave();
        assert_eq!(turns(&[first, second], &[0]).await, [0]);
        // SAFETY: live handles, not used again.
        unsafe {
            velt_rt_timer_free(first);
            velt_rt_timer_free(second);
        }
        assert!(ORDER.lock().is_empty());
    });
}

/// Immediates run once the timers due by the pass have run, before the next pass: one created
/// by a callback runs before a timer it created, however late the clock reads by then.
#[test]
fn immediates_run_between_passes() {
    let mut o = Order::new();
    let x = o.insert_at(10, 0, None);
    let early = o.insert_at(IMMEDIATE, 3, None);
    let mut now = 10;
    let mut clock = || now;
    assert_eq!(step(&mut o, &mut clock), Some(x));
    assert!(o.take(x));
    // A callback of the pass at 10 creates a timeout, then an immediate.
    let t = o.insert_at(1, 12, None);
    let late = o.insert_at(IMMEDIATE, 13, None);
    now = 20;
    let mut clock = || now;
    assert_eq!(step(&mut o, &mut clock), Some(early));
    assert!(o.take(early));
    assert_eq!(step(&mut o, &mut clock), Some(late));
    assert!(o.take(late));
    assert_eq!(step(&mut o, &mut clock), Some(t));
    assert!(o.take(t));
    assert!(o.is_empty());
}
