//! The `cx` of every poll call in C terms (rt_abi_async.md §1): the task's await word and its
//! waker, one pointer to a header whose first word is a table of C functions (the layout of
//! tokio's task headers), so an executor written in Velt (#2649) can make one without Rust.
//! Compiled into velt_rt_wasm too (`#[path]`).
//!
//! * **Rust executors** (the velt and tokio backends, velt_rt_wasm) pass `raw_cx`'s
//!   [`RustCx`]: the header sits in the same stack value, with the table `RUST_VT` and the
//!   executor's `&mut Context` after it. A runtime leaf recognizes the table and uses that
//!   `Context` as it is ([`with_context`]): nothing built. Called from C, the table's `clone`
//!   returns a boxed header owning a clone of the Rust `Waker` (table `RUST_OWNED_VT`).
//! * **Foreign tables** (an executor in Velt, a C library's completion): a leaf's Rust waker is
//!   `RawWaker::new(header, &FORWARD)`, whose entries read the C table from the header's first
//!   word. Borrowing, cloning, waking and dropping allocate nothing, and a Rust clone owns
//!   exactly the reference the C `clone` returned (its `wake` or `drop` gives it back).
//!
//! The contract a table keeps is in rt_abi_async.md §1 (any thread, atomic count, harmless late
//! wakes).

use std::ffi::c_void;
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::task::{Context, RawWaker, RawWakerVTable, Waker};

/// What a waker pointer points to: a block whose first word is its table (a task header, a
/// library's waker). The rest of the block is the table's business.
#[repr(C)]
pub struct VeltWakerHeader {
    pub vt: *const VeltWakerVt,
}

/// A waker's functions, each given the header. `clone` returns a header holding one more
/// reference (usually the same pointer); `wake` wakes and gives a reference back; `wake_by_ref`
/// wakes and keeps it; `drop` gives it back.
#[repr(C)]
pub struct VeltWakerVt {
    pub clone: unsafe extern "C" fn(w: *const VeltWakerHeader) -> *const VeltWakerHeader,
    pub wake: unsafe extern "C" fn(w: *const VeltWakerHeader),
    pub wake_by_ref: unsafe extern "C" fn(w: *const VeltWakerHeader),
    pub drop: unsafe extern "C" fn(w: *const VeltWakerHeader),
}

/// What `cx` points to (16 bytes): the task's await word first (generated code reads it at
/// every `await`: 0 = no other job could observe a microtask step, go on), then the task's
/// waker, borrowed for the poll.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VeltCx {
    pub await_word: *const u32,
    pub waker: *const VeltWakerHeader,
}

/// The header a Rust executor's `cx` points to: `RUST_VT` and the executor's `Context`.
#[repr(C)]
struct RustHeader {
    vt: *const VeltWakerVt,
    cx: *mut c_void,
}

/// A `cx` for a Rust executor's `Context`, with its header beside it (valid while this value
/// lives, at the address [`RustCx::ptr`] is taken at).
#[repr(C)]
pub struct RustCx<'a> {
    cx: std::cell::Cell<VeltCx>,
    header: RustHeader,
    _life: PhantomData<&'a mut Context<'a>>,
}

impl<'a> RustCx<'a> {
    /// `cx` for `cx`, borrowed while this value lives.
    #[inline(always)]
    pub fn new(await_word: *const u32, cx: &'a mut Context<'_>) -> RustCx<'a> {
        RustCx {
            cx: std::cell::Cell::new(VeltCx {
                await_word,
                waker: std::ptr::null(),
            }),
            header: RustHeader {
                vt: &RUST_VT,
                cx: cx as *mut Context<'_> as *mut c_void,
            },
            _life: PhantomData,
        }
    }

    /// The opaque pointer (valid while this value lives and stays where it is: pass
    /// `raw_cx(cx).ptr()` as an argument, the temporary lives until the end of the statement).
    #[inline(always)]
    pub fn ptr(&self) -> *mut c_void {
        let mut cx = self.cx.get();
        cx.waker = &self.header as *const RustHeader as *const VeltWakerHeader;
        self.cx.set(cx);
        self.cx.as_ptr() as *mut c_void
    }
}

/// The task's waker in `cx`.
#[inline(always)]
unsafe fn header(cx: *mut c_void) -> *const VeltWakerHeader {
    (*(cx as *const VeltCx)).waker
}

