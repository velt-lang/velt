//! The rows of the hook table for `then`, `catch` and `finally` of a promise held as `any`
//! (std/lang/hooks/any_promise.velt, #924 step 2): called by sema only where the program's own
//! code uses one of these members on an `any` (`velt_sema` `any_promise_members.rs`), their module
//! loaded on demand. Kept apart from the rest of the table ([`super`]), which
//! [`with_promise_hooks`] gives on, with these rows after it, to `with_decorator_hooks!`.

/// `with_decorator_hooks!` over the rows given, then these.
macro_rules! with_promise_hooks {
    ($($rows:tt)*) => {
        with_decorator_hooks! {
            $($rows)*
            /// `p.then` / `catch` / `finally` read through `any`: the shared function.
            any_promise_method => lang_hooks_any_promise, "anyPromiseMethod", sig((&string) -> any);
            /// `p.then(…)` / `catch` / `finally` called through `any` on a promise.
            any_promise_call => lang_hooks_any_promise, "anyPromiseCall", sig((&unknown, &string, &[any]) -> any);
        }
    };
}
