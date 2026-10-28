//! Bulk operations on `u8[]` (std/prelude/bytes.vlt: `Buffer.alloc`, and `indexOf`, `set`,
//! `copyWithin`, `fill` on byte arrays): memchr / memmove / memset instead of byte loops.
//!
//! Positions follow JS: `copyWithin` and `fill` take relative indices (negative counts from the
//! end) clamped to the array, `indexOf` returns -1 when the byte is absent. The array's buffer is
//! written in place and never reallocated, so its length and capacity stay as they are.

use crate::bytes::VeltBytes;
use crate::result::{invalid_utf8, IoResult, VeltErr};
use crate::str::VeltStr;

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

/// JS relative index: negative counts from `len`, then clamped to `0..=len`. Clamped in `i64`
/// before converting, so an index beyond a 32-bit `usize` (wasm32) isn't truncated.
fn relative(i: i64, len: usize) -> usize {
    // A Rust allocation is at most `isize::MAX` bytes, so `len` fits in an `i64`.
    let n = len as i64;
    (if i < 0 { n + i.max(-n) } else { i.min(n) }) as usize
}

/// A zeroed array of `n` bytes (`Buffer.alloc(n)`): calloc, so large arrays cost no memset.
/// Past the array length limit it is the limit's `RangeError`; a size the target cannot
/// allocate (on wasm32, any past 2^31 - 1, never truncated) stops the program as
/// `velt_rt_alloc` does.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bytes_zeroed(n: u64, out: *mut VeltBytes) {
    if n > crate::bytes::MAX_ARRAY_LEN {
        crate::bytes::array_too_long();
    }
    let len = crate::mem::block_size(n, 1);
    if len == 0 {
        out.write(VeltBytes::from_vec(Vec::new()));
        return;
    }
    let p = match std::alloc::Layout::array::<u8>(len) {
        Ok(layout) => std::alloc::alloc_zeroed(layout),
        Err(_) => std::ptr::null_mut(),
    };
    if p.is_null() {
        crate::mem::out_of_memory(n, 1);
    }
    // SAFETY: `p` is a zeroed block of `len` bytes from the global allocator.
    out.write(VeltBytes::from_vec(Vec::from_raw_parts(p, len, len)));
}

/// An array of `n` bytes whose contents are not set (`Buffer.allocUnsafe(n)`): for a buffer the
/// program fills before it reads it (a decoder's output), which a zeroed one would write twice.
/// Its bytes are whatever the allocator's block held: any `u8` value, never undefined
/// behaviour for Velt code, which reads them only as bytes. Limits as `velt_rt_bytes_zeroed`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bytes_uninit(n: u64, out: *mut VeltBytes) {
    if n > crate::bytes::MAX_ARRAY_LEN {
        crate::bytes::array_too_long();
    }
    let len = crate::mem::block_size(n, 1);
    if len == 0 {
        out.write(VeltBytes::from_vec(Vec::new()));
        return;
    }
    let p = match std::alloc::Layout::array::<u8>(len) {
        Ok(layout) => std::alloc::alloc(layout),
        Err(_) => std::ptr::null_mut(),
    };
    if p.is_null() {
        crate::mem::out_of_memory(n, 1);
    }
    // Not through a `Vec`: its bytes would have to be initialized. Freed by generated code like
    // one of its own blocks (leak counters), as `VeltBytes::from_vec` counts it.
    crate::str::stats::block_alloc();
    out.write(VeltBytes {
        ptr: p,
        len: len as u64,
        cap: len as u64,
    });
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

/// `[start, end)` (relative indices) as a new array (JS `slice`): one copy.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bytes_slice(
    b: *const VeltBytes,
    start: i64,
    end: i64,
    out: *mut VeltBytes,
) {
    let s = (*b).as_bytes();
    let (start, end) = (relative(start, s.len()), relative(end, s.len()));
    let v = if start < end {
        s[start..end].to_vec()
    } else {
        Vec::new()
    };
    out.write(VeltBytes::from_vec(v));
}

