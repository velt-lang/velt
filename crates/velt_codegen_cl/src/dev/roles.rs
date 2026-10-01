//! What a function key (its symbol) says about how `velt dev` may swap the function.
//!
//! - **Pinned** functions run state machines whose layout belongs to one version: async
//!   `$poll`/`$drop` (and the value wrappers' `$value_poll`/`$value_drop`), an HTTP handler's
//!   `$init`, and the `Promise.all`/`allSettled` wrappers (`_Gall_…`). They never go through
//!   trampolines: a future keeps the `poll`/`drop` of the version that created it (its `VeltFut`
//!   header), and code embedding a child's state calls the child's code of its own version.
//! - Everything else is an **entry point**: every call and address goes through its trampoline,
//!   so new calls reach the newest code.
//! - Repeated symbols get a `$dup<n>` suffix in lowering; such keys are **ambiguous**.

/// Suffixes of pinned state-machine functions.
const PINNED_SUFFIXES: [&str; 5] = ["$poll", "$drop", "$init", "$value_poll", "$value_drop"];
/// Prefixes of pinned glue.
const PINNED_PREFIXES: [&str; 4] = [
    "_Gall_poll_",
    "_Gall_drop_",
    "_Gall_settle_poll_",
    "_Gall_settle_drop_",
];
/// Lowering's suffix for repeated symbols.
const DUPLICATE: &str = "$dup";

/// Is the function with this symbol pinned to its version (see the module docs)?
pub(crate) fn is_pinned(symbol: &str) -> bool {
    let base = symbol.split(DUPLICATE).next().unwrap_or(symbol);
    PINNED_SUFFIXES.iter().any(|s| base.ends_with(s))
        || PINNED_PREFIXES.iter().any(|p| base.starts_with(p))
}

/// Is the key ambiguous (a repeated symbol renamed with `$dup<n>`)?
pub(crate) fn is_ambiguous(symbol: &str) -> bool {
    symbol.contains(DUPLICATE)
}

/// Is this the handler-state initializer of an HTTP handler (`<closure>$init`)?
pub(crate) fn is_handler_init(symbol: &str) -> bool {
    symbol.ends_with("$init")
}

/// The closure a closure function belongs to: (enclosing function's name, closure number), from
/// the readable name `parent::{closure#N}` (type arguments may follow).
pub(crate) fn closure_slot(symbol: &str) -> Option<(String, u32)> {
    let name = velt_vir::mangle::demangle(symbol);
    let start = name.rfind("::{closure#")?;
    let rest = &name[start + "::{closure#".len()..];
    let (number, after) = rest.split_once('}')?;
    if !(after.is_empty() || after.starts_with('<')) {
        return None;
    }
    Some((name[..start].to_string(), number.parse().ok()?))
}

/// A readable name for messages.
pub(crate) fn display(symbol: &str) -> String {
    velt_vir::mangle::demangle(symbol)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_from_symbols() {
        assert!(is_pinned("_V4main$poll"));
        assert!(is_pinned("_V4mainN17_7bclosure_230_7d$init"));
        assert!(is_pinned("_Gall_poll_3i64"));
        assert!(is_pinned("_V1f$drop$dup2"));
        assert!(!is_pinned("_V4main"));
        assert!(!is_pinned("_Gdrop_5Point"));
        assert!(is_ambiguous("_V1f$dup2"));
        assert!(!is_ambiguous("_V1f"));
        assert!(!is_ambiguous("_V5x__D2"));
    }

    #[test]
    fn closures_are_numbered_per_parent() {
        let closure = velt_vir::mangle::mangle("main::{closure#1}");
        assert_eq!(closure_slot(&closure), Some(("main".to_string(), 1)));
        assert_eq!(closure_slot(&format!("{closure}$poll")), None);
        assert_eq!(closure_slot("_V4main"), None);
    }
}
