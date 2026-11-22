//! Windows' `shared_native_libs` against std: a debug build links the shared runtime, so the
//! program's own objects call std's `declare c` functions, and every import library one of them
//! lives in must be on the list (a missing `bcrypt.lib` failed the debug link of every program
//! using `Math.random()` with LNK2019 `BCryptGenRandom`). The kit's `.def` files say which DLL
//! exports each function.

use super::WINDOWS;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The repository's root (this crate is `crates/velt_link`).
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The C library's own DLLs: `msvcrt.lib` brings them (`/DEFAULTLIB`), whatever std calls there.
const C_LIBRARY: &[&str] = &["ucrt"];

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
        if C_LIBRARY.contains(&lib.as_str()) {
            continue;
        }
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
fn windows_shared_libs_hold_every_import_library_std_calls() {
    let exports = exports();
    assert!(
        exports.contains_key("BCryptGenRandom"),
        "the kit's bcrypt.def"
    );
    let mut functions = vec![];
    std_c_functions(&root().join("std"), &mut functions);
    let mut missing = vec![];
    for (name, file) in &functions {
        let Some(lib) = exports.get(name) else {
            continue;
        };
        let import = format!("{lib}.lib");
        if !WINDOWS.shared_native_libs.contains(&import.as_str()) {
            missing.push(format!("{import} ({name}, {})", file.display()));
        }
    }
    missing.sort();
    missing.dedup();
    assert!(
        missing.is_empty(),
        "Windows' shared_native_libs lacks import libraries std's C calls need:\n{}",
        missing.join("\n")
    );
}
