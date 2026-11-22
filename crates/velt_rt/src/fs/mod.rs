//! `std/fs`: the chunked reader/writer handles of `stream.rs`. The other file-system operations
//! are std code in a native build (std/fs/whole, tree, paths; #2196) and velt_rt_wasm's in a
//! WebAssembly one, which compiles `ops.rs` too.
//!
//! Every argument is copied at the call, so the caller keeps ownership of its strings and may drop
//! them right away. Data arguments are strings.

mod ops;
pub mod stream;

pub use ops::VeltStat;
