//! Pending HTTP upgrades (WebSocket handshakes) between the request that asks for one and the
//! code that accepts it.
//!
//! hyper hands out an upgrade as an `OnUpgrade` taken from the request. The Velt handler only
//! sees the request through accessors, so the runtime parks the `OnUpgrade` here under a key the request
//! carries (`velt_rt_http_req_upgrade`); `velt_rt_ws_accept` claims it. Once the handler has
//! produced its response, an unclaimed entry is dropped, so nothing outlives its request.

use hyper::upgrade::OnUpgrade;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

fn pending() -> MutexGuard<'static, HashMap<u64, OnUpgrade>> {
    static PENDING: OnceLock<Mutex<HashMap<u64, OnUpgrade>>> = OnceLock::new();
    PENDING
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Parks `on` and returns its key (never 0).
pub fn park(on: OnUpgrade) -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let key = NEXT.fetch_add(1, Ordering::Relaxed);
    pending().insert(key, on);
    key
}

/// Claims the upgrade parked under `key` (once).
pub fn claim(key: u64) -> Option<OnUpgrade> {
    pending().remove(&key)
}

/// Drops the upgrade under `key` if nobody claimed it (0 = none).
pub fn release(key: u64) {
    if key != 0 {
        pending().remove(&key);
    }
}
