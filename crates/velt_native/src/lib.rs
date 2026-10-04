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

mod future;
#[doc(hidden)]
pub use future::{__blocking, __owned};
pub use future::{Completer, ErrorRet, Future, FutureRet};

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
}

static API: AtomicPtr<Api> = AtomicPtr::new(std::ptr::null_mut());

/// The table (after `velt_native_init_<package>` ran).
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
    /// `ENOTSUP`
    pub const UNSUPPORTED: i32 = 12;
    /// `EBADF`: a closed handle.
    pub const BAD_HANDLE: i32 = 16;
    /// `UNKNOWN`
    pub const OTHER: i32 = 99;
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
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        use std::io::ErrorKind as K;
        let c = match e.kind() {
            K::NotFound => code::NOT_FOUND,
            K::PermissionDenied => code::PERMISSION_DENIED,
            K::AlreadyExists => code::ALREADY_EXISTS,
            K::InvalidInput => code::INVALID_INPUT,
            K::InvalidData => code::INVALID_DATA,
            K::TimedOut => code::TIMED_OUT,
            K::Unsupported => code::UNSUPPORTED,
            _ => code::OTHER,
        };
        Error::new(c, e.to_string())
    }
}

fn new_str(s: &str) -> VeltStr {
    let mut out = VeltStr::empty();
    // SAFETY: valid bytes and out-pointer.
    unsafe { (api().str_new)(s.as_ptr(), s.len(), &mut out) };
    out
}

fn new_bytes(b: &[u8]) -> VeltBytes {
    let mut out = VeltBytes::empty();
    // SAFETY: valid bytes and out-pointer.
    unsafe { (api().bytes_new)(b.as_ptr(), b.len(), &mut out) };
    out
}

/// The text of a Velt string: borrowed when it is UTF-8 (well-formed, the usual case), else a
/// converted copy. A Velt string is UTF-16 text stored as WTF-8 (native_abi.md "Strings"): a
/// lone surrogate (`ED A0..BF xx`) becomes one U+FFFD, the same length, as at every other
/// output of a program.
///
/// # Safety
/// `s` must point to a live Velt string; the result borrows it.
unsafe fn str_of<'a>(s: *const VeltStr) -> Cow<'a, str> {
    let mut len = 0usize;
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

/// # Safety
/// `b` must point to a live `u8[]`; the result borrows it.
unsafe fn bytes_of<'a>(b: *const VeltBytes) -> &'a [u8] {
    let mut len = 0usize;
    let p = (api().bytes_data)(b, &mut len);
    if len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(p, len)
    }
}

/// A parameter type of an exported function: how it crosses the C ABI and its signature name.
pub trait Param<'a>: Sized {
    /// The C type of the argument.
    type Raw: Copy;
    /// Its name in a signature (`u64`, `string`, `u8[]`).
    const SIG: &'static str;
    /// Converts the argument (borrowed for the duration of the call).
    ///
    /// # Safety
    /// `raw` must be a valid argument per native_abi.md.
    unsafe fn from_raw(raw: Self::Raw) -> Self;

    /// [`Self::from_raw`] in an exported function's wrapper, where `scope` can own a converted
    /// copy for the duration of the call (a `&str` of a string with a lone surrogate).
    ///
    /// # Safety
    /// As for [`Self::from_raw`].
    #[doc(hidden)]
    unsafe fn from_raw_in(raw: Self::Raw, scope: &'a __Scope) -> Self {
        let _ = scope;
        Self::from_raw(raw)
    }
}

/// Converts an argument of an exported function, borrowed no longer than `scope` (a local of the
/// generated wrapper): a parameter type that demands a longer borrow is a compile error, however
/// it is spelled.
///
/// ```compile_fail
/// type S = &'static str;
/// static mut KEPT: Option<S> = None;
///
/// #[velt_native::export]
/// fn velt_p__keep(s: S) -> u64 {
///     unsafe { KEPT = Some(s) };
///     0
/// }
/// ```
///
/// The same function without the `'static` alias compiles:
///
/// ```
/// #[velt_native::export]
/// fn velt_p__len(s: &str) -> u64 {
///     s.len() as u64
/// }
/// ```
///
/// # Safety
/// `raw` must be a valid argument per native_abi.md that lives at least as long as `scope`.
#[doc(hidden)]
pub unsafe fn __from_raw_scoped<'a, T: Param<'a>>(scope: &'a __Scope, raw: T::Raw) -> T {
    T::from_raw_in(raw, scope)
}

/// A parameter type of a `blocking` export: owned, so it can move to the blocking pool.
pub trait OwnedParam: for<'a> Param<'a> + Send + 'static {}

