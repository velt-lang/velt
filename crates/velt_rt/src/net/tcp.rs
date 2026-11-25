//! The listening socket `velt dev`'s supervisor hands over, which std registers with the
//! executor's reactor (std/net/tcp_sched.velt): every TCP socket is std's.

use super::sys::TcpListener;
use crate::result::IoResult;
use crate::str::VeltStr;
use crate::task::leaf::new_leaf;
use crate::task::VeltFut;

/// Default chunk size for a read without an explicit maximum.
pub const DEFAULT_READ: u64 = 64 * 1024;

/// `listen("host:port")` under `velt dev` → result slot `IoResult<u64>`: the listening socket the
/// supervisor hands over (a non-blocking descriptor, or a `SOCKET` on Windows), which the caller
/// owns and registers with the executor's reactor (std/net/tcp_sched.velt). Port 0 picks a free
/// port.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_listen(addr: *const VeltStr) -> *mut VeltFut {
    // Owned: the leaf outlives the call.
    let addr = (*addr).text_lossy().into_owned();
    new_leaf(async move {
        let r = crate::dev::bind(addr.as_str()).await;
        IoResult::from_io(r.and_then(TcpListener::into_raw), |fd| fd)
    })
}
