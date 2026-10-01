//! Unix transport of the dev channel: a Unix socket in the temp directory; a handed-over
//! listener travels as a file descriptor (`SCM_RIGHTS`) attached to the `ok` reply.

use std::ffi::OsStr;
use std::io::{self, BufRead, BufReader, Read};
use std::net::TcpListener;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use super::MAX_LINE;

/// One connection on the dev channel.
pub type Stream = UnixStream;

/// Connect to the supervisor's dev channel.
pub fn connect(socket: &OsStr) -> io::Result<Stream> {
    UnixStream::connect(socket)
}

/// The supervisor's end of the dev channel; the socket file is removed on drop.
pub struct Server {
    listener: UnixListener,
    path: PathBuf,
}

impl Server {
    /// Bind a Unix socket named after this process in the temp directory.
    pub fn bind() -> io::Result<Server> {
        let path = std::env::temp_dir().join(format!("{}.sock", super::channel_name()));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).map_err(|e| {
            io::Error::new(e.kind(), format!("cannot create `{}`: {e}", path.display()))
        })?;
        Ok(Server { listener, path })
    }

    /// What to pass in `VELT_DEV_SOCKET`.
    pub fn name(&self) -> &OsStr {
        self.path.as_os_str()
    }

    /// Wait for the next connection.
    pub fn accept(&mut self) -> io::Result<Stream> {
        self.listener.accept().map(|(stream, _)| stream)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Supervisor side: reply `ok` and pass `listener`'s descriptor (the supervisor keeps its own).
pub fn reply_ok(stream: &Stream, listener: &TcpListener) -> io::Result<()> {
    send_with_fd(stream, b"ok\n", listener.as_raw_fd())
}

/// Program side: the listener from an `ok` reply, or the `err` reply as an error.
pub(super) fn receive_listener(stream: &Stream) -> io::Result<TcpListener> {
    let (line, fd) = recv_line_with_fd(stream)?;
    match (line.strip_prefix("ok"), fd) {
        (Some(_), Some(fd)) => Ok(TcpListener::from(fd)),
        _ => Err(io::Error::other(
            line.strip_prefix("err ").unwrap_or(&line).to_string(),
        )),
    }
}

/// Buffer for one control message carrying one descriptor (`CMSG_SPACE(sizeof(int))`).
#[repr(C)]
union CmsgBuf {
    align: libc::cmsghdr,
    bytes: [u8; 64],
}

fn send_with_fd(stream: &UnixStream, data: &[u8], fd: RawFd) -> io::Result<()> {
    let mut iov = libc::iovec {
        iov_base: data.as_ptr() as *mut libc::c_void,
        iov_len: data.len(),
    };
    let mut cmsg = CmsgBuf { bytes: [0; 64] };
    // SAFETY: `msghdr` is plain data; every pointer set below outlives the `sendmsg` call, and the
    // control buffer is large and aligned enough for one `int` (checked by `CMSG_SPACE`).
    unsafe {
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        let space = libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) as usize;
        assert!(space <= 64, "ICE: control buffer too small");
        msg.msg_control = cmsg.bytes.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = space as _;
        let header = libc::CMSG_FIRSTHDR(&msg);
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<RawFd>() as u32) as _;
        std::ptr::write_unaligned(libc::CMSG_DATA(header) as *mut RawFd, fd);
        if libc::sendmsg(stream.as_raw_fd(), &msg, 0) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Read one reply line; the descriptor, if any, arrives with its first bytes.
fn recv_line_with_fd(stream: &UnixStream) -> io::Result<(String, Option<OwnedFd>)> {
    let mut data = vec![0u8; MAX_LINE];
    let mut cmsg = CmsgBuf { bytes: [0; 64] };
    let mut iov = libc::iovec {
        iov_base: data.as_mut_ptr() as *mut libc::c_void,
        iov_len: data.len(),
    };
    // SAFETY: as in `send_with_fd`; the kernel writes at most `msg_controllen` control bytes and
    // `iov_len` data bytes, and a received descriptor is owned by this process.
    let (len, fd) = unsafe {
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cmsg.bytes.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = 64;
        let n = libc::recvmsg(stream.as_raw_fd(), &mut msg, 0);
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut fd = None;
        let header = libc::CMSG_FIRSTHDR(&msg);
        if !header.is_null()
            && (*header).cmsg_level == libc::SOL_SOCKET
            && (*header).cmsg_type == libc::SCM_RIGHTS
        {
            let raw = std::ptr::read_unaligned(libc::CMSG_DATA(header) as *const RawFd);
            fd = Some(OwnedFd::from_raw_fd(raw));
        }
        (n as usize, fd)
    };
    data.truncate(len);
    // The rest of the line, if the first read was short.
    let mut reader = BufReader::new(stream.take((MAX_LINE - len) as u64));
    if !data.ends_with(b"\n") {
        reader.read_until(b'\n', &mut data)?;
    }
    let line = String::from_utf8_lossy(&data).trim_end().to_string();
    Ok((line, fd))
}
