//! Velt values in Rust: what crosses an exported function besides scalars and plain strings.
//!
//! A type that implements [`Value`] has Velt's memory layout, so it can be an array's element or
//! a struct's field: the scalars, [`Str`] (a runtime string), [`Array<T>`] (a `T[]`) and structs
//! deriving [`Value`](macro@crate::Value) (a Velt `struct`, [`Record`]). Parameters borrow them
//! (`&[T]`, `&S`, or [`Owned<S>`] in async exports); results own them (`Array<T>`, `Vec<T>`,
//! `S`), allocated by the runtime, which frees them once Velt is done with them. The derive
//! writes a struct's parameter and result impls.

use std::borrow::Cow;
use std::marker::PhantomData;
use std::mem::{align_of, size_of, ManuallyDrop};

use crate::abi::{OutRet, OwnedParam, Param, ResultValue};
use crate::sig::Buf;
use crate::{api, fatal, new_str, str_of, VeltStr};

/// A Rust type with the memory layout of a Velt type, and that type's name in a signature.
///
/// # Safety
/// The layout must be exactly the Velt type's: scalars as themselves (`bool` one byte), strings
/// and arrays as the runtime's 24-byte values, structs `#[repr(C)]` with Velt's field order
/// (what `#[derive(velt_native::Value)]` checks).
pub unsafe trait Value: Sized + 'static {
    /// The Velt type's name in a signature (`i64`, `string[]`, `{a:i64,b:string}`).
    const SIG: Buf;
}

/// A Velt `struct` (derive it: `#[derive(velt_native::Value)] #[repr(C)] struct S { .. }`):
/// passed by reference (`&S`), returned by value.
///
/// # Safety
/// As for [`Value`].
pub unsafe trait Record: Value {}

macro_rules! scalar_values {
    ($($t:ty => $sig:literal),*) => {$(
        // SAFETY: a scalar is laid out as itself (`bool` is one byte, 0 or 1).
        unsafe impl Value for $t {
            const SIG: Buf = Buf::lit($sig);
        }
    )*};
}

scalar_values!(
    bool => "bool", u8 => "u8", u16 => "u16", u32 => "u32", u64 => "u64",
    i8 => "i8", i16 => "i16", i32 => "i32", i64 => "i64", f32 => "f32", f64 => "f64"
);

/// An owned Velt `string` in the runtime's memory (an element or field; a plain `string`
/// parameter or result can be a Rust `&str`/`String` instead).
#[repr(transparent)]
pub struct Str(VeltStr);

// SAFETY: an owned string the runtime made for this library; nothing else refers to it.
unsafe impl Send for Str {}
// SAFETY: `&Str` only reads its bytes.
unsafe impl Sync for Str {}

// SAFETY: the runtime's 24-byte string.
unsafe impl Value for Str {
    const SIG: Buf = Buf::lit("string");
}

impl Str {
    /// A runtime string of `s` (a copy; [`crate::StrBuf`] builds one in place).
    pub fn new(s: &str) -> Str {
        Str(new_str(s))
    }

    /// Takes over an owned runtime string.
    pub(crate) fn from_raw(s: VeltStr) -> Str {
        Str(s)
    }

    /// The bytes as stored: UTF-8, or WTF-8 when the string holds a lone surrogate (borrowed).
    pub fn as_bytes(&self) -> &[u8] {
        let mut len = 0usize;
        // SAFETY: a live string; its bytes live as long as `self`.
        unsafe {
            let p = (api().str_bytes)(&self.0, &mut len);
            if len == 0 {
                &[]
            } else {
                std::slice::from_raw_parts(p, len)
            }
        }
    }

    /// The text (borrowed when well-formed; a lone surrogate becomes U+FFFD).
    pub fn to_str(&self) -> Cow<'_, str> {
        // SAFETY: a live string.
        unsafe { str_of(&self.0) }
    }
}

