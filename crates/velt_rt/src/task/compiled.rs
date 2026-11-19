//! `Compiled<S>`: a Rust `Future` driving a compiled state machine `(poll, drop, state)` as a task
//! root inside the task's local set (local/).
//!
//! The state is stored inline in the future (`Inline<N>`, align 16), or `Borrowed` points at a
//! state owned by the caller. The state is copied into place before the first poll; compiled
//! states contain no self-references until they have been polled, so that copy is sound. After the
//! first poll the future is pinned and the state never moves again.

use super::local::Locals;
use super::{raw_cx, DropFn, PollFn, SendPtr, READY};
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

/// A state owned by someone else that outlives the future.
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
            locals: Locals::default(),
            _pinned: PhantomPinned,
        }
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
        let (poll, state) = (this.poll, this.state.ptr());
        // The location this task's result carries is of a throw in this poll (panic.rs).
        crate::panic::ThrowLoc::clear();
        let r = this.locals.poll_root(cx, |cx| {
            // SAFETY: the compiled poll function upholds the ABI for its own state.
            match unsafe { poll(state, raw_cx(cx).ptr()) } {
                READY => Poll::Ready(()),
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
            let (drop, state) = (self.drop, self.state.ptr());
            // SAFETY: cancelled before completion: the state owns live locals.
            self.locals.drop_with(|| unsafe { drop(state) });
        }
    }
}
