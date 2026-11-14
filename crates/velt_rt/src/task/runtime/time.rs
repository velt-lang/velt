//! Timers of the runtime's host (tokio's): [`Sleep`] completes at its deadline.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

/// Completes at its deadline; can be moved to another with [`Sleep::reset`].
pub struct Sleep(Pin<Box<tokio::time::Sleep>>);

/// A sleep until `deadline`.
pub fn sleep_until(deadline: Instant) -> Sleep {
    let at = tokio::time::Instant::from_std(deadline);
    Sleep(Box::pin(tokio::time::sleep_until(at)))
}

/// Waits `d`.
pub async fn sleep(d: Duration) {
    sleep_until(Instant::now() + d).await
}

impl Sleep {
    pub fn reset(&mut self, deadline: Instant) {
        self.0
            .as_mut()
            .reset(tokio::time::Instant::from_std(deadline))
    }
}

impl Future for Sleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        self.get_mut().0.as_mut().poll(cx)
    }
}
