//! Which executor lowering calls (#2649 step 3.1): the runtime's, or the executor in Velt
//! (std/lang/hooks/sched.velt) with `VELT_SCHEDULER=std`.

/// The environment variable that selects the executor in Velt at compile time:
/// `VELT_SCHEDULER=std` (#2649 step 3.1). The runtime reads the same variable for its own
/// backends (`stealing`, `per-core`) and takes `std` for its default.
pub const SCHEDULER_ENV: &str = "VELT_SCHEDULER";

/// Is the executor in Velt selected (`VELT_SCHEDULER=std` when the compiler starts)? Then sema
/// checks std/lang/hooks/sched and lowering calls its hooks (`sched_block_on`, `sched_spawn`,
/// `sched_yield`, `sched_finish`) in place of the runtime's executor: a compile-time choice, so
/// no program pays a branch for it at run time.
pub fn std_scheduler() -> bool {
    static STD: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *STD.get_or_init(|| std::env::var(SCHEDULER_ENV).is_ok_and(|v| v.trim() == "std"))
}

/// The externs std declares whose jobs the executor in Velt does (lowering calls its hooks in
/// their place, velt_vir `lower/sched.rs`): the prelude's microtasks (std/prelude/promise.velt),
/// `new Promise`'s latch and task ids (std/lang/promise/settle.velt), the prelude's timers
/// (std/prelude/timers.velt) and the process lifetime. Live code that calls one runs on the
/// executor, as code that is async does (velt_sema `lazy_std`, #2649 P3(a)).
pub const SCHED_EXTERNS: &[&str] = &[
    "velt_rt_microtask",
    "velt_rt_latch_new",
    "velt_rt_latch_free",
    "velt_rt_latch_open",
    "velt_rt_latch_wait",
    "velt_rt_keep_alive_acquire",
    "velt_rt_keep_alive_release",
    SCHED_UNREF_DUE,
    "velt_rt_timer_new",
    "velt_rt_timer_turn",
    "velt_rt_timer_again",
    "velt_rt_timer_clear",
    "velt_rt_timer_free",
    "velt_rt_uncaught_now",
    "velt_rt_task_id",
];

/// The prelude's question of a due `unref()`'d timer (std/prelude/timers.velt, an async extern).
/// The executor itself asks the runtime the same through a `declare c` of that symbol, which
/// stays the runtime's.
pub const SCHED_UNREF_DUE: &str = "velt_rt_timer_unref_due";
