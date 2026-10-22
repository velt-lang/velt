//! `velt_native`: write the native half of a Velt package in Rust
//! (docs/internals/contracts/native_abi.md, docs/book/native-packages.md).
//!
//! ```ignore
//! use velt_native::{export, Error};
//!
//! velt_native::package!(greet);
//!
//! #[export]
//! fn velt_greet__hello(name: &str) -> String {
//!     format!("hello, {name}")
//! }
//!
//! #[export(blocking)]                       // `declare async function`: runs on the blocking pool
//! fn velt_greet__slow(n: u64) -> Result<u64, Error> {
//!     Ok(n * 2)
//! }
//! ```
//!
//! The Velt side declares exactly these signatures:
//!
//! ```ts
//! declare function velt_greet__hello(name: string): string;
//! declare async function velt_greet__slow(n: u64): Promise<IoResult<u64>>;
//! ```
//!
//! This crate contains no runtime code: everything it does goes through the function table
//! ([`Api`]) the program hands to `velt_native_init_<package>` at start-up. [`export`] also records
//! each function's signature (a `velt_sig_<name>` symbol) that `velt native build` reads, and
//! that `velt` checks every `declare` against.

use std::borrow::Cow;
use std::ffi::c_void;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicPtr, Ordering};

pub use velt_native_macros::export;

mod abi;
mod future;
pub mod handles;
pub mod net;
pub mod rt;
#[doc(hidden)]
pub mod sig;
mod strbuf;
pub mod time;
pub mod value;
pub use abi::{__from_raw_scoped, DirectRet, OutRet, OwnedParam, Param, ResultValue};
#[doc(hidden)]
pub use future::{__blocking, __owned};
pub use future::{Completer, ErrorRet, Future, FutureRet};
pub use strbuf::StrBuf;
pub use value::{Array, Owned, Record, Str, Value};
pub use velt_native_macros::Value;

// `export` expands to `::velt_native::...` paths, also in this crate's own tests.
extern crate self as velt_native;

/// The table version this crate needs (the runtime's must be at least this).
pub const ABI_VERSION: u32 = 1;

/// A Velt `string` (24 bytes, layout owned by the runtime; read it through [`Api`]).
#[repr(C)]
pub struct VeltStr {
    words: [u64; 3],
}

impl VeltStr {
    /// The empty string.
    pub const fn empty() -> VeltStr {
        VeltStr { words: [0; 3] }
    }

    /// Whether this is the all-zero empty string (nothing to release).
    #[inline]
    pub(crate) fn is_empty_value(&self) -> bool {
        self.words == [0; 3]
    }
}

/// A Velt `u8[]` (24 bytes, layout owned by the runtime; read it through [`Api::bytes_data`]).
#[repr(C)]
pub struct VeltBytes {
    words: [u64; 3],
}

impl VeltBytes {
    /// The empty array.
    pub const fn empty() -> VeltBytes {
        VeltBytes { words: [0; 3] }
    }
}

/// `VeltErr`: the error header of `IoResult<T>` and `IoStatus` (rt_abi_async.md §3).
#[repr(C)]
pub struct VeltErr {
    /// 0 = success.
    pub code: i32,
    /// Padding.
    pub pad: u32,
    /// Owned message on failure, empty on success.
    pub message: VeltStr,
}

/// `IoResult<T>`: `{ VeltErr err; T value; }`, value at offset 32.
#[repr(C)]
pub struct IoResultSlot<T> {
    /// Error header.
    pub err: VeltErr,
    /// Value (zeroed on failure).
    pub value: MaybeUninit<T>,
}

/// The work function of a blocking future.
pub type WorkFn = unsafe extern "C" fn(ctx: *mut c_void, out: *mut u8);
/// Frees the context of a blocking future that never ran.
pub type DropCtxFn = unsafe extern "C" fn(ctx: *mut c_void);
/// Drops a result nobody claimed.
pub type DropResultFn = unsafe extern "C" fn(result: *mut u8);

