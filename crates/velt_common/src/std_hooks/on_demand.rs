//! The hooks and hook modules sema loads or checks only once a program needs them (split from
//! `std_hooks` for size).

/// Lowered hooks that sema checks only once the program's own code converts a value that may hold
/// a float to text (`velt_sema` `lazy_std`), by their entries' names: the float hooks, whose whole
/// numbers take std's integer printing, and, where the program loaded it, std's float printing
/// (velt/lang/number/format, `shortest`, the table of powers of ten). Lowering calls the runtime's
/// printer where one is unchecked. The integer hooks have no fallback (#2197): sema checks them
/// where live code has one of their `constructs` ([`crate::hook_demand`]).
pub const ON_DEMAND_FLOAT: &[&str] = &["number_text", "number_push", "float_text", "float_push"];

/// The lowered hook that sema checks only once live code prints a float (`velt_sema` `lazy_std`):
/// `console.log`'s float writer and its whole-number writer, which hold no integer hook. Lowering
/// calls the runtime's printer while they are unchecked.
pub const ON_DEMAND_WRITE: &[&str] = &["number_write", "whole_number_write"];

/// Lowered hooks with a fallback that sema checks where live code has one of their `constructs`,
/// by their entries' names: the shapes std/lang/hooks/inspect.velt writes for every type, which
/// lowering writes with its own glue where one is unchecked (the array's only once an array type
/// is live), and the cycle tracking of std/lang/hooks/inspect_cycles.velt, checked only where a
/// live type can hold itself (velt_sema `lazy_std` `live_constructs.rs`).
pub const INSPECT_SHAPES: &[&str] = &[
    "open_plain",
    "inspect_array",
    "open_object",
    "inspect_begin",
    "inspect_end",
    "inspect_save",
    "inspect_restore",
    "inspect_enter",
    "inspect_leave",
    "inspect_circular",
];

/// The std modules of hooks no program needs by default: not loaded with the prelude. Sema demands
/// each when it finds the hook needed, during the check (#2161 (a1): `velt_sema` `std_load`), so
/// each follows the rule of `velt_loader::std_index`. `VELT_EAGER_STD=1` loads them all up front,
/// in this order. A check given no load callback adds none of them: those its program did not load
/// stay out, and their hooks are not checked (`velt_sema::ide::check_for_ide`).
pub const ON_DEMAND_MODULES: &[&str] = &[
    "std/lang/hooks/number_text",
    "std/lang/hooks/number_write",
    "std/lang/hooks/number_float",
    "std/lang/hooks/any_record",
    // `then`, `catch` and `finally` of a promise held as `any` (#924 step 2), demanded by sema.
    "std/lang/hooks/any_promise",
    "std/lang/hooks/unwind",
    "std/lang/hooks/inspect",
    "std/lang/hooks/inspect_cycles",
    "std/lang/hooks/pow",
    "std/lang/hooks/prebuilt_probe",
    "std/lang/hooks/number_digits",
    "std/lang/hooks/number_parse",
    // The blocks of module state (#2196 9c): a native program with module state.
    "std/lang/hooks/module_blocks",
    // The weak core (#3145), demanded by a weak collection's intrinsic (`Construct::Weak`).
    "std/lang/hooks/weak",
    // The string searches, which the prelude's methods call by role (`hookCall`, #2198).
    "std/lang/string/search",
    // The other string methods in Velt, called by role too (#2198).
    "std/lang/string/trim",
    "std/lang/string/fill",
    "std/lang/string/split",
    "std/lang/string/replace",
    "std/lang/string/case",
    // The CPU's features (#3222 step 3): every program with an entry demands it (`velt_main`
    // calls `cpuStart` first thing; velt_sema `lazy_std` `cpu_hook`).
    "std/lang/hooks/cpu",
];

/// The qualified names of the hooks named in `names` ([`ON_DEMAND_FLOAT`]).
pub fn on_demand(names: &'static [&'static str]) -> impl Iterator<Item = String> {
    super::HOOKS
        .iter()
        .filter(move |h| names.contains(&h.name))
        .map(super::Hook::qualified)
}
