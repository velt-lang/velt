//! Finding hooks in the table ([`super::HOOKS`]) by name.

use super::{Hook, HOOKS};

/// The hook whose qualified name is `qualified` (`std/lang/hooks/number::f64ToString`).
pub fn find(qualified: &str) -> Option<&'static Hook> {
    let (module, item) = qualified.rsplit_once("::")?;
    let rel = module.strip_prefix(crate::std_modules::CANONICAL_PREFIX)?;
    HOOKS
        .iter()
        .find(|h| h.item == item && crate::std_modules::rel(h.module_entry) == Some(rel))
}

/// The qualified names of the hooks named in `names` ([`super::ON_DEMAND_FLOAT`]).
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
