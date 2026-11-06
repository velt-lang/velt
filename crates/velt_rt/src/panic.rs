//! Panics and process exit: `velt_rt_panic`, `velt_rt_exit`, plus the hook that turns internal
//! Rust panics into the same `panic: <msg>` / exit 101 behavior. Internal Rust panics never unwind
//! across the C ABI: the hook installed by [`install_hook`] exits the process before unwinding
//! starts.
//!
//! A Velt panic (`velt_rt_panic`) ends the process too, unless the thread is polling a promise
//! that catches panics ([`Catching`], `catchPanic` in `velt/task`, #2008). Then it prints the
//! message and unwinds, with a [`VeltPanic`] payload, through the compiled frames (they have
//! unwind tables and no landing pads, so nothing in them is dropped) to the poll of the nearest
//! promise node, which stops it there: the node is marked as panicked and its state is leaked,
//! never polled or dropped again (task/local/node). Awaiting such a node raises the panic again
//! in the awaiter ([`raise`]), so a panic travels through `await`s as a call would, until a
//! `catchPanic` takes it or a poll that catches nothing ends the process with it. Every runtime
//! function an unwind can pass through is `extern "C-unwind"`.
//!
//! Also the per-thread "where was the last error thrown" slot (`velt_rt_set_throw_loc` /
//! `velt_rt_throw_loc`) that compiled code fills at each `throw` and reads when it reports an
//! uncaught error (`Uncaught E: msg at file.vlt:3:5`). A task's error can be read on another
//! thread than the one that threw it (`async main` runs on a worker, a spawned task's handle is
//! awaited anywhere), so finished tasks carry the slot to whoever takes their result
//! ([`ThrowLoc`]). Each poll of a task starts with an empty slot, so a task that threw nothing in
//! its last poll hands over no location and leaves the taker's own in place. Started promises
//! carry it the same way: a promise that rejects keeps where it threw (task/local/node/loc.rs)
//! and hands it to whoever claims or reports the rejection, on whichever thread and in
//! whichever poll that is; so does a shared promise's hub (task/shared.rs). Known limits (the
//! slot is "the last throw on this thread", not part of the error value): a task that throws,
//! then awaits before its error leaves it (in a `finally`), reports no location; and a task that
//! threw and caught an error in its last poll hands that location to a taker that is itself
//! propagating an error (#356).

use std::cell::Cell;

use crate::io;
use crate::str::VeltStr;

/// Exit code used for panics (same as Rust).
pub const PANIC_EXIT_CODE: i32 = 101;

/// Bytes printed to stderr for a panic with message `msg`.
pub fn panic_message(msg: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(msg.len() + 8);
    out.extend_from_slice(b"panic: ");
    // A member key naming a symbol the program made is U+FFFF and `Symbol(desc)` (the
    // compiler's spelling, UTF-8 `EF BF BF` first): the message shows `Symbol(desc)`, as
    // Node's does.
    let mut rest = msg;
    while let Some(i) = rest.windows(3).position(|w| w == [0xef, 0xbf, 0xbf]) {
        out.extend_from_slice(&rest[..i]);
        rest = &rest[i + 3..];
    }
    out.extend_from_slice(rest);
    out.push(b'\n');
    out
}

fn die(msg: &[u8]) -> ! {
    use std::io::Write;
    io::try_flush_stdout();
    let _ = std::io::stderr().lock().write_all(&panic_message(msg));
    std::process::exit(PANIC_EXIT_CODE)
}

/// Internal fatal runtime error (bad ABI arguments etc.): reported like a Velt panic.
#[cold]
pub fn fatal(msg: &str) -> ! {
    die(msg.as_bytes())
}

/// Make Rust panics inside the runtime (bugs, or M3 tasks) report `panic: <msg>` and exit 101
/// instead of unwinding into generated code.
pub fn install_hook() {
    std::panic::set_hook(Box::new(|info| {
        let payload = info.payload();
        let msg = if let Some(s) = payload.downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else {
            "Box<dyn Any>".to_string()
        };
        let msg = match info.location() {
            Some(l) => format!("{msg} (runtime: {}:{})", l.file(), l.line()),
            None => msg,
        };
        die(msg.as_bytes())
    }));
}

#[no_mangle]
pub unsafe extern "C-unwind" fn velt_rt_panic(msg: *const VeltStr) -> ! {
    let text = if msg.is_null() {
        Vec::new()
    } else {
        (*msg).text_lossy().into_owned().into_bytes()
    };
    if CATCHING.with(Cell::get) == 0 {
        die(&text)
    }
    {
        use std::io::Write;
        io::try_flush_stdout();
        let _ = std::io::stderr().lock().write_all(&panic_message(&text));
    }
    unwind(text.into_boxed_slice())
}

