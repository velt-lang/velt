//! The identity of a promise (see velt_rt's task/identity.rs, #1682): the original future of a
//! shared promise for every holder, and the promise a converted promise wraps for the wrapper,
//! which keeps a holder of it until its owner drops it.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use super::{shared, VeltFut};

thread_local! {
    /// The converted promises' holders of the promise they stand for, by the wrapper's address.
    static ALIASES: RefCell<HashMap<usize, usize>> = RefCell::new(HashMap::new());
    /// How many entries `ALIASES` has (most programs have none: a drop then looks nothing up).
    static COUNT: Cell<usize> = const { Cell::new(0) };
}

fn alias_of(f: *mut VeltFut) -> Option<*mut VeltFut> {
    if COUNT.with(Cell::get) == 0 {
        return None;
    }
    ALIASES.with(|m| m.borrow().get(&(f as usize)).map(|&p| p as *mut VeltFut))
}

/// The identity of promise `f` (null: 0).
///
/// # Safety
/// `f` must be null or a live heap future.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fut_id(f: *mut VeltFut) -> u64 {
    let mut f = f;
    for _ in 0..1024 {
        if f.is_null() {
            return 0;
        }
        match shared::origin(f) {
            Some(Ok(orig)) => f = orig,
            Some(Err(proxy)) => {
                f = proxy;
                continue;
            }
            None => {}
        }
        match alias_of(f) {
            Some(a) => f = a,
            None => return f as u64,
        }
    }
    f as u64
}

/// Converted promise `w` (a boxed future) wraps `inner`: it stands for the promise `inner`
/// holds, when anyone else can hold that one.
///
/// # Safety
/// `w` and `inner` must be live heap futures (or null), `w` a boxed future owned by the caller.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fut_alias(
    w: *mut VeltFut,
    inner: *mut VeltFut,
    _quiet: Option<super::all::ResultDropFn>,
) {
    if w.is_null() || inner.is_null() {
        return;
    }
    let of = match shared::origin(inner) {
        Some(Ok(_)) => inner,
        Some(Err(_)) => return,
        None => match alias_of(inner) {
            Some(a) => a,
            None => return,
        },
    };
    let holder = shared::another_holder(of);
    let old = ALIASES.with(|m| m.borrow_mut().insert(w as usize, holder as usize));
    match old {
        Some(stale) => super::velt_rt_fut_drop(stale as *mut VeltFut),
        None => COUNT.with(|c| c.set(c.get() + 1)),
    }
}

/// The holders shared from now on are a converted promise's views (`on`), or holders again.
/// This executor reports a converted promise's rejection as its own (no views).
#[no_mangle]
pub extern "C" fn velt_rt_fut_views(_on: u8) {}

/// The owner of boxed future `f` drops it: if it stood for another promise, it gives up its
/// holder of that one.
///
/// # Safety
/// `f` must be a live boxed future.
pub(super) unsafe fn forget(f: *mut VeltFut) {
    if COUNT.with(Cell::get) == 0 {
        return;
    }
    let holder = ALIASES.with(|m| m.borrow_mut().remove(&(f as usize)));
    if let Some(h) = holder {
        COUNT.with(|c| c.set(c.get() - 1));
        super::velt_rt_fut_drop(h as *mut VeltFut);
    }
}
