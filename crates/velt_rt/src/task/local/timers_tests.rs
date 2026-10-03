use super::*;
use std::sync::Mutex as StdMutex;
use std::task::Wake;
use std::time::Duration;

/// A waker that records its name when woken.
struct Named(&'static str, Arc<StdMutex<Vec<&'static str>>>);

impl Wake for Named {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.1.lock().unwrap().push(self.0);
    }
}

fn named(name: &'static str, log: &Arc<StdMutex<Vec<&'static str>>>) -> Waker {
    Waker::from(Arc::new(Named(name, log.clone())))
}

/// Register a leaf directly (the tests have no task context).
fn leaf(timers: &Arc<Timers>, deadline: Instant, waker: &Waker, task: &Waker) -> Box<TimerLeaf> {
    let mut l = Box::new(TimerLeaf {
        deadline,
        seq: timers.next_seq(),
        fired: AtomicBool::new(false),
        registered: None,
        fallback: None,
    });
    let key = (deadline, l.seq, &l.fired as *const AtomicBool as usize);
    timers.register(key, waker, &l.fired, task);
    l.registered = Some((timers.clone(), key));
    l
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
}

#[test]
fn same_deadline_fires_in_creation_order() {
    rt().block_on(async {
        let log = Arc::new(StdMutex::new(Vec::new()));
        let task = named("task", &log);
        let timers = Arc::new(Timers::default());
        let at = Instant::now() + Duration::from_millis(5);
        let names = ["a", "b", "c", "d", "e"];
        let leaves: Vec<_> = names
            .iter()
            .map(|n| leaf(&timers, at, &named(n, &log), &task))
            .collect();
        tokio::time::sleep(Duration::from_millis(5)).await;
        log.lock().unwrap().clear();
        assert_eq!(timers.fire(&task), None);
        assert_eq!(*log.lock().unwrap(), names);
        assert!(leaves.iter().all(|l| l.fired.load(Ordering::Acquire)));
    });
}

#[test]
fn earlier_deadline_first_then_creation_order() {
    rt().block_on(async {
        let log = Arc::new(StdMutex::new(Vec::new()));
        let task = named("task", &log);
        let timers = Arc::new(Timers::default());
        let now = Instant::now();
        let _late = leaf(
            &timers,
            now + Duration::from_millis(3),
            &named("late", &log),
            &task,
        );
        let _early = leaf(
            &timers,
            now + Duration::from_millis(2),
            &named("early", &log),
            &task,
        );
        let _late2 = leaf(
            &timers,
            now + Duration::from_millis(3),
            &named("late2", &log),
            &task,
        );
        let _never = leaf(
            &timers,
            now + Duration::from_secs(9),
            &named("never", &log),
            &task,
        );
        tokio::time::sleep(Duration::from_millis(3)).await;
        log.lock().unwrap().clear();
        timers.fire(&task);
        assert_eq!(*log.lock().unwrap(), ["early", "late", "late2"]);
    });
}

#[test]
fn reports_where_the_root_resumes() {
    rt().block_on(async {
        let log = Arc::new(StdMutex::new(Vec::new()));
        let task = named("task", &log);
        let timers = Arc::new(Timers::default());
        let at = Instant::now() + Duration::from_millis(1);
        let _a = leaf(&timers, at, &named("a", &log), &task);
        let _root = leaf(&timers, at, &task, &task);
        let _b = leaf(&timers, at, &named("b", &log), &task);
        let _root2 = leaf(&timers, at, &task, &task);
        tokio::time::sleep(Duration::from_millis(1)).await;
        log.lock().unwrap().clear();
        // One promise runs before the root; the root itself is not woken.
        assert_eq!(timers.fire(&task), Some(1));
        assert_eq!(*log.lock().unwrap(), ["a", "b"]);
    });
}

#[test]
fn a_dropped_leaf_is_not_fired() {
    rt().block_on(async {
        let log = Arc::new(StdMutex::new(Vec::new()));
        let task = named("task", &log);
        let timers = Arc::new(Timers::default());
        let at = Instant::now() + Duration::from_millis(1);
        let a = leaf(&timers, at, &named("a", &log), &task);
        let _b = leaf(&timers, at, &named("b", &log), &task);
        drop(a);
        tokio::time::sleep(Duration::from_millis(1)).await;
        log.lock().unwrap().clear();
        timers.fire(&task);
        assert_eq!(*log.lock().unwrap(), ["b"]);
        assert!(timers.state.lock().queue.is_empty());
    });
}

#[test]
fn arms_one_tokio_timer_for_the_earliest_deadline() {
    rt().block_on(async {
        let log = Arc::new(StdMutex::new(Vec::new()));
        let task = named("task", &log);
        let timers = Arc::new(Timers::default());
        let now = Instant::now();
        let _a = leaf(
            &timers,
            now + Duration::from_secs(60),
            &named("a", &log),
            &task,
        );
        assert_eq!(
            timers.state.lock().armed.as_ref().map(|a| a.1),
            Some(now + Duration::from_secs(60))
        );
        let _b = leaf(
            &timers,
            now + Duration::from_millis(4),
            &named("b", &log),
            &task,
        );
        assert_eq!(
            timers.state.lock().armed.as_ref().map(|a| a.1),
            Some(now + Duration::from_millis(4))
        );
        // The tokio timer wakes the task at the earliest deadline.
        while log.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert_eq!(*log.lock().unwrap(), ["task"]);
        log.lock().unwrap().clear();
        timers.fire(&task);
        assert_eq!(*log.lock().unwrap(), ["b"]);
        // ...and is re-armed for the next one.
        assert_eq!(
            timers.state.lock().armed.as_ref().map(|a| a.1),
            Some(now + Duration::from_secs(60))
        );
    });
}

#[test]
fn sequences_are_per_task() {
    let a = Timers::default();
    let b = Timers::default();
    assert_eq!((a.next_seq(), a.next_seq(), b.next_seq()), (1, 2, 1));
}

#[test]
fn the_queue_merges_appended_and_out_of_order_keys_and_skips_removed_ones() {
    let base = Instant::now();
    let flag = AtomicBool::new(false);
    let reg = || Registered {
        waker: Waker::noop().clone(),
        fired: &flag,
    };
    let key = |ms: u64, seq: u64| (base + Duration::from_millis(ms), seq, 0);
    let mut q = Queue::default();
    for (ms, seq) in [(5, 1), (5, 2), (7, 3), (3, 4), (6, 5), (9, 6)] {
        q.insert(key(ms, seq), reg());
    }
    // In order: appended to the deque; earlier than its last key: into the tree.
    assert_eq!((q.tail.len(), q.rest.len()), (4, 2));
    assert!(q.get_mut(&key(5, 2)).is_some() && q.get_mut(&key(3, 4)).is_some());
    assert!(q.remove(&key(5, 1)).is_some());
    assert!(q.remove(&key(5, 1)).is_none(), "removed once");
    assert!(q.remove(&key(6, 5)).is_some());
    let mut order = Vec::new();
    while let Some(k) = q.first_key() {
        q.pop_first(&k);
        order.push(k);
    }
    assert_eq!(order, [key(3, 4), key(5, 2), key(7, 3), key(9, 6)]);
    assert!(q.is_empty() && q.tail.is_empty() && q.rest.is_empty());
}
