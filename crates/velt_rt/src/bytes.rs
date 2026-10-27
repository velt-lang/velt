//! Byte buffers: `VeltBytes` (same layout as `VeltStr`, no UTF-8 guarantee) and `velt_rt_bytes_*`.
//!
//! Heap buffers (`cap > 0`) are Rust `Vec<u8>` buffers from the global allocator (the layout of a
//! `u8[]`), so the runtime can hand network/file data to generated code without copying.

use crate::result::{invalid_utf8, IoResult, VeltErr};
use crate::str::VeltStr;

/// `{ uint8_t* ptr; uint64_t len; uint64_t cap; }` — size 24, align 8. `cap == 0` ⇒ static/borrowed.
#[repr(C)]
#[derive(Debug)]
pub struct VeltBytes {
    /// First byte (dangling or null when `len == 0`).
    pub ptr: *mut u8,
    /// Number of bytes.
    pub len: u64,
    /// Allocation capacity; 0 for static/borrowed bytes.
    pub cap: u64,
}

// SAFETY: an owned byte buffer; the contract forbids sharing a `cap > 0` buffer between owners.
unsafe impl Send for VeltBytes {}

const _: () = assert!(std::mem::size_of::<VeltBytes>() == 24);

/// The most bytes an array's buffer holds, and so the most elements an array holds: 2^52 - 1
/// (rt_abi.md "Array length limit"; velt_vir's `MAX_ARRAY_LEN`, which generated code checks
/// where it chooses a length). The runtime checks it where it hands a Rust `Vec` over as a Velt
/// array ([`check_array_len`]) and in `velt_rt_realloc`, which array growth calls.
pub const MAX_ARRAY_LEN: u64 = (1 << 52) - 1;

/// An array of `len` elements may be handed to generated code: within [`MAX_ARRAY_LEN`], else
/// the program stops with the same `RangeError` generated code reports. On a 32-bit target
/// (wasm32) every length is.
#[inline]
pub fn check_array_len(len: usize) {
    if !array_len_ok(len) {
        array_too_long();
    }
}

#[inline]
fn array_len_ok(len: usize) -> bool {
    len as u64 <= MAX_ARRAY_LEN
}

/// Stop with the `RangeError` of an array past [`MAX_ARRAY_LEN`].
#[cold]
pub(crate) fn array_too_long() -> ! {
    crate::panic::fatal("RangeError: Invalid array length: an array holds at most 2^52 - 1 elements, and its elements at most 2^52 - 1 bytes")
}

impl VeltBytes {
    /// Take ownership of a Vec's buffer.
    pub fn from_vec(v: Vec<u8>) -> VeltBytes {
        if v.capacity() == 0 {
            return VeltBytes {
                ptr: std::ptr::NonNull::dangling().as_ptr(),
                len: 0,
                cap: 0,
            };
        }
        check_array_len(v.len());
        let mut v = std::mem::ManuallyDrop::new(v);
        // Generated code frees the buffer like one of its own blocks (leak counters).
        crate::str::stats::block_alloc();
        VeltBytes {
            ptr: v.as_mut_ptr(),
            len: v.len() as u64,
            cap: v.capacity() as u64,
        }
    }

    /// Reclaim the buffer as a Vec (copies static/borrowed bytes), leaving `self` empty.
    ///
    /// # Safety
    /// `self` must describe a valid buffer per the layout contract.
    pub unsafe fn take_vec(&mut self) -> Vec<u8> {
        let v = if self.cap > 0 {
            // The buffer leaves compiled code's blocks (leak counters).
            crate::str::stats::block_free();
            Vec::from_raw_parts(self.ptr, self.len as usize, self.cap as usize)
        } else {
            self.as_bytes().to_vec()
        };
        *self = VeltBytes {
            ptr: std::ptr::null_mut(),
            len: 0,
            cap: 0,
        };
        v
    }

    /// # Safety
    /// `ptr`/`len` must describe readable bytes (any pointer is fine when `len == 0`).
    pub unsafe fn as_bytes(&self) -> &[u8] {
        if self.len == 0 {
            &[]
        } else {
            std::slice::from_raw_parts(self.ptr, self.len as usize)
        }
    }
}

/// Drop an owned byte buffer (no-op for `cap == 0`) and zero `*b`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bytes_drop(b: *mut VeltBytes) {
    drop((*b).take_vec());
}

