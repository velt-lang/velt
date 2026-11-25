//! The rows of the hook table for the blocks of module state (std/lang/hooks/module_blocks.velt,
//! #2196 9c; rt_abi.md "Module state"): lowered for every access of module state in a native
//! program without the executor, reached by role from the executor's module lock
//! (sched_module.velt), and prebuilt, the module's state its own (`prebuilt::private`). Kept apart
//! from the rest of the table ([`super`]), which [`with_module_hooks`] gives on, with these rows
//! after it, to `with_string_hooks!`. The executor's module lock rows beyond module state (a
//! function's properties, `with` callbacks: sched_module.velt) follow them.

/// The std module table's entry of the module that keeps the blocks of module state: its lowered
/// hooks are live once a program has module state (`velt_sema` `lazy_std` `module_hooks`), not in
/// every program.
pub const BLOCKS_MODULE_ENTRY: &str = "lang_hooks_module_blocks";

/// `with_string_hooks!` over the rows given, then these.
macro_rules! with_module_hooks {
    ($($rows:tt)*) => {
        with_string_hooks! {
            $($rows)*
            // module_blocks.velt: the blocks of module state, prebuilt: a native program with
            // module state calls its module's object.
            /// Module state's lookup in a program without the executor (one thread, no lock), and
            /// under the executor's lock (sched_module.velt, by role): `(desc, slot)`, the block.
            module_state => lang_hooks_module_blocks, "moduleState", sig((usize, u64) -> usize), lowered, prebuilt;
            module_block => lang_hooks_module_blocks, "moduleBlock", sig((usize) -> usize), lowered, prebuilt;
            module_ready => lang_hooks_module_blocks, "moduleReady", sig((usize, u64) -> void), lowered, prebuilt;
            /// The end of such a program's `velt_main` with exit code `(code)`: the blocks go at a
            /// normal end with the debug counters on, as the runtime's did.
            module_exit => lang_hooks_module_blocks, "moduleExit", sig((i32) -> void), lowered, prebuilt;
            /// The blocks go (the executor's counted normal end, sched_module.velt `dropAtExit`, by
            /// role).
            module_drop => lang_hooks_module_blocks, "moduleDrop", sig(() -> void), prebuilt;
            // mutex_held.velt: under `velt dev` (velt_vir `LowerOptions::jit`) a `with` counts the
            // `Mutex` it holds, after its lock and before its unlock, for the executor's module
            // lock (sched_module.velt `take`, #3467); other builds lower and demand neither.
            mutex_held_locked => lang_hooks_mutex_held, "locked", sig(() -> void), lowered,
                constructs(MutexHeld);
            mutex_held_unlocked => lang_hooks_mutex_held, "unlocked", sig(() -> void), lowered,
                constructs(MutexHeld);
            // sched.velt: the executor's module lock (sched_module.velt) beyond module state.
            /// The module lock before a function's properties held as `any` are reached (workers).
            sched_module_touch => lang_hooks_sched, "schedModuleTouch", sig(() -> void), lowered;
            /// The module lock around a `with`, `read` or `write` callback that may reach them:
            /// taken before the lock (whether it was) and given back after the unlock (workers).
            sched_module_with => lang_hooks_sched, "schedModuleWith", sig(() -> u8), lowered, replaces("velt_rt_module_with");
            sched_module_with_end => lang_hooks_sched, "schedModuleWithEnd", sig((u8) -> void), lowered, replaces("velt_rt_module_with_end");
        }
    };
}