impl AsRef<[u8]> for Str {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl Clone for Str {
    /// Another reference to the same immutable string (a count, no copy).
    fn clone(&self) -> Str {
        let mut out = VeltStr::empty();
        // SAFETY: a live string and an out-pointer.
        unsafe { (api().str_retain)(&self.0, &mut out) };
        Str(out)
    }
}

/// A `string` argument kept beyond the call (an async export's): another reference to the
/// caller's string, no copy.
impl<'a> Param<'a> for Str {
    type Raw = *const VeltStr;
    const SIG: Buf = Buf::lit("string");
    unsafe fn from_raw(raw: *const VeltStr) -> Str {
        let mut out = VeltStr::empty();
        (api().str_retain)(raw, &mut out);
        Str(out)
    }
}
impl OwnedParam for Str {}

impl OutRet for Str {
    type Slot = VeltStr;
    const SIG: Buf = Buf::lit("string");
    unsafe fn write(self, out: *mut VeltStr) {
        out.write(ManuallyDrop::new(self).take());
    }
    fn panicked(msg: &str) -> Str {
        fatal(msg)
    }
    unsafe fn drop_slot(slot: *mut VeltStr) {
        (api().str_drop)(slot)
    }
}

impl ResultValue for Str {
    type Slot = VeltStr;
    const SIG: Buf = Buf::lit("IoResult<string>");
    fn into_slot(self) -> VeltStr {
        ManuallyDrop::new(self).take()
    }
    unsafe fn drop_value(slot: *mut VeltStr) {
        (api().str_drop)(slot)
    }
}

impl Str {
    /// The runtime value, leaving this one empty (for moving into a slot).
    fn take(&mut self) -> VeltStr {
        std::mem::replace(&mut self.0, VeltStr::empty())
    }
}

impl Drop for Str {
    fn drop(&mut self) {
        // SAFETY: owned, dropped once.
        unsafe { (api().str_drop)(&mut self.0) }
    }
}

impl From<&str> for Str {
    fn from(s: &str) -> Str {
        Str::new(s)
    }
}

impl From<String> for Str {
    fn from(s: String) -> Str {
        Str::new(&s)
    }
}

impl std::fmt::Debug for Str {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.to_str().fmt(f)
    }
}

/// An owned Velt `T[]`: `{ T* ptr; u64 len; u64 cap; }`, its buffer from the runtime's
/// allocator (generated code frees it).
#[repr(C)]
pub struct Array<T: Value> {
    ptr: *mut T,
    len: u64,
    cap: u64,
    _t: PhantomData<T>,
}

// SAFETY: owns its elements, like a `Vec<T>`.
unsafe impl<T: Value + Send> Send for Array<T> {}
// SAFETY: as for `Vec<T>`.
unsafe impl<T: Value + Sync> Sync for Array<T> {}

// SAFETY: the runtime's 24-byte array of `T`'s layout.
unsafe impl<T: Value> Value for Array<T> {
    const SIG: Buf = T::SIG.push("[]");
}

impl<T: Value> Array<T> {
    /// An empty array (no buffer).
    pub fn new() -> Array<T> {
        Array {
            ptr: std::ptr::NonNull::dangling().as_ptr(),
            len: 0,
            cap: 0,
            _t: PhantomData,
        }
    }

    /// An empty array with room for `n` elements.
    pub fn with_capacity(n: usize) -> Array<T> {
        let mut a = Array::new();
        a.reserve(n);
        a
    }

    /// The elements.
    pub fn as_slice(&self) -> &[T] {
        if self.len == 0 {
            return &[];
        }
        // SAFETY: `len` initialized elements at `ptr`.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len as usize) }
    }

    /// The elements, mutably.
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        if self.len == 0 {
            return &mut [];
        }
        // SAFETY: as above, uniquely borrowed.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len as usize) }
    }

    /// The number of elements.
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Makes room for `more` elements.
    pub fn reserve(&mut self, more: usize) {
        let need = self.len as usize + more;
        if need <= self.cap as usize {
            return;
        }
        let cap = need.max(self.cap as usize * 2).max(4);
        let (size, align) = (size_of::<T>() as u64, align_of::<T>() as u64);
        // SAFETY: the runtime's allocator with this array's element layout; the old buffer (if
        // any) came from it with the old capacity.
        let p = unsafe {
            if self.cap == 0 {
                (api().alloc)(size * cap as u64, align)
            } else {
                (api().realloc)(
                    self.ptr as *mut u8,
                    size * self.cap,
                    align,
                    size * cap as u64,
                )
            }
        };
        self.ptr = p as *mut T;
        self.cap = cap as u64;
    }

    /// Appends `v`.
    pub fn push(&mut self, v: T) {
        self.reserve(1);
        // SAFETY: room for one more.
        unsafe { self.ptr.add(self.len as usize).write(v) };
        self.len += 1;
    }
}

impl<T: Value> Default for Array<T> {
    fn default() -> Array<T> {
        Array::new()
    }
}

impl<T: Value> Drop for Array<T> {
    fn drop(&mut self) {
        // SAFETY: `len` live elements, then the buffer the runtime allocated (`cap` elements).
        unsafe {
            std::ptr::drop_in_place(self.as_mut_slice() as *mut [T]);
            if self.cap != 0 {
                let size = size_of::<T>() as u64;
                (api().free)(self.ptr as *mut u8, size * self.cap, align_of::<T>() as u64);
            }
        }
    }
}

impl<T: Value> From<Vec<T>> for Array<T> {
    fn from(v: Vec<T>) -> Array<T> {
        let mut a = Array::with_capacity(v.len());
        for x in v {
            a.push(x);
        }
        a
    }
}

impl From<Vec<String>> for Array<Str> {
    fn from(v: Vec<String>) -> Array<Str> {
        v.into_iter().map(Str::from).collect()
    }
}

