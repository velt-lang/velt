//! The runtime externs ported to std whose Rust is gone (split from `std_hooks` for size).

/// The runtime externs whose job moved to std and whose Rust was deleted (#2196's port tracker: an
/// entry here is a finished port): hooks took over the externs of [`Hook::replaces`](crate::std_hooks::Hook::replaces), which keep
/// their place there as the record of what the hook replaced, and ordinary std code the ones only
/// std called, which no hook replaces. `velt_common`'s test `hook_replaces.rs` checks that every
/// replaced extern is defined by `velt_rt` unless it is listed here, and that one listed here is
/// defined nowhere and named by no compiler source, runtime or std file.
pub const DELETED: &[&str] = &[
    // #2196 9c: the module lock around a callback (std/lang/hooks/sched_module.velt `withStart`,
    // `withEnd`), with the rest of the runtime's module lock.
    "velt_rt_module_with",
    "velt_rt_module_with_end",
    // #3222 step 3: the CPU check and the flags of crates/velt_rt/src/cpu.rs, now std's
    // (std/lang/hooks/cpu.velt, std/lang/cpu).
    "velt_rt_check_cpu",
    // #2196: byte arrays made and copied (std/lang/bytes/raw.velt), which only std called;
    // `velt_rt_bytes_zeroed` stays for `Buffer.alloc` of 1 MiB or more.
    "velt_rt_bytes_uninit",
    "velt_rt_bytes_set",
    "velt_rt_bytes_copy",
    "velt_rt_bytes_copy_within",
    "velt_rt_bytes_slice",
    // #2196: a byte range as a string (the `bytes_range_text` hook, std/lang/bytes/range_text.velt).
    "velt_rt_bytes_range_to_str",
    "velt_rt_bytes_range_to_str_lossy",
    // #2196: `Number(s)` of a string held as `any` (the `str_to_number` hook,
    // std/lang/hooks/string_number.velt), and deep equality's cycle guard (the `deep_eq_enter` and
    // `deep_eq_leave` hooks, std/lang/hooks/deep_eq.velt).
    "velt_rt_str_to_number",
    "velt_rt_deep_eq_enter",
    "velt_rt_deep_eq_leave",
    // #2196: standard input (std/process/stdin_buffer.velt over `#stdin-sys`), which only std
    // called.
    "velt_rt_stdin_read_line_sync",
    "velt_rt_stdin_read_all_sync",
    "velt_rt_stdin_read_all_bytes_sync",
    // #2196: `Date.now()` and `performance.now()` (the `date_now` and `perf_now` hooks,
    // std/lang/hooks/clock.velt over `#clock`).
    "velt_rt_perf_now",
    "velt_rt_date_now",
    // #3145: the weak core (std/lang/hooks/weak*.velt): lowering's weak hooks, and the drops
    // std's `WeakMap`, `WeakSet` and `WeakRef` call.
    "velt_rt_weak_release",
    "velt_rt_weakmap_new",
    "velt_rt_weakmap_set",
    "velt_rt_weakmap_set_traced",
    "velt_rt_weakmap_get",
    "velt_rt_weakmap_has",
    "velt_rt_weakmap_delete",
    "velt_rt_weakmap_drop",
    "velt_rt_weakref_new",
    "velt_rt_weakref_new_with",
    "velt_rt_weakref_word",
    "velt_rt_weakref_deref",
    "velt_rt_weakref_drop",
    // #3584: the transfer map (std/lang/hooks/transfer.velt): lowering's transfer hooks, and std's
    // catch (std/task/unwind.velt).
    "velt_rt_xfer_begin",
    "velt_rt_xfer_keep_begin",
    "velt_rt_xfer_share_begin",
    "velt_rt_xfer_keeps",
    "velt_rt_xfer_end",
    "velt_rt_xfer_find",
    "velt_rt_xfer_record",
    "velt_rt_xfer_defer",
    "velt_rt_xfer_suspend",
    "velt_rt_xfer_resume",
    // #2196: velt/os's facts (std/os/sys_*.velt over the C library and Win32, the build's
    // `x64`/`arm64` condition for the architecture), which only std called.
    "velt_rt_os_platform",
    "velt_rt_os_arch",
    "velt_rt_os_cpu_count",
    "velt_rt_os_tmpdir",
    "velt_rt_os_hostname",
    // #2196: the local time zone's offset (`#localtime`: std/time/localtime_posix.velt over
    // `localtime_r`, localtime_win32.velt over Win32's time-zone functions, UTC on WebAssembly),
    // which only std called.
    "velt_rt_local_offset_minutes",
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
    // #2816: `charCodeAt` past an ASCII string (the `string_char_code_at` hook,
    // std/lang/string/positions.velt).
    "velt_rt_str_char_code_at",
    // #2816: `slice`, `substring`, `charAt` and `at` past an ASCII string (the `string_slice`
    // hook, std/lang/string/positions.velt).
    "velt_rt_str_slice",
    // #2196: `**` on floats (std/lang/math/pow, #1676).
    "velt_rt_pow_f64",
    // #2196: `**` on integers (std/lang/hooks/pow_int).
    "velt_rt_pow_i64",
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
    // #2196: the decimals of `parseFloat` past 64 bits (std/lang/hooks/number_parse).
    "velt_rt_parse_float",
    // #2198: the string methods in Velt (std/lang/string/trim, fill, split, replace) and the code
    // points of `for (const c of s)` (std/lang/hooks/string).
    "velt_rt_str_trim",
    "velt_rt_str_trim_start",
    "velt_rt_str_trim_end",
    "velt_rt_str_repeat",
    "velt_rt_str_pad_start",
    "velt_rt_str_pad_end",
    "velt_rt_str_split",
    "velt_rt_str_replace",
    "velt_rt_str_replace_all",
    "velt_rt_str_code_points",
    // #2196: floats as text (std/lang/hooks/number_text, number_write, number_shortest,
    // velt/lang/number/format); `JSON.stringify`'s float literals are the compiler's text, and
    // the JSON writers' floats std's.
    "velt_rt_str_from_f64",
    "velt_rt_strbuf_push_f64",
    "velt_rt_strbuf_push_inspect_f64",
    "velt_rt_strbuf_push_json_f64",
    "velt_rt_write_f64",
    "velt_rt_bytes_put_f64",
    // #3530: JSON string escaping, std's (std/encoding/json/writer.velt).
    "velt_rt_strbuf_push_json_str",
    // #2196: bounded drop nesting (std/lang/hooks/drop_queue.velt): its state word is a std
    // `ThreadLocal` that compiled code reads and writes inline, and the queue std's.
    "velt_rt_drop_state",
    "velt_rt_drop_queue",
    "velt_rt_drop_drain",
    // #2196: the properties of function values and the identities of declared functions
    // (std/lang/hooks/fn_props.velt).
    "velt_rt_fnprop_get",
    "velt_rt_fnprop_set",
    "velt_rt_fnprop_delete",
    "velt_rt_fnprop_len",
    "velt_rt_fnprop_key",
    "velt_rt_fnprop_identity",
    "velt_rt_fnprop_identity_of",
];
