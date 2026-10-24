//! The cooperative budget on the Velt backend's workers: refilled for every task poll; the
//! runtime's combinators spend it ([`crate::task::coop`]). Only threads the backend drives (its
//! workers, and a thread inside its `block_on`) use it: code polled anywhere else (the tokio
//! runtime beside it, a test's own runtime) uses tokio's, which that runtime refills.

use std::cell::Cell;

/// Polls of children a task may make per turn (tokio's figure).
const BUDGET: u32 = 128;

thread_local! {
    static LEFT: Cell<u32> = const { Cell::new(BUDGET) };
    static UNCONSTRAINED: Cell<bool> = const { Cell::new(false) };
    static DRIVEN: Cell<bool> = const { Cell::new(false) };
}

/// Marks this thread as one the backend drives (a worker, for its whole life).
pub(super) fn drive() {
    DRIVEN.with(|d| d.set(true));
}

/// Runs `f` with this thread driven by the backend (`block_on`).
pub(super) fn driving<R>(f: impl FnOnce() -> R) -> R {
    let was = DRIVEN.with(|d| d.replace(true));
    let r = f();
    DRIVEN.with(|d| d.set(was));
    r
}

/// Whether the backend drives this thread: its budget applies here.
pub(crate) fn driven() -> bool {
    DRIVEN.with(Cell::get)
}

/// A task poll starts with a full budget.
pub(super) fn refill() {
    LEFT.with(|l| l.set(BUDGET));
}

pub(crate) fn has_budget_remaining() -> bool {
    UNCONSTRAINED.with(Cell::get) || LEFT.with(Cell::get) > 0
}

/// Spends one unit; false when the budget was already spent.
pub(crate) fn spend() -> bool {
    if UNCONSTRAINED.with(Cell::get) {
        return true;
    }
    LEFT.with(|l| match l.get() {
        0 => false,
        n => {
            l.set(n - 1);
            true
        }
    })
}

/// Runs `f` with the budget switched off.
pub(crate) fn unconstrained<R>(f: impl FnOnce() -> R) -> R {
    let was = UNCONSTRAINED.with(|u| u.replace(true));
    let r = f();
    UNCONSTRAINED.with(|u| u.set(was));
    r
}
