//! The listening socket `velt dev`'s supervisor hands over, which std registers with the
//! executor's reactor (std/net/tcp_sched.velt). Every other TCP socket of a native program is
//! std's.

use crate::result::IoResult;
use crate::str::VeltStr;
use crate::task::leaf::new_leaf;
use crate::task::VeltFut;

/// Default chunk size for `read()` / `readString()` without an explicit maximum.
pub const DEFAULT_READ: u64 = 64 * 1024;

/// Owned text of a string argument (copied: async operations outlive the call).
unsafe fn text_arg(s: *const VeltStr) -> String {
    (*s).text_lossy().into_owned()
}

/// `listen("host:port")` under `velt dev` → result slot `IoResult<u64>`: the listening socket the
/// supervisor hands over (a non-blocking descriptor, or a `SOCKET` on Windows), which the caller
/// owns and registers with the executor's reactor (std/net/tcp_sched.velt). Port 0 picks a free
/// port.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_listen(addr: *const VeltStr) -> *mut VeltFut {
    let addr = text_arg(addr);
    new_leaf(async move {
        IoResult::from_io(crate::dev::bind(addr.as_str()).await, |l| {
            #[cfg(unix)]
            let raw = std::os::fd::IntoRawFd::into_raw_fd(l) as u64;
            #[cfg(windows)]
            let raw = std::os::windows::io::IntoRawSocket::into_raw_socket(l);
            raw
        })
    })
}