/// Copy a string's UTF-8 into a new owned byte buffer (one U+FFFD per lone surrogate, as
/// `TextEncoder` does).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bytes_from_str(s: *const VeltStr, out: *mut VeltBytes) {
    out.write(VeltBytes::from_vec((*s).to_string_lossy().into_bytes()));
}

/// Copy bytes into a new owned string, validating UTF-8 (`INVALID_DATA` on failure; the
/// encoding of a surrogate, `ED A0..BF xx`, is invalid UTF-8 too).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bytes_to_str(b: *const VeltBytes, out: *mut IoResult<VeltStr>) {
    let r = match std::str::from_utf8((*b).as_bytes()) {
        Ok(s) => IoResult::ok(VeltStr::from_bytes(s.as_bytes())),
        Err(_) => IoResult::err(VeltErr::from_io(&invalid_utf8("byte buffer"))),
    };
    r.write_to(out);
}

/// The text of the UTF-8 bytes `b`, an invalid sequence as U+FFFD (WHATWG's decoder, as std's
/// `utf8DecodeLossy`: one per maximal invalid subpart).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bytes_to_str_lossy(b: *const VeltBytes, out: *mut VeltStr) {
    out.write(VeltStr::from_bytes(
        String::from_utf8_lossy((*b).as_bytes()).as_bytes(),
    ));
}

/// Whether `b` is valid UTF-8: the bytes of a string, as it stores them (a lone surrogate's
/// three bytes are not UTF-8, and decoding replaces them).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bytes_is_utf8(b: *const VeltBytes) -> u8 {
    u8::from(std::str::from_utf8((*b).as_bytes()).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::MaybeUninit;

    #[test]
    fn roundtrip_and_invalid_utf8() {
        let s = VeltStr::from_static("héllo".as_bytes());
        let mut b = MaybeUninit::<VeltBytes>::uninit();
        unsafe { velt_rt_bytes_from_str(&s, b.as_mut_ptr()) };
        let mut b = unsafe { b.assume_init() };
        assert_eq!(b.len, 6);
        let mut r = MaybeUninit::<IoResult<VeltStr>>::uninit();
        unsafe { velt_rt_bytes_to_str(&b, r.as_mut_ptr()) };
        let r = unsafe { r.assume_init() };
        assert_eq!(r.err.code, 0);
        let mut v = unsafe { r.value.assume_init() };
        assert_eq!(unsafe { v.as_bytes() }, "héllo".as_bytes());
        unsafe { crate::str::velt_rt_str_drop(&mut v) };
        unsafe { velt_rt_bytes_drop(&mut b) };
        assert_eq!((b.len, b.cap), (0, 0));

        let bad = VeltBytes::from_vec(vec![0xff, 0xfe]);
        let mut r = MaybeUninit::<IoResult<VeltStr>>::uninit();
        unsafe { velt_rt_bytes_to_str(&bad, r.as_mut_ptr()) };
        let mut r = unsafe { r.assume_init() };
        assert_eq!(r.err.code, crate::result::code::INVALID_DATA);
        unsafe { crate::str::velt_rt_str_drop(&mut r.err.message) };
        let mut bad = bad;
        unsafe { velt_rt_bytes_drop(&mut bad) };
    }
}

#[cfg(test)]
mod array_len_tests {
    use super::{array_len_ok, MAX_ARRAY_LEN};

    #[test]
    fn lengths_up_to_the_limit_pass() {
        assert!(array_len_ok(0));
        assert!(array_len_ok(u32::MAX as usize));
        #[cfg(target_pointer_width = "64")]
        {
            assert!(array_len_ok(MAX_ARRAY_LEN as usize));
            assert!(!array_len_ok(MAX_ARRAY_LEN as usize + 1));
            assert!(!array_len_ok(usize::MAX));
        }
    }

    /// wasm32: every length a 32-bit `usize` can hold is within the limit, so the check holds
    /// there without ever stopping a program.
    #[test]
    fn every_32_bit_length_is_within_the_limit() {
        assert!(u64::from(u32::MAX) <= MAX_ARRAY_LEN);
        #[cfg(target_pointer_width = "32")]
        assert!(array_len_ok(usize::MAX));
    }
}
