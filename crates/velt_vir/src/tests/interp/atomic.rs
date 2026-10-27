//! The `velt_atomic_*` externs (`crate::atomic`), which the backends emit as instructions: one
//! thread, so every ordering is the plain operation and a weak compare-and-swap never fails
//! spuriously.

use super::arith::mask;
use super::Interp;
use crate::atomic::{AtomicCall, Rmw};

/// `x` (the low `bits` bits) sign-extended.
fn signed(x: u64, bits: u32) -> i64 {
    ((x << (64 - bits)) as i64) >> (64 - bits)
}

impl Interp<'_> {
    /// Run the atomic operation `sym` on `a` (the word's address first).
    pub(super) fn atomic(&mut self, sym: &str, a: &[u64]) -> u64 {
        let call = AtomicCall::of_symbol(sym)
            .unwrap_or_else(|| panic!("interp: invalid atomic operation {sym}"));
        let bits = call.bits();
        let (m, n) = (mask(bits), (bits / 8) as usize);
        let word = |it: &mut Self| {
            let mut buf = [0u8; 8];
            buf[..n].copy_from_slice(&it.read_bytes(a[0], n));
            u64::from_le_bytes(buf)
        };
        let put = |it: &mut Self, v: u64| it.write_bytes(a[0], &(v & m).to_le_bytes()[..n]);
        match call {
            AtomicCall::Load { .. } => word(self),
            AtomicCall::Store { .. } => {
                put(self, a[1]);
                0
            }
            AtomicCall::Rmw { op, .. } => {
                let (old, v) = (word(self), a[1] & m);
                let (so, sv) = (signed(old, bits), signed(v, bits));
                let new = match op {
                    Rmw::Xchg => v,
                    Rmw::Add => old.wrapping_add(v),
                    Rmw::Sub => old.wrapping_sub(v),
                    Rmw::And => old & v,
                    Rmw::Or => old | v,
                    Rmw::Xor => old ^ v,
                    Rmw::Max => so.max(sv) as u64,
                    Rmw::Min => so.min(sv) as u64,
                    Rmw::UMax => old.max(v),
                    Rmw::UMin => old.min(v),
                };
                put(self, new);
                old
            }
            AtomicCall::Cas { weak, .. } => {
                let old = word(self);
                let swapped = old == a[1] & m;
                if swapped {
                    put(self, a[2]);
                }
                if weak {
                    swapped as u64
                } else {
                    old
                }
            }
            AtomicCall::Fence(_) | AtomicCall::SpinHint => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vir::Program;

    #[test]
    fn operations_give_the_previous_value_at_every_width() {
        let p = Program::default();
        let mut it = Interp::new(&p);
        let w = it.heap_alloc(16);
        for bits in [8u32, 16, 32, 64] {
            let m = mask(bits);
            let sym = |op: &str| format!("velt_atomic_{op}_{bits}_seqcst");
            it.write_bytes(w, &[0xAA; 16]);
            it.atomic(&format!("velt_atomic_store_{bits}_rel"), &[w, 5]);
            assert_eq!(it.atomic(&format!("velt_atomic_load_{bits}_acq"), &[w]), 5);
            // The bytes past the word are untouched.
            assert_eq!(it.read_bytes(w + u64::from(bits / 8), 1), [0xAA]);
            assert_eq!(it.atomic(&sym("add"), &[w, m]), 5); // 5 + -1
            assert_eq!(it.atomic(&sym("sub"), &[w, 6]), 4); // 4 - 6 = -2
            assert_eq!(it.atomic(&sym("xchg"), &[w, 0b1100]), m - 1);
            assert_eq!(it.atomic(&sym("and"), &[w, 0b1010]), 0b1100);
            assert_eq!(it.atomic(&sym("or"), &[w, 0b0101]), 0b1000);
            assert_eq!(it.atomic(&sym("xor"), &[w, 0b1111]), 0b1101);
            // -1 is less than 2 signed and more unsigned.
            it.atomic(&sym("xchg"), &[w, m]);
            assert_eq!(it.atomic(&sym("max"), &[w, 2]), m);
            assert_eq!(it.atomic(&sym("min"), &[w, m]), 2);
            assert_eq!(it.atomic(&sym("umin"), &[w, 7]), m);
            assert_eq!(it.atomic(&sym("umax"), &[w, m]), 7);
            let cas = format!("velt_atomic_cas_{bits}_acqrel_acq");
            let weak = format!("velt_atomic_casweak_{bits}_rlx_rlx");
            assert_eq!(it.atomic(&cas, &[w, 1, 9]), m, "fails: gives the word");
            assert_eq!(it.atomic(&cas, &[w, m, 9]), m, "succeeds");
            assert_eq!(it.atomic(&weak, &[w, 1, 3]), 0);
            assert_eq!(it.atomic(&weak, &[w, 9, 3]), 1);
            assert_eq!(it.atomic(&sym("load").replace("seqcst", "rlx"), &[w]), 3);
        }
        assert_eq!(it.atomic("velt_atomic_fence_acqrel", &[]), 0);
        assert_eq!(it.atomic("velt_atomic_spin_hint", &[]), 0);
    }
}
