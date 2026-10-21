//! The state of the array methods a proxy runs (`velt:proxy/array`, issue #823; rt_abi.md
//! "Array methods on a proxy"): how many run on this thread, and the holes they made in the
//! arrays their traps reached. Node leaves a hole where `pop`, `shift` and `splice` delete an
//! element before they write the shorter length, and before an element `unshift` and `splice`
//! write past the end; Velt's arrays have none, so the element stays (a gap is filled) and the
//! standard library's `Reflect` reads these records instead. They are cleared when the last
//! method returns, so an array's address (its identity while a method runs) is never reused for
//! another one in them.

use std::cell::{Cell, RefCell};

thread_local! {
    static DEPTH: Cell<i64> = const { Cell::new(0) };
    static HOLES: RefCell<Vec<(usize, i64)>> = const { RefCell::new(Vec::new()) };
}

/// Adds `delta` to the number of array methods running on proxies on this thread and returns
/// the new number; at 0 the holes are forgotten.
#[no_mangle]
pub extern "C" fn velt_rt_proxy_array_depth(delta: i64) -> i64 {
    let d = DEPTH.with(|c| {
        let d = (c.get() + delta).max(0);
        c.set(d);
        d
    });
    if d == 0 {
        HOLES.with(|h| h.borrow_mut().clear());
    }
    d
}

/// A hole of the array at `array`: `op` 0 tests element `index` (1: a hole), 1 records it and 2
/// forgets it (both answer whether it was one), 3 forgets the holes from `index` on (a shorter
/// length; 0), and 4 gives the first hole below `index`, or -1.
#[no_mangle]
pub extern "C" fn velt_rt_proxy_array_hole(array: *const u8, index: i64, op: i64) -> i64 {
    let a = array as usize;
    HOLES.with(|h| {
        let mut h = h.borrow_mut();
        let at = h.iter().position(|&(x, i)| x == a && i == index);
        match op {
            0 => at.is_some() as i64,
            1 => {
                if at.is_none() {
                    h.push((a, index));
                }
                at.is_some() as i64
            }
            2 => {
                if let Some(p) = at {
                    h.swap_remove(p);
                }
                at.is_some() as i64
            }
            3 => {
                h.retain(|&(x, i)| x != a || i < index);
                0
            }
            _ => h
                .iter()
                .filter(|&&(x, i)| x == a && i < index)
                .map(|&(_, i)| i)
                .min()
                .unwrap_or(-1),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holes_are_per_array_and_go_with_the_last_method() {
        let (a, b) = (8 as *const u8, 16 as *const u8);
        assert_eq!(velt_rt_proxy_array_depth(1), 1);
        assert_eq!(velt_rt_proxy_array_hole(a, 2, 1), 0);
        assert_eq!(velt_rt_proxy_array_hole(a, 2, 1), 1);
        assert_eq!(velt_rt_proxy_array_hole(a, 2, 0), 1);
        assert_eq!(velt_rt_proxy_array_hole(b, 2, 0), 0);
        assert_eq!(velt_rt_proxy_array_hole(a, 5, 1), 0);
        assert_eq!(velt_rt_proxy_array_hole(a, 9, 4), 2);
        velt_rt_proxy_array_hole(a, 3, 3);
        assert_eq!(velt_rt_proxy_array_hole(a, 5, 0), 0);
        assert_eq!(velt_rt_proxy_array_hole(a, 2, 2), 1);
        assert_eq!(velt_rt_proxy_array_hole(a, 9, 4), -1);
        assert_eq!(velt_rt_proxy_array_depth(1), 2);
        velt_rt_proxy_array_hole(b, 0, 1);
        assert_eq!(velt_rt_proxy_array_depth(-1), 1);
        assert_eq!(velt_rt_proxy_array_hole(b, 0, 0), 1);
        assert_eq!(velt_rt_proxy_array_depth(-1), 0);
        assert_eq!(velt_rt_proxy_array_hole(b, 0, 0), 0);
    }
}
