//! The rows of the hook table for the copies made during one transfer of a value to another
//! thread (std/lang/hooks/transfer.velt, #3584; rt_abi_async.md "Transfers"): lowered for the
//! transfer and clone glue (velt_vir `lower/transfer_map.rs`) of a program with a construct
//! that can start a transfer (`Construct::Transfer`), which loads the module. The runtime calls
//! none of them. Kept apart from the rest of the table ([`super`]), which [`with_transfer_hooks`]
//! gives to `with_yield_hooks!` with these rows after it.

/// `with_yield_hooks!` over the rows given, then the transfer map's.
macro_rules! with_transfer_hooks {
    ($($rows:tt)*) => {
        with_yield_hooks! {
            $($rows)*
            // transfer.velt: the transfer map (#3584).
            /// The start of a transfer (nested ones nest).
            xfer_begin => lang_hooks_transfer, "xferBegin", sig(() -> void), lowered, constructs(Transfer),
                replaces("velt_rt_xfer_begin"), bench("transfer_graph");
            /// The start of a transfer keeping what a `read` callback's caller holds.
            xfer_keep_begin => lang_hooks_transfer, "xferKeepBegin", sig(() -> void), lowered, constructs(Transfer),
                replaces("velt_rt_xfer_keep_begin");
            /// The start of a transfer keeping what a `read` callback made.
            xfer_share_begin => lang_hooks_transfer, "xferShareBegin", sig(() -> void), lowered, constructs(Transfer),
                replaces("velt_rt_xfer_share_begin");
            /// 1 in a transfer `xfer_keep_begin` started, 2 in one `xfer_share_begin` started,
            /// else 0; generated code passes it to read mode's `velt_rt_lk_leave`.
            xfer_keeps => lang_hooks_transfer, "xferKeeps", sig(() -> u8), lowered, constructs(Transfer),
                replaces("velt_rt_xfer_keeps");
            /// The end of a transfer: the outermost forgets the copies and releases what it
            /// deferred.
            xfer_end => lang_hooks_transfer, "xferEnd", sig(() -> void), lowered, constructs(Transfer),
                replaces("velt_rt_xfer_end");
            /// `(object)`: its copy in this transfer, 0 if none yet, 1 outside a transfer.
            xfer_find => lang_hooks_transfer, "xferFind", sig((usize) -> usize), lowered, constructs(Transfer),
                replaces("velt_rt_xfer_find"), bench("transfer_graph");
            /// `(object, copy)`: the copy made of the object in this transfer.
            xfer_record => lang_hooks_transfer, "xferRecord", sig((usize, usize) -> void), lowered, constructs(Transfer),
                replaces("velt_rt_xfer_record"), bench("transfer_graph");
            /// `(object, drop glue)`: its reference released at the end of the transfer (1), or 0
            /// outside one (the caller releases it now).
            xfer_defer => lang_hooks_transfer, "xferDefer", sig((usize, usize) -> u8), lowered, constructs(Transfer),
                replaces("velt_rt_xfer_defer");
            /// The transfer set aside while user code runs: its depth.
            xfer_suspend => lang_hooks_transfer, "xferSuspend", sig(() -> u32), lowered, constructs(Transfer),
                replaces("velt_rt_xfer_suspend");
            /// `(depth)`: the transfer `xfer_suspend` set aside, continued.
            xfer_resume => lang_hooks_transfer, "xferResume", sig((u32) -> void), lowered, constructs(Transfer),
                replaces("velt_rt_xfer_resume");
        }
    };
}
