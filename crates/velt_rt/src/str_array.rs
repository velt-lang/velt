//! `VeltStrArray`: an owned list of owned strings (`readDir` results, `process.argv`).
//!
//! The element buffer is a Rust `Vec<VeltStr>` (element size 24, align 8), i.e. the same memory
//! `velt_rt_alloc(cap * 24, 8)` returns, so generated code may adopt or free it.

use crate::str::{velt_rt_str_drop, VeltStr};

/// `{ VeltStr* ptr; uint64_t len; uint64_t cap; }` — size 24, align 8.
#[repr(C)]
#[derive(Debug)]
pub struct VeltStrArray {
    /// First element.
    pub ptr: *mut VeltStr,
    /// Number of elements.
    pub len: u64,
    /// Capacity in elements; 0 ⇒ nothing to free.
    pub cap: u64,
}

// SAFETY: owns its elements, which are owned strings.
unsafe impl Send for VeltStrArray {}

impl VeltStrArray {
    /// Build from Rust strings (each copied into an owned `VeltStr`).
    pub fn from_strings<I: IntoIterator<Item = String>>(items: I) -> VeltStrArray {
        VeltStrArray::from_vec(
            items
                .into_iter()
                .map(|s| VeltStr::from_vec(s.into_bytes()))
                .collect(),
        )
    }

    /// Take ownership of a Vec's buffer and its elements.
    pub fn from_vec(v: Vec<VeltStr>) -> VeltStrArray {
        if v.capacity() == 0 {
            return VeltStrArray {
                ptr: std::ptr::NonNull::dangling().as_ptr(),
                len: 0,
                cap: 0,
            };
        }
        let mut v = std::mem::ManuallyDrop::new(v);
        // Generated code frees the buffer like one of its own blocks (leak counters).
        crate::str::stats::block_alloc();
        VeltStrArray {
            ptr: v.as_mut_ptr(),
            len: v.len() as u64,
            cap: v.capacity() as u64,
        }
    }
}

/// Drop every element and the buffer, then zero `*a`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_array_drop(a: *mut VeltStrArray) {
    let arr = &mut *a;
    if arr.cap > 0 {
        crate::str::stats::block_free();
        let mut v = Vec::from_raw_parts(arr.ptr, arr.len as usize, arr.cap as usize);
        for s in v.iter_mut() {
            velt_rt_str_drop(s);
        }
    }
    *arr = VeltStrArray {
        ptr: std::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_and_drop() {
        let mut a = VeltStrArray::from_strings(["a".to_string(), "bc".to_string()]);
        assert_eq!(a.len, 2);
        let second = unsafe { &*a.ptr.add(1) };
        assert_eq!(unsafe { second.as_bytes() }, b"bc");
        unsafe { velt_rt_str_array_drop(&mut a) };
        assert!(a.ptr.is_null());
        let mut e = VeltStrArray::from_strings(Vec::new());
        unsafe { velt_rt_str_array_drop(&mut e) };
    }
}
