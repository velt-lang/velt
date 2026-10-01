//! What a built native library exports: read from the shared library (ELF, Mach-O or PE) with
//! the `object` crate, so it works for any target, not only the host's. The SDK's `#[export]`
//! emits a `velt_sig_<name>` data symbol holding each function's NUL-terminated signature.

use std::collections::{BTreeMap, BTreeSet};

use object::{Object, ObjectSection};

use crate::native::{export_prefix, init_symbol, SIG_PREFIX};

/// Exported symbols a Rust `cdylib` may carry besides the crate's own.
const TOOLCHAIN_EXPORTS: &[&str] = &["rust_eh_personality", "_fltused", "__rust_probestack"];

/// The exports of library `bytes` with their signatures; checks the naming rules for `package`:
/// `velt_native_init_<pkg>`, signature records, and `<pkg>_*` functions that each have one.
pub fn read(bytes: &[u8], package: &str) -> Result<BTreeMap<String, String>, String> {
    let file = object::File::parse(bytes).map_err(|e| format!("cannot read the library: {e}"))?;
    let macho = matches!(file.format(), object::BinaryFormat::MachO);
    let mut names = BTreeSet::new();
    let mut sigs = BTreeMap::new();
    for export in file
        .exports()
        .map_err(|e| format!("cannot read the library's exports: {e}"))?
    {
        let raw = String::from_utf8_lossy(export.name()).into_owned();
        let name = match raw.strip_prefix('_') {
            Some(n) if macho => n.to_string(),
            _ => raw,
        };
        if let Some(func) = name.strip_prefix(SIG_PREFIX) {
            sigs.insert(func.to_string(), read_c_string(&file, export.address())?);
        } else {
            names.insert(name);
        }
    }
    check(names, sigs, package)
}

fn read_c_string(file: &object::File, address: u64) -> Result<String, String> {
    for section in file.sections() {
        let (start, size) = (section.address(), section.size());
        if address < start || address >= start + size {
            continue;
        }
        let data = section
            .data()
            .map_err(|e| format!("cannot read the library: {e}"))?;
        let rest = data.get((address - start) as usize..).unwrap_or_default();
        let end = rest
            .iter()
            .position(|&b| b == 0)
            .ok_or("a signature record is not NUL-terminated")?;
        return String::from_utf8(rest[..end].to_vec())
            .map_err(|_| "a signature record is not UTF-8".into());
    }
    Err(format!(
        "no section holds the signature record at {address:#x}"
    ))
}

fn check(
    names: BTreeSet<String>,
    mut sigs: BTreeMap<String, String>,
    package: &str,
) -> Result<BTreeMap<String, String>, String> {
    let prefix = export_prefix(package);
    let init = init_symbol(package);
    if !names.contains(&init) {
        return Err(format!(
            "the library does not export `{init}` (add `velt_native::package!({});`)",
            package.replace('-', "_")
        ));
    }
    let mut out = BTreeMap::new();
    let mut problems = vec![];
    for name in names {
        if name == init || TOOLCHAIN_EXPORTS.contains(&name.as_str()) {
            continue;
        }
        if !name.starts_with(&prefix) {
            problems.push(format!(
                "`{name}` does not start with `{prefix}` (every export of package `{package}` must)"
            ));
            continue;
        }
        match sigs.remove(&name) {
            Some(sig) => {
                out.insert(name, sig);
            }
            None => problems.push(format!(
                "`{name}` has no signature record (export it with `#[velt_native::export]`)"
            )),
        }
    }
    for name in sigs.keys() {
        problems.push(format!(
            "a signature record names `{name}`, which is not exported"
        ));
    }
    if problems.is_empty() {
        Ok(out)
    } else {
        Err(format!(
            "invalid native library exports:\n  {}",
            problems.join("\n  ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn naming_rules() {
        let sigs = BTreeMap::from([("db_open".to_string(), "(string)->u64".to_string())]);
        let ok = check(
            set(&["velt_native_init_db", "db_open", "rust_eh_personality"]),
            sigs.clone(),
            "db",
        )
        .unwrap();
        assert_eq!(ok, sigs);

        let e = check(set(&["db_open"]), sigs.clone(), "db").unwrap_err();
        assert!(e.contains("velt_native::package!(db)"), "{e}");

        let e = check(
            set(&["velt_native_init_db", "db_open", "helper", "db_close"]),
            sigs,
            "db",
        )
        .unwrap_err();
        assert!(e.contains("`helper` does not start with `db_`"), "{e}");
        assert!(e.contains("`db_close` has no signature record"), "{e}");
    }

    #[test]
    fn reads_a_real_library() {
        // The test binary itself is an ELF/Mach-O/PE file; it has no Velt exports.
        let exe = std::fs::read(std::env::current_exe().unwrap()).unwrap();
        let e = read(&exe, "nothing").unwrap_err();
        assert!(
            e.contains("velt_native_init_nothing") || e.contains("cannot read"),
            "{e}"
        );
    }
}
