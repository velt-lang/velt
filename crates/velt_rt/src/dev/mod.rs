//! Dev mode: behavior under the `velt dev` supervisor (docs/internals/contracts/rt_abi_async.md §13).
//!
//! - `handover`: the dev channel's protocol (`VELT_DEV_SOCKET`: a Unix socket, or a named pipe
//!   on Windows), whose supervisor side `velt dev` uses. The program's side is std's: listeners
//!   come from the supervisor (std/dev/handover_posix.velt, handover_win32.velt), so a restarted
//!   program serves the same socket and connections arriving during a reload wait in the kernel
//!   backlog instead of being refused; the graceful stop before the next version (SIGTERM on
//!   Unix, `stop` on the stop channel on Windows) is std/dev/stop.velt.
//!
//! Outside `velt dev` (variable not set) std binds its listeners itself and nothing else changes.

pub mod handover;

/// Names the supervisor's dev channel for listener handover.
pub const SOCKET_ENV: &str = "VELT_DEV_SOCKET";
