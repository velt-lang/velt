//! The rows of the hook table for a promise held as `any` (#924): an `await` of one
//! (std/lang/hooks/any_ops.velt, step 1), and its `then`, `catch` and `finally`
//! (std/lang/hooks/any_promise.velt, step 2), which sema calls only where the program's own code
//! uses one of these members on an `any` (`velt_sema` `any_promise_members.rs`), their module
//! loaded on demand. Kept apart from the rest of the table ([`super`]), which
//! [`with_promise_hooks`] gives on, with these rows after it, to `with_drop_hooks!`.

/// `with_drop_hooks!` over the rows given, then these.
macro_rules! with_promise_hooks {
    ($($rows:tt)*) => {
        with_drop_hooks! {
            $($rows)*
            /// What an `await` of an `any` or `unknown` waits for (`velt_sema` `await_value.rs`).
            await_any => lang_hooks_any_ops, "awaitAny", sig((&unknown) -> Promise(unknown, unknown));
            /// Does an `any` hold an object or a function (the thenable stop, `await_value.rs`, #3412)?
            any_holds_object => lang_hooks_any_ops, "anyHoldsObject", sig((&any) -> bool);
            /// `p.then` / `catch` / `finally` read through `any`: the shared function.
            any_promise_method => lang_hooks_any_promise, "anyPromiseMethod", sig((&string) -> any);
            /// `p.then(…)` / `catch` / `finally` called through `any` on a promise.
            any_promise_call => lang_hooks_any_promise, "anyPromiseCall", sig((&unknown, &string, &[any]) -> any);
        }
    };
}
