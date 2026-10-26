//! `std/crypto` randomness: bytes from the operating system's CSPRNG (`getrandom`), for
//! `randomBytes`, UUIDs and anything else that must be unpredictable. Velt code has no other
//! source of entropy (module state is immutable, so a user-space generator would need seeding
//! from here anyway).

use crate::bytes::VeltBytes;

/// `randomBytes(n)`: `n` bytes from the OS CSPRNG as an owned buffer. The OS generator failing
/// is not recoverable (no safe fallback exists), so it is fatal like an allocation failure.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_random_bytes(n: u64, out: *mut VeltBytes) {
    out.write(VeltBytes::from_vec(random_vec(n as usize)));
}

/// A uniformly random `u64` from the OS CSPRNG.
#[no_mangle]
pub extern "C" fn velt_rt_random_u64() -> u64 {
    let bytes = random_vec(8);
    let mut word = [0u8; 8];
    word.copy_from_slice(&bytes);
    u64::from_le_bytes(word)
}

fn random_vec(n: usize) -> Vec<u8> {
    let mut v = vec![0u8; n];
    if let Err(e) = getrandom::fill(&mut v) {
        crate::panic::fatal(&format!("the OS random number generator failed: {e}"));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_have_the_requested_length_and_vary() {
        let mut a = VeltBytes::from_vec(vec![]);
        let mut b = VeltBytes::from_vec(vec![]);
        // SAFETY: valid out-pointers; the buffers are reclaimed below.
        unsafe {
            velt_rt_random_bytes(32, &mut a);
            velt_rt_random_bytes(32, &mut b);
            let (x, y) = (a.take_vec(), b.take_vec());
            assert_eq!((x.len(), y.len()), (32, 32));
            assert_ne!(x, y, "two 256-bit draws collided");
        }
        assert_ne!(velt_rt_random_u64(), velt_rt_random_u64());
    }
}
