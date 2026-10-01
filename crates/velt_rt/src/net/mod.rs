//! `std/net`: TCP listeners and streams (`tcp.rs`) as leaf `VeltFut`s over tokio, with incremental
//! UTF-8 decoding for `readString` (`utf8.rs`), UDP sockets and DNS lookups (`udp.rs`).

pub mod tcp;
pub mod udp;
pub(crate) mod utf8;