/// The runtime's function table, version 1 (native_abi.md "The function table"). Append-only:
/// fields after these exist when `abi_version` is higher.
#[repr(C)]
pub struct Api {
    /// The table version.
    pub abi_version: u32,
    /// The table size in bytes.
    pub size: u32,
    /// `(ptr, len, out)`: owned string from UTF-8 bytes.
    pub str_new: unsafe extern "C" fn(*const u8, usize, *mut VeltStr),
    /// `(s, len_out) -> ptr`: a string's bytes.
    pub str_bytes: unsafe extern "C" fn(*const VeltStr, *mut usize) -> *const u8,
    /// Drop an owned string.
    pub str_drop: unsafe extern "C" fn(*mut VeltStr),
    /// `(ptr, len, out)`: owned `u8[]` copied from bytes.
    pub bytes_new: unsafe extern "C" fn(*const u8, usize, *mut VeltBytes),
    /// Drop an owned `u8[]`.
    pub bytes_drop: unsafe extern "C" fn(*mut VeltBytes),
    /// `(b, len_out) -> ptr`: a `u8[]`'s bytes (borrowed).
    pub bytes_data: unsafe extern "C" fn(*const VeltBytes, *mut usize) -> *const u8,
    /// `(work, ctx, drop_ctx, result_size, drop_result) -> VeltFut*`.
    pub fut_blocking: unsafe extern "C" fn(
        WorkFn,
        *mut c_void,
        DropCtxFn,
        usize,
        Option<DropResultFn>,
    ) -> *mut c_void,
    /// `(result_size, drop_result, handle_out) -> VeltFut*`.
    pub fut_completer: unsafe extern "C" fn(usize, Option<DropResultFn>, *mut u64) -> *mut c_void,
    /// `(handle, result)`.
    pub complete: unsafe extern "C" fn(u64, *const u8),
    /// `(msg, len)`: report like a Velt panic and exit.
    pub fatal: unsafe extern "C" fn(*const u8, usize) -> !,
    // Async I/O (native_abi.md "Async I/O"); see [`rt`], [`net`], [`time`].
    /// `(poll, state, drop_state, result_size) -> VeltFut*`.
    pub fut_poll: unsafe extern "C" fn(rt::PollFn, *mut c_void, DropCtxFn, usize) -> *mut c_void,
    /// `(cx) -> runtime waker` (borrowed for the poll).
    pub cx_waker: unsafe extern "C" fn(*mut c_void) -> *const c_void,
    /// `(runtime waker) -> owned runtime waker`.
    pub waker_clone: unsafe extern "C" fn(*const c_void) -> *mut c_void,
    /// `(owned waker)`: wake and free.
    pub waker_wake: unsafe extern "C" fn(*mut c_void),
    /// `(runtime waker)`: wake, keep.
    pub waker_wake_by_ref: unsafe extern "C" fn(*const c_void),
    /// `(owned waker)`: free.
    pub waker_drop: unsafe extern "C" fn(*mut c_void),
    /// `(VeltFut*, waker) -> READY/PENDING`.
    pub fut_poll_ext: unsafe extern "C" fn(*mut c_void, *const rt::WakerArg) -> u32,
    /// `(VeltFut*)`: free.
    pub fut_drop: unsafe extern "C" fn(*mut c_void),
    /// `(VeltFut*)`: run as a detached task.
    pub spawn: unsafe extern "C" fn(*mut c_void),
    /// `(addr, len) -> VeltFut*` of `IoResult<u64>`.
    pub tcp_connect: unsafe extern "C" fn(*const u8, usize) -> *mut c_void,
    /// `(socket, out) -> err`.
    pub stream_from_socket: unsafe extern "C" fn(i64, *mut u64) -> i32,
    /// `(h, waker, buf, cap, n, err) -> READY/PENDING`.
    pub stream_poll_read:
        unsafe extern "C" fn(u64, *const rt::WakerArg, *mut u8, usize, *mut usize, *mut i32) -> u32,
    /// `(h, waker, buf, len, n, err) -> READY/PENDING`.
    pub stream_poll_write: unsafe extern "C" fn(
        u64,
        *const rt::WakerArg,
        *const u8,
        usize,
        *mut usize,
        *mut i32,
    ) -> u32,
    /// `(h, waker, err) -> READY/PENDING`.
    pub stream_poll_flush: unsafe extern "C" fn(u64, *const rt::WakerArg, *mut i32) -> u32,
    /// `(h, waker, err) -> READY/PENDING`.
    pub stream_poll_shutdown: unsafe extern "C" fn(u64, *const rt::WakerArg, *mut i32) -> u32,
    /// `(h) -> socket`.
    pub stream_socket: unsafe extern "C" fn(u64) -> i64,
    /// `(h)`: close and free.
    pub stream_close: unsafe extern "C" fn(u64),
    /// `() -> ns`.
    pub now_ns: extern "C" fn() -> u64,
    /// `(deadline_ns) -> sleep`.
    pub sleep_new: extern "C" fn(u64) -> *mut c_void,
    /// `(sleep, waker) -> READY/PENDING`.
    pub sleep_poll: unsafe extern "C" fn(*mut c_void, *const rt::WakerArg) -> u32,
    /// `(sleep, deadline_ns)`.
    pub sleep_reset: unsafe extern "C" fn(*mut c_void, u64),
    /// `(sleep)`: free.
    pub sleep_drop: unsafe extern "C" fn(*mut c_void),
    /// `() -> worker` (-1: none).
    pub worker_index: extern "C" fn() -> i64,
    /// `(size, align) -> ptr`: the runtime's allocator (for values Velt frees).
    pub alloc: extern "C" fn(u64, u64) -> *mut u8,
    /// `(p, old_size, align, new_size) -> ptr`.
    pub realloc: unsafe extern "C" fn(*mut u8, u64, u64, u64) -> *mut u8,
    /// `(p, size, align)`.
    pub free: unsafe extern "C" fn(*mut u8, u64, u64),
    /// `(ptr, len, out)`: a string of UTF-8 bytes (trusted: valid UTF-8).
    pub str_from_utf8: unsafe extern "C" fn(*const u8, usize, *mut VeltStr),
    /// `(s, out)`: another reference to a string.
    pub str_retain: unsafe extern "C" fn(*const VeltStr, *mut VeltStr),
    /// `(buf, min, avail) -> ptr`: room to write at the end of a builder.
    pub strbuf_spare: unsafe extern "C" fn(*mut VeltStr, u64, *mut u64) -> *mut u8,
    /// `(buf, n, ascii)`: the bytes written become text (counted unless `ascii` is 1).
    pub strbuf_commit: unsafe extern "C" fn(*mut VeltStr, u64, u8),
    /// `(s, len_out) -> ptr`: the bytes when they are UTF-8, else null.
    pub str_utf8: unsafe extern "C" fn(*const VeltStr, *mut usize) -> *const u8,
    /// `(poll, drop_state, result_size, state_size, state_align, state_out) -> fut`: `fut_poll`
    /// with the state in the future's block (written to `*state_out`, dropped in place).
    pub fut_poll_in: unsafe extern "C" fn(
        rt::PollFn,
        DropCtxFn,
        usize,
        usize,
        usize,
        *mut *mut c_void,
    ) -> *mut c_void,
}

