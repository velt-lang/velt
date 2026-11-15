//! The runtime externs ported to std whose Rust is gone (split from `std_hooks` for size).

/// The runtime externs whose job moved to std and whose Rust was deleted (#2196's port tracker: an
/// entry here is a finished port): hooks took over the externs of [`Hook::replaces`](crate::std_hooks::Hook::replaces), which keep
/// their place there as the record of what the hook replaced, and ordinary std code the ones only
/// std called, which no hook replaces. `velt_common`'s test `hook_replaces.rs` checks that every
/// replaced extern is defined by `velt_rt` unless it is listed here, and that one listed here is
/// defined nowhere and named by no compiler source, runtime or std file.
pub const DELETED: &[&str] = &[
    // #2196: velt/os's facts (std/os/sys_*.velt over the C library and Win32, the build's
    // `x64`/`arm64` condition for the architecture), which only std called.
    "velt_rt_os_platform",
    "velt_rt_os_arch",
    "velt_rt_os_cpu_count",
    "velt_rt_os_tmpdir",
    "velt_rt_os_hostname",
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
    // #2196: polling and dropping a heap future, which compiled code (velt_vir
    // `lower/raw_memory.rs`) and std (`futPoll`, `futDrop`) do through its header, inline.
    "velt_rt_fut_poll",
    "velt_rt_fut_drop",
    // #2197: `console.log` of an integer (std/lang/hooks/number_text).
    "velt_rt_write_i64",
    "velt_rt_write_u64",
    // #2196: HTML escaping (std/text/html), which only std called, so no hook replaces it.
    "velt_rt_html_escape",
    "velt_rt_jsx_escape",
    // #2649: velt/sync's channel (std/sync/channel.velt), and the process-wide slots built on it
    // (velt/net's topics and HTTP/2 sessions keep a static atomic now), which only std called.
    "velt_rt_chan_new",
    "velt_rt_chan_close",
    "velt_rt_chan_closed",
    "velt_rt_chan_len",
    "velt_rt_chan_send",
    "velt_rt_chan_try_send",
    "velt_rt_chan_receive",
    "velt_rt_chan_try_receive",
    "velt_rt_global_lock",
    "velt_rt_global_unlock",
    "velt_rt_global_chan",
    // #1967: the cycles and the `... n more items` of what `console.log` prints
    // (std/lang/hooks/inspect_cycles).
    "velt_rt_strbuf_inspect_enter",
    "velt_rt_strbuf_inspect_leave",
    "velt_rt_strbuf_inspect_circular",
    "velt_rt_strbuf_inspect_more",
    // #3029: `AbortSignal.timeout`'s timer, one of Node's timers now (std/task/signal_timeout),
    // and its delay, which the abort message carries; only std called them.
    "velt_rt_signal_timeout",
    "velt_rt_signal_timeout_ms",
    // #2196: `toString(radix)` (std/lang/hooks/number, velt/lang/number/radix).
    "velt_rt_f64_to_radix",
    "velt_rt_i64_to_radix",
    "velt_rt_u64_to_radix",
    // #2196: `toExponential`, `toPrecision` and `toFixed`'s exact digits
    // (std/lang/hooks/number_digits, velt/lang/number/exact).
    "velt_rt_f64_to_exponential",
    "velt_rt_f64_to_precision",
    "velt_rt_f64_to_fixed",
    // #2196: `parseInt` (velt/lang/number/parse_int), which only std called.
    "velt_rt_parse_int",
];
