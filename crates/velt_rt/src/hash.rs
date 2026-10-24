//! `velt_rt_str_hash`: the hash behind string `Map`/`Set` keys.
//!
//! A fast multiply–xor hash over 8-byte words with a murmur3 `fmix64` finalizer. It uses a fixed
//! seed, so hashes (and anything whose order depends on them) are deterministic across runs; it is
//! not designed to resist hash-flooding by adversarial keys.

const SEED: u64 = 0x243f_6a88_85a3_08d3;
const K: u64 = 0x9e37_79b9_7f4a_7c15;

#[inline]
fn fmix64(mut h: u64) -> u64 {
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
    h ^= h >> 33;
    h = h.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    h ^ (h >> 33)
}

#[inline]
fn mix(h: u64, word: u64) -> u64 {
    (h ^ word).wrapping_mul(K).rotate_left(29)
}

/// Hash of `bytes`.
pub fn hash(bytes: &[u8]) -> u64 {
    let mut h = SEED ^ (bytes.len() as u64).wrapping_mul(K);
    let (chunks, rest) = bytes.as_chunks::<8>();
    for c in chunks {
        h = mix(h, u64::from_le_bytes(*c));
    }
    if !rest.is_empty() {
        let mut tail = [0u8; 8];
        tail[..rest.len()].copy_from_slice(rest);
        h = mix(h, u64::from_le_bytes(tail));
    }
    fmix64(h)
}

/// Hash of the bytes of a string (any form: static, inline or heap).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_hash(s: *const crate::str::VeltStr) -> u64 {
    hash((*s).as_bytes())
}

/// The hash of the string whose UTF-8 bytes are `b` (`velt_rt_str_hash` of it), for looking up a
/// string key by its bytes without making the string (`Map.getUtf8`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bytes_hash(b: *const crate::bytes::VeltBytes) -> u64 {
    hash((*b).as_bytes())
}

/// Whether string `s` is the text whose UTF-8 bytes are `b` (string keys compare by bytes).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_eq_bytes(
    s: *const crate::str::VeltStr,
    b: *const crate::bytes::VeltBytes,
) -> u8 {
    u8::from((*s).as_bytes() == (*b).as_bytes())
}

/// `b[start..end]` with JS relative positions (negative ones count from the end, both clamped
/// to the bytes; an inverted range is empty), as `slice` takes them.
unsafe fn range(b: &crate::bytes::VeltBytes, start: i64, end: i64) -> &[u8] {
    let bytes = b.as_bytes();
    let start = crate::bytes_ops::relative(start, bytes.len());
    let end = crate::bytes_ops::relative(end, bytes.len());
    &bytes[start..end.max(start)]
}

/// Whether string `s`'s UTF-8 bytes are `b[start..end]` (relative positions): `u8[]`'s
/// `utf8Equals`, a server comparing a path or a method in its read buffer without making a
/// string.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_eq_range(
    s: *const crate::str::VeltStr,
    b: *const crate::bytes::VeltBytes,
    start: i64,
    end: i64,
) -> u8 {
    u8::from((*s).as_bytes() == range(&*b, start, end))
}

/// [`velt_rt_str_eq_range`] with ASCII letters compared regardless of case (header names).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_eq_range_ci(
    s: *const crate::str::VeltStr,
    b: *const crate::bytes::VeltBytes,
    start: i64,
    end: i64,
) -> u8 {
    u8::from((*s).as_bytes().eq_ignore_ascii_case(range(&*b, start, end)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn deterministic_and_spread() {
        let inline = crate::str::VeltStr::from_bytes(b"key");
        assert_eq!(hash(b"key"), unsafe { velt_rt_str_hash(&inline) });
        let empty = crate::str::VeltStr::empty();
        assert_eq!(unsafe { velt_rt_str_hash(&empty) }, hash(b""));
        assert_ne!(hash(b"a"), hash(b"a\0"));
        let keys: HashSet<u64> = (0..100_000)
            .map(|i| hash(format!("k{i}").as_bytes()))
            .collect();
        assert_eq!(keys.len(), 100_000);
        // Low bits (bucket index) are well distributed too.
        let buckets: HashSet<u64> = (0..4096)
            .map(|i| hash(format!("k{i}").as_bytes()) & 1023)
            .collect();
        assert!(buckets.len() > 950);
    }

    #[test]
    fn strings_compare_with_byte_ranges() {
        let buf =
            crate::bytes::VeltBytes::from_vec(b"GET /json HTTP/1.1\r\nContent-Length".to_vec());
        let json = crate::str::VeltStr::from_bytes(b"/json");
        let name = crate::str::VeltStr::from_bytes(b"content-length");
        unsafe {
            assert_eq!(velt_rt_str_eq_range(&json, &buf, 4, 9), 1);
            assert_eq!(velt_rt_str_eq_range(&json, &buf, 4, 8), 0);
            let tail = crate::str::VeltStr::from_bytes(b"Length");
            assert_eq!(
                velt_rt_str_eq_range(&tail, &buf, 28, 99),
                1,
                "an end past the bytes is clamped"
            );
            assert_eq!(
                velt_rt_str_eq_range(&tail, &buf, -6, i64::MAX),
                1,
                "from the end"
            );
            assert_eq!(velt_rt_str_eq_range(&name, &buf, 20, 34), 0, "case matters");
            assert_eq!(velt_rt_str_eq_range_ci(&name, &buf, 20, 34), 1);
            assert_eq!(
                velt_rt_str_eq_range_ci(&name, &buf, 9, 5),
                0,
                "an inverted range"
            );
        }
    }
}