/// `a` followed by `b`, as a new array (JS `concat`). Past the array length limit it is the
/// limit's `RangeError`, and a size the target cannot allocate is out of memory, as in
/// `velt_rt_bytes_zeroed`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bytes_concat(
    a: *const VeltBytes,
    b: *const VeltBytes,
    out: *mut VeltBytes,
) {
    let (a, b) = ((*a).as_bytes(), (*b).as_bytes());
    let total = a.len() as u64 + b.len() as u64;
    if total > crate::bytes::MAX_ARRAY_LEN {
        crate::bytes::array_too_long();
    }
    let mut v = Vec::new();
    if v.try_reserve_exact(crate::mem::block_size(total, 1))
        .is_err()
    {
        crate::mem::out_of_memory(total, 1);
    }
    v.extend_from_slice(a);
    v.extend_from_slice(b);
    out.write(VeltBytes::from_vec(v));
}

/// The bytes of `b` in `[start, end)` (relative indices).
unsafe fn range<'a>(b: *const VeltBytes, start: i64, end: i64) -> &'a [u8] {
    let s = (*b).as_bytes();
    let (start, end) = (relative(start, s.len()), relative(end, s.len()));
    &s[start..end.max(start)]
}

/// The bytes in `[start, end)` (relative indices) as a new string, validating UTF-8
/// (`INVALID_DATA` on failure): `velt_rt_bytes_to_str` of a slice, without making the slice.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bytes_range_to_str(
    b: *const VeltBytes,
    start: i64,
    end: i64,
    out: *mut IoResult<VeltStr>,
) {
    let bytes = range(b, start, end);
    let r = match std::str::from_utf8(bytes) {
        Ok(s) => IoResult::ok(VeltStr::from_bytes(s.as_bytes())),
        Err(_) => IoResult::err(VeltErr::from_io(&invalid_utf8("byte buffer"))),
    };
    r.write_to(out);
}

/// The bytes in `[start, end)` (relative indices) as a new string, an invalid sequence as
/// U+FFFD (one per maximal invalid subpart, as `TextDecoder`): Node's `buf.utf8Slice`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bytes_range_to_str_lossy(
    b: *const VeltBytes,
    start: i64,
    end: i64,
    out: *mut VeltStr,
) {
    out.write(text_of(range(b, start, end)));
}

/// `bytes` decoded as UTF-8, invalid sequences as U+FFFD (as JS decodes). Valid text, nearly
/// all of it, is only validated (ASCII, the most common, only scanned) and copied once, where
/// [`String::from_utf8_lossy`] would walk it chunk by chunk at several times the cost.
pub(crate) fn text_of(bytes: &[u8]) -> VeltStr {
    if bytes.is_ascii() {
        // SAFETY: ASCII is UTF-8 with one UTF-16 unit per byte.
        return unsafe {
            VeltStr::from_text_counted(std::str::from_utf8_unchecked(bytes), bytes.len())
        };
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => VeltStr::from_text(s),
        Err(_) => VeltStr::from_text(&String::from_utf8_lossy(bytes)),
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

    #[test]
    fn relative_indices_clamp_before_converting() {
        for (i, want) in [
            (0, 0),
            (3, 3),
            (99, 10),
            (-3, 7),
            (-99, 0),
            (1 << 32, 10),
            (-(1 << 32), 0),
            ((1 << 32) + 3, 10),
            (-(1 << 32) - 3, 0),
            (i64::MAX, 10),
            (i64::MIN, 0),
        ] {
            assert_eq!(relative(i, 10), want, "relative({i}, 10)");
        }
    }

    #[test]
    fn slice_concat() {
        let b = arr(b"hello world");
        // SAFETY: valid owned buffers; `out` is written before it is read.
        unsafe {
            let mut out = std::mem::MaybeUninit::<VeltBytes>::uninit();
            velt_rt_bytes_slice(&b, 6, 99, out.as_mut_ptr());
            assert_eq!(out.assume_init_ref().as_bytes(), b"world");
            velt_rt_bytes_slice(&b, -5, -3, out.as_mut_ptr());
            assert_eq!(out.assume_init_ref().as_bytes(), b"wo");
            velt_rt_bytes_slice(&b, 4, 2, out.as_mut_ptr());
            assert_eq!(out.assume_init_ref().as_bytes(), b"");
            velt_rt_bytes_concat(&arr(b"ab"), &arr(b"cd"), out.as_mut_ptr());
            assert_eq!(out.assume_init_ref().as_bytes(), b"abcd");
        }
    }
}
