//! `std/net`: TCP listeners and streams (`tcp.rs`, `stream.rs`) as leaf `VeltFut`s over tokio, with incremental
//! UTF-8 decoding for `readString` (`utf8.rs`), UDP sockets and DNS lookups (`udp.rs`).

#[cfg(test)]
mod handoff_tests;
pub mod stream;
pub mod sys;
pub mod tcp;
pub mod udp;
pub(crate) mod utf8;
pub mod views;
