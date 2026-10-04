//! Velt runtime for WebAssembly (`wasm32-wasip1` under wasmtime & co., `wasm32-unknown-unknown`
//! in the browser): the C ABI of docs/internals/contracts/rt_abi.md + rt_abi_async.md for
//! single-threaded programs. Programs link `libvelt_rt_wasm.a` built for their target (see
//! velt_link).
//!
//! Two kinds of modules:
//! - **shared with velt_rt**, compiled from `crates/velt_rt/src` (`#[path]`): strings, string
//!   methods and builders, number formatting, JSON (incl. `json.Value` handles), byte buffers
//!   and their bulk operations, BigInts, results, object handles, hashing (also `velt:hash`'s
//!   FNV-1a), HTML escaping, math, memory, the fs operations and the `*_sync` fs calls. Their
//!   `repr(C)` types put a pointer only where an 8-byte field follows, so on wasm32 each pointer
//!   occupies the first half of its 8-byte VIR slot (see `velt_codegen_llvm`'s
//!   `Target::wide_pointer_slots`): identical layouts, identical behavior.
//! - **own**: `platform` (host services per target), `entry`, `io`, `localtime` (UTC),
//!   `memory_usage`, `panic`, `prng`, `process`, `shared` and the current-thread executor in
//!   `task` (no tokio, no threads). Every
//!   function has exactly the signature `std/*.vlt` declares (WebAssembly links only exact
//!   matches; crates/velt_rt/tests/std_externs.rs checks both runtimes).
//!
//! Not provided: TCP, HTTP and child processes (programs using them fail to link, with a note).
#![allow(clippy::missing_safety_doc)]

#[path = "../../velt_rt/src/bigint.rs"]
pub mod bigint;
#[path = "../../velt_rt/src/bytes.rs"]
pub mod bytes;
#[path = "../../velt_rt/src/bytes_ops.rs"]
pub mod bytes_ops;
pub mod entry;
#[path = "../../velt_rt/src/fmt.rs"]
pub mod fmt;
#[path = "../../velt_rt/src/fnv.rs"]
pub mod fnv;
pub mod fs;
#[path = "../../velt_rt/src/handle.rs"]
pub mod handle;
#[path = "../../velt_rt/src/hash.rs"]
pub mod hash;
#[path = "../../velt_rt/src/html.rs"]
pub mod html;
#[path = "../../velt_rt/src/inspect.rs"]
pub mod inspect;
#[path = "../../velt_rt/src/inspect_layout/mod.rs"]
mod inspect_layout;
pub mod io;
pub mod json;
pub mod localtime;
#[path = "../../velt_rt/src/math.rs"]
pub mod math;
#[path = "../../velt_rt/src/mem.rs"]
pub mod mem;
pub mod memory_usage;
pub mod panic;
pub mod platform;
pub mod prng;
pub mod process;
#[path = "../../velt_rt/src/result.rs"]
pub mod result;
pub mod shared;
#[path = "../../velt_rt/src/str/mod.rs"]
pub mod str;
#[path = "../../velt_rt/src/str_array.rs"]
pub mod str_array;
#[path = "../../velt_rt/src/str_ops/mod.rs"]
pub mod str_ops;
#[path = "../../velt_rt/src/strbuf.rs"]
pub mod strbuf;
pub mod task;

pub use crate::str::VeltStr;
