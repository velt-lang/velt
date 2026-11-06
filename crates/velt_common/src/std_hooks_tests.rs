//! Tests of the hook table (`std_hooks.rs`).

use super::*;

#[test]
fn names_are_unique() {
    let mut names: Vec<&str> = HOOKS.iter().map(|h| h.name).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), HOOKS.len(), "a hook is listed twice");
    let mut items: Vec<String> = HOOKS.iter().map(|h| h.qualified()).collect();
    items.sort();
    items.dedup();
    assert_eq!(items.len(), HOOKS.len(), "two entries name one definition");
}

#[test]
fn the_fallback_hooks_are_the_on_demand_ones() {
    // An unchecked hook is missing to lowering: only these may be, and lowering falls back.
    for h in HOOKS {
        let on_demand = ON_DEMAND_FLOAT.contains(&h.name);
        assert_eq!(h.fallback, on_demand, "{}", h.name);
        assert!(!h.fallback || h.lowered, "{}", h.name);
    }
}

#[test]
fn every_key_is_a_constant_of_keys_velt() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std/lang/hooks/keys.velt");
    let src = std::fs::read_to_string(path).expect("std/lang/hooks/keys.velt");
    for k in KEYS {
        let make = if k.registered { "Symbol.for" } else { "Symbol" };
        let decl = format!(
            "export const {} = {make}(\"{}\");",
            k.constant, k.description
        );
        assert!(src.contains(&decl), "keys.velt lacks `{decl}`");
    }
    assert_eq!(std_key!(map_cursor), "\u{FFFF}Symbol(mapCursor)");
}

#[test]
fn every_hook_is_in_lang() {
    // Outside `lang/` only where the hook needs a module's internals, in a file no program
    // can import (not a module's `index.velt`).
    for h in HOOKS {
        let rel = crate::std_modules::rel(h.module_entry).expect("ICE: a std module");
        assert!(
            h.module().starts_with("std/lang/")
                || !crate::std_modules::importable(&format!("{rel}.velt")),
            "{}: {}",
            h.name,
            h.module()
        );
    }
    let f64_to_string = "std/lang/hooks/number::f64ToString";
    assert_eq!(find(f64_to_string).map(|h| h.name), Some("f64_to_string"));
    assert!(is_lowered("std/lang/hooks/number::floatIndex"));
    assert!(!is_lowered(f64_to_string));
    assert!(find("std/lang/hooks/number::nothing").is_none());
}

#[test]
fn each_module_loaded_on_demand_holds_a_hook() {
    for m in ON_DEMAND_MODULES {
        assert!(HOOKS.iter().any(|h| h.module() == *m), "{m}");
    }
}