static API: AtomicPtr<Api> = AtomicPtr::new(std::ptr::null_mut());

/// The table (after `velt_native_init_<package>` ran, or for a library linked into the runtime
/// itself, after the runtime's start-up handed it over: [`__embed`]).
pub fn api() -> &'static Api {
    let p = API.load(Ordering::Acquire);
    if p.is_null() {
        eprintln!("error: a native Velt function ran before its package was initialized");
        std::process::abort();
    }
    // SAFETY: set once from the runtime's static table, which lives for the whole program.
    unsafe { &*p }
}

/// Defines `velt_native_init_<package>`: `velt_native::package!(sqlite);` once per crate.
#[macro_export]
macro_rules! package {
    ($name:ident) => {
        #[export_name = concat!("velt_native_init_", stringify!($name))]
        pub unsafe extern "C" fn __velt_native_init(api: *const $crate::Api) -> i32 {
            $crate::__init(api, stringify!($name))
        }
    };
}

/// # Safety
/// `api` must be null or point to a function table that lives for the whole program.
#[doc(hidden)]
pub unsafe fn __init(api: *const Api, package: &str) -> i32 {
    if api.is_null() {
        return 2;
    }
    let (version, size) = ((*api).abi_version, (*api).size as usize);
    let needed = std::mem::size_of::<Api>();
    if version < ABI_VERSION || size < needed {
        eprintln!(
            "error: the native library of package `{package}` needs Velt native ABI {ABI_VERSION} \
             ({needed}-byte table), but this program's runtime provides ABI {version} ({size}-byte \
             table); update velt and rebuild"
        );
        return 1;
    }
    API.store(api as *mut Api, Ordering::Release);
    // Panics are caught at every export; don't let std's hook print them as well.
    std::panic::set_hook(Box::new(|_| {}));
    0
}

