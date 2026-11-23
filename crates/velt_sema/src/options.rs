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
    /// A key of this compiler and std's sources, which the driver gives a check by units with
    /// std as a unit of its own (`VELT_UNITS=2`): with `VELT_UNITS_DISK` set, std's unit is kept
    /// on disk under it and loaded instead of checking std (`crate::units::disk`). `None`: std
    /// is checked. Additive.
    pub std_key: Option<u64>,
    /// The target is wasm32, where C's pointers, `size_t` and `ssize_t` are 4 bytes and Velt's
    /// `Ptr<T>`, `usize` and `isize` 8: a read or write through a `Ptr` of a type holding one
    /// isn't C's memory there, and is refused (`Ctx::c_memory_refused`, #3094).
    pub wasm32: bool,
}

impl Default for CheckOptions {
    fn default() -> Self {
        Self {
            require_main: true,
            eager_std: false,
            native_executor: true,
            units: false,
            std_key: None,
            wasm32: false,
        }
    }
}
