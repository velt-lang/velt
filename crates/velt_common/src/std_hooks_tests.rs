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
        let on_demand = ON_DEMAND_FLOAT.contains(&h.name) || INSPECT_SHAPES.contains(&h.name);
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

/// The functions std's sources declare (`function <name>`) whose name a receiver's prefix names
/// (`anyStr_trim`), each with its module (`std/lang/hooks/any_methods`).
fn std_functions_named_as_methods() -> Vec<(String, String)> {
    fn walk(dir: &std::path::Path, std: &std::path::Path, out: &mut Vec<(String, String)>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .expect("a std directory")
            .map(|e| e.expect("a std entry").path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(&path, std, out);
                continue;
            }
            if path.extension().is_none_or(|e| e != "velt") {
                continue;
            }
            let rel = path.strip_prefix(std).expect("under std");
            let rel = rel.with_extension("").to_string_lossy().replace('\\', "/");
            let module = match rel.strip_suffix("/index") {
                Some(dir) => format!("std/{dir}"),
                None => format!("std/{rel}"),
            };
            let src = std::fs::read_to_string(&path).expect("a std file");
            for (at, _) in src.match_indices("function ") {
                let name: String = src[at + "function ".len()..]
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '$')
                    .collect();
                if Receiver::ALL
                    .into_iter()
                    .any(|r| any_methods::named_by(&name, r))
                {
                    out.push((name, module.clone()));
                }
            }
        }
    }
    let std = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
    let mut out = Vec::new();
    walk(&std, &std, &mut out);
    out
}

#[test]
fn every_std_function_named_as_a_method_of_an_any_is_a_row() {
    // Lowering calls a receiver's methods by their rows only: a std function under the prefix
    // without one would be a method silently not there (`velt_vir` `std_defs.rs` stops lowering
    // a program that loads one with an internal error; this finds it in all of std).
    let found = std_functions_named_as_methods();
    assert!(
        found.iter().any(|(n, _)| n == "anyStr_trim"),
        "the scan finds no method: {found:?}"
    );
    for (name, module) in &found {
        let row = HOOKS
            .iter()
            .find(|h| h.method.is_some() && h.item == name.as_str());
        let Some(h) = row else {
            panic!(
                "std function `{module}::{name}` is named as a built-in method of an `any` \
                 but is no `method(…)` row of the hook table (std_hooks/any_methods.rs)"
            );
        };
        assert_eq!(h.module(), module.as_str(), "{}: the row's module", h.name);
        assert!(
            any_methods::named_by(name, h.method.expect("a method row")),
            "{}: its receiver's prefix doesn't name it",
            h.name
        );
        assert_eq!(any_methods::unlisted(name), None, "{name}");
    }
    // Each row is found once: none is left that std doesn't declare.
    let rows = HOOKS.iter().filter(|h| h.method.is_some()).count();
    assert_eq!(
        found.len(),
        rows,
        "std's methods and the rows differ in number"
    );
    assert_eq!(
        any_methods::unlisted("anyStr_trimLeft"),
        Some(Receiver::Str)
    );
    assert_eq!(any_methods::unlisted("anyFn_call"), Some(Receiver::Fn));
    assert_eq!(any_methods::unlisted("anyText"), None);
    assert_eq!(any_methods::unlisted("anyStr_"), None);
}

#[test]
fn every_built_in_method_of_an_any_is_a_row() {
    use any_methods::{is_any_method, methods, record_key};
    let strs: Vec<&str> = methods(Receiver::Str).map(|(n, _)| n).collect();
    assert!(
        strs.contains(&"trim") && strs.contains(&"localeCompare"),
        "{strs:?}"
    );
    assert_eq!(methods(Receiver::Fn).count(), 0);
    for r in [
        Receiver::Str,
        Receiver::Arr,
        Receiver::Num,
        Receiver::Bool,
        Receiver::Promise,
        Receiver::Rec,
        Receiver::Map,
        Receiver::Set,
    ] {
        let mut names: Vec<&str> = methods(r).map(|(n, _)| n).collect();
        assert!(!names.is_empty(), "{r:?}");
        // What `is_any_method` tests first.
        assert!(r.prefix().starts_with("any") && r.prefix().ends_with('_'));
        let n = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), n, "a method of {r:?} is listed twice");
    }
    assert_eq!(Receiver::of_class("Map"), Some(Receiver::Map));
    assert_eq!(Receiver::of_class("Array"), None);
    assert!(is_any_method("anyArr_push") && !is_any_method("anyArr_nothing"));
    assert!(!is_any_method("anyText"));
    // A record's methods are called by std's symbols `record<Name>` (std calls
    // `o[RECORD_SYM_GET](k)` on an `any` holding a record): each one's is a constant of
    // keys.velt. No std method is declared with it, so it is no row of the key table.
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std/lang/hooks/keys.velt");
    let src = std::fs::read_to_string(path).expect("std/lang/hooks/keys.velt");
    for (name, h) in methods(Receiver::Rec) {
        let key = record_key(name);
        let desc = key
            .strip_prefix("\u{FFFF}Symbol(")
            .and_then(|k| k.strip_suffix(')'))
            .expect("a symbol's member name");
        assert!(
            src.contains(&format!(" = Symbol(\"{desc}\");")),
            "{}: keys.velt has no `Symbol(\"{desc}\")`",
            h.name
        );
    }
    assert_eq!(record_key("symGet"), "\u{FFFF}Symbol(recordSymGet)");
}
