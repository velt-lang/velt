//! The options of a check ([`crate::check_with`]).

/// What [`crate::check_with`] requires of the program beyond being well-typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckOptions {
    /// The root module must define `main` (a program to build or run). Without it a root with no
    /// `main` is a library module: every body is still checked and the program's `entry` is
    /// `None`; a `main` that is there is validated either way.
    pub require_main: bool,
    /// Check every definition of std, also those the program never reaches (`velt check
    /// --std`); otherwise std's bodies are checked as the program reaches them (lazy std).
    pub eager_std: bool,
    /// The program runs on the executor in Velt (std/lang/hooks/sched.velt): a native target.
    /// Live code that runs something on it demands the executor's hooks. A wasm target keeps
    /// `velt_rt_wasm`'s executor (#2649 step 6), so nothing demands them there.
    pub native_executor: bool,
    /// Check by units (#2943 M1, `crate::units`): std collected and checked whole as its own
    /// unit, frozen, then the program checked against it; writes across the boundary are counted
    /// and printed. Needs every std module loaded and `eager_std`. Additive.
    pub units: bool,
    /// With `units`: a key for std's sources (the caller's, e.g. a hash of their contents). A
    /// long-lived caller that checks again keeps std's unit between checks under it
    /// (`crate::std_unit`). Additive.
    pub std_key: Option<u64>,
    /// With `units`: std's modules are the stubs a unit on disk keeps (EXPERIMENT,
    /// `crate::units::disk`), so std's unit must come from disk: the check refuses to check std
    /// over them. Additive.
    pub std_stubbed: bool,
    /// The caller only checks (`velt check`): no program is lowered. With std's unit from disk
    /// (EXPERIMENT, `crate::units::disk`), the program then leaves std's definitions out
    /// (`Ctx::std_from_disk`). Additive.
    pub check_only: bool,
}

impl Default for CheckOptions {
    fn default() -> Self {
        Self {
            require_main: true,
            eager_std: false,
            native_executor: true,
            units: false,
            std_key: None,
            std_stubbed: false,
            check_only: false,
        }
    }
}
