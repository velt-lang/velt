//! UDP sockets and DNS lookups over tokio.
//!
//! A socket handle is a key into a handle table (`crate::registry`): in-flight sends/receives
//! hold their own `Arc`, so `close` may be called any time, and a closed handle fails with
//! `EBADF`. Datagrams carry their peer as an `"ip:port"` string, the same address format
//! `connect`/`listen` take.

use super::tcp::text_arg;
use crate::bytes::VeltBytes;
use crate::registry::{closed_error, Key, Registry};
use crate::result::{code, IoResult, VeltErr};
use crate::str::VeltStr;
use crate::str_array::VeltStrArray;
use crate::task::leaf::new_leaf;
use crate::task::VeltFut;
use tokio::net::UdpSocket;

/// Opaque socket handle.
pub type UdpHandle = Key<UdpSocket>;

static SOCKETS: Registry<UdpSocket> = Registry::new();

/// `{ VeltBytes data; VeltStr addr; }` — a received datagram and its sender (48 bytes).
#[repr(C)]
pub struct VeltDatagram {
    /// The payload.
    pub data: VeltBytes,
    /// Sender as `"ip:port"`.
    pub addr: VeltStr,
}

/// `bindUdp("host:port")` → `IoResult<VeltUdp*>`; port 0 picks a free port.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_udp_bind(addr: *const VeltStr) -> *mut VeltFut {
    let addr = text_arg(addr);
    new_leaf(async move {
        IoResult::from_io(UdpSocket::bind(addr.as_str()).await, |s| SOCKETS.insert(s))
    })
}

/// The bound local port (0 on error or once closed).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_udp_port(u: UdpHandle) -> u32 {
    SOCKETS
        .get(u)
        .and_then(|s| s.local_addr().ok())
        .map_or(0, |a| a.port() as u32)
}

/// `sendTo(data, "host:port")` → `IoResult<u64>` bytes sent (the whole datagram or an error);
/// `data` (string or bytes) and the address are copied. Host names are resolved.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_udp_send_to(
    u: UdpHandle,
    data: *const VeltBytes,
    addr: *const VeltStr,
) -> *mut VeltFut {
    let data = (*data).as_bytes().to_vec();
    let addr = text_arg(addr);
    SOCKETS.op::<u64>(u, |sock| {
        new_leaf(async move {
            IoResult::from_io(sock.send_to(&data, addr.as_str()).await, |n| n as u64)
        })
    })
}

/// `recvFrom(max)` → `IoResult<VeltDatagram>`: the next datagram (truncated to `max` bytes;
/// `max == 0` ⇒ 64 KiB, the largest UDP payload).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_udp_recv_from(u: UdpHandle, max: u64) -> *mut VeltFut {
    let Some(sock) = SOCKETS.get(u) else {
        return crate::registry::closed_leaf::<VeltDatagram>();
    };
    let size = if max == 0 {
        65536
    } else {
        max.min(65536) as usize
    };
    new_leaf(async move {
        // Always receive into a full-size buffer and truncate afterwards: Windows fails a too-small
        // receive with WSAEMSGSIZE (losing the sender address) where Unix silently truncates.
        let mut buf = vec![0u8; 65536];
        IoResult::from_io(sock.recv_from(&mut buf).await, |(n, from)| {
            buf.truncate(n.min(size));
            VeltDatagram {
                data: VeltBytes::from_vec(buf),
                addr: VeltStr::from_vec(from.to_string().into_bytes()),
            }
        })
    })
}

/// Enable or disable sending to broadcast addresses.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_udp_set_broadcast(u: UdpHandle, on: u8, out: *mut VeltErr) {
    let e = match SOCKETS.get(u).map(|s| s.set_broadcast(on != 0)) {
        None => closed_error(),
        Some(Ok(())) => VeltErr::ok(),
        Some(Err(e)) => VeltErr::from_io(&e),
    };
    out.write(e);
}

/// Release a socket handle (closing it again, through any copy, is a no-op).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_udp_close(u: UdpHandle) {
    SOCKETS.remove(u);
}

/// `lookup(host)` → `IoResult<VeltStrArray>`: the host's IP addresses (IPv4 and IPv6, resolver
/// order, duplicates removed); `ENOENT` if the name does not resolve.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_dns_lookup(host: *const VeltStr) -> *mut VeltFut {
    let host = text_arg(host);
    new_leaf(async move {
        match tokio::net::lookup_host((host.as_str(), 0)).await {
            Ok(addrs) => {
                let mut ips: Vec<String> = Vec::new();
                for a in addrs {
                    let ip = a.ip().to_string();
                    if !ips.contains(&ip) {
                        ips.push(ip);
                    }
                }
                IoResult::ok(VeltStrArray::from_strings(ips))
            }
            Err(e) => IoResult::err(VeltErr::new(
                code::NOT_FOUND,
                &format!("getaddrinfo ENOTFOUND {host}: {e}"),
            )),
        }
    })
}
