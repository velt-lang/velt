//! std/crypto's hashes: SHA-1, SHA-256 and their HMACs (RFC 2104), on the RustCrypto
//! implementations, which use the CPU's SHA instructions where it has them (ARMv8 crypto
//! extensions, x86 SHA-NI) and portable code elsewhere (also on wasm). Every result is a new
//! owned buffer; the inputs are borrowed.

use crate::bytes::VeltBytes;
use hmac::{EagerHash, Hmac, KeyInit, Mac};
use sha1::Sha1;
use sha2::{Digest, Sha256};

/// `sha1(data)`: the 20-byte digest.
///
/// # Safety
/// `data` must be a valid byte buffer and `out` writable.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_sha1(data: *const VeltBytes, out: *mut VeltBytes) {
    out.write(VeltBytes::from_vec(
        Sha1::digest((*data).as_bytes()).to_vec(),
    ));
}

/// `sha256(data)`: the 32-byte digest.
///
/// # Safety
/// `data` must be a valid byte buffer and `out` writable.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_sha256(data: *const VeltBytes, out: *mut VeltBytes) {
    out.write(VeltBytes::from_vec(
        Sha256::digest((*data).as_bytes()).to_vec(),
    ));
}

/// The HMAC of `data` under `key` with hash `D` (any key length: a longer key is hashed first).
fn hmac<D: EagerHash>(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<D>::new_from_slice(key).expect("ICE: HMAC takes keys of any length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// `hmacSha1(key, data)`: the 20-byte MAC.
///
/// # Safety
/// `key` and `data` must be valid byte buffers and `out` writable.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_hmac_sha1(
    key: *const VeltBytes,
    data: *const VeltBytes,
    out: *mut VeltBytes,
) {
    out.write(VeltBytes::from_vec(hmac::<Sha1>(
        (*key).as_bytes(),
        (*data).as_bytes(),
    )));
}

/// `hmacSha256(key, data)`: the 32-byte MAC.
///
/// # Safety
/// `key` and `data` must be valid byte buffers and `out` writable.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_hmac_sha256(
    key: *const VeltBytes,
    data: *const VeltBytes,
    out: *mut VeltBytes,
) {
    out.write(VeltBytes::from_vec(hmac::<Sha256>(
        (*key).as_bytes(),
        (*data).as_bytes(),
    )));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    fn call1(f: unsafe extern "C" fn(*const VeltBytes, *mut VeltBytes), data: &[u8]) -> String {
        let input = VeltBytes::from_vec(data.to_vec());
        let mut out = std::mem::MaybeUninit::<VeltBytes>::uninit();
        unsafe {
            f(&input, out.as_mut_ptr());
            hex(out.assume_init().as_bytes())
        }
    }

    fn call2(
        f: unsafe extern "C" fn(*const VeltBytes, *const VeltBytes, *mut VeltBytes),
        key: &[u8],
        data: &[u8],
    ) -> String {
        let (k, d) = (
            VeltBytes::from_vec(key.to_vec()),
            VeltBytes::from_vec(data.to_vec()),
        );
        let mut out = std::mem::MaybeUninit::<VeltBytes>::uninit();
        unsafe {
            f(&k, &d, out.as_mut_ptr());
            hex(out.assume_init().as_bytes())
        }
    }

    #[test]
    fn digests_match_fips_180_vectors() {
        assert_eq!(
            call1(velt_rt_sha1, b"abc"),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            call1(velt_rt_sha256, b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            call1(velt_rt_sha1, b""),
            "da39a3ee5e6b4b0d3255bfef95601890afd80709"
        );
    }

    #[test]
    fn hmacs_match_rfc_vectors() {
        // RFC 2202 test case 2 and RFC 4231 test case 2.
        assert_eq!(
            call2(velt_rt_hmac_sha1, b"Jefe", b"what do ya want for nothing?"),
            "effcdf6ae5eb2fa2d27416d5f184df9c259a7c79"
        );
        assert_eq!(
            call2(
                velt_rt_hmac_sha256,
                b"Jefe",
                b"what do ya want for nothing?"
            ),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // A key longer than the block is hashed first (RFC 4231 test case 6).
        assert_eq!(
            call2(
                velt_rt_hmac_sha256,
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            ),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }
}
