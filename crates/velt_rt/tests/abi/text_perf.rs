//! The string builder on large inputs. Timings are only printed
//! (`cargo test -p velt_rt --release --lib text_perf -- --nocapture`): what is checked is
//! counted, so a loaded machine can't fail it. Speed is measured by `bench/strings.velt`.

use crate::str::{velt_rt_str_drop, VeltStr};
use crate::strbuf::*;
use std::mem::MaybeUninit;
use std::time::Instant;

/// The capacity of a heap builder: the third word of the `VeltStr` (docs/internals/contracts).
fn heap_capacity(b: &VeltStr) -> Option<u64> {
    // SAFETY: a `VeltStr` is three `u64` words.
    b.is_heap()
        .then(|| unsafe { *(b as *const VeltStr as *const u64).add(2) })
}

/// 1M small pushes grow the builder geometrically: its capacity changes a few dozen times, not
/// once every few pushes (which would copy the text again each time: quadratic). Counted, not
/// timed, so a loaded machine can't fail it; the time is only printed (the speed of string
/// building is measured by bench/strings.velt).
#[test]
fn builder_1m_small_pushes() {
    let part = VeltStr::from_static(b"abc");
    let t = Instant::now();
    let mut grows = 0;
    let mut b = MaybeUninit::uninit();
    let mut out = MaybeUninit::uninit();
    unsafe {
        velt_rt_strbuf_new(0, b.as_mut_ptr());
        let mut b = b.assume_init();
        let mut cap = heap_capacity(&b);
        for i in 0..1_000_000i64 {
            velt_rt_strbuf_push_str(&mut b, &part);
            velt_rt_strbuf_push_byte(&mut b, b'0' + (i & 7) as u8);
            let now = heap_capacity(&b);
            if now != cap {
                grows += 1;
                cap = now;
            }
        }
        velt_rt_strbuf_finish(&mut b, out.as_mut_ptr());
    }
    let elapsed = t.elapsed();
    let mut s = unsafe { out.assume_init() };
    assert_eq!(s.len(), 4_000_000);
    unsafe { velt_rt_str_drop(&mut s) };
    eprintln!("strbuf: 1M push_str + 1M push_i64: {elapsed:?}, {grows} grows");
    // Doubling from 32 bytes to 4 MB: about 17.
    assert!(grows <= 40, "the builder grew {grows} times");
}
