//! The compiler's helpers that lowering looks up by name stay checked ([`is_helper`]).

use super::*;

/// The methods of [`LAZY_PRELUDE_TYPES`] lowering calls by name: each only where it has a value
/// of the type, so its type is used and its methods are checked.
const LAZY_PRELUDE_METHODS: &[&str] = &["__runtimeHandle", "__writeJson"];

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
        for (i, _) in src.match_indices("__") {
            let before = src[..i].chars().last();
            if !matches!(before, Some('"') | Some(':') | Some('.')) {
                continue;
            }
            // A definition of a std module outside the prelude (`"std/proxy::Proxy.__view"`):
            // `every_std_definition_lowering_names_stays_checked` checks those.
            let line = &src[src[..i].rfind('\n').map_or(0, |l| l + 1)..i];
            let lit = line.rfind('"').map_or("", |q| &line[q + 1..]);
            if lit.starts_with("std/") && !HELPERS.iter().any(|p| lit.starts_with(p)) {
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
    assert!(names.iter().any(|n| n == "__anyLooseEq"), "{names:?}");
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

/// The `"std/<module>::<name>"` literals in the Rust sources under `dir`, outside tests.
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
        for (i, _) in src.match_indices("\"std/") {
            let lit: String = src[i + 1..].chars().take_while(|c| *c != '"').collect();
            if let Some((module, name)) = lit.split_once("::") {
                out.push((module.to_string(), name.to_string()));
            }
        }
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
        names.iter().any(|(m, _)| m == "std/sync/epoch"),
        "{names:?}"
    );
    const INTERFACES: &[(&str, &str)] = &[("std/db", "RowSource")];
    let missing: Vec<_> = (names.iter())
        .filter(|(m, name)| !INTERFACES.contains(&(m.as_str(), name.as_str())))
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
    assert!(is_helper("__anyLooseEq"));
    assert!(is_helper("__anyRec_get"));
    assert!(is_helper("__anyMap_set"));
    assert!(!is_helper("__anyMember"));
    assert!(!is_helper("Number"));
}
