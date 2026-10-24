//! The futures a native library creates through the table: blocking work on the runtime's pool
//! ([`fut_blocking`]) and futures completed from any thread ([`fut_completer`]).

use std::alloc::Layout;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Arc, Mutex, OnceLock};
use std::task::Waker;

use super::{DropCtxFn, DropResultFn, WorkFn};
use crate::task::{context, VeltFut, FUT_RESULT_OFFSET, PENDING, READY};

/// Result bytes, 16-aligned (results have align <= 8; the slot itself is at a 16-byte offset).
type Buf = Vec<u128>;

fn buf(size: usize) -> Buf {
    vec![0u128; size.div_ceil(16).max(1)]
}

enum Slot {
    /// Not finished; the waker of the last poll.
    Pending(Option<Waker>),
    /// Finished, result not yet moved into the future's slot.
    Ready(Buf),
    /// The result was moved out (the future completed).
    Taken,
    /// The future was dropped before the result arrived.
    Cancelled,
}

/// Shared between a native future and whoever completes it (the blocking task or a completion
/// handle).
pub(super) struct Shared {
    slot: Mutex<Slot>,
    size: usize,
    drop_result: Option<DropResultFn>,
}

// SAFETY: `drop_result` is a plain function pointer into a native library that is never unloaded;
// result bytes are owned values the contract allows to move between threads.
unsafe impl Send for Shared {}
// SAFETY: as above; all mutable state is behind the mutex.
unsafe impl Sync for Shared {}

impl Shared {
    /// Deliver the result (`size` bytes at `src`, moved). After cancellation it is dropped.
    unsafe fn complete(&self, src: *const u8) {
        let mut b = buf(self.size);
        std::ptr::copy_nonoverlapping(src, b.as_mut_ptr() as *mut u8, self.size);
        let mut slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        match std::mem::replace(&mut *slot, Slot::Taken) {
            Slot::Pending(waker) => {
                *slot = Slot::Ready(b);
                drop(slot);
                if let Some(w) = waker {
                    w.wake();
                }
            }
            Slot::Cancelled => {
                *slot = Slot::Cancelled;
                drop(slot);
                if let Some(d) = self.drop_result {
                    d(b.as_mut_ptr() as *mut u8);
                }
            }
            Slot::Ready(_) | Slot::Taken => crate::panic::fatal("native future completed twice"),
        }
    }
}

/// The blocking work of a [`fut_blocking`] future, until its first poll hands it to the pool.
struct Work {
    work: WorkFn,
    ctx: *mut c_void,
    drop_ctx: DropCtxFn,
}

// SAFETY: the contract requires `ctx` to be movable to the blocking pool's threads.
unsafe impl Send for Work {}

impl Drop for Work {
    fn drop(&mut self) {
        if !self.ctx.is_null() {
            // SAFETY: the work never ran, so the context is still owned here.
            unsafe { (self.drop_ctx)(self.ctx) };
        }
    }
}

/// Lives in front of the `VeltFut` header: `[Meta][VeltFut][result: size bytes]`.
#[repr(C, align(16))]
struct Meta {
    shared: Arc<Shared>,
    start: Option<Work>,
    layout: Layout,
    /// While it waits: native work keeps the process alive, as I/O does (task/quiet.rs).
    pending: Option<crate::task::quiet::Pending>,
}

const META: usize = std::mem::size_of::<Meta>();
const _: () = assert!(META.is_multiple_of(16));

unsafe fn meta<'a>(f: *mut VeltFut) -> &'a mut Meta {
    &mut *((f as *mut u8).sub(META) as *mut Meta)
}

fn new_fut(shared: Arc<Shared>, start: Option<Work>) -> *mut VeltFut {
    let size = META + FUT_RESULT_OFFSET + shared.size;
    let layout = Layout::from_size_align(size, 16).expect("ICE: native future layout");
    // SAFETY: non-zero size; every field is initialized below before use.
    unsafe {
        let base = std::alloc::alloc(layout);
        if base.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        (base as *mut Meta).write(Meta {
            shared,
            start,
            layout,
            pending: None,
        });
        let f = base.add(META) as *mut VeltFut;
        f.write(VeltFut {
            poll: native_poll,
            drop: native_drop,
        });
        f
    }
}

