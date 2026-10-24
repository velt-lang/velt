//! Counted work of the parts of a check that once grew faster than the program: the
//! breadth-first type searches (`type_reach`, #859), the checks of arrow functions
//! (`body::expr::closure`, #859), of expressions (`FnCx::expr`, spread operands checked again
//! per operator) and the resolution of exports (`collect::exports`, barrels walked again per
//! imported name). The scaling tests compare these counts between program sizes, which unlike CPU
//! time doesn't depend on the load of the machine (#1502).

use std::sync::atomic::{AtomicU64, Ordering};

// Counted with a plain load and store, not an atomic add: the expression count is taken once per
// expression checked, where a locked add would cost more than the rest of the bookkeeping. Checks
// running at the same time may lose counts; the scaling tests run one check at a time.
static TYPE_SEARCH: AtomicU64 = AtomicU64::new(0);
static CLOSURE_CHECKS: AtomicU64 = AtomicU64::new(0);
static EXPR_CHECKS: AtomicU64 = AtomicU64::new(0);
static EXPORTS: AtomicU64 = AtomicU64::new(0);

/// Adds `n` to `counter`.
fn add(counter: &AtomicU64, n: u64) {
    counter.store(counter.load(Ordering::Relaxed) + n, Ordering::Relaxed);
}

/// Adds `n` types looked at by a type search.
pub(crate) fn add_type_search(n: u64) {
    add(&TYPE_SEARCH, n);
}

/// Counts one check of an arrow function's body (a trial against a member of a union of
/// function types, or the real one).
pub(crate) fn add_closure_check() {
    add(&CLOSURE_CHECKS, 1);
}

/// Counts one check of an expression (trials included).
pub(crate) fn add_expr_check() {
    add(&EXPR_CHECKS, 1);
}

/// Adds `n` module items or export names the resolution of exports looked at.
pub(crate) fn add_export_work(n: u64) {
    add(&EXPORTS, n);
}

/// The work of these kinds done in this process so far (checks run at the same time may lose
/// counts). A deterministic cost measure, like [`crate::InstantiationWork`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CheckWork {
    /// Types the breadth-first searches of the type graph looked at, past the first one of each
    /// search (`owns_resource`, `uncopyable_part`, `holds_promise`).
    pub type_search: u64,
    /// Arrow functions checked, trials included.
    pub closure_checks: u64,
    /// Expressions checked, trials included.
    pub expr_checks: u64,
    /// Module items and export names the resolution of exports looked at (`export *` chains,
    /// export lists, export tables).
    pub exports: u64,
}

/// See [`CheckWork`].
pub fn check_work() -> CheckWork {
    CheckWork {
        type_search: TYPE_SEARCH.load(Ordering::Relaxed),
        closure_checks: CLOSURE_CHECKS.load(Ordering::Relaxed),
        expr_checks: EXPR_CHECKS.load(Ordering::Relaxed),
        exports: EXPORTS.load(Ordering::Relaxed),
    }
}
