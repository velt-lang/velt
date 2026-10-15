//! Module state (module-level `let`s, rt_abi.md "Module state"): one block per task, allocated
//! zeroed and initialized by the program's `init` on the task's first access, dropped with the
//! task (its local promises included). Code that runs outside a task (a synchronous `main`)
//! uses a block of its thread.

use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::cell::UnsafeCell;

/// What the compiler emits once per program that has module state.
#[repr(C)]
pub struct ModuleDesc {
    pub size: u64,
    pub align: u64,
    /// Evaluates the initializers in module order and writes every slot.
    pub init: unsafe extern "C" fn(*mut u8),
    /// Drops every slot (the state tuple's drop glue).
    pub drop: unsafe extern "C" fn(*mut u8),
}

/// One task's (or thread's) block: null until first accessed.
pub(super) struct ModState {
    ptr: *mut u8,
    desc: *const ModuleDesc,
}

impl ModState {
    pub(super) const NONE: ModState = ModState {
        ptr: std::ptr::null_mut(),
        desc: std::ptr::null(),
    };

    /// The block, allocated and initialized on first use.
    ///
    /// # Safety
    /// `desc` must be a valid descriptor, the same one at every call for this state.
    pub(super) unsafe fn get(this: *mut ModState, desc: *const ModuleDesc) -> *mut u8 {
        if !(*this).ptr.is_null() {
            if (*this).desc == desc {
                return (*this).ptr;
            }
            // Code swapped in by `velt dev` with a state of its own: its module-level `let`s
            // start over. The old block is left as it is (old code may still refer to it).
            (*this).ptr = std::ptr::null_mut();
        }
        let d = &*desc;
        let layout = layout(d);
        let p = alloc_zeroed(layout);
        if p.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        // Set before `init` runs: the initializers read the slots written before them.
        (*this).ptr = p;
        (*this).desc = desc;
        (d.init)(p);
        p
    }
}

fn layout(d: &ModuleDesc) -> Layout {
    Layout::from_size_align((d.size as usize).max(1), (d.align as usize).max(1))
        .unwrap_or_else(|_| crate::panic::fatal("ICE: module state layout"))
}

impl Drop for ModState {
    fn drop(&mut self) {
        if self.ptr.is_null() {
            return;
        }
        let p = std::mem::replace(&mut self.ptr, std::ptr::null_mut());
        // SAFETY: allocated in `get` with this descriptor's layout and initialized by its `init`.
        unsafe {
            let d = &*self.desc;
            (d.drop)(p);
            dealloc(p, layout(d));
        }
    }
}

thread_local! {
    /// The block of code that runs outside a task on this thread.
    static OUTSIDE: UnsafeCell<ModState> = const { UnsafeCell::new(ModState::NONE) };
}

/// The module state of the task being polled (of this thread outside a task), initialized on
/// first access by `desc.init`.
///
/// # Safety
/// `desc` must point to the program's module descriptor.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_module_state(desc: *const ModuleDesc) -> *mut u8 {
    let tcx = super::CURRENT.with(std::cell::Cell::get);
    match tcx.as_ref() {
        Some(t) => ModState::get(t.state, desc),
        None => ModState::get(OUTSIDE.with(UnsafeCell::get), desc),
    }
}
