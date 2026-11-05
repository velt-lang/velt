//! One walk of the many-threads check (velt_vir glue/many.rs): whether it is the strict one, for a
//! function about to be called from several threads on a copy of what it captured, and which
//! objects it has been through, so that a cycle ends. The compiler brackets every walk with `begin`
//! and `end`; a walk runs no user code, so walks don't nest, and a walk a panic stopped is
//! replaced by the next `begin`.

use std::cell::RefCell;
use std::collections::HashSet;

struct Walk {
    strict: bool,
    seen: HashSet<usize>,
}

thread_local! {
    static WALK: RefCell<Option<Walk>> = const { RefCell::new(None) };
}

/// Start a walk; `strict` is not 0 when the value is a function in `shared(...)`.
#[no_mangle]
pub extern "C" fn velt_rt_many_begin(strict: u8) {
    WALK.with(|w| {
        *w.borrow_mut() = Some(Walk {
            strict: strict != 0,
            seen: HashSet::new(),
        });
    });
}

/// End the walk.
#[no_mangle]
pub extern "C" fn velt_rt_many_end() {
    WALK.with(|w| *w.borrow_mut() = None);
}

/// 1 inside a strict walk, else 0.
#[no_mangle]
pub extern "C" fn velt_rt_many_strict() -> u8 {
    WALK.with(|w| u8::from(w.borrow().as_ref().is_some_and(|w| w.strict)))
}

/// 1 the first time this walk reaches the object at `p`, else 0.
#[no_mangle]
pub extern "C" fn velt_rt_many_first(p: *const u8) -> u8 {
    WALK.with(|w| match w.borrow_mut().as_mut() {
        Some(w) => u8::from(w.seen.insert(p as usize)),
        None => 1,
    })
}
