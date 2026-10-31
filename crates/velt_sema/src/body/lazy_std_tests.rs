//! The compiler's helpers that lowering looks up by name stay checked ([`is_helper`]).

use super::*;

/// The methods of [`LAZY_PRELUDE_TYPES`] lowering calls by name: each only where it has a value
/// of the type, so its type is used and its methods are checked.
const LAZY_PRELUDE_METHODS: &[&str] = &["__fromReader", "__runtimeHandle", "__writeJson"];

/// The definitions named by `std_item!(<entry>, "<item>")` in `src` (velt_common's std module
/// table): per use, the byte offset of the item's text, its module's canonical path and the
/// item (`"Proxy.__view"`).
fn std_items(src: &str) -> Vec<(usize, String, String)> {
    let mut out = vec![];
    for (i, m) in src.match_indices("std_item!(") {
        let rest = &src[i + m.len()..];
        let Some((entry, rest)) = rest.split_once(',') else {
            continue;
        };
        let entry = entry.trim();
        let Some(open) = rest.find('"') else {
            continue;
        };
        let item: String = rest[open + 1..].chars().take_while(|c| *c != '"').collect();
        let rel = velt_common::std_modules::rel(entry)
            .unwrap_or_else(|| panic!("ICE: `{entry}` is not in velt_common's std module table"));
        let at = i + m.len() + entry.len() + 1 + open + 1;
        out.push((at, format!("std/{rel}"), item));
    }
    out
}

/// The `"__…"` names in the Rust sources under `dir` (string literals, with or without a module
/// path or an owner before them: `"__x"`, `"…::__x"`, `"{}.__x"`).
fn helper_literals(dir: &std::path::Path, out: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).expect("source directory") {
        let path = entry.expect("source entry").path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n != "tests") {
                helper_literals(&path, out);
            }
            continue;
        }
        if path.extension().is_none_or(|e| e != "rs")
            || path.to_string_lossy().ends_with("_tests.rs")
        {
            continue;
        }
        let src = std::fs::read_to_string(&path).expect("source file");
        let items = std_items(&src);
        for (i, _) in src.match_indices("__") {
            let before = src[..i].chars().last();
            if !matches!(before, Some('"') | Some(':') | Some('.')) {
                continue;
            }
            // A definition of a std module outside the prelude (`"std/lang/proxy::Proxy.__view"`):
            // `every_std_definition_lowering_names_stays_checked` checks those.
            let item = items
                .iter()
                .find(|(at, _, item)| (*at..at + item.len()).contains(&i));
            if item.is_some_and(|(_, m, _)| !HELPERS.iter().any(|p| m.starts_with(p))) {
                continue;
            }
            let name: String = src[i..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            let after = src[i + name.len()..].chars().next();
            // A literal name (`"__x"`, `"…::__x"`, `"{}.__x"`) or a prefix (`"__any{class}_"`).
            if matches!(after, Some('"') | Some('{')) && name.len() > 2 {
                out.push(name);
            }
        }
    }
}

/// Is `name` (a short name) the last part of one of [`JSON_GLUE_PRELUDE`]'s definitions?
fn in_json_glue_prelude(name: &str) -> bool {
    (JSON_GLUE_PRELUDE.iter().flat_map(|l| l.iter()))
        .any(|full| full.rsplit(['.', ':']).next() == Some(name))
}

#[test]
fn every_name_lowering_looks_up_is_a_helper() {
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut names = vec![];
    for c in [
        "velt_vir",
        "velt_opt",
        "velt_codegen_llvm",
        "velt_codegen_cl",
    ] {
        helper_literals(&crates.join(c).join("src"), &mut names);
    }
    assert!(names.iter().any(|n| n == "__get"), "{names:?}");
    // C preprocessor macros (`__AVX2__`, `__GCC_…`), DWARF names and the C library's own
    // symbols (`errno`'s location, velt_vir `system/posix.rs`) are no definitions.
    const C_SYMBOLS: &[&str] = &["__error", "__errno_location"];
    let missing: Vec<&String> = (names.iter())
        .filter(|n| !n.ends_with("__") && !n.starts_with("__DWARF") && !n.starts_with("__GCC"))
        .filter(|n| !C_SYMBOLS.contains(&n.as_str()))
        .filter(|n| !is_helper(n) && !in_json_glue_prelude(n))
        .filter(|n| !LAZY_PRELUDE_METHODS.contains(&n.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "add to HELPER_NAMES (or, for one only JSON glue calls, to JSON_GLUE_PRELUDE): {missing:?}"
    );
}

/// The `std_item!(<module>, "<name>")` uses in the Rust sources under `dir`, outside tests.
fn std_literals(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
    for entry in std::fs::read_dir(dir).expect("source directory") {
        let path = entry.expect("source entry").path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n != "tests") {
                std_literals(&path, out);
            }
            continue;
        }
        if path.extension().is_none_or(|e| e != "rs")
            || path.to_string_lossy().ends_with("_tests.rs")
        {
            continue;
        }
        let src = std::fs::read_to_string(&path).expect("source file");
        let src = src.split("#[cfg(test)]").next().unwrap_or_default();
        out.extend(std_items(src).into_iter().map(|(_, m, name)| (m, name)));
    }
}

