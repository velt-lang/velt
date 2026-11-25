//! Shared promises (`velt_rt_fut_share`, rt_abi_async.md §1.2): holders of one promise awaited
//! and dropped in different orders, with a heap result that every holder gets its own share of.
//! The same sequences run again in a child process under the checking allocator
//! (`VELT_RT_DEBUG_ALLOC=1`), which aborts on a free of a block it did not hand out, a double
//! free or a use after free.

use super::fake::{block_on_state, yield_once};
use crate::task::shared::velt_rt_fut_share;
use crate::task::{drop_fut, poll_fut, VeltFut, FUT_RESULT_OFFSET, PENDING, READY};
use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Mutex;

/// Result boxes alive (made by the promise or by the share glue, minus those dropped).
static LIVE: AtomicIsize = AtomicIsize::new(0);
/// The tests count `LIVE`, so they run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn new_box(v: i64) -> *mut i64 {
    LIVE.fetch_add(1, Ordering::SeqCst);
    Box::into_raw(Box::new(v))
}

unsafe fn free_box(b: *mut i64) {
    LIVE.fetch_sub(1, Ordering::SeqCst);
    drop(Box::from_raw(b));
}

/// Share glue of the result slot: another reference to the result (here a copy of the box).
unsafe extern "C-unwind" fn share_result(src: *const u8, dst: *mut u8) {
    let b = *(src as *const *mut i64);
    (dst as *mut *mut i64).write(new_box(*b));
}

/// Drop glue of the result slot.
unsafe extern "C-unwind" fn drop_result(slot: *mut u8) {
    free_box(*(slot as *const *mut i64));
}

// A runtime future that yields once, then finishes with `new Box(v)`: its header, its result
// slot at +16, its state.
#[repr(C)]
struct Value {
    hdr: VeltFut,
    result: *mut i64,
    tag: u32,
    v: i64,
    y: *mut VeltFut,
}

const _: () = assert!(std::mem::offset_of!(Value, result) == FUT_RESULT_OFFSET);

unsafe extern "C-unwind" fn value_poll(f: *mut VeltFut, cx: *mut c_void) -> u32 {
    let st = &mut *(f as *mut Value);
    if st.tag == 0 {
        st.y = yield_once();
        st.tag = 1;
    }
    if poll_fut(st.y, cx) == PENDING {
        return PENDING;
    }
    drop_fut(st.y);
    st.y = null_mut();
    st.result = new_box(st.v);
    st.tag = 2;
    READY
}

/// Frees the future; never its result (whoever saw `READY` moved it out).
unsafe extern "C-unwind" fn value_drop(f: *mut VeltFut) {
    let st = Box::from_raw(f as *mut Value);
    if !st.y.is_null() {
        drop_fut(st.y);
    }
}

/// `const p = value(v);`: a promise that runs when it is awaited.
fn value(v: i64) -> *mut VeltFut {
    Box::into_raw(Box::new(Value {
        hdr: VeltFut {
            poll: value_poll,
            drop: value_drop,
        },
        result: null_mut(),
        tag: 0,
        v,
        y: null_mut(),
    })) as *mut VeltFut
}

unsafe fn share(f: *mut VeltFut) -> *mut VeltFut {
    velt_rt_fut_share(f, 8, Some(share_result), Some(drop_result))
}

/// Poll holder `f`; when it is ready, take its result (and free it) and drop the holder.
unsafe fn take(f: &mut *mut VeltFut, cx: *mut c_void) -> Option<i64> {
    if poll_fut(*f, cx) == PENDING {
        return None;
    }
    let b = *((*f as *const u8).add(FUT_RESULT_OFFSET) as *const *mut i64);
    let v = *b;
    free_box(b);
    drop_fut(*f);
    *f = null_mut();
    Some(v)
}

// async function main() {
//   const p = value(7); const q = p; const r = q;   // three holders
//   const a = await q; const b = await p;            // the second holder drives
//   r is dropped unawaited; return a * 10 + b; }
#[repr(C)]
struct AwaitBoth {
    result: i64,
    tag: u32,
    p: *mut VeltFut,
    q: *mut VeltFut,
    r: *mut VeltFut,
    a: i64,
}

unsafe extern "C-unwind" fn await_both_poll(s: *mut u8, cx: *mut c_void) -> u32 {
    let st = &mut *(s as *mut AwaitBoth);
    if st.tag == 0 {
        st.p = value(7);
        st.q = share(st.p);
        st.r = share(st.q);
        st.tag = 1;
    }
    if st.tag == 1 {
        let Some(a) = take(&mut st.q, cx) else {
            return PENDING;
        };
        st.a = a;
        st.tag = 2;
    }
    let Some(b) = take(&mut st.p, cx) else {
        return PENDING;
    };
    drop_fut(st.r);
    st.result = st.a * 10 + b;
    READY
}

fn await_both() {
    let mut st = AwaitBoth {
        result: 0,
        tag: 0,
        p: null_mut(),
        q: null_mut(),
        r: null_mut(),
        a: 0,
    };
    unsafe { block_on_state(await_both_poll, &mut st as *mut AwaitBoth as *mut u8) };
    assert_eq!(st.result, 77);
}

// async function main() { const p = value(5); const q = p; await yieldNow(); }
// The second holder starts waiting (it drives the promise to its first suspension), then both
// holders are dropped before the promise settles, the original first: the last one gives the
// promise back its own drop, which cancels it mid-way, and no result is ever made.
#[repr(C)]
struct DropUnsettled {
    tag: u32,
    y: *mut VeltFut,
}

unsafe extern "C-unwind" fn drop_unsettled_poll(s: *mut u8, cx: *mut c_void) -> u32 {
    let st = &mut *(s as *mut DropUnsettled);
    if st.tag == 0 {
        let p = value(5);
        let q = share(p);
        assert_eq!(poll_fut(q, cx), PENDING);
        drop_fut(p);
        drop_fut(q);
        st.y = yield_once();
        st.tag = 1;
    }
    if poll_fut(st.y, cx) == PENDING {
        return PENDING;
    }
    drop_fut(st.y);
    READY
}

fn drop_unsettled() {
    let mut st = DropUnsettled {
        tag: 0,
        y: null_mut(),
    };
    unsafe {
        block_on_state(
            drop_unsettled_poll,
            &mut st as *mut DropUnsettled as *mut u8,
        )
    };
}

fn run_all() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let before = LIVE.load(Ordering::SeqCst);
    await_both();
    assert_eq!(LIVE.load(Ordering::SeqCst), before, "a result box leaked");
    drop_unsettled();
    assert_eq!(LIVE.load(Ordering::SeqCst), before, "a result box leaked");
}

#[test]
fn holders_await_and_drop_in_any_order() {
    run_all();
}

/// Set in the child process of [`holders_under_the_checking_allocator`].
#[cfg(debug_assertions)]
const CHILD: &str = "VELT_RT_SHARED_PROMISE_CHILD";

/// The same sequences with `VELT_RT_DEBUG_ALLOC=1`: every handle and result is allocated and
/// freed with matching sizes, once.
#[cfg(debug_assertions)]
#[test]
fn holders_under_the_checking_allocator() {
    if std::env::var_os(CHILD).is_some() {
        run_all();
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    let out = super::command::command(exe)
        .args([
            "--exact",
            "abi_tests::shared::holders_under_the_checking_allocator",
        ])
        .env(CHILD, "1")
        .env("VELT_RT_DEBUG_ALLOC", "1")
        .output()
        .expect("run the child");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("1 passed"),
        "child: {}\n{stdout}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}
