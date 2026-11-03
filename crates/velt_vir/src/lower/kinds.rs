//! How a borrow-ABI entry point adapts its first argument, and when a function runs in read
//! mode.

use velt_sema::hir::TyId;

/// How a borrow-ABI entry point adapts its first `Ptr` argument (see glue/thunk.rs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum ThunkKind {
    /// First param is a closure env pointer that the target (a named function) ignores. With
    /// an error type: the function value's type allows more errors than the target throws,
    /// so the thunk returns `Result<ret, E>` and converts the target's result (or errors).
    Env(Option<TyId>),
    /// First param is the receiver's data pointer (Dyn data / class object) → target's `this`.
    SelfData,
    /// First param is the data pointer of an interface value holding a closure (a generic
    /// closure as a generic function value, glue/generic_slots.rs): the closure's code runs
    /// with the closure's env.
    BoxedClosure,
    /// The code of a method read as a value through `any` (a typed proxy's `get` trap answers
    /// a method call with it, proxy_method.rs): the method's parameters after a closure env;
    /// called on its own (without the proxy as `this`) it throws a `TypeError`.
    Unbound,
}

/// When a function runs in velt_rt's read mode (boxing/locks.rs), which decides how it counts
/// and writes lock-set objects (rc.rs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub(crate) enum LkMode {
    /// Never: inline counts.
    #[default]
    Plain,
    /// Always (code only a `read` callback calls directly): read mode's inline rules.
    Read,
    /// Either: asks the thread once (`velt_rt_lk_mode`).
    Checked,
    /// Plain's counts, in code that changes the caller's containers inside a `read` callback
    /// (`Work::Deferred`): an object of the lock set whose last reference goes is freed once the
    /// thread's reads end (velt_rt `velt_rt_free_after_reads`), since a reader may still point
    /// at it without counting it (read mode cannot tell it from the store's).
    Deferred,
}