/// Runs `f` with a Rust `Context` for the waker of `cx`.
///
/// # Safety
/// `cx` must be the pointer the caller's poll function was given (a live `VeltCx`).
#[inline(always)]
pub unsafe fn with_context<R>(cx: *mut c_void, f: impl FnOnce(&mut Context<'_>) -> R) -> R {
    let h = header(cx);
    if std::ptr::eq((*h).vt, &RUST_VT) {
        return f(&mut *((*(h as *const RustHeader)).cx as *mut Context<'_>));
    }
    with_foreign(h, f)
}

/// [`with_context`] for a waker of another table: the `Waker` of a `RustOwned` header, else
/// a borrowed Rust waker over the header.
#[cold]
#[inline(never)]
unsafe fn with_foreign<R>(h: *const VeltWakerHeader, f: impl FnOnce(&mut Context<'_>) -> R) -> R {
    if std::ptr::eq((*h).vt, &RUST_OWNED_VT) {
        return f(&mut Context::from_waker(&(*(h as *const RustOwned)).waker));
    }
    // Never dropped or consumed: it borrows the reference of `cx`.
    let waker = ManuallyDrop::new(Waker::from_raw(RawWaker::new(h as *const (), &FORWARD)));
    f(&mut Context::from_waker(&waker))
}

/// For a poll function with a large body (a leaf polling its Rust future): the Rust `Context`
/// of `cx`, or, for a waker of another table, the result of calling `poll(f, ..)` again with
/// a Rust `cx` over that waker. The body then has one caller and inlines as it did before
/// `cx` held a C waker (with [`with_context`] its closure would have two and stay a call).
///
/// # Safety
/// As [`with_context`]; `poll` must be the poll function of `f` that the caller is.
#[inline(always)]
pub unsafe fn context_or_repoll<'a, T>(
    cx: *mut c_void,
    f: *mut T,
    poll: unsafe extern "C-unwind" fn(*mut T, *mut c_void) -> u32,
) -> Result<&'a mut Context<'a>, u32> {
    let h = header(cx);
    if std::ptr::eq((*h).vt, &RUST_VT) {
        return Ok(&mut *((*(h as *const RustHeader)).cx as *mut Context<'a>));
    }
    Err(repoll(cx, f, poll))
}

#[cold]
#[inline(never)]
unsafe fn repoll<T>(
    cx: *mut c_void,
    f: *mut T,
    poll: unsafe extern "C-unwind" fn(*mut T, *mut c_void) -> u32,
) -> u32 {
    with_rust_cx(cx, |rust_cx| poll(f, rust_cx))
}

/// The Rust executor's waker behind `cx`, if `cx` came from one.
///
/// # Safety
/// As [`with_context`]; the reference lives as long as the poll call.
pub unsafe fn rust_waker<'a>(cx: *mut c_void) -> Option<&'a Waker> {
    let h = header(cx);
    std::ptr::eq((*h).vt, &RUST_VT)
        .then(|| (*((*(h as *const RustHeader)).cx as *const Context<'a>)).waker())
}

/// Runs `f` with a `cx` whose waker is a Rust one ([`rust_waker`] finds it): `cx` itself when
/// it is, else a new one with the same await word over a Rust waker forwarding to the C one.
///
/// # Safety
/// As [`with_context`].
pub unsafe fn with_rust_cx<R>(cx: *mut c_void, f: impl FnOnce(*mut c_void) -> R) -> R {
    if rust_waker(cx).is_some() {
        return f(cx);
    }
    let word = (*(cx as *const VeltCx)).await_word;
    with_context(cx, |c| f(RustCx::new(word, c).ptr()))
}

// ---- Rust wakers as C ------------------------------------------------------------------------

/// The table of a Rust executor's header ([`RustCx`]), borrowed for the poll: a clone is a
/// [`RustOwned`].
static RUST_VT: VeltWakerVt = VeltWakerVt {
    clone: rust_clone,
    // A borrowed header holds no reference of its own to give back: `wake` keeps it.
    wake: rust_wake_by_ref,
    wake_by_ref: rust_wake_by_ref,
    drop: rust_drop,
};

unsafe fn rust_header_waker<'a>(h: *const VeltWakerHeader) -> &'a Waker {
    (*((*(h as *const RustHeader)).cx as *const Context<'a>)).waker()
}

unsafe extern "C" fn rust_clone(h: *const VeltWakerHeader) -> *const VeltWakerHeader {
    RustOwned::boxed(rust_header_waker(h).clone())
}

unsafe extern "C" fn rust_wake_by_ref(h: *const VeltWakerHeader) {
    rust_header_waker(h).wake_by_ref();
}

unsafe extern "C" fn rust_drop(_: *const VeltWakerHeader) {}

/// A Rust waker as an owned C header (one allocation per clone; only C code clones through
/// these tables, a Rust leaf clones the `Waker`).
#[repr(C)]
struct RustOwned {
    vt: *const VeltWakerVt,
    waker: Waker,
}

impl RustOwned {
    fn boxed(waker: Waker) -> *const VeltWakerHeader {
        let b = Box::new(RustOwned {
            vt: &RUST_OWNED_VT,
            waker,
        });
        Box::into_raw(b) as *const VeltWakerHeader
    }
}

/// The table of a [`RustOwned`].
static RUST_OWNED_VT: VeltWakerVt = VeltWakerVt {
    clone: owned_clone,
    wake: owned_wake,
    wake_by_ref: owned_wake_by_ref,
    drop: owned_drop,
};

unsafe extern "C" fn owned_clone(h: *const VeltWakerHeader) -> *const VeltWakerHeader {
    RustOwned::boxed((*(h as *const RustOwned)).waker.clone())
}

unsafe extern "C" fn owned_wake(h: *const VeltWakerHeader) {
    Box::from_raw(h as *mut RustOwned).waker.wake();
}

unsafe extern "C" fn owned_wake_by_ref(h: *const VeltWakerHeader) {
    (*(h as *const RustOwned)).waker.wake_by_ref();
}

unsafe extern "C" fn owned_drop(h: *const VeltWakerHeader) {
    drop(Box::from_raw(h as *mut RustOwned));
}

// ---- C wakers as Rust ------------------------------------------------------------------------

/// The table of a header's C table (its first word).
unsafe fn table<'a>(p: *const ()) -> &'a VeltWakerVt {
    &*(*(p as *const VeltWakerHeader)).vt
}

/// A Rust waker whose data is a C header: each entry calls the header's own table.
static FORWARD: RawWakerVTable = RawWakerVTable::new(
    |p| unsafe {
        let h = (table(p).clone)(p as *const VeltWakerHeader);
        RawWaker::new(h as *const (), &FORWARD)
    },
    |p| unsafe { (table(p).wake)(p as *const VeltWakerHeader) },
    |p| unsafe { (table(p).wake_by_ref)(p as *const VeltWakerHeader) },
    |p| unsafe { (table(p).drop)(p as *const VeltWakerHeader) },
);

#[cfg(test)]
#[path = "waker_tests.rs"]
pub(crate) mod tests;
