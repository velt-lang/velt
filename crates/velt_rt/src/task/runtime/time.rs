//! Timers for whichever backend runs: [`Sleep`] is tokio's on the tokio backends and the Velt
//! backend's own there (its workers keep the deadlines, [`super::velt::timer`]).

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

/// Completes at its deadline; can be moved to another with [`Sleep::reset`].
pub enum Sleep {
    Tokio(Pin<Box<tokio::time::Sleep>>),
    Velt(super::velt::timer::Sleep),
}

/// A sleep until `deadline`.
pub fn sleep_until(deadline: Instant) -> Sleep {
    if super::is_velt() {
        Sleep::Velt(super::velt::timer::Sleep::until(deadline))
    } else {
        let at = tokio::time::Instant::from_std(deadline);
        Sleep::Tokio(Box::pin(tokio::time::sleep_until(at)))
    }
}

/// Waits `d`.
pub async fn sleep(d: Duration) {
    sleep_until(Instant::now() + d).await
}

impl Sleep {
    pub fn reset(&mut self, deadline: Instant) {
        match self {
            Sleep::Tokio(s) => s.as_mut().reset(tokio::time::Instant::from_std(deadline)),
            Sleep::Velt(s) => s.reset(deadline),
        }
    }
}

impl Future for Sleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        match self.get_mut() {
            Sleep::Tokio(s) => s.as_mut().poll(cx),
            Sleep::Velt(s) => Pin::new(s).poll(cx),
        }
    }
}
