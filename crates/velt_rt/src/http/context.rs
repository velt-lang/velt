//! The request a worker is running: while a handler's state machine is polled, its request and
//! the response it hands back are reached through this thread's frame rather than through the
//! registries.
//!
//! A registry lookup locks a shard and counts a reference, and a registered response is an
//! allocation of its own; a handler that reads `req.url` and returns a `Response` paid for three
//! of them per request. The handler's future (`server.rs`) holds its request, so during a poll
//! the frame can point at it: an accessor given that request's key reads it directly, and
//! `velt_rt_http_req_respond` leaves the response in the frame for the future to send. Any other
//! key (a request handed to another task, or used after its handler finished) still goes through
//! the registry, which keeps the use-after-finish check: `velt_rt_http_req_drop` takes the
//! request out of the frame as well as out of the registry.

use super::request::ReqObj;
use super::response::RespObj;
use std::cell::Cell;
use std::ptr::null_mut;

/// What a handler returns when its response is in the frame (`velt_rt_http_req_respond`); never
/// a registry key (keys have a nonzero generation in their high word).
pub const RESPONDED: u64 = 1;

/// One poll of a handler: its request, and the response it handed back.
pub(super) struct Frame {
    /// The request's key; 0 once the handler released it.
    key: u64,
    req: *const ReqObj,
    /// Set by `velt_rt_http_req_respond`.
    pub response: Option<RespObj>,
}

impl Frame {
    /// The frame of request `key`, whose object the poller keeps alive at `req`.
    pub fn new(key: u64, req: *const ReqObj) -> Frame {
        Frame {
            key,
            req,
            response: None,
        }
    }
}

thread_local! {
    static CURRENT: Cell<*mut Frame> = const { Cell::new(null_mut()) };
}

/// Runs `poll` with `frame` as this thread's current frame (the previous one is restored after,
/// so a nested poll is fine). `frame` must stay valid, and not be used otherwise, until it
/// returns.
pub(super) fn enter<R>(frame: *mut Frame, poll: impl FnOnce() -> R) -> R {
    struct Restore(*mut Frame);
    impl Drop for Restore {
        fn drop(&mut self) {
            CURRENT.with(|c| c.set(self.0));
        }
    }
    let _restore = Restore(CURRENT.with(|c| c.replace(frame)));
    poll()
}

/// The current frame, if it is request `key`'s.
fn frame(key: u64) -> Option<*mut Frame> {
    let f = CURRENT.with(Cell::get);
    // SAFETY: a non-null current frame is valid for the duration of `enter`, which is running.
    (key != 0 && !f.is_null() && unsafe { (*f).key } == key).then_some(f)
}

/// Request `key`'s object, if this thread is polling its handler. Valid until that poll returns.
pub(super) fn request(key: u64) -> Option<*const ReqObj> {
    // SAFETY: as in `frame`.
    frame(key).map(|f| unsafe { (*f).req })
}

/// The handler released request `key`: later uses in this poll go to the registry (and fail).
pub(super) fn forget(key: u64) {
    if let Some(f) = frame(key) {
        // SAFETY: as in `frame`.
        unsafe { (*f).key = 0 };
    }
}

/// Leaves `resp` in request `key`'s frame; gives it back if this thread is not polling that
/// request's handler (it ran on in a task of its own, or the key is stale).
pub(super) fn respond(key: u64, resp: RespObj) -> Option<RespObj> {
    match frame(key) {
        Some(f) => {
            // SAFETY: as in `frame`.
            unsafe { (*f).response = Some(resp) };
            None
        }
        None => Some(resp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::str::VeltStr;

    fn response() -> RespObj {
        hyper::Response::new(super::super::body::RespBody::full(bytes::Bytes::new()))
    }

    /// `velt_rt_http_req_respond` of a 201 with a text body for request `key`.
    fn respond_201(key: u64) -> u64 {
        let mut text = VeltStr::from_text("made");
        let reason = VeltStr::empty();
        let (name, value) = (VeltStr::empty(), VeltStr::empty());
        unsafe {
            super::super::respond::velt_rt_http_req_respond(
                key, 201, &reason, &name, &value, 1, &mut text, 1,
            )
        }
    }

    #[test]
    fn the_frame_answers_for_its_request_only_while_it_runs() {
        // Never dereferenced here: the frame only hands the pointer out.
        let req = std::ptr::NonNull::<ReqObj>::dangling().as_ptr() as *const ReqObj;
        let mut frame = Frame::new(7, req);
        let at: *mut Frame = &mut frame;
        assert!(request(7).is_none(), "no frame outside a poll");
        enter(at, || {
            assert_eq!(request(7), Some(req));
            assert!(request(8).is_none(), "another request's key");
            assert!(request(0).is_none());
            // A nested poll of another request has its own frame, and gives this one back.
            let mut inner = Frame::new(9, req);
            let inner_at: *mut Frame = &mut inner;
            enter(inner_at, || {
                assert!(request(7).is_none());
                assert!(respond(9, response()).is_none());
            });
            assert_eq!(request(7), Some(req));
            assert!(respond(8, response()).is_some());
            assert_eq!(respond_201(7), RESPONDED);
            // Released: later uses in this poll no longer find it here.
            forget(7);
            assert!(request(7).is_none());
            assert!(respond(7, response()).is_some());
        });
        assert!(request(7).is_none());
        assert_eq!(frame.response.map(|r| r.status().as_u16()), Some(201));
    }

    #[test]
    fn without_a_frame_a_response_is_registered() {
        let key = respond_201(7);
        assert!(key > RESPONDED);
        let r = super::super::response::take(crate::registry::Key::from_bits(key));
        assert_eq!(r.map(|r| r.status().as_u16()), Some(201));
    }
}
