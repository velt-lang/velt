//! Windows transport of the dev channel: a local named pipe (`\\.\pipe\velt-dev-<pid>-<n>`).
//!
//! A handed-over listener travels as a `WSAPROTOCOL_INFOW` record: the supervisor calls
//! `WSADuplicateSocketW` for the requesting process (its id comes from the pipe,
//! `GetNamedPipeClientProcessId`) and replies `ok <record as hex>\n`; the program turns the record
//! into its own descriptor of the same socket with `WSASocketW(FROM_PROTOCOL_INFO)`. Both
//! descriptors name one socket, so the supervisor's keeps it (and its backlog) open across
//! program versions, as with `SCM_RIGHTS` on Unix.

use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, AsRawSocket, FromRawHandle, FromRawSocket, OwnedHandle};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Networking::WinSock::{
    WSADuplicateSocketW, WSASocketW, WSAStartup, FROM_PROTOCOL_INFO, INVALID_SOCKET, SOCKET,
    WSADATA, WSAPROTOCOL_INFOW, WSA_FLAG_NO_HANDLE_INHERIT, WSA_FLAG_OVERLAPPED,
};
use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

use super::MAX_LINE;

/// One connection on the dev channel (either end of a pipe instance).
pub type Stream = File;

/// How long a program retries while every pipe instance is busy.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Pipe buffer sizes (requests and replies are small; a build report streams through).
const PIPE_BUFFER: u32 = 4096;

/// Connect to the supervisor's dev channel. Between two connections the supervisor has no
/// free pipe instance for a moment (`ERROR_PIPE_BUSY`); retry then.
pub fn connect(socket: &OsStr) -> io::Result<Stream> {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    loop {
        match OpenOptions::new().read(true).write(true).open(socket) {
            Err(e) if retry_connect(&e) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(1));
            }
            other => return other,
        }
    }
}

fn retry_connect(e: &io::Error) -> bool {
    matches!(e.raw_os_error(), Some(code) if code as u32 == ERROR_PIPE_BUSY || code as u32 == ERROR_FILE_NOT_FOUND)
}

/// The supervisor's end of the dev channel: always one pipe instance waiting for the next
/// connection, created before the previous connection is handled.
pub struct Server {
    name: OsString,
    wide: Vec<u16>,
    next: Option<OwnedHandle>,
}

impl Server {
    /// Create the pipe (first instance: fails if another process already owns the name).
    pub fn bind() -> io::Result<Server> {
        let name = OsString::from(format!(r"\\.\pipe\{}", super::channel_name()));
        let wide = name.encode_wide().chain([0]).collect();
        let mut server = Server {
            name,
            wide,
            next: None,
        };
        server.next = Some(server.instance(true)?);
        Ok(server)
    }

    /// What to pass in `VELT_DEV_SOCKET`.
    pub fn name(&self) -> &OsStr {
        &self.name
    }

    /// Wait for the next connection.
    pub fn accept(&mut self) -> io::Result<Stream> {
        let pending = match self.next.take() {
            Some(handle) => handle,
            None => self.instance(false)?,
        };
        // SAFETY: a pipe handle this server created; blocking mode (no OVERLAPPED).
        let connected = unsafe { ConnectNamedPipe(pending.as_raw_handle(), std::ptr::null_mut()) }
            != 0
            // SAFETY: reads this thread's last error.
            || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
        let error = io::Error::last_os_error();
        self.next = self.instance(false).ok();
        if connected {
            Ok(File::from(pending))
        } else {
            Err(error)
        }
    }

    fn instance(&self, first: bool) -> io::Result<OwnedHandle> {
        let first = if first {
            FILE_FLAG_FIRST_PIPE_INSTANCE
        } else {
            0
        };
        // SAFETY: `wide` is NUL-terminated; default security (the creating user and
        // administrators); the handle is owned by the returned value.
        unsafe {
            let handle = CreateNamedPipeW(
                self.wide.as_ptr(),
                PIPE_ACCESS_DUPLEX | first,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                PIPE_BUFFER,
                PIPE_BUFFER,
                0,
                std::ptr::null(),
            );
            if handle == INVALID_HANDLE_VALUE {
                let e = io::Error::last_os_error();
                let name = self.name.to_string_lossy();
                return Err(io::Error::new(
                    e.kind(),
                    format!("cannot create `{name}`: {e}"),
                ));
            }
            Ok(OwnedHandle::from_raw_handle(handle))
        }
    }
}

