//! Futures that native code hands to Velt: blocking work on the runtime's pool (`#[export(blocking)]`)
//! and [`Completer`]s for libraries with their own async runtime (`#[export(future)]`).

use std::ffi::c_void;
use std::mem::MaybeUninit;

use crate::{__panic_message, api, fatal, Error, OutRet, OwnedParam};

/// Requires an owned parameter type for `#[export(blocking)]` (its value moves to another thread).
#[doc(hidden)]
pub fn __owned<T: OwnedParam>(t: T) -> T {
    t
}

/// Runs `f` on the runtime's blocking pool; the future's result is `R`'s slot.
#[doc(hidden)]
pub fn __blocking<R, F>(f: F) -> *mut c_void
where
    R: OutRet,
    F: FnOnce() -> R + Send + 'static,
{
    unsafe extern "C" fn work<R: OutRet, F: FnOnce() -> R>(ctx: *mut c_void, out: *mut u8) {
        let f = Box::from_raw(ctx as *mut F);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
            .unwrap_or_else(|p| R::panicked(&__panic_message(p)));
        r.write(out as *mut R::Slot);
    }
    unsafe extern "C" fn drop_ctx<F>(ctx: *mut c_void) {
        drop(Box::from_raw(ctx as *mut F));
    }
    unsafe extern "C" fn drop_result<R: OutRet>(slot: *mut u8) {
        R::drop_slot(slot as *mut R::Slot);
    }
    let ctx = Box::into_raw(Box::new(f)) as *mut c_void;
    // SAFETY: the callbacks match the table's contract for `R::Slot`-sized results.
    unsafe {
        (api().fut_blocking)(
            work::<R, F>,
            ctx,
            drop_ctx::<F>,
            std::mem::size_of::<R::Slot>(),
            Some(drop_result::<R>),
        )
    }
}

/// A result that can also say "failed" ([`Completer`] completes a dropped one with an error).
pub trait ErrorRet: OutRet {
    /// The failure with `message`.
    fn error(message: &str) -> Self;
}

impl<T> ErrorRet for Result<T, Error>
where
    Result<T, Error>: OutRet,
{
    fn error(message: &str) -> Self {
        Err(Error::other(message))
    }
}

/// A pending future plus the handle that completes it (for libraries with their own async
/// runtime): return [`Completer::future`] from a `#[export(future)]` function, then call
/// [`Completer::complete`] once, from any thread. A `Completer` dropped without completing
/// completes its future with the error "completer dropped", so the awaiting Velt code never
/// hangs.
pub struct Completer<R: ErrorRet> {
    handle: u64,
    future: *mut c_void,
    done: bool,
    _r: std::marker::PhantomData<fn(R)>,
}

// SAFETY: the handle may be completed from any thread (native_abi.md).
unsafe impl<R: ErrorRet> Send for Completer<R> {}

impl<R: ErrorRet> Completer<R> {
    /// A new pending future.
    pub fn new() -> Completer<R> {
        unsafe extern "C" fn drop_result<R: OutRet>(slot: *mut u8) {
            R::drop_slot(slot as *mut R::Slot);
        }
        let mut handle = 0u64;
        // SAFETY: valid out-pointer and drop callback for `R::Slot`.
        let future = unsafe {
            (api().fut_completer)(
                std::mem::size_of::<R::Slot>(),
                Some(drop_result::<R>),
                &mut handle,
            )
        };
        Completer {
            handle,
            future,
            done: false,
            _r: std::marker::PhantomData,
        }
    }

    /// The future to return to Velt (take it once).
    pub fn future(&mut self) -> Future<R> {
        let f = std::mem::replace(&mut self.future, std::ptr::null_mut());
        if f.is_null() {
            fatal("Completer::future taken twice");
        }
        Future {
            raw: f,
            _r: std::marker::PhantomData,
        }
    }

    /// Completes the future with `value` (the only way to complete it: exactly once).
    pub fn complete(mut self, value: R) {
        self.send(value);
    }

    fn send(&mut self, value: R) {
        self.done = true;
        let mut slot = MaybeUninit::<R::Slot>::uninit();
        // SAFETY: the slot is written, then moved into the future by `complete`; `done` makes
        // this the handle's only completion.
        unsafe {
            value.write(slot.as_mut_ptr());
            (api().complete)(self.handle, slot.as_ptr() as *const u8);
        }
    }
}

impl<R: ErrorRet> Drop for Completer<R> {
    fn drop(&mut self) {
        if !self.done {
            self.send(R::error("completer dropped"));
        }
        if !self.future.is_null() {
            // Never handed to Velt: free it through its `VeltFut` header (`{ poll, drop }`).
            // SAFETY: a live runtime future owned by this completer.
            unsafe {
                let hdr = self.future as *const [unsafe extern "C" fn(*mut c_void); 2];
                ((*hdr)[1])(self.future);
            }
        }
    }
}

impl<R: ErrorRet> Default for Completer<R> {
    fn default() -> Self {
        Completer::new()
    }
}

/// A runtime future that resolves to `R` (the return type of `#[export(future)]` functions,
/// `declare async function ...: Promise<R>` in Velt).
pub struct Future<R: OutRet> {
    raw: *mut c_void,
    _r: std::marker::PhantomData<fn() -> R>,
}

impl<R: OutRet> Future<R> {
    #[doc(hidden)]
    pub fn __into_raw(self) -> *mut c_void {
        self.raw
    }
}

/// The result type of a `#[export(future)]` function (only [`Future`]).
pub trait FutureRet {
    /// The awaited result's name in a signature.
    const SIG: &'static str;
    #[doc(hidden)]
    fn __into_raw(self) -> *mut c_void;
}

impl<R: OutRet> FutureRet for Future<R> {
    const SIG: &'static str = R::SIG;
    fn __into_raw(self) -> *mut c_void {
        self.raw
    }
}
