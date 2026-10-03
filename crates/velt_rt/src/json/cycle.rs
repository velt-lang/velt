//! `JSON.stringify` of a value that contains itself: a recursive object type
//! (`interface Node { next?: Node }`) can form a cycle (`n.next = n`), which JavaScript reports
//! as `TypeError: Converting circular structure to JSON`. The generated writer for such a type
//! calls `enter` with the object's address before writing its members and `leave` after; the
//! objects being written form a stack per thread (writing never suspends).

use std::cell::RefCell;

thread_local! {
    static WRITING: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

/// Start writing the object at `p`: 1, or 0 if it is already being written (a cycle).
#[no_mangle]
pub extern "C" fn velt_rt_json_enter(p: *const u8) -> u8 {
    WRITING.with(|w| {
        let mut w = w.borrow_mut();
        if w.contains(&(p as usize)) {
            return 0;
        }
        w.push(p as usize);
        1
    })
}

/// Done writing the innermost object started with `velt_rt_json_enter`.
#[no_mangle]
pub extern "C" fn velt_rt_json_leave() {
    WRITING.with(|w| {
        w.borrow_mut().pop();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_object_inside_itself_is_a_cycle() {
        let (a, b) = (1u8, 2u8);
        assert_eq!(velt_rt_json_enter(&a), 1);
        assert_eq!(velt_rt_json_enter(&b), 1);
        assert_eq!(velt_rt_json_enter(&a), 0);
        velt_rt_json_leave();
        velt_rt_json_leave();
        // Written twice side by side is not a cycle.
        assert_eq!(velt_rt_json_enter(&b), 1);
        velt_rt_json_leave();
    }
}
