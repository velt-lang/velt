//! The state of the array methods a proxy runs (`velt/lang/proxy/array`, issue #823; rt_abi.md
//! "Array methods on a proxy"): the targets of the methods running on this thread, the target
//! whose trap call is running, and the holes the methods made in their targets. Node leaves a
//! hole where `pop`, `shift` and `splice` delete an element before they write the shorter length,
//! and before an element `unshift` and `splice` write past the end; Velt's arrays have none, so
//! the element stays (a gap is filled) and the standard library's `Reflect` reads these records
//! instead. A target is identified by its address, which cannot be reused while the method runs
//! (its proxy keeps it alive).

use std::cell::RefCell;

#[derive(Default)]
struct State {
    /// The targets of the running methods, innermost last.
    targets: Vec<usize>,
    /// The target whose trap the running method calls (0: none, as in a user callback),
    /// innermost last.
    windows: Vec<usize>,
    /// The holes, as (target, index).
    holes: Vec<(usize, i64)>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

/// A method on a proxy of the array at `target`: `op` 1 starts one (no trap call of it runs
/// yet), -1 ends it and answers the first hole it leaves in `target` (forgotten then; -1 for
/// none), 2 starts a trap call of it, -2 ends that call. Other `op`s answer -1.
#[no_mangle]
pub extern "C" fn velt_rt_proxy_array_call(target: *const u8, op: i64) -> i64 {
    let t = target as usize;
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        match op {
            1 => {
                s.targets.push(t);
                s.windows.push(0);
                0
            }
            -1 => {
                s.windows.pop();
                if let Some(p) = s.targets.iter().rposition(|&x| x == t) {
                    s.targets.remove(p);
                }
                if s.targets.contains(&t) {
                    return -1;
                }
                let first = s.holes.iter().filter(|h| h.0 == t).map(|h| h.1).min();
                s.holes.retain(|h| h.0 != t);
                first.unwrap_or(-1)
            }
            2 => {
                s.windows.push(t);
                0
            }
            -2 => {
                s.windows.pop();
                0
            }
            _ => -1,
        }
    })
}

/// A hole of the array at `array`: `op` 0 tests element `index` (1: a hole), 1 records it, only
/// in a trap call of a method on that array (else -1, and nothing is recorded), 2 forgets it
/// (both answer whether it was one), 3 forgets the holes from `index` on (a shorter length; 0),
/// and 4 gives the first hole below `index`, or -1.
#[no_mangle]
pub extern "C" fn velt_rt_proxy_array_hole(array: *const u8, index: i64, op: i64) -> i64 {
    let a = array as usize;
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let at = s.holes.iter().position(|&(x, i)| x == a && i == index);
        match op {
            0 => at.is_some() as i64,
            1 => {
                if a == 0 || s.windows.last() != Some(&a) {
                    return -1;
                }
                if at.is_none() {
                    s.holes.push((a, index));
                }
                at.is_some() as i64
            }
            2 => {
                if let Some(p) = at {
                    s.holes.swap_remove(p);
                }
                at.is_some() as i64
            }
            3 => {
                s.holes.retain(|&(x, i)| x != a || i < index);
                0
            }
            _ => s
                .holes
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
    fn holes_only_in_a_trap_call_on_the_target_and_reported_at_the_end() {
        let (a, b) = (8 as *const u8, 16 as *const u8);
        // No method running: nothing is recorded.
        assert_eq!(velt_rt_proxy_array_hole(a, 2, 1), -1);
        velt_rt_proxy_array_call(a, 1);
        // Between trap calls (a user callback) neither.
        assert_eq!(velt_rt_proxy_array_hole(a, 2, 1), -1);
        velt_rt_proxy_array_call(a, 2);
        assert_eq!(velt_rt_proxy_array_hole(a, 2, 1), 0);
        assert_eq!(velt_rt_proxy_array_hole(a, 2, 1), 1);
        // Another array is not the target.
        assert_eq!(velt_rt_proxy_array_hole(b, 2, 1), -1);
        // A method on another proxy inside the trap call.
        velt_rt_proxy_array_call(b, 1);
        assert_eq!(velt_rt_proxy_array_hole(a, 3, 1), -1);
        velt_rt_proxy_array_call(b, 2);
        assert_eq!(velt_rt_proxy_array_hole(b, 0, 1), 0);
        velt_rt_proxy_array_call(b, -2);
        assert_eq!(velt_rt_proxy_array_call(b, -1), 0);
        assert_eq!(velt_rt_proxy_array_hole(b, 0, 0), 0);
        assert_eq!(velt_rt_proxy_array_hole(a, 5, 1), 0);
        assert_eq!(velt_rt_proxy_array_hole(a, 9, 4), 2);
        velt_rt_proxy_array_hole(a, 3, 3);
        assert_eq!(velt_rt_proxy_array_hole(a, 5, 0), 0);
        velt_rt_proxy_array_call(a, -2);
        assert_eq!(velt_rt_proxy_array_call(a, -1), 2);
        assert_eq!(velt_rt_proxy_array_hole(a, 2, 0), 0);
    }
}
