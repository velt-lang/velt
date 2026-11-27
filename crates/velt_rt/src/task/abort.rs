//! What is left of the runtime's own abort signal: the argument of a runtime wait that takes one
//! (net/views.rs `velt_rt_tcp_readable_within`), which std always passes as 0 (none). Every
//! `AbortSignal` is std's (std/task/signal_core.velt), and so is `velt dev`'s graceful stop
//! (std/dev/stop.velt). Goes with net/views.rs.

use std::sync::Arc;

/// A runtime signal: none can be made any more.
pub struct Signal(());

impl Signal {
    /// Completes once the signal is aborted: never.
    pub(crate) async fn aborted(&self) {
        std::future::pending().await
    }
}

/// The signal behind handle bits `h`: none (std passes 0).
///
/// # Safety
/// No precondition (kept for the callers' signature).
pub(crate) unsafe fn signal_of(h: u64) -> Option<Arc<Signal>> {
    debug_assert_eq!(
        h, 0,
        "ICE: a runtime signal handle; std's signals are its own"
    );
    None
}
