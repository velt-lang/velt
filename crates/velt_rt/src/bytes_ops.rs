//! Bulk operations on `u8[]` (std/prelude/bytes.vlt: `Buffer.alloc`, and `indexOf`, `set`,
//! `copyWithin`, `fill` on byte arrays): memchr / memmove / memset instead of byte loops.
//!
//! Positions follow JS: `copyWithin` and `fill` take relative indices (negative counts from the
//! end) clamped to the array, `indexOf` returns -1 when the byte is absent. The array's buffer is
//! written in place and never reallocated, so its length and capacity stay as they are.

use crate::bytes::VeltBytes;

/// The array's bytes, writable.
///
/// # Safety
/// `b` describes a valid, uniquely borrowed buffer.
unsafe fn bytes_mut<'a>(b: *const VeltBytes) -> &'a mut [u8] {
    let b = &*b;
    if b.len == 0 {
        &mut []
    } else {
        std::slice::from_raw_parts_mut(b.ptr, b.len as usize)
    }
}

/// JS relative index: negative counts from `len`, then clamped to `0..=len`.
fn relative(i: i64, len: usize) -> usize {
    if i < 0 {
        len.saturating_sub(i.unsigned_abs() as usize)
    } else {
        (i as usize).min(len)
    }
}

/// A zeroed array of `n` bytes (`Buffer.alloc(n)`): calloc, so large arrays cost no memset.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bytes_zeroed(n: u64, out: *mut VeltBytes) {
    out.write(VeltBytes::from_vec(vec![0; n as usize]));
}

/// First position at or after `from` (relative) holding `byte`, or -1.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bytes_index_of(b: *const VeltBytes, byte: u8, from: i64) -> i64 {
    let s = (*b).as_bytes();
    let from = relative(from, s.len());
    memchr::memchr(byte, &s[from..]).map_or(-1, |i| (from + i) as i64)
}

/// Last position at or before `from` (relative; clamped to the last byte) holding `byte`, or -1.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bytes_last_index_of(
    b: *const VeltBytes,
    byte: u8,
    from: i64,
) -> i64 {
    let s = (*b).as_bytes();
    if from < 0 && from.unsigned_abs() as usize > s.len() {
        return -1;
    }
    let end = (relative(from, s.len()) + 1).min(s.len());
    memchr::memrchr(byte, &s[..end]).map_or(-1, |i| i as i64)
}

/// Copies `src` into `dst` starting at `offset` (JS `typedArray.set(src, offset)`); 0 (nothing
/// copied) when it does not fit. `src` may be `dst`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bytes_set(
    dst: *const VeltBytes,
    src: *const VeltBytes,
    offset: u64,
) -> u8 {
    let (d, s) = (&*dst, &*src);
    let fits = offset.checked_add(s.len).is_some_and(|end| end <= d.len);
    if !fits {
        return 0;
    }
    if s.len > 0 {
        // memmove: `src` and `dst` may be the same array.
        std::ptr::copy(s.ptr, d.ptr.add(offset as usize), s.len as usize);
    }
    1
}

/// Copies `[start, end)` to `target` within the array (JS `copyWithin`, relative indices).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bytes_copy_within(
    b: *const VeltBytes,
    target: i64,
    start: i64,
    end: i64,
) {
    let s = bytes_mut(b);
    let len = s.len();
    let (target, start, end) = (
        relative(target, len),
        relative(start, len),
        relative(end, len),
    );
    let count = end.saturating_sub(start).min(len - target);
    s.copy_within(start..start + count, target);
}

/// Sets `[start, end)` (relative indices) to `byte` (JS `fill`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bytes_fill(b: *const VeltBytes, byte: u8, start: i64, end: i64) {
    let s = bytes_mut(b);
    let (start, end) = (relative(start, s.len()), relative(end, s.len()));
    if start < end {
        s[start..end].fill(byte);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arr(v: &[u8]) -> VeltBytes {
        VeltBytes::from_vec(v.to_vec())
    }

    #[test]
    fn index_of_like_js() {
        let b = arr(b"a\nbc\n");
        // SAFETY: a valid owned buffer.
        unsafe {
            assert_eq!(velt_rt_bytes_index_of(&b, b'\n', 0), 1);
            assert_eq!(velt_rt_bytes_index_of(&b, b'\n', 2), 4);
            assert_eq!(velt_rt_bytes_index_of(&b, b'\n', -1), 4);
            assert_eq!(velt_rt_bytes_index_of(&b, b'x', 0), -1);
            assert_eq!(velt_rt_bytes_index_of(&b, b'a', 99), -1);
            assert_eq!(velt_rt_bytes_last_index_of(&b, b'\n', 3), 1);
            assert_eq!(velt_rt_bytes_last_index_of(&b, b'\n', 99), 4);
            assert_eq!(velt_rt_bytes_last_index_of(&b, b'a', -9), -1);
        }
    }

    #[test]
    fn set_copy_within_fill() {
        let mut d = arr(b"......");
        let src = arr(b"ab");
        // SAFETY: valid owned buffers.
        unsafe {
            assert_eq!(velt_rt_bytes_set(&d, &src, 4), 1);
            assert_eq!(velt_rt_bytes_set(&d, &src, 5), 0);
            assert_eq!(d.as_bytes(), b"....ab");
            velt_rt_bytes_copy_within(&d, 0, 4, 6);
            assert_eq!(d.as_bytes(), b"ab..ab");
            velt_rt_bytes_copy_within(&d, 1, 0, -2);
            assert_eq!(d.as_bytes(), b"aab..b");
            velt_rt_bytes_fill(&d, b'z', -2, 99);
            assert_eq!(d.as_bytes(), b"aab.zz");
            let mut z = VeltBytes::from_vec(Vec::new());
            velt_rt_bytes_zeroed(3, &mut z);
            assert_eq!(z.as_bytes(), [0, 0, 0]);
            drop(z.take_vec());
            drop(d.take_vec());
        }
    }
}
