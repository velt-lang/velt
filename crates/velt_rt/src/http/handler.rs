//! The request-handler descriptor a server runs, and how a running server finds it.
//!
//! Outside dev mode the descriptor is fixed for the server's lifetime. Under `velt dev` every
//! request re-reads it through an atomic pointer, so a hot swap (docs/internals/design/hot-reload.md,
//! phase 3) can give new requests new code ([`update_handlers`], [`replace_handler`]) while requests already
//! running finish on the code they started with (their future copied `poll`/`drop` at
//! creation). Replaced descriptors are never freed: another worker may be reading one, and the
//! code they point to stays loaded for the session anyway.
//!
//! Every connection and every in-flight request holds the server's [`Shared`], so it is dropped
//! only once the server was closed and its last request finished. That is when the handler's
//! closure environment is released (its captures' drop hooks run, e.g. a captured store's
//! `dispose()`), and when `server.shutdown()` resolves.

use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::watch;

use super::request::ReqObj;
use crate::task::{DropFn, PollFn};

/// `void init(void* env, void* req, void* state)`: write the initial handler state for `req` (the
/// request handle's address as a pointer: the compiler generates `init` with a `ptr` parameter).
pub type InitFn = unsafe extern "C" fn(env: *mut c_void, req: *mut ReqObj, state: *mut u8);

/// Request handler descriptor (copied by `velt_rt_http_serve`). Size 48, align 8.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VeltHandler {
    /// Builds the per-request state (closure captures come from `env`).
    pub init: InitFn,
    /// Handler state machine poll; result at state offset 0 is a `VeltResp*` (null ⇒ 500).
    pub poll: PollFn,
    /// Handler state machine drop (request cancelled, e.g. client went away).
    pub drop: DropFn,
    /// Size of the handler state in bytes.
    pub state_size: u64,
    /// Alignment of the handler state.
    pub state_align: u64,
    /// Closure environment shared by all requests, read concurrently from many workers: null, or
    /// a closure environment box whose first word is its drop function (`void drop(void* env)`,
    /// null = not owned). The runtime calls it once the server is closed and idle.
    pub env: *mut c_void,
}

/// A server's handler as its requests see it.
pub(super) struct Shared {
    /// Always a leaked `Box<VeltHandler>`; only the current one is freed (on drop).
    current: AtomicPtr<VeltHandler>,
    /// Dropped (after the environment is released) with `Shared`; see [`Shared::released`].
    released: watch::Sender<()>,
}

// SAFETY: `env` is shared read-only by all requests, as the `VeltHandler` contract requires; the
// function pointers are plain code addresses; the descriptor itself is immutable once published.
unsafe impl Send for Shared {}
// SAFETY: as above.
unsafe impl Sync for Shared {}

/// Dev mode: every server started, in `serve` order.
static DEV_SERVERS: Mutex<Vec<Weak<Shared>>> = Mutex::new(Vec::new());

impl Shared {
    /// A server's handler; registered for [`replace_handler`] in dev mode.
    pub(super) fn new(desc: VeltHandler) -> Arc<Shared> {
        let shared = Arc::new(Shared {
            current: AtomicPtr::new(Box::into_raw(Box::new(desc))),
            released: watch::Sender::new(()),
        });
        if crate::dev::enabled() {
            if let Ok(mut servers) = DEV_SERVERS.lock() {
                servers.push(Arc::downgrade(&shared));
            }
        }
        shared
    }

    /// The descriptor for a new request.
    pub(super) fn handler(&self) -> VeltHandler {
        // SAFETY: always a valid descriptor that is never freed while `self` lives.
        unsafe { *self.current.load(Ordering::Acquire) }
    }

    /// Resolves (with an error, which is the signal) once `Shared` was dropped: the server is
    /// closed, every request has finished and the handler's environment was released.
    pub(super) fn released(&self) -> watch::Receiver<()> {
        self.released.subscribe()
    }

