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

// One test: the order is process-wide.
#[test]
fn turns_follow_deadline_then_creation_order() {
    rt().block_on(async {
        // Equal deadlines (#1328), polled in reverse.
        let eq: Vec<u64> = (0..5).map(|_| velt_rt_timer_new(3)).collect();
        assert_eq!(turns(&eq, &[4, 3, 2, 1, 0]).await, [0, 1, 2, 3, 4]);

        // By deadline first, then creation; a cleared (freed) timer does not hold the others.
        let hs = [
            velt_rt_timer_new(6),
            velt_rt_timer_new(2),
            velt_rt_timer_new(4),
            velt_rt_timer_new(2),
            velt_rt_timer_new(1),
        ];
        // SAFETY: a live handle, not used again.
        unsafe { velt_rt_timer_free(hs[4]) };
        assert_eq!(turns(&hs, &[0, 2, 3, 1]).await, [1, 3, 2, 0]);

        // An interval's next run goes behind the timers queued before it.
        let a = velt_rt_timer_new(0);
        // SAFETY: live handles.
        unsafe { velt_rt_timer_again(a, 2) };
        let b = velt_rt_timer_new(2);
        let hs2 = [a, b];
        assert_eq!(turns(&hs2, &[1, 0]).await, [1, 0]);
        // A timer that took its turn is ready again at once.
        assert_eq!(turns(&hs2, &[0]).await, [0]);
        // SAFETY: live handles, not used again.
        unsafe {
            for h in eq.into_iter().chain(hs.into_iter().take(4)).chain(hs2) {
                velt_rt_timer_free(h);
            }
        }
        assert!(ORDER.lock().queue.is_empty());
    });
}