/// Supervisor side: reply `ok` with `listener` duplicated for the process on the other end of
/// `stream` (the supervisor keeps its own descriptor).
pub fn reply_ok(stream: &Stream, listener: &TcpListener) -> io::Result<()> {
    let pid = peer_pid(stream)?;
    // SAFETY: `WSAPROTOCOL_INFOW` is plain data that the call fills in.
    let mut info: WSAPROTOCOL_INFOW = unsafe { std::mem::zeroed() };
    let socket = listener.as_raw_socket() as SOCKET;
    // SAFETY: a valid socket (std initialized Winsock when it bound it) and an out-pointer.
    if unsafe { WSADuplicateSocketW(socket, pid, &mut info) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut line = String::with_capacity(4 + 2 * std::mem::size_of_val(&info));
    line.push_str("ok ");
    for byte in record_bytes(&info) {
        line.push_str(&format!("{byte:02x}"));
    }
    line.push('\n');
    (&*stream).write_all(line.as_bytes())
}

/// Supervisor side: the process id of the program on the other end of `stream`.
pub fn peer_pid(stream: &Stream) -> io::Result<u32> {
    let mut pid = 0u32;
    // SAFETY: `stream` is the server end of a connected pipe; `pid` is an out-pointer.
    if unsafe { GetNamedPipeClientProcessId(stream.as_raw_handle(), &mut pid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(pid)
}

/// Program side: the listener from an `ok <record>` reply, or the `err` reply as an error.
pub(super) fn receive_listener(stream: &Stream) -> io::Result<TcpListener> {
    let mut line = String::new();
    BufReader::new(stream.take(MAX_LINE as u64)).read_line(&mut line)?;
    let line = line.trim_end();
    let Some(hex) = line.strip_prefix("ok ") else {
        let message = line.strip_prefix("err ").unwrap_or(line);
        return Err(io::Error::other(message.to_string()));
    };
    // SAFETY: plain data, overwritten from the reply below.
    let mut info: WSAPROTOCOL_INFOW = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info);
    let bytes = decode_hex(hex).filter(|b| b.len() == size).ok_or_else(|| {
        io::Error::other(format!(
            "velt dev: malformed listener record ({} chars)",
            hex.len()
        ))
    })?;
    // SAFETY: `bytes` holds exactly one `WSAPROTOCOL_INFOW` (plain data, any bit pattern).
    unsafe {
        std::ptr::copy_nonoverlapping(
            bytes.as_ptr(),
            (&mut info as *mut WSAPROTOCOL_INFOW).cast(),
            size,
        );
    }
    start_winsock();
    // SAFETY: `info` came from `WSADuplicateSocketW` for this process. Same flags as std's
    // sockets (overlapped for the async runtime, not inherited by child processes).
    let socket = unsafe {
        WSASocketW(
            FROM_PROTOCOL_INFO,
            FROM_PROTOCOL_INFO,
            FROM_PROTOCOL_INFO,
            &info,
            0,
            WSA_FLAG_OVERLAPPED | WSA_FLAG_NO_HANDLE_INHERIT,
        )
    };
    if socket == INVALID_SOCKET {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a new socket descriptor owned by nothing else.
    Ok(unsafe { TcpListener::from_raw_socket(socket as _) })
}

/// The bytes of a protocol record.
fn record_bytes(info: &WSAPROTOCOL_INFOW) -> &[u8] {
    // SAFETY: plain `repr(C)` data, read as bytes for its whole size.
    unsafe {
        std::slice::from_raw_parts(
            (info as *const WSAPROTOCOL_INFOW).cast::<u8>(),
            std::mem::size_of_val(info),
        )
    }
}

fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Winsock must be started before `WSASocketW`; std starts it only inside its own calls.
fn start_winsock() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        // SAFETY: plain data out-parameter; the reference count stays taken for the process.
        let mut data: WSADATA = unsafe { std::mem::zeroed() };
        unsafe { WSAStartup(0x0202, &mut data) };
    });
}
