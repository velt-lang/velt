//! The definitions lowering looks up by name are roots of the live set ([`is_helper`]).

use super::*;

/// The definitions named by `std_item!(<entry>, "<item>")` in `src` (velt_common's std module
/// table): per use, the byte offset of the item's text, its module's canonical path and the
/// item (`"Proxy.from"`).
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
            // A definition of a std module outside the prelude (`"std/lang/proxy::Proxy.from"`):
            // `every_std_definition_lowering_names_is_a_root` checks those.
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
    assert!(names.iter().any(|n| n == "__error"), "{names:?}");
    // Arm's CRC32 C intrinsics (`__crc32b`, velt_vir's arch table names each row's C intrinsic)
    // are documentation. C preprocessor macros (`__AVX2__`, `__GCC_…`, `__ARM_FEATURE_…`), DWARF names and the C library's own
    // symbols (`errno`'s location, velt_vir `c_abi.rs`) and Mach-O segment and section
    // names (velt_codegen_cl `eh.rs`) are no definitions, nor is the import library's prefix of a
    // Windows DLL's variable (velt_vir `lower/c_global.rs`), nor the C runtime's handler of
    // structured exceptions (velt_codegen_cl `eh_seh.rs`).
    const C_SYMBOLS: &[&str] = &[
        "__error",
        "__C_specific_handler",
        "__errno_location",
        "__TEXT",
        "__gcc_except_tab",
        "__imp_",
    ];
    let missing: Vec<&String> = (names.iter())
        .filter(|n| {
            !n.ends_with("__")
                && !n.starts_with("__DWARF")
                && !n.starts_with("__GCC")
                && !n.starts_with("__ARM_")
                && !n.starts_with("__crc32")
        })
        .filter(|n| !C_SYMBOLS.contains(&n.as_str()))
        .filter(|n| !is_helper(n))
        .collect();
    assert!(missing.is_empty(), "add to HELPER_NAMES: {missing:?}");
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

/// A definition lowering, the optimizer or a backend looks up by name must be live with it: it is
/// a root of the live set: its module's free functions are ([`BY_NAME`]), or it is a prelude
/// helper, or it is a prelude type (live with a value of it) or an interface (no bodies; the
/// methods lowering calls through it are those of a live type).
#[test]
fn every_std_definition_lowering_names_is_a_root() {
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
    // or a value's type with (`RegExpMatch`: its `AnyIndex` reads `at`, a member live with the
    // type's `any` table), never looks up: a call of one is checked code calling a checked
    // function.
    const RECOGNIZED: &[(&str, &str)] = &[
        ("std/text/string_builder", "StringBuilder.append"),
        ("std/text/regex/regexp_match", "RegExpMatch"),
    ];
    let missing: Vec<_> = (names.iter())
        .filter(|(m, name)| !INTERFACES.contains(&(m.as_str(), name.as_str())))
        .filter(|(m, name)| !RECOGNIZED.contains(&(m.as_str(), name.as_str())))
        .filter(|(m, name)| {
            let eager = BY_NAME.iter().any(|p| m.starts_with(p));
            let prelude = HELPERS.iter().any(|p| m.starts_with(p))
                && (name.is_empty()
                    || is_helper(name)
                    || name.starts_with(|c: char| c.is_ascii_uppercase()));
            !eager && !prelude
        })
        .collect();
    assert!(
        missing.is_empty(),
        "add to BY_NAME (or to HELPER_NAMES): {missing:?}"
    );
}

#[test]
fn helpers_by_name_and_by_family() {
    assert!(is_helper(velt_common::std_hook!(any_loose_eq)));
    assert!(is_helper("anyRec_get"));
    assert!(is_helper("anyMap_set"));
    assert!(!is_helper(velt_common::std_hook!(any_member)));
    assert!(!is_helper("Number"));
    // A hook whose entry lists constructs is checked where live code has one (`live.rs`).
    assert!(!is_helper(velt_common::std_hook!(int_push)));
}

/// The hooks named in the Rust sources under `dir`, outside tests: `std_hook!(<entry>)`, and
/// calls of `velt_vir`'s typed builders, `hooks::<entry>(` (lower/hook_calls.rs, which names
/// each through a macro variable, `std_hook!($name)`, skipped here).
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
        let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
        for pat in ["std_hook!(", "hooks::"] {
            for (i, m) in src.match_indices(pat) {
                // `hooks::` alone, not `std_hooks::`.
                if src[..i].chars().next_back().is_some_and(ident) {
                    continue;
                }
                let rest = &src[i + m.len()..];
                let name: String = rest.chars().take_while(|&c| ident(c)).collect();
                let call = pat == "std_hook!(" || rest[name.len()..].starts_with('(');
                if !name.is_empty() && call {
                    out.push(name);
                }
            }
        }
    }
}

/// A hook lowering, the optimizer or a backend calls is marked `lowered` in velt_common's
/// table, so lazy std keeps it checked: nothing in the checked code refers to it. A decorator
/// lowering reads (`@inline`) is not called: a program that uses it has its module loaded.
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
            !velt_common::std_hooks::HOOKS.iter().any(|h| {
                let decorator = h.sig == velt_common::hook_sig::Sig::Decorator;
                h.name == n.as_str() && (h.lowered || decorator)
            })
        })
        .collect();
    assert!(
        unmarked.is_empty(),
        "mark `lowered` in velt_common std_hooks.rs: {unmarked:?}"
    );
}

/// A context over one std module `path`, with no definitions.
fn empty_std_module(path: &str) -> crate::SourceModule {
    let file = velt_common::FileId(0);
    let (ast, diags) = velt_syntax::parse_file(file, "");
    assert!(diags.is_empty());
    crate::SourceModule {
        path: path.to_string(),
        is_std: true,
        is_dependency: false,
        file,
        ast,
        imports: vec![],
        jsx_runtime: None,
    }
}

/// A hook whose module the program has but that the module doesn't define is an internal error
/// at the demand, naming both, not a hook dropped for lowering to miss later (#3745).
#[test]
fn a_demanded_hook_its_module_lacks_is_an_internal_error() {
    let modules = [empty_std_module(velt_common::std_module!(lang_hooks_sched))];
    let mut cx = Ctx::new(&modules, 0);
    let mut found = vec![];
    demanded::hooks(&mut cx, SCHED_HOOKS, &mut found);
    assert!(found.is_empty());
    let start = velt_common::std_hook!(sched_start);
    let want = format!(
        "ICE: std hook `{start}` demanded, but its module `std/lang/hooks/sched` doesn't define it"
    );
    assert!(
        cx.diags.iter().any(|d| d.message == want),
        "{:#?}",
        cx.diags
    );
}

/// A hook whose module the program doesn't have (no std, a load refused) is no error: the
/// feature it serves is not in the program.
#[test]
fn a_hook_whose_module_is_not_loaded_is_not_found_and_no_error() {
    let modules = [empty_std_module(velt_common::std_module!(lang_hooks_cpu))];
    let mut cx = Ctx::new(&modules, 0);
    assert_eq!(
        demanded::hook(&mut cx, velt_common::std_hook!(sched_start)),
        None
    );
    assert!(cx.diags.is_empty(), "{:#?}", cx.diags);
}