/// The runtime's own start-up, for the libraries linked into it (std's drivers): they share its
/// table, and its std (so the panic hook stays the runtime's).
///
/// # Safety
/// `api` must point to the runtime's table, of this SDK's layout, which lives for the whole
/// program.
#[doc(hidden)]
pub unsafe fn __embed(api: *const Api) {
    API.store(api as *mut Api, Ordering::Release);
}

/// Report a fatal error like a Velt panic (`panic: <msg>`, exit 101).
pub fn fatal(msg: &str) -> ! {
    // SAFETY: valid bytes for the call.
    unsafe { (api().fatal)(msg.as_ptr(), msg.len()) }
}

/// Error codes of `IoError.code` (rt_abi_async.md §3).
pub mod code {
    /// `ENOENT`
    pub const NOT_FOUND: i32 = 1;
    /// `EACCES`
    pub const PERMISSION_DENIED: i32 = 2;
    /// `EEXIST`
    pub const ALREADY_EXISTS: i32 = 3;
    /// `EINVAL`
    pub const INVALID_INPUT: i32 = 4;
    /// `EILSEQ`
    pub const INVALID_DATA: i32 = 5;
    /// `ETIMEDOUT`
    pub const TIMED_OUT: i32 = 6;
    /// `ECONNREFUSED`
    pub const CONNECTION_REFUSED: i32 = 7;
    /// `ECONNRESET`
    pub const CONNECTION_RESET: i32 = 8;
    /// `EADDRINUSE`
    pub const ADDR_IN_USE: i32 = 9;
    /// `EPIPE`
    pub const BROKEN_PIPE: i32 = 10;
    /// `EOF`
    pub const UNEXPECTED_EOF: i32 = 11;
    /// `ENOTSUP`
    pub const UNSUPPORTED: i32 = 12;
    /// `ENOTDIR`
    pub const NOT_A_DIRECTORY: i32 = 13;
    /// `ENOTEMPTY`
    pub const DIRECTORY_NOT_EMPTY: i32 = 14;
    /// `EISDIR`
    pub const IS_A_DIRECTORY: i32 = 15;
    /// `EBADF`: a closed handle.
    pub const BAD_HANDLE: i32 = 16;
    /// `UNKNOWN`
    pub const OTHER: i32 = 99;

    /// The Node-style name of a code (`IoError.code` in Velt: `"ENOENT"`, `"ECONNREFUSED"`...).
    pub fn name(c: i32) -> &'static str {
        match c {
            0 => "",
            NOT_FOUND => "ENOENT",
            PERMISSION_DENIED => "EACCES",
            ALREADY_EXISTS => "EEXIST",
            INVALID_INPUT => "EINVAL",
            INVALID_DATA => "EILSEQ",
            TIMED_OUT => "ETIMEDOUT",
            CONNECTION_REFUSED => "ECONNREFUSED",
            CONNECTION_RESET => "ECONNRESET",
            ADDR_IN_USE => "EADDRINUSE",
            BROKEN_PIPE => "EPIPE",
            UNEXPECTED_EOF => "EOF",
            UNSUPPORTED => "ENOTSUP",
            NOT_A_DIRECTORY => "ENOTDIR",
            DIRECTORY_NOT_EMPTY => "ENOTEMPTY",
            IS_A_DIRECTORY => "EISDIR",
            BAD_HANDLE => "EBADF",
            _ => "UNKNOWN",
        }
    }

    /// The code of an I/O error (as the runtime codes it).
    pub fn of(e: &std::io::Error) -> i32 {
        use std::io::ErrorKind as K;
        match e.kind() {
            K::NotFound => NOT_FOUND,
            K::PermissionDenied => PERMISSION_DENIED,
            K::AlreadyExists => ALREADY_EXISTS,
            K::InvalidInput => INVALID_INPUT,
            K::InvalidData => INVALID_DATA,
            K::TimedOut => TIMED_OUT,
            K::ConnectionRefused => CONNECTION_REFUSED,
            K::ConnectionReset | K::ConnectionAborted => CONNECTION_RESET,
            K::AddrInUse => ADDR_IN_USE,
            K::BrokenPipe => BROKEN_PIPE,
            K::UnexpectedEof => UNEXPECTED_EOF,
            K::Unsupported => UNSUPPORTED,
            K::NotADirectory => NOT_A_DIRECTORY,
            K::IsADirectory => IS_A_DIRECTORY,
            K::DirectoryNotEmpty => DIRECTORY_NOT_EMPTY,
            _ => OTHER,
        }
    }
}

