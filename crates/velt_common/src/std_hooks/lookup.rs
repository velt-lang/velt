//! Finding hooks in the table ([`super::HOOKS`]) by name, and those sema checks on demand.

use super::{Hook, HOOKS};

/// The hook whose qualified name is `qualified` (`std/lang/hooks/number::f64ToString`).
pub fn find(qualified: &str) -> Option<&'static Hook> {
    let (module, item) = qualified.rsplit_once("::")?;
    let rel = module.strip_prefix(crate::std_modules::CANONICAL_PREFIX)?;
    HOOKS
        .iter()
        .find(|h| h.item == item && crate::std_modules::rel(h.module_entry) == Some(rel))
}

/// The qualified names of the hooks named in `names` ([`ON_DEMAND_FLOAT`]).
pub fn on_demand(names: &'static [&'static str]) -> impl Iterator<Item = String> {
    HOOKS
        .iter()
        .filter(move |h| names.contains(&h.name))
        .map(Hook::qualified)
}

/// Is the definition with qualified name `qualified` a hook lowering calls by name?
pub fn is_lowered(qualified: &str) -> bool {
    static LOWERED: std::sync::OnceLock<std::collections::HashSet<String>> =
        std::sync::OnceLock::new();
    LOWERED
        .get_or_init(|| {
            HOOKS
                .iter()
                .filter(|h| h.lowered)
                .map(Hook::qualified)
                .collect()
        })
        .contains(qualified)
}

/// Lowered hooks that sema checks only once the program's own code converts a value that may hold
/// a float to text (`velt_sema` `lazy_std`), by their entries' names: the float hooks, whose whole
/// numbers take std's integer printing, and, where the program loaded it, std's float printing
/// (velt/lang/number/format, `shortest`, the table of powers of ten). Lowering calls the runtime's
/// printer where one is unchecked. The integer hooks have no fallback (#2197): sema checks them
/// where live code has one of their `constructs` ([`crate::hook_demand`]).
pub const ON_DEMAND_FLOAT: &[&str] = &["number_text", "number_push", "float_text", "float_push"];

/// The std modules of hooks no program needs by default: not loaded with the prelude. Sema demands
/// each when it finds the hook needed, during the check (#2161 (a1): `velt_sema` `std_load`), so
/// each follows the rule of `velt_loader::std_index`. `VELT_EAGER_STD=1` loads them all up front,
/// in this order. A check given no load callback adds none of them: those its program did not load
/// stay out, and their hooks are not checked (`velt_sema::ide::check_for_ide`).
pub const ON_DEMAND_MODULES: &[&str] = &[
    "std/lang/hooks/number_text",
    "std/lang/hooks/number_float",
    "std/lang/hooks/any_record",
    "std/lang/hooks/unwind",
];
