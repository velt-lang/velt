//! The yields' rows of the hook table (std/lang/hooks/sched_yield.velt: `yieldNow()` under the
//! executor in Velt, #3241, #3687): an awaited yield, its resume and its cancel, and a stored
//! one. Kept apart from the rest of the table ([`super`]), which [`with_yield_hooks`] gives to
//! `with_any_method_hooks!` with these rows after it.

/// `with_any_method_hooks!` over the rows given, then the yields'.
macro_rules! with_yield_hooks {
    ($($rows:tt)*) => {
        with_any_method_hooks! {
            $($rows)*
            /// `await yieldNow()`'s: `(cx)`, its record (the token, 0: none, #3687); its resume `(cx,
            /// record)`, 1 to go on (#3241); its cancel's drop of the record; a stored one's heap future.
            sched_yield => lang_hooks_sched_yield, "schedYield", sig((usize) -> usize), lowered;
            sched_yield_ready => lang_hooks_sched_yield, "schedYieldReady", sig((usize, usize) -> u32), lowered;
            sched_yield_drop => lang_hooks_sched_yield, "schedYieldDrop", sig((usize) -> void), lowered;
            sched_yield_fut => lang_hooks_sched_yield, "schedYieldFut", sig(() -> usize), lowered;
        }
    };
}
