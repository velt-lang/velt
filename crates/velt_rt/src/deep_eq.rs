//! The cycle guard of `toEqual` and `deepEqual` (velt_vir `lower/glue/eq_cycles.rs`): the pairs
//! of objects being compared field by field on this thread, with Vitest's
//! `aStack`/`bStack` rule: a pair met again while it is being compared (the same cycle on both
//! sides: a parent pointer, rings of one length) counts as equal, an object met again with
//! another partner (a cycle on one side, rings of two lengths) as different. The glue is
//! synchronous, so one comparison stays on one thread.

use std::cell::RefCell;

thread_local! {
    static PAIRS: RefCell<Vec<(usize, usize)>> = const { RefCell::new(Vec::new()) };
}

/// Whether `a` and `b`, about to be compared, are on the stack of pairs being compared, as
/// Vitest's `equals` checks it (from the innermost pair out): 1 if `a` is being compared with
/// `b` (a cycle at the same depth: equal), 2 if `a` is being compared with another object or `b`
/// is (a cycle on one side only: they differ); else 0, and the pair is being compared from now
/// until `velt_rt_deep_eq_leave`.
#[no_mangle]
pub extern "C" fn velt_rt_deep_eq_enter(a: *const u8, b: *const u8) -> u8 {
    let (a, b) = (a as usize, b as usize);
    PAIRS.with(|p| {
        let mut p = p.borrow_mut();
        for &(x, y) in p.iter().rev() {
            if x == a {
                return if y == b { 1 } else { 2 };
            }
            if y == b {
                return 2;
            }
        }
        p.push((a, b));
        0
    })
}

/// The pair `velt_rt_deep_eq_enter` last took on is compared.
#[no_mangle]
pub extern "C" fn velt_rt_deep_eq_leave() {
    PAIRS.with(|p| {
        p.borrow_mut().pop();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pair_met_again_is_equal_and_one_side_met_again_differs() {
        let (a, b, c) = (8 as *const u8, 16 as *const u8, 24 as *const u8);
        assert_eq!(velt_rt_deep_eq_enter(a, b), 0);
        assert_eq!(velt_rt_deep_eq_enter(a, b), 1);
        // `a` with another partner, or `b` with another one: a cycle on one side.
        assert_eq!(velt_rt_deep_eq_enter(a, c), 2);
        assert_eq!(velt_rt_deep_eq_enter(c, b), 2);
        velt_rt_deep_eq_leave();
        assert_eq!(velt_rt_deep_eq_enter(a, c), 0);
        velt_rt_deep_eq_leave();
    }
}