    fn replace(&self, desc: VeltHandler) {
        // The old descriptor is leaked on purpose (see the module docs).
        self.current
            .swap(Box::into_raw(Box::new(desc)), Ordering::AcqRel);
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        // SAFETY: the current descriptor came from `Box::into_raw` and nothing else frees it.
        let desc = unsafe { Box::from_raw(*self.current.get_mut()) };
        // SAFETY: no request is running (each holds `Shared`), so nothing borrows the environment
        // any more; its first word is its drop function (`VeltHandler::env`).
        unsafe { release_env(desc.env) };
    }
}

/// Runs a closure environment's drop function (its first word), if it has one.
unsafe fn release_env(env: *mut c_void) {
    if env.is_null() {
        return;
    }
    let drop_fn = *(env as *const Option<unsafe extern "C" fn(*mut c_void)>);
    if let Some(drop_fn) = drop_fn {
        drop_fn(env);
    }
}

/// Dev mode: server number `index` (in `serve` order) runs `desc` for requests that start from
/// now on. False if there is no such running server (or not in dev mode).
pub fn replace_handler(index: usize, desc: VeltHandler) -> bool {
    let server = DEV_SERVERS
        .lock()
        .ok()
        .and_then(|servers| servers.get(index).and_then(Weak::upgrade));
    match server {
        Some(server) => {
            server.replace(desc);
            true
        }
        None => false,
    }
}

/// Dev mode: offer every running server's current descriptor to `update`; servers for which it
/// returns a new descriptor run that for requests that start from now on (a hot swap that
/// recompiled their handler). Returns how many servers were updated.
pub fn update_handlers(mut update: impl FnMut(&VeltHandler) -> Option<VeltHandler>) -> usize {
    let servers: Vec<Arc<Shared>> = match DEV_SERVERS.lock() {
        Ok(servers) => servers.iter().filter_map(Weak::upgrade).collect(),
        Err(_) => return 0,
    };
    let mut updated = 0;
    for server in servers {
        if let Some(desc) = update(&server.handler()) {
            server.replace(desc);
            updated += 1;
        }
    }
    updated
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "C" fn init(_: *mut c_void, _: *mut ReqObj, _: *mut u8) {}
    unsafe extern "C" fn poll(_: *mut u8, _: *mut c_void) -> u32 {
        0
    }
    unsafe extern "C" fn drop_state(_: *mut u8) {}

    fn desc(state_size: u64) -> VeltHandler {
        VeltHandler {
            init,
            poll,
            drop: drop_state,
            state_size,
            state_align: 8,
            env: std::ptr::null_mut(),
        }
    }

    static RELEASED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    unsafe extern "C" fn env_drop(env: *mut c_void) {
        RELEASED.fetch_add(1, Ordering::SeqCst);
        drop(Box::from_raw(env as *mut [usize; 2]));
    }

    #[test]
    fn dropping_the_server_releases_the_environment_once() {
        let env = Box::into_raw(Box::new([env_drop as *const () as usize, 0]));
        let shared = Shared::new(VeltHandler {
            env: env as *mut c_void,
            ..desc(16)
        });
        let released = shared.released();
        let request = shared.clone();
        drop(shared);
        assert_eq!(
            RELEASED.load(Ordering::SeqCst),
            0,
            "a request still holds it"
        );
        assert!(released.has_changed().is_ok());
        drop(request);
        assert_eq!(RELEASED.load(Ordering::SeqCst), 1);
        assert!(
            released.has_changed().is_err(),
            "shutdown() waiters are woken"
        );
    }

    #[test]
    fn new_requests_see_the_replacement() {
        let shared = Shared::new(desc(16));
        assert_eq!(shared.handler().state_size, 16);
        shared.replace(desc(4096));
        assert_eq!(shared.handler().state_size, 4096);
        assert!(!replace_handler(usize::MAX, desc(1)));
    }

    #[test]
    fn update_offers_each_running_server() {
        let shared = Shared::new(desc(24));
        // Outside dev mode nothing is registered; register this one by hand.
        DEV_SERVERS.lock().unwrap().push(Arc::downgrade(&shared));
        let n = update_handlers(|d| (d.state_size == 24).then(|| desc(48)));
        assert!(n >= 1);
        assert_eq!(shared.handler().state_size, 48);
        assert_eq!(
            update_handlers(|d| (d.state_size == 24).then(|| desc(1))),
            0
        );
    }
}
