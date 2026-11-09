//! The waker of `cx` (waker.rs): Rust wakers through the C table, C headers through Rust.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::Wake;

/// A test task header: a static C table first, then an atomic reference count and what the
/// table's functions saw. A clone returns the same header with one more reference.
#[repr(C)]
pub(crate) struct TestHeader {
    vt: *const VeltWakerVt,
    refs: AtomicUsize,
    clones: AtomicUsize,
    wakes: AtomicUsize,
    by_ref: AtomicUsize,
    drops: AtomicUsize,
}

// SAFETY: the counts are atomic; the table is static.
unsafe impl Send for TestHeader {}
// SAFETY: as above.
unsafe impl Sync for TestHeader {}

impl TestHeader {
    /// A header with one reference (the executor's).
    pub(crate) fn new() -> TestHeader {
        TestHeader {
            vt: &COUNTING,
            refs: AtomicUsize::new(1),
            clones: AtomicUsize::new(0),
            wakes: AtomicUsize::new(0),
            by_ref: AtomicUsize::new(0),
            drops: AtomicUsize::new(0),
        }
    }

    /// The waker pointer for `cx`.
    pub(crate) fn ptr(&self) -> *const VeltWakerHeader {
        self as *const TestHeader as *const VeltWakerHeader
    }

    /// `cx` with this waker.
    pub(crate) fn cx(&self, await_word: *const u32) -> VeltCx {
        VeltCx {
            await_word,
            waker: self.ptr(),
        }
    }

    /// clones, wakes, wakes by reference, drops.
    fn get(&self) -> [usize; 4] {
        [&self.clones, &self.wakes, &self.by_ref, &self.drops].map(|c| c.load(Ordering::SeqCst))
    }

    /// Every reference a clone took was given back (by a wake or a drop).
    pub(crate) fn balanced(&self) -> bool {
        self.refs.load(Ordering::SeqCst) == 1
    }

    /// Wake-ups of any kind.
    pub(crate) fn woken(&self) -> usize {
        self.wakes.load(Ordering::SeqCst) + self.by_ref.load(Ordering::SeqCst)
    }
}

unsafe fn test<'a>(h: *const VeltWakerHeader) -> &'a TestHeader {
    &*(h as *const TestHeader)
}

unsafe extern "C" fn c_clone(h: *const VeltWakerHeader) -> *const VeltWakerHeader {
    test(h).clones.fetch_add(1, Ordering::SeqCst);
    test(h).refs.fetch_add(1, Ordering::SeqCst);
    h
}
unsafe extern "C" fn c_wake(h: *const VeltWakerHeader) {
    test(h).wakes.fetch_add(1, Ordering::SeqCst);
    test(h).refs.fetch_sub(1, Ordering::SeqCst);
}
unsafe extern "C" fn c_wake_by_ref(h: *const VeltWakerHeader) {
    test(h).by_ref.fetch_add(1, Ordering::SeqCst);
}
unsafe extern "C" fn c_drop(h: *const VeltWakerHeader) {
    test(h).drops.fetch_add(1, Ordering::SeqCst);
    test(h).refs.fetch_sub(1, Ordering::SeqCst);
}

/// A foreign table, as an executor in Velt would have.
static COUNTING: VeltWakerVt = VeltWakerVt {
    clone: c_clone,
    wake: c_wake,
    wake_by_ref: c_wake_by_ref,
    drop: c_drop,
};

#[test]
fn a_foreign_waker_wakes_clones_and_drops_through_its_table() {
    let t = TestHeader::new();
    let mut cx = t.cx(std::ptr::null());
    let cx = &mut cx as *mut VeltCx as *mut c_void;
    // SAFETY: `cx` is live for the call.
    unsafe {
        with_context(cx, |c| {
            c.waker().wake_by_ref();
            let owned = c.waker().clone();
            assert_eq!(
                owned.data(),
                t.ptr() as *const (),
                "the header itself, not a copy"
            );
            owned.wake_by_ref();
            drop(owned.clone());
            assert_eq!(t.refs.load(Ordering::SeqCst), 2);
            owned.wake();
        });
    }
    assert_eq!(t.get(), [2, 1, 2, 1]);
    assert!(t.balanced());
}