/// A definition lowering, the optimizer or a backend looks up by name must stay checked: its
/// module is checked whole ([`EAGER`]) or reached with the JSON glue ([`JSON_GLUE`]), or it is
/// a prelude helper or a prelude type (whose methods stay checked), or an interface (no bodies;
/// the methods lowering calls through it are those of a type checked code uses).
#[test]
fn every_std_definition_lowering_names_stays_checked() {
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut names = vec![];
    for c in [
        "velt_vir",
        "velt_opt",
        "velt_codegen_llvm",
        "velt_codegen_cl",
    ] {
        std_literals(&crates.join(c).join("src"), &mut names);
    }
    assert!(
        names.iter().any(|(m, _)| m == "std/sync/epoch/reclaim"),
        "{names:?}"
    );
    const INTERFACES: &[(&str, &str)] = &[("std/db/row_source", "RowSource")];
    // Names lowering compares the callee of a call with (it compiles such a call its own way),
    // never looks up: a call of one is checked code calling a checked function.
    const RECOGNIZED: &[(&str, &str)] = &[("std/text/string_builder", "StringBuilder.append")];
    let missing: Vec<_> = (names.iter())
        .filter(|(m, name)| !INTERFACES.contains(&(m.as_str(), name.as_str())))
        .filter(|(m, name)| !RECOGNIZED.contains(&(m.as_str(), name.as_str())))
        .filter(|(m, name)| {
            let eager = EAGER.iter().any(|p| m.starts_with(p)) || JSON_GLUE.contains(&m.as_str());
            let prelude = HELPERS.iter().any(|p| m.starts_with(p))
                && (name.is_empty()
                    || is_helper(name)
                    || name.starts_with(|c: char| c.is_ascii_uppercase()));
            !eager && !prelude
        })
        .collect();
    assert!(
        missing.is_empty(),
        "add to EAGER (or to HELPER_NAMES): {missing:?}"
    );
}

#[test]
fn helpers_by_name_and_by_family() {
    assert!(is_helper(velt_common::std_hook!(any_loose_eq)));
    assert!(is_helper("anyRec_get"));
    assert!(is_helper("anyMap_set"));
    assert!(!is_helper(velt_common::std_hook!(any_member)));
    assert!(!is_helper("Number"));
}

/// The `std_hook!(<entry>)` uses in the Rust sources under `dir`, outside tests.
fn hook_uses(dir: &std::path::Path, out: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).expect("source directory") {
        let path = entry.expect("source entry").path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n != "tests") {
                hook_uses(&path, out);
            }
            continue;
        }
        if path.extension().is_none_or(|e| e != "rs")
            || path.to_string_lossy().ends_with("_tests.rs")
        {
            continue;
        }
        let src = std::fs::read_to_string(&path).expect("source file");
        let src = src.split("#[cfg(test)]").next().unwrap_or_default();
        for (i, m) in src.match_indices("std_hook!(") {
            let name: String = src[i + m.len()..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            out.push(name);
        }
    }
}

/// A hook lowering, the optimizer or a backend calls is marked `lowered` in velt_common's
/// table, so lazy std keeps it checked: nothing in the checked code refers to it.
#[test]
fn every_hook_lowering_calls_is_marked_lowered() {
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut names = vec![];
    for c in [
        "velt_vir",
        "velt_opt",
        "velt_codegen_llvm",
        "velt_codegen_cl",
    ] {
        hook_uses(&crates.join(c).join("src"), &mut names);
    }
    assert!(names.iter().any(|n| n == "any_loose_eq"), "{names:?}");
    let unmarked: Vec<&String> = (names.iter())
        .filter(|n| {
            !velt_common::std_hooks::HOOKS
                .iter()
                .any(|h| h.name == n.as_str() && h.lowered)
        })
        .collect();
    assert!(
        unmarked.is_empty(),
        "mark `lowered` in velt_common std_hooks.rs: {unmarked:?}"
    );
}
