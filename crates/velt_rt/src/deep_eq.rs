//! `toEqual`'s cycle guard (`Glue::DeepEq`, velt_vir `lower/glue/eq.rs`): the pairs of class
//! instances being compared field by field on this thread. A pair met again while it is being
//! compared (a cycle: a parent pointer, a ring) counts as equal, as Vitest's `aStack`/`bStack`
//! rule has it. The glue is synchronous, so one comparison stays on one thread.

use std::cell::RefCell;

thread_local! {
    static PAIRS: RefCell<Vec<(usize, usize)>> = const { RefCell::new(Vec::new()) };
}

/// 1 if `a` and `b` are being compared already (the comparison may take them as equal); else
/// 0, and the pair is being compared from now until `velt_rt_deep_eq_leave`.
#[no_mangle]
pub extern "C" fn velt_rt_deep_eq_enter(a: *const u8, b: *const u8) -> u8 {
    let pair = (a as usize, b as usize);
    PAIRS.with(|p| {
        let mut p = p.borrow_mut();
        if p.contains(&pair) {
            return 1;
        }
        p.push(pair);
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
    fn a_pair_met_again_while_compared_counts_as_equal() {
        let (a, b) = (8 as *const u8, 16 as *const u8);
        assert_eq!(velt_rt_deep_eq_enter(a, b), 0);
        assert_eq!(velt_rt_deep_eq_enter(b, a), 0);
        assert_eq!(velt_rt_deep_eq_enter(a, b), 1);
        velt_rt_deep_eq_leave();
        velt_rt_deep_eq_leave();
        assert_eq!(velt_rt_deep_eq_enter(a, b), 0);
        velt_rt_deep_eq_leave();
    }
}
