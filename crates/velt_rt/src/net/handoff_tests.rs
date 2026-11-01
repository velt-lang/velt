//! Sending on a socket is a hand-off: the peer is often a task of the same program, which may
//! read on another worker while the sending task's poll goes on, so what the sender printed so
//! far is published first (before the bytes leave). Checked on closed handles and invalid
//! targets, which publish just the same.

use crate::bytes::VeltBytes;
use crate::io::handoff_probe::{buffer_output, published};
use crate::registry::Key;
use crate::result::VeltErr;
use crate::str::VeltStr;
use crate::task::{velt_rt_fut_drop, VeltFut};

/// Buffers output, runs `send`, and checks the output was published; drops the returned future.
fn publishes(what: &str, send: impl FnOnce() -> *mut VeltFut) {
    buffer_output();
    let f = send();
    assert!(published(), "{what} publishes this thread's output first");
    // SAFETY: a heap future the call returned, dropped once.
    unsafe { velt_rt_fut_drop(f) };
}

/// [`publishes`] for a call that returns no future.
fn publishes_now(what: &str, send: impl FnOnce()) {
    buffer_output();
    send();
    assert!(published(), "{what} publishes this thread's output first");
}

#[test]
fn tcp_sends_publish_first() {
    use super::tcp::*;
    let closed = Key::NULL;
    let text = VeltStr::from_text("x");
    let bytes = VeltBytes::from_vec(b"x".to_vec());
    // SAFETY: valid values; the handle is never open.
    unsafe {
        publishes("write", || velt_rt_tcp_write(closed, &text));
        publishes("writeBytes", || velt_rt_tcp_write_bytes(closed, &bytes));
        publishes_now("tryWrite", || {
            velt_rt_tcp_try_write(closed, &bytes);
        });
        publishes_now("shutdown", || {
            let mut out = std::mem::MaybeUninit::<VeltErr>::uninit();
            velt_rt_tcp_shutdown(closed, out.as_mut_ptr());
        });
        publishes_now("abort", || velt_rt_tcp_abort(closed));
        publishes_now("close", || velt_rt_tcp_close(closed));
        let addr = VeltStr::from_text("127.0.0.1:1");
        publishes("connect", || velt_rt_tcp_connect(&addr));
    }
}

#[test]
fn udp_send_publishes_first() {
    let bytes = VeltBytes::from_vec(b"x".to_vec());
    let addr = VeltStr::from_text("127.0.0.1:1");
    // SAFETY: valid values; the handle is never open.
    publishes("sendTo", || unsafe {
        super::udp::velt_rt_udp_send_to(Key::NULL, &bytes, &addr)
    });
}
