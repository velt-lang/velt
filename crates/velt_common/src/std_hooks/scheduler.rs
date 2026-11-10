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
