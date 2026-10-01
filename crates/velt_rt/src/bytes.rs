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
        let mut v = std::mem::ManuallyDrop::new(v);
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

/// Copy a string's bytes into a new owned byte buffer.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bytes_from_str(s: *const VeltStr, out: *mut VeltBytes) {
    out.write(VeltBytes::from_vec((*s).as_bytes().to_vec()));
}

/// Copy bytes into a new owned string, validating UTF-8 (`INVALID_DATA` on failure).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bytes_to_str(b: *const VeltBytes, out: *mut IoResult<VeltStr>) {
    let r = match std::str::from_utf8((*b).as_bytes()) {
        Ok(s) => IoResult::ok(VeltStr::from_bytes(s.as_bytes())),
        Err(_) => IoResult::err(VeltErr::from_io(&invalid_utf8("byte buffer"))),
    };
    r.write_to(out);
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
