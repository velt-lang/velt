//! Interleavings of writers revoking an `RwLock`'s bias, forced with the `Point` hooks.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use super::{velt_rt_rwlock_write, velt_rt_rwlock_write_unlock, Point, PAUSE};
use crate::shared::{
    rebias, velt_rt_rwlock_init, velt_rt_rwlock_read, velt_rt_rwlock_read_unlock, Words,
    RWLOCK_WORDS,
};

/// The lock words, shared by the test's threads (an address, as generated code holds it).
#[derive(Clone, Copy)]
struct Lock(usize);

impl Lock {
    fn new() -> Self {
        let l = Box::leak(Box::new([0u64; RWLOCK_WORDS]));
        unsafe { velt_rt_rwlock_init(l.as_mut_ptr()) };
        Lock(l.as_mut_ptr() as usize)
    }
    fn p(self) -> *mut u64 {
        self.0 as *mut u64
    }
    fn words(self) -> &'static Words {
        unsafe { &*(self.0 as *const Words) }
    }
}

/// A writer on its own thread that stops at the `Point`s in `stops`: it says where (`at`) and
/// waits for `go`. It writes once, noting whether a reader marked `reading` was still reading.
fn writer(
    l: Lock,
    stops: &'static [Point],
    at: Sender<Point>,
    go: Receiver<()>,
    reading: Arc<AtomicBool>,
    overlapped: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        PAUSE.with(|h| {
            *h.borrow_mut() = Some(Box::new(move |p| {
                if stops.contains(&p) {
                    let _ = at.send(p);
                    let _ = go.recv_timeout(Duration::from_secs(10));
                }
            }))
        });
        unsafe {
            let r = velt_rt_rwlock_write(l.p(), 0);
            if reading.load(Ordering::SeqCst) {
                overlapped.store(true, Ordering::SeqCst);
            }
            velt_rt_rwlock_write_unlock(l.p(), r);
        }
    })
}

/// Two writers revoke together; one is held up after waiting for the slots while the other
/// writes, a reader biases the lock again, a new reader enters through its slot and a third
/// writer starts revoking. The held-up writer must not take the third's revocation for its
/// own and write while that reader reads (the state goes 0 → revoking → 0 → revoking: ABA).
#[test]
fn a_held_up_revoker_never_writes_beside_a_slot_reader() {
    let l = Lock::new();
    let reading = Arc::new(AtomicBool::new(false));
    let overlapped = Arc::new(AtomicBool::new(false));
    let (c_at, c_seen) = channel();
    let (c_go, c_wait) = channel();
    let (a_at, a_seen) = channel();
    let (a_go, a_wait) = channel();
    // C: held up once it starts revoking, and once it has waited for the slots.
    let c = writer(
        l,
        &[Point::Revoking, Point::Scanned],
        c_at,
        c_wait,
        reading.clone(),
        overlapped.clone(),
    );
    assert_eq!(c_seen.recv().unwrap(), Point::Revoking);
    // A revokes too and is held up once the slots are clear; then C goes on.
    let a = writer(
        l,
        &[Point::Scanned],
        a_at,
        a_wait,
        reading.clone(),
        overlapped.clone(),
    );
    assert_eq!(a_seen.recv().unwrap(), Point::Scanned);
    c_go.send(()).unwrap();
    let c_scanned = c_seen.recv_timeout(Duration::from_millis(300)).is_ok();
    // A writes; its revocation is over at once.
    a_go.send(()).unwrap();
    a.join().unwrap();
    // A reader holding the lock biases it again.
    let w = l.words();
    w.until.store(0, Ordering::SeqCst);
    let held = unsafe { velt_rt_rwlock_read(l.p(), 0) };
    // (A reader that waited for the lock may have biased it already.)
    rebias(w);
    assert_eq!(w.state.load(Ordering::SeqCst), 0);
    // A reader enters through its slot (when the lock is biased) and stays.
    let (r2_in, r2_entered) = channel();
    let (r2_go, r2_leave) = channel::<()>();
    let r2_reading = reading.clone();
    let r2 = std::thread::spawn(move || unsafe {
        let how = velt_rt_rwlock_read(l.p(), 0);
        r2_reading.store(true, Ordering::SeqCst);
        r2_in.send(()).unwrap();
        let _ = r2_leave.recv_timeout(Duration::from_secs(10));
        r2_reading.store(false, Ordering::SeqCst);
        velt_rt_rwlock_read_unlock(l.p(), how);
    });
    r2_entered.recv().unwrap();
    // D starts revoking and waits for the reader's slot.
    let (d_at, _d_seen) = channel();
    let (_d_go, d_wait) = channel();
    let d = writer(l, &[], d_at, d_wait, reading.clone(), overlapped.clone());
    let t = std::time::Instant::now();
    while w.state.load(Ordering::SeqCst) == 0 && t.elapsed() < Duration::from_secs(5) {
        std::thread::yield_now();
    }
    // C goes on; the first reader leaves, so C could take the lock.
    if c_scanned {
        c_go.send(()).unwrap();
    }
    std::thread::sleep(Duration::from_millis(50));
    unsafe { velt_rt_rwlock_read_unlock(l.p(), held) };
    std::thread::sleep(Duration::from_millis(100));
    r2_go.send(()).unwrap();
    r2.join().unwrap();
    c.join().unwrap();
    d.join().unwrap();
    assert!(
        !overlapped.load(Ordering::SeqCst),
        "a writer wrote while a reader read through its slot"
    );
}