pub(super) unsafe extern "C" fn native_poll(f: *mut VeltFut, cx: *mut c_void) -> u32 {
    let m = meta(f);
    if let Some(mut work) = m.start.take() {
        let shared = m.shared.clone();
        let size = shared.size;
        let ctx = std::mem::replace(&mut work.ctx, std::ptr::null_mut());
        let (run, ctx) = (work.work, crate::task::SendPtr(ctx));
        drop(work);
        crate::task::runtime::spawn_blocking(move || {
            let ctx = ctx;
            let mut b = buf(size);
            // SAFETY: the library's work function fills `size` bytes and consumes `ctx`.
            unsafe {
                run(ctx.0, b.as_mut_ptr() as *mut u8);
                shared.complete(b.as_ptr() as *const u8);
            }
        });
    }
    let mut slot = m.shared.slot.lock().unwrap_or_else(|e| e.into_inner());
    match &mut *slot {
        Slot::Pending(waker) => {
            let w = context(cx).waker();
            if !waker.as_ref().is_some_and(|old| old.will_wake(w)) {
                *waker = Some(w.clone());
            }
            if m.pending.is_none() {
                m.pending = Some(crate::task::quiet::Pending::new());
            }
            PENDING
        }
        Slot::Ready(_) => {
            let Slot::Ready(b) = std::mem::replace(&mut *slot, Slot::Taken) else {
                unreachable!()
            };
            let dst = (f as *mut u8).add(FUT_RESULT_OFFSET);
            std::ptr::copy_nonoverlapping(b.as_ptr() as *const u8, dst, m.shared.size);
            m.pending = None;
            READY
        }
        Slot::Taken => READY,
        Slot::Cancelled => crate::panic::fatal("ICE: polled a dropped native future"),
    }
}

pub(super) unsafe extern "C" fn native_drop(f: *mut VeltFut) {
    let base = (f as *mut u8).sub(META) as *mut Meta;
    let m = base.read();
    drop(m.start); // never polled: its context is freed by `Work::drop`
    let mut slot = m.shared.slot.lock().unwrap_or_else(|e| e.into_inner());
    match std::mem::replace(&mut *slot, Slot::Cancelled) {
        Slot::Ready(mut b) => {
            drop(slot);
            if let Some(d) = m.shared.drop_result {
                d(b.as_mut_ptr() as *mut u8);
            }
        }
        Slot::Taken => *slot = Slot::Taken,
        Slot::Pending(_) | Slot::Cancelled => {}
    }
    std::alloc::dealloc(base as *mut u8, m.layout);
}

fn shared(result_size: usize, drop_result: Option<DropResultFn>) -> Arc<Shared> {
    Arc::new(Shared {
        slot: Mutex::new(Slot::Pending(None)),
        size: result_size,
        drop_result,
    })
}

/// `VeltFut* fut_blocking(work, ctx, drop_ctx, result_size, drop_result)`: a future whose first
/// poll runs `work(ctx, out)` on the runtime's blocking pool; `work` writes `result_size` bytes to
/// `out` (and owns `ctx` from then on). Dropped before its first poll, `drop_ctx(ctx)` runs
/// instead; dropped while `work` runs, the finished result goes to `drop_result` (if any).
pub unsafe extern "C" fn fut_blocking(
    work: WorkFn,
    ctx: *mut c_void,
    drop_ctx: DropCtxFn,
    result_size: usize,
    drop_result: Option<DropResultFn>,
) -> *mut VeltFut {
    let start = Work {
        work,
        ctx,
        drop_ctx,
    };
    new_fut(shared(result_size, drop_result), Some(start))
}

/// Completion handles: ids (never reused) of futures waiting for `complete`. A handle is removed
/// by its first `complete`, so a second one, or an id that was never handed out, finds nothing
/// instead of freed memory.
type Completers = Mutex<(u64, HashMap<u64, Arc<Shared>>)>;

fn completers() -> &'static Completers {
    static C: OnceLock<Completers> = OnceLock::new();
    C.get_or_init(|| Mutex::new((0, HashMap::new())))
}

pub(super) fn take_completer(handle: u64) -> Option<Arc<Shared>> {
    let mut c = completers().lock().unwrap_or_else(|e| e.into_inner());
    c.1.remove(&handle)
}

/// `VeltFut* fut_completer(result_size, drop_result, u64* handle)`: a pending future completed by
/// one later `complete(*handle, result)` from any thread (e.g. the library's own async runtime).
pub unsafe extern "C" fn fut_completer(
    result_size: usize,
    drop_result: Option<DropResultFn>,
    handle: *mut u64,
) -> *mut VeltFut {
    let s = shared(result_size, drop_result);
    let mut c = completers().lock().unwrap_or_else(|e| e.into_inner());
    c.0 += 1;
    let id = c.0;
    c.1.insert(id, s.clone());
    drop(c);
    handle.write(id);
    new_fut(s, None)
}

/// `void complete(u64 handle, const void* result)`: moves `result_size` bytes into the future of
/// `handle` (exactly once per handle); if that future was dropped, `drop_result` gets them.
/// A handle that was already completed (or never handed out) is a fatal error of the library, not
/// memory corruption.
pub unsafe extern "C" fn complete(handle: u64, result: *const u8) {
    match take_completer(handle) {
        Some(s) => s.complete(result),
        None => crate::panic::fatal(
            "a native library completed a future twice, or with an unknown handle",
        ),
    }
}