#[test]
fn a_foreign_waker_will_wake_its_clone_from_an_earlier_poll() {
    let t = TestHeader::new();
    let mut cx = t.cx(std::ptr::null());
    let cx = &mut cx as *mut VeltCx as *mut c_void;
    // SAFETY: `cx` is live for the calls.
    let kept = unsafe { with_context(cx, |c| c.waker().clone()) };
    assert!(unsafe { with_context(cx, |c| c.waker().will_wake(&kept)) });
    drop(kept);
    assert!(t.balanced());
}

#[test]
fn a_foreign_waker_wakes_from_another_thread() {
    let t = Arc::new(TestHeader::new());
    let mut cx = t.cx(std::ptr::null());
    let cx = &mut cx as *mut VeltCx as *mut c_void;
    // SAFETY: `cx` is live for the call; `t` outlives the thread.
    let w = unsafe { with_context(cx, |c| c.waker().clone()) };
    std::thread::spawn(move || w.wake()).join().unwrap();
    assert_eq!(t.get(), [1, 1, 0, 0]);
    assert!(t.balanced());
}

/// A Rust waker counting its wake-ups.
#[derive(Default)]
struct RustCount(AtomicUsize);

impl Wake for RustCount {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn a_rust_executors_context_is_used_as_it_is() {
    let a = Arc::new(RustCount::default());
    let waker = Waker::from(a.clone());
    let mut rust_cx = Context::from_waker(&waker);
    let at = &rust_cx as *const Context<'_> as usize;
    let cx = RustCx::new(std::ptr::null(), &mut rust_cx);
    assert_eq!(
        std::mem::size_of::<VeltCx>(),
        2 * std::mem::size_of::<usize>()
    );
    // SAFETY: `cx` is live for the calls.
    unsafe {
        with_context(cx.ptr(), |c| {
            assert_eq!(
                c as *const Context<'_> as usize, at,
                "the executor's own Context"
            );
        });
        assert!(std::ptr::eq(rust_waker(cx.ptr()).unwrap(), &waker));
        with_rust_cx(cx.ptr(), |p| assert_eq!(p, cx.ptr()));
    }
    assert_eq!(Arc::strong_count(&a), 2);
}

#[test]
fn a_rust_waker_works_through_the_c_table() {
    let a = Arc::new(RustCount::default());
    let waker = Waker::from(a.clone());
    let mut rust_cx = Context::from_waker(&waker);
    let cx = RustCx::new(std::ptr::null(), &mut rust_cx);
    // SAFETY: what C code may do with a borrowed waker and its clones.
    unsafe {
        let h = header(cx.ptr());
        ((*(*h).vt).wake_by_ref)(h);
        let owned = ((*(*h).vt).clone)(h);
        assert_eq!(Arc::strong_count(&a), 3);
        ((*(*owned).vt).wake_by_ref)(owned);
        let other = ((*(*owned).vt).clone)(owned);
        // An owned header passed on as a `cx` gives the leaf its `Waker`, not a forwarder.
        let mut ocx = VeltCx {
            await_word: std::ptr::null(),
            waker: other,
        };
        with_context(&mut ocx as *mut VeltCx as *mut c_void, |c| {
            assert!(c.waker().will_wake(&waker));
        });
        ((*(*other).vt).drop)(other);
        ((*(*owned).vt).wake)(owned);
    }
    assert_eq!(a.0.load(Ordering::SeqCst), 3);
    assert_eq!(Arc::strong_count(&a), 2);
}

#[test]
fn a_foreign_cx_becomes_a_rust_one_with_the_same_word() {
    let t = TestHeader::new();
    let word = 7u32;
    let mut cx = t.cx(&word);
    let cx = &mut cx as *mut VeltCx as *mut c_void;
    // SAFETY: `cx` is live for the call.
    unsafe {
        with_rust_cx(cx, |p| {
            assert_ne!(p, cx);
            assert_eq!(*(*(p as *const VeltCx)).await_word, 7);
            rust_waker(p).unwrap().wake_by_ref();
            drop(rust_waker(p).unwrap().clone());
        });
    }
    assert_eq!(t.get(), [1, 0, 1, 1]);
    assert!(t.balanced());
}