/// Velt panics stopped at a catching promise so far (the leak-check report, str/stats.rs).
static CAUGHT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Count a Velt panic stopped at a catching promise.
pub(crate) fn count_caught() {
    CAUGHT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// How many Velt panics were stopped at a catching promise (the debug runtime's report).
#[cfg(debug_assertions)]
pub(crate) fn caught_count() -> u64 {
    CAUGHT.load(std::sync::atomic::Ordering::Relaxed)
}

/// The payload of a Velt panic unwinding to a promise poll that catches it: the message.
pub(crate) struct VeltPanic(pub Box<[u8]>);

thread_local! {
    /// How many polls up this thread's stack catch Velt panics ([`Catching`]).
    static CATCHING: Cell<u32> = const { Cell::new(0) };
}

/// Unwind with message `msg` (already printed) to the nearest catching poll.
#[cold]
fn unwind(msg: Box<[u8]>) -> ! {
    // `resume_unwind` does not run the hook (which would end the process).
    std::panic::resume_unwind(Box::new(VeltPanic(msg)))
}

/// Raise the panic of a promise that panicked in the code awaiting it (its message was printed
/// when it happened): unwind if a poll up the stack catches it, end the process otherwise.
#[cold]
pub(crate) fn raise(msg: &[u8]) -> ! {
    if CATCHING.with(Cell::get) == 0 {
        // The process ends with the message, as it would have at the panic itself.
        io::try_flush_stdout();
        std::process::exit(PANIC_EXIT_CODE)
    }
    unwind(msg.into())
}

/// While alive, Velt panics on this thread unwind instead of ending the process. Held around
/// the polls of promises that catch panics.
pub(crate) struct Catching(());

impl Catching {
    pub(crate) fn enter() -> Catching {
        CATCHING.with(|c| c.set(c.get() + 1));
        Catching(())
    }
}

impl Drop for Catching {
    fn drop(&mut self) {
        CATCHING.with(|c| c.set(c.get() - 1));
    }
}

/// Is a poll up this thread's stack catching Velt panics?
pub(crate) fn catching() -> bool {
    CATCHING.with(Cell::get) > 0
}

/// Set once anything arms `catchPanic` (task/local/catch.rs): until then no poll can catch, and
/// promise polls skip the thread-local [`catching`] check (one load of this flag instead).
static MAY_CATCH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Promises may catch panics from now on.
pub(crate) fn enable_catching() {
    MAY_CATCH.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Can any poll catch a panic yet (something armed `catchPanic`)?
#[inline(always)]
pub(crate) fn may_catch() -> bool {
    MAY_CATCH.load(std::sync::atomic::Ordering::Relaxed)
}

/// Run `poll`, stopping a Velt panic that unwinds out of it: `Err(message)`. Any other panic
/// (a runtime bug, which ends the process in the hook before it unwinds) keeps unwinding.
pub(crate) fn catch<R>(poll: impl FnOnce() -> R) -> Result<R, Box<[u8]>> {
    let open = crate::transfer_map::open_mark();
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(poll)) {
        Ok(r) => Ok(r),
        Err(payload) => match payload.downcast::<VeltPanic>() {
            Ok(p) => {
                // The panic may have left a transfer of this poll's open on this thread.
                crate::transfer_map::forget_since(open);
                Err(p.0)
            }
            Err(other) => std::panic::resume_unwind(other),
        },
    }
}

thread_local! {
    /// Location suffix (` at <path>:<line>:<col>`, a static string) of the last `throw`.
    static THROW_LOC: Cell<*const VeltStr> = const { Cell::new(std::ptr::null()) };
}

/// Record where the error being thrown now comes from. `loc` is a static string (compiled
/// code passes read-only data) or null for "unknown".
#[no_mangle]
pub extern "C" fn velt_rt_set_throw_loc(loc: *const VeltStr) {
    THROW_LOC.with(|c| c.set(loc));
}

/// Forget this thread's throw location (`velt/test`, before each test, so an error that is
/// never thrown here, such as a rejection, isn't reported where an earlier test threw).
#[no_mangle]
pub extern "C" fn velt_rt_clear_throw_loc() {
    velt_rt_set_throw_loc(std::ptr::null());
}

/// The location recorded by the last [`velt_rt_set_throw_loc`] on this thread (null if none).
#[no_mangle]
pub extern "C" fn velt_rt_throw_loc() -> *const VeltStr {
    THROW_LOC.with(|c| c.get())
}

/// The throw location of one thread, carried to another with a task's result.
#[derive(Clone, Copy)]
pub(crate) struct ThrowLoc(*const VeltStr);

// SAFETY: compiled code records only static strings (read-only data) or null.
unsafe impl Send for ThrowLoc {}

impl ThrowLoc {
    /// No location.
    pub(crate) const NONE: ThrowLoc = ThrowLoc(std::ptr::null());

    /// This thread's location (of the last `throw` here).
    pub(crate) fn current() -> ThrowLoc {
        ThrowLoc(velt_rt_throw_loc())
    }

    /// Make it this thread's location.
    pub(crate) fn restore(self) {
        velt_rt_set_throw_loc(self.0);
    }

    /// Make it this thread's location if it is known (a task that threw nothing keeps the
    /// taker's location).
    pub(crate) fn restore_if_known(self) {
        if !self.0.is_null() {
            velt_rt_set_throw_loc(self.0);
        }
    }

    /// Forget this thread's location (a task poll starts with none).
    pub(crate) fn clear() {
        velt_rt_set_throw_loc(std::ptr::null());
    }

    /// This thread's location, which is forgotten (one thread-local access).
    pub(crate) fn take() -> ThrowLoc {
        ThrowLoc(THROW_LOC.with(|c| c.replace(std::ptr::null())))
    }

    /// Make it this thread's location and return the one it replaces.
    pub(crate) fn replace(self) -> ThrowLoc {
        ThrowLoc(THROW_LOC.with(|c| c.replace(self.0)))
    }

    /// Is a location recorded?
    pub(crate) fn is_known(self) -> bool {
        !self.0.is_null()
    }
}

/// The thread reporting an uncaught error ([`velt_rt_uncaught_report`]); 0: none.
static REPORTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

thread_local! {
    /// Its address tells the threads apart for [`REPORTER`].
    static THREAD_MARK: u8 = const { 0 };
}

/// An uncaught error is about to be reported (`Uncaught …`), which ends the process: as in Node,
/// only the first one is. Tasks on other workers may fail at the same time (two spawned tasks
/// that reject, a task and `main`): their reports would follow the first before the process is
/// gone, so a thread that comes second waits here until the reporting thread ends the process.
/// The reporting thread itself goes on (an error reported while dropping the first one's
/// values ends the process the same way).
#[no_mangle]
pub extern "C" fn velt_rt_uncaught_report() {
    if !may_report(THREAD_MARK.with(|m| m as *const u8 as usize)) {
        loop {
            std::thread::park();
        }
    }
}

/// May the thread marked `me` report an uncaught error: it is the first to, or reports again?
fn may_report(me: usize) -> bool {
    use std::sync::atomic::Ordering;
    match REPORTER.compare_exchange(0, me, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => true,
        Err(first) => first == me,
    }
}

/// Nobody reported yet (tests).
#[cfg(test)]
fn reset_uncaught_report() {
    REPORTER.store(0, std::sync::atomic::Ordering::Release);
}

#[no_mangle]
pub extern "C" fn velt_rt_exit(code: i32) -> ! {
    io::flush_stdout();
    crate::str::stats::report();
    io::stats::report();
    #[cfg(all(debug_assertions, not(velt_rt_host)))]
    crate::debug_alloc::check_quarantine(&crate::GLOBAL_QUARANTINE);
    std::process::exit(code)
}

#[cfg(test)]
mod tests {
    #[test]
    fn throw_location_is_per_thread() {
        let s = crate::str::VeltStr::empty();
        assert!(super::velt_rt_throw_loc().is_null());
        super::velt_rt_set_throw_loc(&s);
        assert_eq!(super::velt_rt_throw_loc(), &s as *const _);
        std::thread::spawn(|| assert!(super::velt_rt_throw_loc().is_null()))
            .join()
            .unwrap();
        super::velt_rt_clear_throw_loc();
        assert!(super::velt_rt_throw_loc().is_null());
    }

    #[test]
    fn only_the_first_thread_reports_an_uncaught_error() {
        // The only test that reports (marks are thread-local addresses, never 1 or 2).
        super::reset_uncaught_report();
        assert!(super::may_report(1));
        // The reporting thread may report again (an error while dropping the first one's
        // values); another one may not.
        assert!(super::may_report(1));
        assert!(!super::may_report(2));
        super::reset_uncaught_report();
    }

    #[test]
    fn message_format() {
        assert_eq!(
            super::panic_message(b"division by zero"),
            b"panic: division by zero\n"
        );
    }
}

#[cfg(test)]
mod catch_tests {
    use super::*;

    #[test]
    fn a_velt_panic_unwinds_only_while_something_catches() {
        assert!(!catching());
        let caught = {
            let _c = Catching::enter();
            assert!(catching());
            catch(|| -> u32 { unwind(b"boom".to_vec().into_boxed_slice()) })
        };
        assert_eq!(caught.err().as_deref(), Some(&b"boom"[..]));
        assert!(!catching(), "the guard restores the depth");
    }

    #[test]
    fn nested_catches_stop_at_the_innermost() {
        let _outer = Catching::enter();
        let outer = catch(|| {
            let _inner = Catching::enter();
            let inner = catch(|| -> u32 { unwind(b"inner".to_vec().into_boxed_slice()) });
            assert_eq!(inner.err().as_deref(), Some(&b"inner"[..]));
            7
        });
        assert_eq!(outer.ok(), Some(7));
    }

    #[test]
    fn other_panics_are_not_taken_for_velt_panics() {
        let r = std::panic::catch_unwind(|| {
            catch(|| -> u32 { std::panic::resume_unwind(Box::new(5u8)) })
        });
        assert!(r.is_err(), "a non-Velt panic keeps unwinding past `catch`");
    }
}
