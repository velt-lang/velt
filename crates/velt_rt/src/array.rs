//! `VeltArray<T>`: a Velt `T[]` of a Copy element type (`i64[]`, `u32[]`, …) handed to generated
//! code from a Rust `Vec<T>`.
//!
//! Velt arrays are `{ptr, len, cap}` with the capacity in elements, and their buffers come from
//! the same global allocator as a `Vec<T>` with `T`'s alignment, so a Vec's buffer can be adopted
//! as-is (the same trick `VeltStrArray` and `VeltBytes` use for strings and bytes).

/// `{ T* ptr; uint64_t len; uint64_t cap; }` — size 24, align 8.
#[repr(C)]
#[derive(Debug)]
pub struct VeltArray<T: Copy> {
    /// First element (dangling when `cap == 0`).
    pub ptr: *mut T,
    /// Number of elements.
    pub len: u64,
    /// Capacity in elements; 0 ⇒ nothing to free.
    pub cap: u64,
}

// SAFETY: owns a buffer of Copy elements, like a `Vec<T>`.
unsafe impl<T: Copy + Send> Send for VeltArray<T> {}

impl<T: Copy> VeltArray<T> {
    /// Take ownership of a Vec's buffer.
    pub fn from_vec(v: Vec<T>) -> VeltArray<T> {
        if v.capacity() == 0 {
            return VeltArray {
                ptr: std::ptr::NonNull::dangling().as_ptr(),
                len: 0,
                cap: 0,
            };
        }
        let mut v = std::mem::ManuallyDrop::new(v);
        // Generated code frees the buffer like one of its own blocks (leak counters).
        crate::str::stats::block_alloc();
        VeltArray {
            ptr: v.as_mut_ptr(),
            len: v.len() as u64,
            cap: v.capacity() as u64,
        }
    }

    /// The elements (borrowed).
    ///
    /// # Safety
    /// `self` must describe a valid array per the layout contract.
    pub unsafe fn as_slice(&self) -> &[T] {
        if self.len == 0 {
            &[]
        } else {
            std::slice::from_raw_parts(self.ptr, self.len as usize)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adopts_and_returns_the_buffer() {
        let a = VeltArray::from_vec(vec![1i64, -2, 3]);
        // SAFETY: built from a Vec just above; reclaimed once.
        unsafe {
            assert_eq!(a.as_slice(), &[1, -2, 3]);
            drop(Vec::from_raw_parts(a.ptr, a.len as usize, a.cap as usize));
        }
        let empty = VeltArray::<i64>::from_vec(vec![]);
        assert_eq!(empty.cap, 0);
    }
}
