//! The runtime externs ported to std whose Rust is gone (split from `std_hooks` for size).

/// The runtime externs whose job moved to std and whose Rust was deleted (#2196's port tracker: an
/// entry here is a finished port): hooks took over the externs of [`Hook::replaces`](crate::std_hooks::Hook::replaces), which keep
/// their place there as the record of what the hook replaced, and ordinary std code the ones only
/// std called, which no hook replaces. `velt_common`'s test `hook_replaces.rs` checks that every
/// replaced extern is defined by `velt_rt` unless it is listed here, and that one listed here is
/// defined nowhere and named by no compiler source, runtime or std file.
pub const DELETED: &[&str] = &[
    // #2197: integers as text (std/lang/hooks/number_text, velt/lang/number/integer).
    "velt_rt_str_from_i64",
    "velt_rt_str_from_u64",
    "velt_rt_strbuf_push_i64",
    "velt_rt_strbuf_push_u64",
    // #2196, only std called them: error-code names (std/native/error).
    "velt_rt_err_code_name",
    // #2196: the generator (std/lang/random/wyrand), which only std called, so no hook replaces it.
    "velt_rt_prng_f64",
    "velt_rt_prng_range",
    // #2196: collation (std/lang/string/collate).
    "velt_rt_str_locale_compare",
    // #2196: FNV-1a (std/hash/fnv), which only std called, so no hook replaces it.
    "velt_rt_fnv1a64_str",
    "velt_rt_fnv1a64_bytes",
];
