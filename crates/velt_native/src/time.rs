//! Timers on the runtime's clock (on Velt's scheduler, the worker's own timer heap).

use std::ffi::c_void;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use crate::api;
use crate::rt::{WakerArg, PENDING};

/// A point on the runtime's monotonic clock (nanoseconds; only differences mean anything).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Instant(u64);

impl Instant {
    /// Now.
    pub fn now() -> Instant {
        Instant((api().now_ns)())
    }

    /// The time since `earlier` (zero if it is later).
    pub fn duration_since(self, earlier: Instant) -> Duration {
        Duration::from_nanos(self.0.saturating_sub(earlier.0))
    }

    /// The time since this instant.
    pub fn elapsed(self) -> Duration {
        Instant::now().duration_since(self)
    }
}

impl std::ops::Add<Duration> for Instant {
    type Output = Instant;

    fn add(self, d: Duration) -> Instant {
        Instant(
            self.0
                .saturating_add(u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)),
        )
    }
}

/// Completes at its deadline; [`Sleep::reset`] moves it.
pub struct Sleep {
    h: *mut c_void,
}

// SAFETY: the runtime's timer may be polled from any worker.
unsafe impl Send for Sleep {}
// SAFETY: no `&self` method touches the timer.
unsafe impl Sync for Sleep {}

/// A sleep until `deadline`.
pub fn sleep_until(deadline: Instant) -> Sleep {
    Sleep {
        h: (api().sleep_new)(deadline.0),
    }
}

/// A sleep of `d`.
pub fn sleep(d: Duration) -> Sleep {
    sleep_until(Instant::now() + d)
}

impl Sleep {
    /// Moves the deadline (the sleep is pending again if it moved later).
    pub fn reset(&mut self, deadline: Instant) {
        // SAFETY: a live timer.
        unsafe { (api().sleep_reset)(self.h, deadline.0) }
    }
}

impl Future for Sleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let arg = WakerArg::of(cx.waker());
        // SAFETY: a live timer and a waker valid for the call.
        if unsafe { (api().sleep_poll)(self.h, &arg) } == PENDING {
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    }
}

impl Drop for Sleep {
    fn drop(&mut self) {
        // SAFETY: owned; freed once.
        unsafe { (api().sleep_drop)(self.h) }
    }
}

/// `fut`, or `Err(Elapsed)` once `d` passed.
pub async fn timeout<F: Future>(d: Duration, fut: F) -> Result<F::Output, Elapsed> {
    let mut fut = std::pin::pin!(fut);
    let mut timer = sleep(d);
    std::future::poll_fn(|cx| {
        if let Poll::Ready(v) = fut.as_mut().poll(cx) {
            return Poll::Ready(Ok(v));
        }
        match Pin::new(&mut timer).poll(cx) {
            Poll::Ready(()) => Poll::Ready(Err(Elapsed)),
            Poll::Pending => Poll::Pending,
        }
    })
    .await
}

/// [`timeout`] ran out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Elapsed;

impl std::fmt::Display for Elapsed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("deadline has elapsed")
    }
}

impl std::error::Error for Elapsed {}
