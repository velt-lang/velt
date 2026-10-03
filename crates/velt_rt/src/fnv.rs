//! `velt:hash` (std/hash.vlt): 64-bit FNV-1a, a stable, non-cryptographic hash for sharding and
//! bucketing. The algorithm and its constants are part of the std contract: the same input gives
//! the same hash on every platform, run and version. Not seeded, so not DoS-resistant.

use crate::bytes::VeltBytes;
use crate::str::VeltStr;

const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a (64-bit) of `bytes`.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(OFFSET_BASIS, |h, &b| (h ^ u64::from(b)).wrapping_mul(PRIME))
}

/// `fnv1a64(s)`: the hash of the UTF-8 bytes of a string.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fnv1a64_str(s: *const VeltStr) -> u64 {
    fnv1a64((*s).as_bytes())
}

/// `fnv1a64Bytes(data)`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fnv1a64_bytes(b: *const VeltBytes) -> u64 {
    fnv1a64((*b).as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test vectors from the FNV reference (draft-eastlake-fnv).
    #[test]
    fn matches_the_reference_vectors() {
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(b"foobar"), 0x8594_4171_f739_67e8);
        let s = VeltStr::from_bytes(b"foobar");
        assert_eq!(unsafe { velt_rt_fnv1a64_str(&s) }, 0x8594_4171_f739_67e8);
    }
}
