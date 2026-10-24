//! The compat bridge of the Velt backend: a small tokio runtime for everything that still uses
//! tokio's own types (files, processes, signals, Postgres, Redis, the HTTP client, UDP, DNS,
//! blocking work). Every Velt worker stays inside its context, so such code runs unchanged; its
//! I/O and timers are driven by the bridge's thread and wake Velt tasks across threads.

use std::sync::OnceLock;
use tokio::runtime::{Handle, Runtime};

static BRIDGE: OnceLock<Runtime> = OnceLock::new();

pub(super) fn handle() -> &'static Handle {
    BRIDGE
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .thread_name("velt-bridge")
                .enable_all()
                .build()
                .unwrap_or_else(|e| {
                    crate::panic::fatal(&format!("cannot start the async runtime: {e}"))
                })
        })
        .handle()
}
