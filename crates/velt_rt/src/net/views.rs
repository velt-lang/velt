//! Streams and the program's own buffers (`TcpStream.readInto`, and writes of a `Uint8Array`;
//! rt_abi_async.md §14.8): a synchronous read into a borrowed `u8[]` range, a wait for
//! readability that holds no buffer, and writes of a range. No borrow outlives a call: std's
//! `readInto` tries first and only awaits `velt_rt_tcp_readable` when nothing is there, then
//! tries again.

use super::tcp::{try_write_now, write_copy, StreamHandle, STREAMS};
use crate::bytes::VeltBytes;
use crate::result::IoResult;
use crate::task::leaf::new_leaf;
use crate::task::VeltFut;
use std::io;

/// `velt_rt_tcp_try_read_into(s, data, start, end)` → `IoResult<i64>`: reads what is there into
/// `data[start..end]` (a borrowed `u8[]` it writes), without waiting: the bytes read, 0 at the
/// end of the stream, -1 when there is nothing yet. An empty or out-of-range range reads
/// nothing (-1 is never returned for it: 0 would read as the end, so std does not call with
/// one).
///
/// # Safety
/// `data` is a valid `u8[]` the caller lends for the call; `out` is writable.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_try_read_into(
    s: StreamHandle,
    data: *const VeltBytes,
    start: u64,
    end: u64,
    out: *mut IoResult<i64>,
) {
    let b = &*data;
    let (start, end) = (start.min(b.len), end.min(b.len));
    if start >= end {
        return out.write(IoResult::ok(0));
    }
    let Some(obj) = STREAMS.get(s) else {
        return out.write(IoResult::err(crate::registry::closed_error()));
    };
    // SAFETY: the caller lends `data` for the call; `start < end <= len`.
    let dst = std::slice::from_raw_parts_mut(b.ptr.add(start as usize), (end - start) as usize);
    let r = match obj.try_read_into(dst) {
        Ok(n) => IoResult::ok(n as i64),
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => IoResult::ok(-1),
        Err(e) => IoResult::err(crate::result::VeltErr::from_io(&e)),
    };
    out.write(r);
}

/// `velt_rt_tcp_readable(s)` → `IoResult<()>` once a `velt_rt_tcp_try_read_into` may find
/// data, the end of the stream or an error (it may also find nothing yet: try again).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_readable(s: StreamHandle) -> *mut VeltFut {
    STREAMS.op::<()>(s, |obj| {
        new_leaf(async move { IoResult::from_io(obj.readable().await, |()| ()) })
    })
}

/// The bytes `data[start..end]` (clamped to the array).
unsafe fn range<'a>(data: *const VeltBytes, start: u64, end: u64) -> &'a [u8] {
    let b = &*data;
    let (start, end) = (start.min(b.len), end.min(b.len));
    if start >= end {
        return &[];
    }
    std::slice::from_raw_parts(b.ptr.add(start as usize), (end - start) as usize)
}

/// `velt_rt_tcp_write_range(s, data, start, end)`: `velt_rt_tcp_write_bytes` of
/// `data[start..end]` (clamped to the array).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_write_range(
    s: StreamHandle,
    data: *const VeltBytes,
    start: u64,
    end: u64,
) -> *mut VeltFut {
    write_copy(s, range(data, start, end))
}

/// `velt_rt_tcp_try_write_range(s, data, start, end)`: `velt_rt_tcp_try_write` of
/// `data[start..end]` (clamped to the array).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_try_write_range(
    s: StreamHandle,
    data: *const VeltBytes,
    start: u64,
    end: u64,
) -> u64 {
    try_write_now(s, range(data, start, end))
}