/// The error half of `IoResult<T>` / `IoStatus`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    /// A [`code`] (or a package-defined code above 1000).
    pub code: i32,
    /// Message (`IoError.message` in Velt).
    pub message: String,
}

impl Error {
    /// An error with `code` and `message`. `code` must not be 0.
    pub fn new(code: i32, message: impl Into<String>) -> Error {
        Error {
            code: if code == 0 { code::OTHER } else { code },
            message: message.into(),
        }
    }

    /// An error with code [`code::OTHER`].
    pub fn other(message: impl Into<String>) -> Error {
        Error::new(code::OTHER, message)
    }

    /// The error of an operation on a closed handle (`EBADF`, "handle is closed").
    pub fn closed() -> Error {
        Error::new(code::BAD_HANDLE, "handle is closed")
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        Error::new(code::of(&e), e.to_string())
    }
}

/// The text of an owned runtime string, which is freed.
pub(crate) fn take_str(mut s: VeltStr) -> String {
    // SAFETY: an owned string, read then dropped once.
    unsafe {
        let text = str_of(&s).into_owned();
        (api().str_drop)(&mut s);
        text
    }
}

pub(crate) fn new_str(s: &str) -> VeltStr {
    let mut out = VeltStr::empty();
    // SAFETY: valid bytes (UTF-8: a `&str`) and out-pointer.
    unsafe { (api().str_from_utf8)(s.as_ptr(), s.len(), &mut out) };
    out
}

/// The text of a Velt string: borrowed when it is UTF-8 (well-formed, the usual case), else a
/// converted copy. A Velt string is UTF-16 text stored as WTF-8 (native_abi.md "Strings"): a
/// lone surrogate (`ED A0..BF xx`) becomes one U+FFFD, the same length, as at every other
/// output of a program.
///
/// # Safety
/// `s` must point to a live Velt string; the result borrows it.
pub(crate) unsafe fn str_of<'a>(s: *const VeltStr) -> Cow<'a, str> {
    let mut len = 0usize;
    // The string knows whether it is UTF-8: borrowed without a check then.
    let p = (api().str_utf8)(s, &mut len);
    if !p.is_null() {
        if len == 0 {
            return Cow::Borrowed("");
        }
        // SAFETY: the runtime says these bytes are UTF-8; they live as long as `s`.
        return Cow::Borrowed(std::str::from_utf8_unchecked(std::slice::from_raw_parts(
            p, len,
        )));
    }
    let p = (api().str_bytes)(s, &mut len);
    let bytes = if len == 0 {
        &[][..]
    } else {
        std::slice::from_raw_parts(p, len)
    };
    match std::str::from_utf8(bytes) {
        Ok(s) => Cow::Borrowed(s),
        Err(_) => Cow::Owned(wtf8_to_utf8_lossy(bytes)),
    }
}

/// WTF-8 as UTF-8: each lone surrogate (`ED A0..BF xx`) becomes one U+FFFD (3 bytes, as the
/// surrogate); anything else that is not UTF-8 (never in a runtime's string) by the WHATWG rule.
fn wtf8_to_utf8_lossy(bytes: &[u8]) -> String {
    let mut v = bytes.to_vec();
    let mut i = 0;
    while i + 2 < v.len() {
        if v[i] == 0xED && v[i + 1] >= 0xA0 {
            v[i..i + 3].copy_from_slice("\u{FFFD}".as_bytes());
            i += 3;
        } else {
            i += 1;
        }
    }
    match String::from_utf8(v) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    }
}

/// Owner of the text converted for `&str` arguments of one call (see [`__from_raw_scoped`]):
/// an argument with a lone surrogate is converted into a copy that lives here until the
/// generated wrapper returns.
#[doc(hidden)]
#[derive(Default)]
pub struct __Scope {
    texts: std::cell::RefCell<Vec<Box<str>>>,
}

impl __Scope {
    /// Keep `text` until the scope ends and borrow it.
    fn keep(&self, text: String) -> &str {
        let text = text.into_boxed_str();
        let p: *const str = &*text;
        self.texts.borrow_mut().push(text);
        // SAFETY: the box is never removed or dropped before `self`, and moving a box into the
        // vector does not move its heap text.
        unsafe { &*p }
    }
}

/// The message of a caught panic.
#[doc(hidden)]
pub fn __panic_message(p: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".into()
    }
}

#[cfg(test)]
mod tests;
