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

/// A fresh WebSocket masking key (RFC 6455 §5.3, §10.3: unpredictable, from the OS random
/// source), taken from a per-thread buffer refilled 64 keys at a time.
#[no_mangle]
pub extern "C" fn velt_rt_random_mask() -> u32 {
    use std::cell::RefCell;
    thread_local! {
        static KEYS: RefCell<([u8; 256], usize)> = const { RefCell::new(([0; 256], 256)) };
    }
    KEYS.with(|k| {
        let (buf, used) = &mut *k.borrow_mut();
        if *used == buf.len() {
            if let Err(e) = getrandom::fill(buf) {
                crate::panic::fatal(&format!("the OS random number generator failed: {e}"));
            }
            *used = 0;
        }
        let key = u32::from_be_bytes([buf[*used], buf[*used + 1], buf[*used + 2], buf[*used + 3]]);
        *used += 4;
        key
    })
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
        let keys: std::collections::HashSet<u32> =
            (0..200).map(|_| velt_rt_random_mask()).collect();
        assert!(keys.len() > 190, "{} distinct mask keys of 200", keys.len());
    }
}
