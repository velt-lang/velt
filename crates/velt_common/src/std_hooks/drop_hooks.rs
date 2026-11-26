//! The rows of the hook table for bounded drop nesting (#543; std/lang/hooks/drop_queue.velt,
//! `velt_vir` `lower/glue/drop_depth.rs`): the queue a drop past the depth limit hands its value
//! to, and its drain, which lowering calls on those cold paths. The state word the common path
//! reads and writes is the module's `STATE`, which lowering names directly
//! (`velt_vir::tls::DROP_STATE_SLOT`), so no hook stands for it. Kept apart from the rest of the
//! table ([`super`]), which [`with_drop_hooks`] gives on, with these rows after it, to
//! `with_decorator_hooks!`.

/// `with_decorator_hooks!` over the rows given, then these.
macro_rules! with_drop_hooks {
    ($($rows:tt)*) => {
        with_decorator_hooks! {
            $($rows)*
            /// A bracketed drop past the depth limit: `(value, glue)`, dropped by `glue` when the
            /// outermost drop ends.
            drop_queue => lang_hooks_drop_queue, "dropQueue", sig((usize, usize) -> void), lowered,
                replaces("velt_rt_drop_queue");
            /// The outermost bracketed drop's end with a value queued: drops the queue.
            drop_drain => lang_hooks_drop_queue, "dropDrain", sig(() -> void), lowered,
                replaces("velt_rt_drop_drain");
        }
    };
}
