//! `Compiled<S>`: a Rust `Future` driving a compiled state machine `(poll, drop, state)`.
//!
//! The state is stored inline in the future (`Inline<N>` size classes, align 16) so that a spawned
//! task is exactly one allocation — tokio's task cell — or, for states larger than the biggest
//! class, in a separate heap block (`Heap`). `Borrowed` points at a state owned by the caller
//! (`block_on`). The state is copied into place before the first poll; compiled states contain no
//! self-references until they have been polled, so that copy is sound. After the first poll the
//! future is pinned and the state never moves again.

use super::all::ResultDropFn;
use super::local::Locals;
use super::{raw_cx, DropFn, PollFn, SendPtr, READY};
use std::alloc::Layout;
use std::future::Future;
use std::marker::PhantomPinned;
use std::mem::MaybeUninit;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Where a compiled state lives.
pub trait StateStore: Send + 'static {
    /// Address of the state.
    fn ptr(&mut self) -> *mut u8;
}

/// A store the runtime allocates (and frees) itself.
pub trait OwnedStore: StateStore {
    /// Storage for a state of `size` bytes aligned to `align` (uninitialized).
    fn new(size: usize, align: usize) -> Self;
}

/// Inline storage of `N` bytes, 16-aligned.
#[repr(C, align(16))]
pub struct Inline<const N: usize>([MaybeUninit<u8>; N]);

impl<const N: usize> StateStore for Inline<N> {
    fn ptr(&mut self) -> *mut u8 {
        self.0.as_mut_ptr() as *mut u8
    }
}

impl<const N: usize> OwnedStore for Inline<N> {
    fn new(size: usize, align: usize) -> Self {
        debug_assert!(size <= N && align <= 16);
        Inline([MaybeUninit::uninit(); N])
    }
}

/// Separately allocated storage for states larger than the inline classes (or over-aligned).
pub struct Heap {
    ptr: SendPtr<u8>,
    layout: Layout,
}

impl StateStore for Heap {
    fn ptr(&mut self) -> *mut u8 {
        self.ptr.0
    }
}

impl OwnedStore for Heap {
    fn new(size: usize, align: usize) -> Self {
        let layout = Layout::from_size_align(size.max(1), align.max(1))
            .unwrap_or_else(|_| crate::panic::fatal("invalid async state layout"));
        // SAFETY: non-zero size.
        let p = unsafe { std::alloc::alloc(layout) };
        if p.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        Heap {
            ptr: SendPtr(p),
            layout,
        }
    }
}

impl Drop for Heap {
    fn drop(&mut self) {
        // SAFETY: allocated in `new` with this layout.
        unsafe { std::alloc::dealloc(self.ptr.0, self.layout) }
    }
}

/// A state owned by someone else that outlives the future (the `block_on` caller's frame).
pub struct Borrowed(pub(crate) SendPtr<u8>);

impl StateStore for Borrowed {
    fn ptr(&mut self) -> *mut u8 {
        self.0 .0
    }
}

/// Future driving a compiled state machine as a task root. Output is `()`: the result stays at
/// state offset 0. Promises the task starts are driven by its [`Locals`].
pub struct Compiled<S: StateStore> {
    poll: PollFn,
    drop: DropFn,
    live: bool,
    state: S,
    /// Transfers the result in place for the task that joins this one (compiled transfer glue,
    /// `velt_rt_spawn_transfer`), run as the state finishes, inside the task's local set.
    transfer: Option<ResultDropFn>,
    /// Dropped after the state (a cancelled root first releases the promises it owns).
    locals: Locals,
    _pinned: PhantomPinned,
}

impl<S: StateStore> Compiled<S> {
    /// Wrap an already-initialized store.
    pub fn from_store(poll: PollFn, drop: DropFn, state: S) -> Self {
        Compiled {
            poll,
            drop,
            live: true,
            state,
            transfer: None,
            locals: Locals::default(),
            _pinned: PhantomPinned,
        }
    }

    /// Run `transfer` on the result as the state finishes (see the field).
    pub fn with_transfer(mut self, transfer: Option<ResultDropFn>) -> Self {
        self.transfer = transfer;
        self
    }

    /// Address of the state (the result is at offset 0 once `poll` returned `Ready`).
    pub fn state_ptr(self: Pin<&mut Self>) -> *mut u8 {
        // SAFETY: we only hand out the address; nothing is moved.
        unsafe { self.get_unchecked_mut().state.ptr() }
    }
}

impl<S: OwnedStore> Compiled<S> {
    /// Allocate a store and let `init` write the initial state into it.
    pub fn with_init(
        poll: PollFn,
        drop: DropFn,
        size: usize,
        align: usize,
        init: impl FnOnce(*mut u8),
    ) -> Self {
        let mut state = S::new(size, align);
        init(state.ptr());
        Self::from_store(poll, drop, state)
    }

    /// Copy `size` bytes of initial state from `src`.
    ///
    /// # Safety
    /// `src` must be readable for `size` bytes; ownership of the state's contents moves to `Self`.
    pub unsafe fn copy_from(
        poll: PollFn,
        drop: DropFn,
        src: *const u8,
        size: usize,
        align: usize,
    ) -> Self {
        Self::with_init(poll, drop, size, align, |dst| {
            std::ptr::copy_nonoverlapping(src, dst, size)
        })
    }
}

impl<S: StateStore> Future for Compiled<S> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        // SAFETY: the state is never moved out of the pinned future.
        let this = unsafe { self.get_unchecked_mut() };
        if !this.live {
            return Poll::Ready(());
        }
        let (poll, state, transfer) = (this.poll, this.state.ptr(), this.transfer);
        let r = this.locals.poll_root(cx, |cx| {
            // SAFETY: the compiled poll function upholds the ABI for its own state.
            match unsafe { poll(state, raw_cx(cx)) } {
                READY => {
                    if let Some(t) = transfer {
                        // SAFETY: the finished state's result is at offset 0. The glue runs
                        // here, inside this task's set, while promises the task started (which
                        // may still reference the result's objects) are on this thread; a
                        // promise it starts joins the set (see `velt_rt_fut_transfer`).
                        unsafe { t(state) }
                    }
                    Poll::Ready(())
                }
                _ => Poll::Pending,
            }
        });
        // Output printed during this poll must be ordered before anything printed after the task
        // resumes on another worker.
        crate::io::publish_thread_output();
        this.live = r.is_pending();
        r
    }
}

impl<S: StateStore> Drop for Compiled<S> {
    fn drop(&mut self) {
        if self.live {
            // SAFETY: cancelled before completion: the state owns live locals.
            unsafe { (self.drop)(self.state.ptr()) }
        }
    }
}

/// Run `$body` with the type alias `$S` bound to the storage class fitting `(size, align)`.
macro_rules! with_state_store {
    ($size:expr, $align:expr, |$S:ident| $body:expr) => {{
        let (size, align): (usize, usize) = ($size, $align);
        if align > 16 || size > 1024 {
            type $S = $crate::task::compiled::Heap;
            $body
        } else if size <= 64 {
            type $S = $crate::task::compiled::Inline<64>;
            $body
        } else if size <= 256 {
            type $S = $crate::task::compiled::Inline<256>;
            $body
        } else {
            type $S = $crate::task::compiled::Inline<1024>;
            $body
        }
    }};
}
pub(crate) use with_state_store;
