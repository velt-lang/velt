//! `velt_rt_group_*`: the runtime half of `taskScope` (std/task.vlt): a count of the scope's
//! live child tasks that the scope waits on before it settles, so children never outlive it.
//! The handle is a registry key: a `TaskScope` copy used after its scope ended finds no group
//! (`enter` fails) instead of freed memory. No code pointers are stored (rt_abi_async.md §13.5).

use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::Notify;

use super::leaf::new_leaf;
use super::VeltFut;
use crate::registry::{Key, Registry};

/// Set in `Group::live` once the scope's wait saw no live child: the scope is closing, and a
/// late `enter` fails rather than start a child that would outlive it.
const CLOSED: usize = 1 << (usize::BITS - 1);

/// The live children of one scope.
#[derive(Default)]
pub struct Group {
    /// The number of live children, plus `CLOSED`.
    live: AtomicUsize,
    idle: Notify,
}

static GROUPS: Registry<Group> = Registry::new();

/// A new scope's group (no children).
#[no_mangle]
pub extern "C" fn velt_rt_group_new() -> Key<Group> {
    GROUPS.insert(Group::default())
}

/// A child is about to start; false (nothing counted) if the scope already ended or is closing.
#[no_mangle]
pub extern "C" fn velt_rt_group_enter(g: Key<Group>) -> bool {
    GROUPS.get(g).is_some_and(|grp| {
        let mut n = grp.live.load(Ordering::Acquire);
        while n & CLOSED == 0 {
            match grp
                .live
                .compare_exchange_weak(n, n + 1, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return true,
                Err(cur) => n = cur,
            }
        }
        false
    })
}

/// A child finished (fulfilled or rejected).
#[no_mangle]
pub extern "C" fn velt_rt_group_leave(g: Key<Group>) {
    if let Some(grp) = GROUPS.get(g) {
        if grp.live.fetch_sub(1, Ordering::AcqRel) == 1 {
            crate::io::publish_before_handoff();
            grp.idle.notify_waiters();
        }
    }
}

/// A future (unit result) that completes once no child is live, and closes the group then
/// (later `enter`s fail).
#[no_mangle]
pub extern "C" fn velt_rt_group_wait(g: Key<Group>) -> *mut VeltFut {
    let grp = GROUPS.get(g);
    new_leaf(async move {
        let Some(grp) = grp else { return };
        loop {
            let idle = grp.idle.notified();
            let done = grp
                .live
                .compare_exchange(0, CLOSED, Ordering::AcqRel, Ordering::Acquire);
            if matches!(done, Ok(_) | Err(CLOSED)) {
                return;
            }
            idle.await;
        }
    })
}

/// The scope ended: later `enter`s fail.
#[no_mangle]
pub extern "C" fn velt_rt_group_free(g: Key<Group>) {
    GROUPS.remove(g);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{raw_cx, velt_rt_fut_drop, velt_rt_fut_poll, PENDING, READY};
    use std::task::{Context, Waker};

    fn poll(f: *mut VeltFut) -> u32 {
        let mut cx = Context::from_waker(Waker::noop());
        // SAFETY: a live leaf future.
        unsafe { velt_rt_fut_poll(f, raw_cx(&mut cx)) }
    }

    #[test]
    fn wait_completes_when_the_last_child_leaves() {
        let g = velt_rt_group_new();
        assert!(velt_rt_group_enter(g));
        assert!(velt_rt_group_enter(g));
        let w = velt_rt_group_wait(g);
        assert_eq!(poll(w), PENDING);
        velt_rt_group_leave(g);
        assert_eq!(poll(w), PENDING);
        velt_rt_group_leave(g);
        assert_eq!(poll(w), READY);
        assert!(!velt_rt_group_enter(g), "the scope is closing");
        // SAFETY: owned future.
        unsafe { velt_rt_fut_drop(w) };
        velt_rt_group_free(g);
        assert!(!velt_rt_group_enter(g), "the scope ended");
    }
}
