//! `std/net`'s remaining runtime part: the listening socket `velt dev`'s supervisor hands over
//! (`tcp.rs`). Native programs' TCP sockets, UDP sockets and DNS lookups are std's
//! (std/net/tcp_sched.velt, std/net/udp/udp_sched.velt, std/net/dns/lookup_sched.velt).

pub mod tcp;
