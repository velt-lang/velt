//! C layout for fallible runtime operations: `VeltErr` and `IoResult<T>`
//! (docs/internals/contracts/rt_abi_async.md, "Results and errors").
//!
//! Generated code cannot receive Rust enums, so every fallible operation writes an `IoResult<T>`
//! into caller-provided (or rt-future-owned) memory: a 32-byte error header followed by the value at
//! offset 32. `code == 0` means success and the value is initialized; any other code means failure,
//! the value bytes are zeroed and must not be read or dropped, and `message` is an owned string.

use crate::str::VeltStr;
use std::io;
use std::mem::MaybeUninit;
use std::path::Path;

/// Stable error codes (`VeltErr::code`). `std/fs` and `std/net` map them to error classes.
pub mod code {
    /// Success.
    pub const OK: i32 = 0;
    /// File or host not found.
    pub const NOT_FOUND: i32 = 1;
    /// Permission denied.
    pub const PERMISSION_DENIED: i32 = 2;
    /// Target already exists.
    pub const ALREADY_EXISTS: i32 = 3;
    /// Invalid argument (bad path, bad URL, bad header...).
    pub const INVALID_INPUT: i32 = 4;
    /// Data is not valid (e.g. not UTF-8 when a string was requested).
    pub const INVALID_DATA: i32 = 5;
    /// Operation timed out.
    pub const TIMED_OUT: i32 = 6;
    /// Connection refused by the peer.
    pub const CONNECTION_REFUSED: i32 = 7;
    /// Connection reset or aborted by the peer.
    pub const CONNECTION_RESET: i32 = 8;
    /// Address already in use.
    pub const ADDR_IN_USE: i32 = 9;
    /// Writing to a closed connection.
    pub const BROKEN_PIPE: i32 = 10;
    /// Unexpected end of stream.
    pub const UNEXPECTED_EOF: i32 = 11;
    /// Operation not supported (e.g. `https` URLs in `fetch`).
    pub const UNSUPPORTED: i32 = 12;
    /// A directory was expected but something else was found.
    pub const NOT_A_DIRECTORY: i32 = 13;
    /// Directory is not empty.
    pub const DIRECTORY_NOT_EMPTY: i32 = 14;
    /// A file was expected but a directory was found.
    pub const IS_A_DIRECTORY: i32 = 15;
    /// The handle (socket, file stream, child process, WebSocket) was already closed.
    pub const BAD_HANDLE: i32 = 16;
    /// Any other failure; see the message.
    pub const OTHER: i32 = 99;
}

/// `{ int32_t code; uint32_t _pad; VeltStr message; }` — size 32, align 8.
#[repr(C)]
#[derive(Debug)]
pub struct VeltErr {
    /// One of [`code`]; 0 = success.
    pub code: i32,
    /// Always 0.
    pub pad: u32,
    /// Empty static string on success, owned message on failure.
    pub message: VeltStr,
}

const _: () = assert!(std::mem::size_of::<VeltErr>() == 32);

impl VeltErr {
    /// The success value.
    pub const fn ok() -> VeltErr {
        VeltErr {
            code: code::OK,
            pad: 0,
            message: VeltStr::empty(),
        }
    }

    /// Error with an explicit code and message.
    pub fn new(code: i32, message: &str) -> VeltErr {
        VeltErr {
            code,
            pad: 0,
            message: VeltStr::from_vec(message.as_bytes().to_vec()),
        }
    }

    /// Map an `io::Error` to its code and display message.
    pub fn from_io(e: &io::Error) -> VeltErr {
        VeltErr::new(code_of(e), &e.to_string())
    }
}

