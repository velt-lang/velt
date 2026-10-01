//! Cancellation entry for tests: create a promise with an async function's boxed constructor,
//! poll it a number of times (driving the emulated clock between polls), then drop it — as the
//! runtime does when a pending future is cancelled — and report what is still allocated.

use super::{Interp, V};
use crate::vir::Program;

/// An argument of the constructor: an owned string (passed by pointer) or a scalar.
pub(in crate::tests) enum Arg<'a> {
    Str(&'a str),
    Int(u64),
}

/// Result of [`poll_then_drop`]: stdout, readiness of the last poll, live heap blocks after
/// the drop.
pub(in crate::tests) struct Cancelled {
    pub stdout: String,
    pub ready: bool,
    pub live_allocs: usize,
}

/// Call `ctor(args)` (a promise constructor), poll the future up to `polls` times (until
/// ready), drop it.
pub(in crate::tests) fn poll_then_drop(
    p: &Program,
    ctor: &str,
    args: &[Arg],
    polls: usize,
) -> Cancelled {
    let mut it = Interp::new(p);
    let f = p
        .funcs
        .iter()
        .position(|f| f.symbol == ctor)
        .unwrap_or_else(|| panic!("no function {ctor}"));
    let argv = args
        .iter()
        .map(|a| match a {
            Arg::Str(s) => {
                let hdr = it.raw_alloc(24);
                it.new_str(hdr, s.as_bytes());
                V::S(hdr)
            }
            Arg::Int(v) => V::S(*v),
        })
        .collect();
    let fut = it.call_fn(f, argv).expect("constructor exited").s();
    let mut ready = false;
    for _ in 0..polls {
        if ready {
            break;
        }
        ready = it
            .rt("velt_rt_fut_poll", &[fut, super::async_rt::CX])
            .expect("poll exited")
            == 1;
        it.exec.now += 1.0;
    }
    it.rt("velt_rt_fut_drop", &[fut]).expect("drop exited");
    Cancelled {
        stdout: it.stdout,
        ready,
        live_allocs: it.allocs.len(),
    }
}
