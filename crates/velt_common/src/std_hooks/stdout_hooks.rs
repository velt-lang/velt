//! The rows of the hook table for standard output and standard error (#2196):
//! std/lang/hooks/stdout.velt, std's writer on a native target (out_native.velt, prebuilt, through
//! out_hooked.velt) and the WebAssembly runtime's elsewhere (out_rt.velt). `console.log` and `console.error` write through
//! the writers (`velt_vir` `console.rs`), and every exit Velt code reaches calls `flushForExit`
//! first. Not `replaces(…)`: a WebAssembly build keeps velt_rt_wasm's `velt_rt_write_str` and the
//! rest (out_rt.velt forwards to them), while velt_rt's go once nothing calls them. No construct
//! lists them: every program reaches an exit, and process.velt imports the module, so every
//! program loads it. Kept apart from the rest of the table ([`super`]), which
//! [`with_stdout_hooks`] gives on, with these rows after it, to `with_any_hooks!`.

/// `with_any_hooks!` over the rows given, then these.
macro_rules! with_stdout_hooks {
    ($($rows:tt)*) => {
        with_any_hooks! {
            $($rows)*
            /// A string written to stdout (`stream` 1) or stderr (2): a `console.log` argument, a
            /// formatted line, an uncaught error's report (`velt_vir` `console.rs`, `entry.rs`).
            write_str => lang_hooks_stdout, "writeStr", sig((u32, &string) -> void), lowered;
            /// `true` or `false` written to a stream.
            write_bool => lang_hooks_stdout, "writeBool", sig((u32, bool) -> void), lowered;
            /// One byte written to a stream: the space or newline after a `console.log` argument.
            write_byte => lang_hooks_stdout, "writeByte", sig((u32, u8) -> void), lowered;
            /// The flush before the process ends: the end of `velt_main`, before `process.exit`, an
            /// uncaught error's exit and a panic (`velt_vir` `lower/stdout.rs`).
            flush_for_exit => lang_hooks_stdout, "flushForExit", sig(() -> void), lowered;
            /// `velt_main`'s start: whether stdout is a terminal, asked once before any task
            /// writes (`entry.rs`).
            init_output => lang_hooks_stdout, "initOutput", sig(() -> void), lowered;
            /// `velt_main`'s start outside the dev JIT: the flush registered with the C library's
            /// `atexit`, for the exits the runtime takes by itself (`entry.rs`).
            flush_at_exit => lang_hooks_stdout, "registerExitFlush", sig(() -> void), lowered;
            // std's writer on a native target (out_native.velt), which out_hooked.velt reaches
            // through these alone, by `hookCall`: prebuilt, its buffers in its private modules
            // (`prebuilt::private`), so that a build linking its object neither loads nor
            // compiles it (docs/internals/design/std-object.md "Prebuilt hook modules").
            /// `writeStr` on a native target.
            out_write_str => lang_hooks_out_native, "writeStr", sig((u32, &string) -> void), prebuilt;
            /// `writeByte` on a native target.
            out_write_byte => lang_hooks_out_native, "writeByte", sig((u32, u8) -> void), prebuilt;
            /// The `n` bytes at address `p` to stdout (`Stdout.write`'s array's).
            out_write_bytes => lang_hooks_out_native, "writeBytesAt", sig((usize, usize) -> void),
                prebuilt;
            /// `flushOutput` on a native target.
            out_flush => lang_hooks_out_native, "flushOutput", sig(() -> void), prebuilt;
            /// `publishOutput` on a native target.
            out_publish => lang_hooks_out_native, "publishOutput", sig(() -> void), prebuilt;
            /// `threadEnd` on a native target.
            out_thread_end => lang_hooks_out_native, "threadEnd", sig(() -> void), prebuilt;
            /// `flushIdle` on a native target.
            out_flush_idle => lang_hooks_out_native, "flushIdle", sig(() -> void), prebuilt;
            /// `initOutput` on a native target.
            out_init => lang_hooks_out_native, "initOutput", sig(() -> void), prebuilt;
            /// `registerExitFlush` on a native target.
            out_register_exit_flush => lang_hooks_out_native, "registerExitFlush",
                sig(() -> void), prebuilt;
            /// `flushForExit` on a native target.
            out_flush_for_exit => lang_hooks_out_native, "flushForExit", sig(() -> void), prebuilt;
        }
    };
}