/// The [`code`] of an I/O error.
pub(crate) fn code_of(e: &io::Error) -> i32 {
    use io::ErrorKind as K;
    match e.kind() {
        K::NotFound => code::NOT_FOUND,
        K::PermissionDenied => code::PERMISSION_DENIED,
        K::AlreadyExists => code::ALREADY_EXISTS,
        K::InvalidInput => code::INVALID_INPUT,
        K::InvalidData => code::INVALID_DATA,
        K::TimedOut => code::TIMED_OUT,
        K::ConnectionRefused => code::CONNECTION_REFUSED,
        K::ConnectionReset | K::ConnectionAborted => code::CONNECTION_RESET,
        K::AddrInUse => code::ADDR_IN_USE,
        K::BrokenPipe => code::BROKEN_PIPE,
        K::UnexpectedEof => code::UNEXPECTED_EOF,
        K::Unsupported => code::UNSUPPORTED,
        K::NotADirectory => code::NOT_A_DIRECTORY,
        K::IsADirectory => code::IS_A_DIRECTORY,
        K::DirectoryNotEmpty => code::DIRECTORY_NOT_EMPTY,
        _ => code::OTHER,
    }
}

/// `{ VeltErr err; T value; }` — `value` at offset 32 for any `T` with align <= 8.
#[repr(C)]
pub struct IoResult<T> {
    /// Error header; `err.code == 0` means `value` is initialized.
    pub err: VeltErr,
    /// The success value (zeroed on failure).
    pub value: MaybeUninit<T>,
}

impl<T> IoResult<T> {
    /// Successful result.
    pub fn ok(value: T) -> Self {
        IoResult {
            err: VeltErr::ok(),
            value: MaybeUninit::new(value),
        }
    }

    /// Failed result with zeroed value bytes.
    pub fn err(err: VeltErr) -> Self {
        IoResult {
            err,
            value: MaybeUninit::zeroed(),
        }
    }

    /// Convert a Rust result, mapping the success value with `f`.
    pub fn from_io<U>(r: io::Result<U>, f: impl FnOnce(U) -> T) -> Self {
        match r {
            Ok(v) => IoResult::ok(f(v)),
            Err(e) => IoResult::err(VeltErr::from_io(&e)),
        }
    }

    /// Write `self` to `out`, the result slot of a sync ABI function (which returns nothing:
    /// rt_abi_async.md §3.1).
    ///
    /// # Safety
    /// `out` must be valid for writes of `IoResult<T>`.
    pub unsafe fn write_to(self, out: *mut IoResult<T>) {
        out.write(self);
    }
}

/// Node-style name of an error code (`IoError.code` in Velt: `"ENOENT"`, `"EACCES"`...).
pub fn code_name(c: i32) -> &'static str {
    match c {
        code::OK => "",
        code::NOT_FOUND => "ENOENT",
        code::PERMISSION_DENIED => "EACCES",
        code::ALREADY_EXISTS => "EEXIST",
        code::INVALID_INPUT => "EINVAL",
        code::INVALID_DATA => "EILSEQ",
        code::TIMED_OUT => "ETIMEDOUT",
        code::CONNECTION_REFUSED => "ECONNREFUSED",
        code::CONNECTION_RESET => "ECONNRESET",
        code::ADDR_IN_USE => "EADDRINUSE",
        code::BROKEN_PIPE => "EPIPE",
        code::UNEXPECTED_EOF => "EOF",
        code::UNSUPPORTED => "ENOTSUP",
        code::NOT_A_DIRECTORY => "ENOTDIR",
        code::DIRECTORY_NOT_EMPTY => "ENOTEMPTY",
        code::IS_A_DIRECTORY => "EISDIR",
        code::BAD_HANDLE => "EBADF",
        _ => "UNKNOWN",
    }
}