macro_rules! scalar_params {
    ($($t:ty => $sig:literal),*) => {$(
        impl<'a> Param<'a> for $t {
            type Raw = $t;
            const SIG: &'static str = $sig;
            unsafe fn from_raw(raw: $t) -> $t { raw }
        }
        impl OwnedParam for $t {}
        impl DirectRet for $t {
            const SIG: &'static str = $sig;
            fn panicked(msg: &str) -> $t { fatal(msg) }
        }
    )*};
}

scalar_params!(
    bool => "bool", u8 => "u8", u16 => "u16", u32 => "u32", u64 => "u64",
    i8 => "i8", i16 => "i16", i32 => "i32", i64 => "i64", f32 => "f32", f64 => "f64"
);

/// A string argument as `&str`: borrowed when well-formed. In an exported function a string
/// with a lone surrogate arrives converted (one U+FFFD each); outside one ([`Param::from_raw`]
/// called directly) there is nowhere to keep the copy, so that is a fatal error, as it was for
/// every ill-formed string before the SDK converted them.
impl<'a> Param<'a> for &'a str {
    type Raw = *const VeltStr;
    const SIG: &'static str = "string";
    unsafe fn from_raw(raw: *const VeltStr) -> &'a str {
        match str_of(raw) {
            Cow::Borrowed(s) => s,
            Cow::Owned(_) => fatal("a Velt string with a lone surrogate needs a `Cow<str>` here"),
        }
    }
    unsafe fn from_raw_in(raw: *const VeltStr, scope: &'a __Scope) -> &'a str {
        match str_of(raw) {
            Cow::Borrowed(s) => s,
            Cow::Owned(s) => scope.keep(s),
        }
    }
}

/// A string argument as `Cow<str>`: borrowed when well-formed, else converted (one U+FFFD per
/// lone surrogate).
impl<'a> Param<'a> for Cow<'a, str> {
    type Raw = *const VeltStr;
    const SIG: &'static str = "string";
    unsafe fn from_raw(raw: *const VeltStr) -> Cow<'a, str> {
        str_of(raw)
    }
}

impl<'a> Param<'a> for String {
    type Raw = *const VeltStr;
    const SIG: &'static str = "string";
    unsafe fn from_raw(raw: *const VeltStr) -> String {
        str_of(raw).into_owned()
    }
}
impl OwnedParam for String {}

impl<'a> Param<'a> for &'a [u8] {
    type Raw = *const VeltBytes;
    const SIG: &'static str = "u8[]";
    unsafe fn from_raw(raw: *const VeltBytes) -> &'a [u8] {
        bytes_of(raw)
    }
}

impl<'a> Param<'a> for Vec<u8> {
    type Raw = *const VeltBytes;
    const SIG: &'static str = "u8[]";
    unsafe fn from_raw(raw: *const VeltBytes) -> Vec<u8> {
        bytes_of(raw).to_vec()
    }
}
impl OwnedParam for Vec<u8> {}

/// A result returned directly (scalars and `()`).
pub trait DirectRet {
    /// Its name in a signature.
    const SIG: &'static str;
    /// What a panicking export returns (scalars: a fatal error).
    fn panicked(msg: &str) -> Self;
}

impl DirectRet for () {
    const SIG: &'static str = "void";
    fn panicked(msg: &str) {
        fatal(msg)
    }
}

/// A result written through a trailing out-pointer (`string`, `u8[]`, `IoResult<T>`, `IoStatus`).
pub trait OutRet: Sized {
    /// The C type of the result slot.
    type Slot;
    /// Its name in a signature.
    const SIG: &'static str;
    /// Writes the result.
    ///
    /// # Safety
    /// `out` must be valid for writes of `Slot`.
    unsafe fn write(self, out: *mut Self::Slot);
    /// What a panicking export returns (results: an error; others: a fatal error).
    fn panicked(msg: &str) -> Self;
    /// Drops a written slot nobody claimed (a cancelled blocking future).
    ///
    /// # Safety
    /// `slot` must hold a value written by [`OutRet::write`].
    unsafe fn drop_slot(slot: *mut Self::Slot);
}

impl OutRet for String {
    type Slot = VeltStr;
    const SIG: &'static str = "string";
    unsafe fn write(self, out: *mut VeltStr) {
        out.write(new_str(&self));
    }
    fn panicked(msg: &str) -> String {
        fatal(msg)
    }
    unsafe fn drop_slot(slot: *mut VeltStr) {
        (api().str_drop)(slot)
    }
}

impl OutRet for Vec<u8> {
    type Slot = VeltBytes;
    const SIG: &'static str = "u8[]";
    unsafe fn write(self, out: *mut VeltBytes) {
        out.write(new_bytes(&self));
    }
    fn panicked(msg: &str) -> Vec<u8> {
        fatal(msg)
    }
    unsafe fn drop_slot(slot: *mut VeltBytes) {
        (api().bytes_drop)(slot)
    }
}

