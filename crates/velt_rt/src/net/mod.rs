//! `std/net`: TCP, UDP and DNS are std's, on the executor's reactor (std/net/tcp_sched.velt,
//! std/net/udp/udp_sched.velt, std/net/dns/lookup_sched.velt). What is left here: the listener
//! `velt dev`'s supervisor hands over (`tcp.rs`) and the host's sockets of native libraries'
//! streams (`sys.rs`).

pub mod sys;
pub mod tcp;