/// Node's description of an error code (libuv's `uv_strerror`), or the operating system's
/// message without Rust's ` (os error N)` suffix for codes Node has no fixed text for.
fn describe(e: &io::Error) -> String {
    let fixed = match code_of(e) {
        code::NOT_FOUND => "no such file or directory",
        code::PERMISSION_DENIED => "permission denied",
        code::ALREADY_EXISTS => "file already exists",
        code::INVALID_INPUT => "invalid argument",
        code::NOT_A_DIRECTORY => "not a directory",
        code::DIRECTORY_NOT_EMPTY => "directory not empty",
        code::IS_A_DIRECTORY => "illegal operation on a directory",
        _ => "",
    };
    if !fixed.is_empty() {
        return fixed.to_string();
    }
    let text = e.to_string();
    let text = match text.rfind(" (os error ") {
        Some(i) if text.ends_with(')') => &text[..i],
        _ => &text,
    };
    // Windows' texts are sentences ("… by another process."); Node's descriptions aren't.
    let text = text.strip_suffix('.').unwrap_or(text);
    let mut chars = text.chars();
    match chars.next() {
        Some(c) => c.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// `e` with Node's message for a failed file-system call: `ENOENT: no such file or directory,
/// open 'x'` (`rename 'a' -> 'b'` with a destination). The error kind (and so the code) is kept.
pub fn fs_error(e: io::Error, syscall: &str, path: &Path, dest: Option<&Path>) -> io::Error {
    let mut msg = format!(
        "{}: {}, {syscall} '{}'",
        code_name(code_of(&e)),
        describe(&e),
        path.display()
    );
    if let Some(d) = dest {
        msg.push_str(&format!(" -> '{}'", d.display()));
    }
    io::Error::new(e.kind(), msg)
}

/// `e` with Node's message for a failed read or write of an open file, which names no path:
/// `EISDIR: illegal operation on a directory, read`.
pub fn op_error(e: io::Error, syscall: &str) -> io::Error {
    let msg = format!("{}: {}, {syscall}", code_name(code_of(&e)), describe(&e));
    io::Error::new(e.kind(), msg)
}

/// Write the Node-style name of `code` (a static string: `cap == 0`, nothing to free) to `out`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_err_code_name(code: i32, out: *mut VeltStr) {
    out.write(VeltStr::from_static(code_name(code).as_bytes()));
}

/// Error for a byte buffer that is not valid UTF-8 where a string was requested.
pub fn invalid_utf8(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{what} is not valid UTF-8"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_and_codes() {
        assert_eq!(std::mem::offset_of!(IoResult<u64>, value), 32);
        assert_eq!(std::mem::offset_of!(IoResult<VeltStr>, value), 32);
        let e = io::Error::from(io::ErrorKind::NotFound);
        let r: IoResult<u8> = IoResult::from_io(Err(e), |v| v);
        assert_eq!(r.err.code, code::NOT_FOUND);
        assert_eq!(code_name(r.err.code), "ENOENT");
        assert!(!r.err.message.is_static());
        let mut m = r.err.message;
        unsafe { crate::str::velt_rt_str_drop(&mut m) };
        let ok: IoResult<u8> = IoResult::from_io(Ok(7u8), |v| v + 1);
        assert_eq!((ok.err.code, unsafe { ok.value.assume_init() }), (0, 8));
    }

    #[test]
    fn node_style_fs_messages() {
        let e = io::Error::from(io::ErrorKind::NotFound);
        let e = fs_error(e, "unlink", Path::new("x"), None);
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
        assert_eq!(
            e.to_string(),
            "ENOENT: no such file or directory, unlink 'x'"
        );
        let e = io::Error::from(io::ErrorKind::AlreadyExists);
        let e = fs_error(e, "rename", Path::new("a"), Some(Path::new("b")));
        assert_eq!(
            e.to_string(),
            "EEXIST: file already exists, rename 'a' -> 'b'"
        );
        let e = io::Error::other("Something odd (os error 5)");
        let e = fs_error(e, "open", Path::new("f"), None);
        assert_eq!(e.to_string(), "UNKNOWN: something odd, open 'f'");
        // Windows error 32 (ERROR_SHARING_VIOLATION) as Rust displays it.
        let text = concat!(
            "The process cannot access the file because it is being used by another process.",
            " (os error 32)"
        );
        let e = fs_error(io::Error::other(text), "open", Path::new("x"), None);
        assert_eq!(
            e.to_string(),
            concat!(
                "UNKNOWN: the process cannot access the file because it is being used by another",
                " process, open 'x'"
            )
        );
        let e = op_error(io::Error::from(io::ErrorKind::IsADirectory), "read");
        assert_eq!(
            e.to_string(),
            "EISDIR: illegal operation on a directory, read"
        );
    }
}
