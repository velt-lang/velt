//! `std/net`: TCP listeners and streams (`tcp.rs`, `stream.rs`) as leaf `VeltFut`s over tokio, with incremental
//! UTF-8 decoding for `readString` (`utf8.rs`). UDP sockets and DNS lookups are std's
//! (std/net/udp/udp_sched.velt, std/net/dns/lookup_sched.velt).

pub(crate) mod later;
pub mod park;
pub mod stream;
pub mod sys;
pub mod tcp;
pub(crate) mod utf8;
pub mod views;
