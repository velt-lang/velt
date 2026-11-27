//! The rows of the hook table for standard output and standard error (#2196):
//! std/lang/hooks/stdout.velt, which writes through the runtime's writer for now (out_rt.velt).
//! `console.log` and `console.error` write through the writers (`velt_vir` `console.rs`), and
//! every exit Velt code reaches calls `flushForExit` first. Not `replaces(…)`: the runtimes keep
//! `velt_rt_write_str` and the rest, which out_rt.velt forwards to. No construct
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
        }
    };
}
