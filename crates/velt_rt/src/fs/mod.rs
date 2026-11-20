//! `std/fs`: blocking `*_sync` operations in `sync.rs` (a native build runs the whole-file
//! ones on the executor's blocking pool: std/fs/rt_sched.velt), and chunked reader/writer handles
//! in `stream.rs`; `remove.rs` is Windows' `rm`. velt_rt_wasm compiles `ops.rs` and `sync.rs`
//! with its own async variants.
//!
//! Every argument is copied at the call, so the caller keeps ownership of its strings and may drop
//! them right away. Data arguments are strings.

mod ops;
pub mod remove;
pub mod stream;
pub mod sync;

pub use ops::VeltStat;
