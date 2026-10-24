//! How values cross an exported function's C signature: parameter and result traits and their
//! names in signature records (native_abi.md "Calls from Velt into the library").

use std::borrow::Cow;
use std::mem::MaybeUninit;

use crate::sig::Buf;
use crate::{__Scope, api, fatal, new_str, str_of, Error, IoResultSlot, VeltErr, VeltStr};

/// A parameter type of an exported function: how it crosses the C ABI and its signature name.
pub trait Param<'a>: Sized {
    /// The C type of the argument.
    type Raw: Copy;
    /// Its name in a signature (`u64`, `string`, `u8[]`).
    const SIG: Buf;
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
/// An `#[export(async)]` `fn` returning a future borrows its arguments for the synchronous part
/// only: a future that keeps one does not compile.
///
/// ```compile_fail
/// use velt_native::Error;
/// #[velt_native::export(async)]
/// fn velt_p__later(s: &str) -> impl std::future::Future<Output = Result<u64, Error>> + Send + 'static {
///     async move { Ok(s.len() as u64) }
/// }
/// ```
///
/// ```
/// use velt_native::Error;
/// #[velt_native::export(async)]
/// fn velt_p__now(s: &str) -> impl std::future::Future<Output = Result<u64, Error>> + Send + 'static {
///     let n = s.len() as u64;
///     async move { Ok(n) }
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
            const SIG: Buf = Buf::lit($sig);
            unsafe fn from_raw(raw: $t) -> $t { raw }
        }
        impl OwnedParam for $t {}
        impl DirectRet for $t {
            const SIG: Buf = Buf::lit($sig);
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
    const SIG: Buf = Buf::lit("string");
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
    const SIG: Buf = Buf::lit("string");
    unsafe fn from_raw(raw: *const VeltStr) -> Cow<'a, str> {
        str_of(raw)
    }
}

impl<'a> Param<'a> for String {
    type Raw = *const VeltStr;
    const SIG: Buf = Buf::lit("string");
    unsafe fn from_raw(raw: *const VeltStr) -> String {
        str_of(raw).into_owned()
    }
}
impl OwnedParam for String {}

/// A result returned directly (scalars and `()`).
pub trait DirectRet {
    /// Its name in a signature.
    const SIG: Buf;
    /// What a panicking export returns (scalars: a fatal error).
    fn panicked(msg: &str) -> Self;
}

impl DirectRet for () {
    const SIG: Buf = Buf::lit("void");
    fn panicked(msg: &str) {
        fatal(msg)
    }
}

/// A result written through a trailing out-pointer (`string`, `u8[]`, `IoResult<T>`, `IoStatus`).
pub trait OutRet: Sized {
    /// The C type of the result slot.
    type Slot;
    /// Its name in a signature.
    const SIG: Buf;
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
    const SIG: Buf = Buf::lit("string");
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

fn err_header(e: Error) -> VeltErr {
    VeltErr {
        code: e.code,
        pad: 0,
        message: new_str(&e.message),
    }
}

impl OutRet for Result<(), Error> {
    type Slot = VeltErr;
    const SIG: Buf = Buf::lit("IoStatus");
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
    const SIG: Buf;
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
            const SIG: Buf = Buf::lit($sig);
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
    const SIG: Buf = Buf::lit("IoResult<string>");
    fn into_slot(self) -> VeltStr {
        new_str(&self)
    }
    unsafe fn drop_value(slot: *mut VeltStr) {
        (api().str_drop)(slot)
    }
}

impl<T: ResultValue> OutRet for Result<T, Error> {
    type Slot = IoResultSlot<T::Slot>;
    const SIG: Buf = T::SIG;
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
