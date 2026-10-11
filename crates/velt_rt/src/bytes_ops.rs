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

/// JS relative index: negative counts from `len`, then clamped to `0..=len`. Clamped in `i64`
/// before converting, so an index beyond a 32-bit `usize` (wasm32) isn't truncated.
fn relative(i: i64, len: usize) -> usize {
    // A Rust allocation is at most `isize::MAX` bytes, so `len` fits in an `i64`.
    let n = len as i64;
    (if i < 0 { n + i.max(-n) } else { i.min(n) }) as usize
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

/// `a` followed by `b`, as a new array (JS `concat`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bytes_concat(
    a: *const VeltBytes,
    b: *const VeltBytes,
    out: *mut VeltBytes,
) {
    let (a, b) = ((*a).as_bytes(), (*b).as_bytes());
    let mut v = Vec::with_capacity(a.len() + b.len());
    v.extend_from_slice(a);
    v.extend_from_slice(b);
    out.write(VeltBytes::from_vec(v));
}

/// Whether the array is well-formed UTF-8.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bytes_utf8_valid(b: *const VeltBytes) -> u8 {
    std::str::from_utf8((*b).as_bytes()).is_ok() as u8
}

/// XORs `[start, end)` (relative indices) with the four bytes of `mask` (most significant first),
/// repeating from `start`: WebSocket masking (RFC 6455 §5.3), its own inverse.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bytes_xor_mask(
    b: *const VeltBytes,
    mask: u32,
    start: i64,
    end: i64,
) {
    let s = bytes_mut(b);
    let (start, end) = (relative(start, s.len()), relative(end, s.len()));
    if start >= end {
        return;
    }
    let m = mask.to_be_bytes();
    let wide = u64::from_ne_bytes([m[0], m[1], m[2], m[3], m[0], m[1], m[2], m[3]]);
    let (chunks, rest) = s[start..end].as_chunks_mut::<8>();
    for c in chunks {
        *c = (u64::from_ne_bytes(*c) ^ wide).to_ne_bytes();
    }
    for (i, byte) in rest.iter_mut().enumerate() {
        *byte ^= m[i & 3];
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
    fn slice_concat_utf8_xor() {
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
            assert_eq!(velt_rt_bytes_utf8_valid(&arr("héllo ✓".as_bytes())), 1);
            assert_eq!(velt_rt_bytes_utf8_valid(&arr(b"\xff\xfe")), 0);
            // Every length around the 8-byte chunks, and an offset start.
            for n in 0..20 {
                let plain: Vec<u8> = (0..n as u8).collect();
                let x = arr(&[&[9u8, 9][..], &plain].concat());
                velt_rt_bytes_xor_mask(&x, 0x01020304, 2, i64::MAX);
                let want: Vec<u8> = plain
                    .iter()
                    .enumerate()
                    .map(|(i, b)| b ^ [1, 2, 3, 4][i % 4])
                    .collect();
                assert_eq!(&x.as_bytes()[2..], &want[..]);
                assert_eq!(&x.as_bytes()[..2], &[9, 9]);
            }
        }
    }
}