fn err_header(e: Error) -> VeltErr {
    VeltErr {
        code: e.code,
        pad: 0,
        message: new_str(&e.message),
    }
}

impl OutRet for Result<(), Error> {
    type Slot = VeltErr;
    const SIG: &'static str = "IoStatus";
    unsafe fn write(self, out: *mut VeltErr) {
        out.write(match self {
            Ok(()) => VeltErr {
                code: 0,
                pad: 0,
                message: VeltStr::empty(),
            },
            Err(e) => err_header(e),
        });
    }
    fn panicked(msg: &str) -> Self {
        Err(Error::other(format!("native panic: {msg}")))
    }
    unsafe fn drop_slot(slot: *mut VeltErr) {
        (api().str_drop)(&mut (*slot).message)
    }
}

/// A success value of `IoResult<T>`.
pub trait ResultValue: Sized {
    /// The C type of the value.
    type Slot;
    /// The whole result's name in a signature (`IoResult<u64>`).
    const SIG: &'static str;
    /// The value as stored.
    fn into_slot(self) -> Self::Slot;
    /// Drops a stored value.
    ///
    /// # Safety
    /// `slot` must hold a value from [`ResultValue::into_slot`].
    unsafe fn drop_value(slot: *mut Self::Slot);
}

macro_rules! scalar_results {
    ($($t:ty => $sig:literal),*) => {$(
        impl ResultValue for $t {
            type Slot = $t;
            const SIG: &'static str = $sig;
            fn into_slot(self) -> $t { self }
            unsafe fn drop_value(_: *mut $t) {}
        }
    )*};
}

scalar_results!(
    bool => "IoResult<bool>", u8 => "IoResult<u8>", u16 => "IoResult<u16>",
    u32 => "IoResult<u32>", u64 => "IoResult<u64>", i8 => "IoResult<i8>",
    i16 => "IoResult<i16>", i32 => "IoResult<i32>", i64 => "IoResult<i64>",
    f32 => "IoResult<f32>", f64 => "IoResult<f64>"
);

impl ResultValue for String {
    type Slot = VeltStr;
    const SIG: &'static str = "IoResult<string>";
    fn into_slot(self) -> VeltStr {
        new_str(&self)
    }
    unsafe fn drop_value(slot: *mut VeltStr) {
        (api().str_drop)(slot)
    }
}

impl ResultValue for Vec<u8> {
    type Slot = VeltBytes;
    const SIG: &'static str = "IoResult<u8[]>";
    fn into_slot(self) -> VeltBytes {
        new_bytes(&self)
    }
    unsafe fn drop_value(slot: *mut VeltBytes) {
        (api().bytes_drop)(slot)
    }
}

impl<T: ResultValue> OutRet for Result<T, Error> {
    type Slot = IoResultSlot<T::Slot>;
    const SIG: &'static str = T::SIG;
    unsafe fn write(self, out: *mut Self::Slot) {
        out.write(match self {
            Ok(v) => IoResultSlot {
                err: VeltErr {
                    code: 0,
                    pad: 0,
                    message: VeltStr::empty(),
                },
                value: MaybeUninit::new(v.into_slot()),
            },
            Err(e) => IoResultSlot {
                err: err_header(e),
                value: MaybeUninit::zeroed(),
            },
        });
    }
    fn panicked(msg: &str) -> Self {
        Err(Error::other(format!("native panic: {msg}")))
    }
    unsafe fn drop_slot(slot: *mut Self::Slot) {
        if (*slot).err.code == 0 {
            T::drop_value((*slot).value.as_mut_ptr());
        } else {
            (api().str_drop)(&mut (*slot).err.message);
        }
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

/// Signature-record helpers used by [`export`].
#[doc(hidden)]
pub mod sig {
    /// Total length of the NUL-terminated concatenation of `parts`.
    pub const fn len(parts: &[&str]) -> usize {
        let mut n = 1;
        let mut i = 0;
        while i < parts.len() {
            n += parts[i].len();
            i += 1;
        }
        n
    }

    /// The NUL-terminated concatenation of `parts` (`N` = [`len`]).
    pub const fn build<const N: usize>(parts: &[&str]) -> [u8; N] {
        let mut out = [0u8; N];
        let (mut i, mut k) = (0, 0);
        while i < parts.len() {
            let b = parts[i].as_bytes();
            let mut j = 0;
            while j < b.len() {
                out[k] = b[j];
                k += 1;
                j += 1;
            }
            i += 1;
        }
        out
    }
}

#[cfg(test)]
mod tests;
