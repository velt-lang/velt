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
}
