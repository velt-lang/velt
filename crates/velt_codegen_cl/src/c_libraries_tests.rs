//! `velt dev` on Windows resolves every C function std declares from `c` that a linked build
//! imports from a system DLL: the kit's `.def` files (crates/velt_link/kit/windows) say which DLL
//! exports each one. bcrypt.dll's `BCryptGenRandom` (std/lang/random/os_win32.velt) went missing
//! when the runtime stopped loading bcrypt.dll itself, and every `velt dev` of a program using
//! `Math.random()` failed to build.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The repository's root (this crate is `crates/velt_codegen_cl`).
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Each function a kit `.def` exports, with the `.def`'s name (`bcrypt` for bcrypt.def).
fn exports() -> BTreeMap<String, String> {
    let dir = root().join("crates/velt_link/kit/windows");
    let mut out = BTreeMap::new();
    for entry in std::fs::read_dir(&dir).expect("ICE: the kit's Windows directory") {
        let path = entry.expect("ICE: a kit entry").path();
        if path.extension().is_none_or(|e| e != "def") {
            continue;
        }
        let lib = path.file_stem().unwrap().to_string_lossy().to_string();
        let text = std::fs::read_to_string(&path).expect("ICE: a .def file");
        let names = text.lines().skip_while(|l| l.trim() != "EXPORTS").skip(1);
        for name in names.filter_map(|l| l.split_whitespace().next()) {
            out.entry(name.to_string()).or_insert_with(|| lib.clone());
        }
    }
    out
}

/// The C names of std's `declare c("c")` functions (a `$suffix` names the same C function).
fn std_c_functions(dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    for entry in std::fs::read_dir(dir).expect("ICE: a std directory") {
        let path = entry.expect("ICE: a std entry").path();
        if path.is_dir() {
            std_c_functions(&path, out);
            continue;
        }
        if path.extension().is_none_or(|e| e != "velt") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("ICE: a std file");
        for line in text.lines() {
            let Some(at) = line.find("declare c(\"c\"") else {
                continue;
            };
            let Some(rest) = line[at..].split("function ").nth(1) else {
                continue;
            };
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            out.push((name, path.clone()));
        }
    }
}

#[test]
fn the_system_dlls_load() {
    let names = super::platform::system_dll_names();
    let failed: Vec<_> = names.iter().filter(|(_, ok)| !ok).collect();
    assert!(failed.is_empty(), "not loaded: {failed:?}");
}

#[test]
fn velt_dev_finds_every_system_function_std_declares() {
    let exports = exports();
    let mut functions = vec![];
    std_c_functions(&root().join("std"), &mut functions);
    assert!(
        functions.iter().any(|(n, _)| n == "BCryptGenRandom"),
        "std/lang/random/os_win32.velt declares BCryptGenRandom"
    );
    let mut missing = vec![];
    for (name, file) in &functions {
        let Some(lib) = exports.get(name) else {
            continue;
        };
        if super::loaded(name).is_none() {
            missing.push(format!("{name} ({lib}.dll, {})", file.display()));
        }
    }
    missing.sort();
    missing.dedup();
    assert!(
        missing.is_empty(),
        "C functions std declares that `velt dev` doesn't find:\n{}",
        missing.join("\n")
    );
}