impl<T: Value> FromIterator<T> for Array<T> {
    fn from_iter<I: IntoIterator<Item = T>>(it: I) -> Array<T> {
        let it = it.into_iter();
        let mut a = Array::with_capacity(it.size_hint().0);
        for x in it {
            a.push(x);
        }
        a
    }
}

impl<T: Value + Clone> Clone for Array<T> {
    fn clone(&self) -> Array<T> {
        self.as_slice().iter().cloned().collect()
    }
}

impl<T: Value + std::fmt::Debug> std::fmt::Debug for Array<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.as_slice().fmt(f)
    }
}

impl<T: Value> std::ops::Deref for Array<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        self.as_slice()
    }
}

/// A borrowed `T[]` argument: `{ ptr, len, cap }`.
#[repr(C)]
pub struct RawArray<T> {
    ptr: *const T,
    len: u64,
    cap: u64,
}

impl<T> Clone for RawArray<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for RawArray<T> {}

/// # Safety
/// `raw` points to a live array of `T`; the slice borrows it.
unsafe fn slice_of<'a, T>(raw: *const RawArray<T>) -> &'a [T] {
    let a = &*raw;
    if a.len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(a.ptr, a.len as usize)
    }
}

/// A `T[]` argument, borrowed for the call (`&[u8]` is a `u8[]`, `&[Str]` a `string[]`).
impl<'a, T: Value> Param<'a> for &'a [T] {
    type Raw = *const RawArray<T>;
    const SIG: Buf = <Array<T> as Value>::SIG;
    unsafe fn from_raw(raw: *const RawArray<T>) -> &'a [T] {
        slice_of(raw)
    }
}

/// A `T[]` argument, copied (for `blocking` and `async` exports).
impl<'a, T: Value + Clone> Param<'a> for Vec<T> {
    type Raw = *const RawArray<T>;
    const SIG: Buf = <Array<T> as Value>::SIG;
    unsafe fn from_raw(raw: *const RawArray<T>) -> Vec<T> {
        slice_of(raw).to_vec()
    }
}
impl<T: Value + Clone + Send> OwnedParam for Vec<T> {}

/// A struct argument, copied (for `blocking` and `async` exports, whose futures outlive the
/// call); a plain export takes `&S`.
pub struct Owned<S>(pub S);

impl<'a, S: Record + Clone> Param<'a> for Owned<S> {
    type Raw = *const S;
    const SIG: Buf = S::SIG;
    unsafe fn from_raw(raw: *const S) -> Owned<S> {
        Owned((*raw).clone())
    }
}
impl<S: Record + Clone + Send> OwnedParam for Owned<S> {}

impl<T: Value> OutRet for Array<T> {
    type Slot = Array<T>;
    const SIG: Buf = <Array<T> as Value>::SIG;
    unsafe fn write(self, out: *mut Array<T>) {
        out.write(self);
    }
    fn panicked(msg: &str) -> Array<T> {
        fatal(msg)
    }
    unsafe fn drop_slot(slot: *mut Array<T>) {
        std::ptr::drop_in_place(slot)
    }
}

impl<T: Value> OutRet for Vec<T> {
    type Slot = Array<T>;
    const SIG: Buf = <Array<T> as Value>::SIG;
    unsafe fn write(self, out: *mut Array<T>) {
        out.write(Array::from(self));
    }
    fn panicked(msg: &str) -> Vec<T> {
        fatal(msg)
    }
    unsafe fn drop_slot(slot: *mut Array<T>) {
        std::ptr::drop_in_place(slot)
    }
}

/// `IoResult<t>`.
#[doc(hidden)]
pub const fn __io_result(t: Buf) -> Buf {
    Buf::lit("IoResult<").cat(t).push(">")
}

impl<T: Value> ResultValue for Array<T> {
    type Slot = Array<T>;
    const SIG: Buf = __io_result(<Array<T> as Value>::SIG);
    fn into_slot(self) -> Array<T> {
        self
    }
    unsafe fn drop_value(slot: *mut Array<T>) {
        std::ptr::drop_in_place(slot)
    }
}

impl<T: Value> ResultValue for Vec<T> {
    type Slot = Array<T>;
    const SIG: Buf = __io_result(<Array<T> as Value>::SIG);
    fn into_slot(self) -> Array<T> {
        Array::from(self)
    }
    unsafe fn drop_value(slot: *mut Array<T>) {
        std::ptr::drop_in_place(slot)
    }
}

/// Builds a struct's name in a signature (used by the derive): `{` then each `name:type`
/// separated by `,`, then `}`.
#[doc(hidden)]
pub const fn __record_sig(fields: &[(&str, Buf)]) -> Buf {
    let mut b = Buf::lit("{");
    let mut i = 0;
    while i < fields.len() {
        if i > 0 {
            b = b.push(",");
        }
        b = b.push(fields[i].0).push(":").cat(fields[i].1);
        i += 1;
    }
    b.push("}")
}
