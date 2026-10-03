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

/// A minimal ELF shared library (x86-64) whose dynamic symbols are `velt_native_init_<package>`,
/// each of `exports` and its signature record, with `note` as unreferenced data: what [`read`]
/// sees in a library built with the SDK. For tests that need a valid bundle without cargo; it
/// cannot be loaded or linked.
#[doc(hidden)]
pub fn sample_library(package: &str, exports: &BTreeMap<String, String>, note: &str) -> Vec<u8> {
    // Section 1 (.rodata) holds the signature strings and the note; it is placed at file
    // offset == address.
    const RODATA: usize = 64;
    let mut rodata = Vec::new();
    let mut syms: Vec<(String, u8, u64)> = vec![(init_symbol(package), 2, RODATA as u64)];
    for (name, sig) in exports {
        let at = (RODATA + rodata.len()) as u64;
        rodata.extend_from_slice(sig.as_bytes());
        rodata.push(0);
        syms.push((name.clone(), 2, RODATA as u64)); // STT_FUNC
        syms.push((format!("{SIG_PREFIX}{name}"), 1, at)); // STT_OBJECT
    }
    rodata.extend_from_slice(note.as_bytes());
    rodata.push(0);
    let mut dynstr = vec![0u8];
    let mut dynsym = vec![0u8; 24];
    for (name, kind, value) in &syms {
        let name_at = dynstr.len() as u32;
        dynstr.extend_from_slice(name.as_bytes());
        dynstr.push(0);
        dynsym.extend_from_slice(&name_at.to_le_bytes());
        dynsym.push((1 << 4) | kind); // STB_GLOBAL
        dynsym.push(0);
        dynsym.extend_from_slice(&1u16.to_le_bytes()); // .rodata
        dynsym.extend_from_slice(&value.to_le_bytes());
        dynsym.extend_from_slice(&0u64.to_le_bytes());
    }
    let shstrtab = b"\0.rodata\0.dynstr\0.dynsym\0.shstrtab\0".to_vec();
    let align = |n: usize| n.div_ceil(8) * 8;
    let rodata_at = RODATA;
    let dynstr_at = align(rodata_at + rodata.len());
    let dynsym_at = align(dynstr_at + dynstr.len());
    let shstrtab_at = align(dynsym_at + dynsym.len());
    let sh_at = align(shstrtab_at + shstrtab.len());
    let mut out = vec![0u8; sh_at];
    // ELF header: 64-bit, little endian, ET_DYN, x86-64, 5 sections, names in section 4.
    out[..16].copy_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    out[16..18].copy_from_slice(&3u16.to_le_bytes());
    out[18..20].copy_from_slice(&62u16.to_le_bytes());
    out[20..24].copy_from_slice(&1u32.to_le_bytes());
    out[40..48].copy_from_slice(&(sh_at as u64).to_le_bytes());
    out[52..54].copy_from_slice(&64u16.to_le_bytes());
    out[58..60].copy_from_slice(&64u16.to_le_bytes());
    out[60..62].copy_from_slice(&5u16.to_le_bytes());
    out[62..64].copy_from_slice(&4u16.to_le_bytes());
    for (at, data) in [
        (rodata_at, &rodata),
        (dynstr_at, &dynstr),
        (dynsym_at, &dynsym),
        (shstrtab_at, &shstrtab),
    ] {
        out[at..at + data.len()].copy_from_slice(data);
    }
    // Section headers: name, type, flags, addr, offset, size, link, info, align, entsize (the
    // fields of an `Elf64_Shdr`, in their sizes below).
    let (ro, ds, sy, sh) = (
        rodata_at as u64,
        dynstr_at as u64,
        dynsym_at as u64,
        shstrtab_at as u64,
    );
    let headers: [[u64; 10]; 5] = [
        [0; 10],
        [1, 1, 2, ro, ro, rodata.len() as u64, 0, 0, 1, 0], // .rodata: PROGBITS, ALLOC
        [9, 3, 2, 0, ds, dynstr.len() as u64, 0, 0, 1, 0],  // .dynstr: STRTAB
        [17, 11, 2, 0, sy, dynsym.len() as u64, 2, 1, 8, 24], // .dynsym: DYNSYM
        [25, 3, 0, 0, sh, shstrtab.len() as u64, 0, 0, 1, 0], // .shstrtab: STRTAB
    ];
    const SIZES: [usize; 10] = [4, 4, 8, 8, 8, 8, 4, 4, 8, 8];
    for header in headers {
        for (value, size) in header.iter().zip(SIZES) {
            out.extend_from_slice(&value.to_le_bytes()[..size]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sample_library_reads_back() {
        let exports = BTreeMap::from([
            ("db_open".to_string(), "(string)->IoResult<u64>".to_string()),
            ("db_close".to_string(), "(u64)->void".to_string()),
        ]);
        let bytes = sample_library("db", &exports, "v1");
        assert_eq!(read(&bytes, "db").unwrap(), exports);
        assert_ne!(bytes, sample_library("db", &exports, "v2"));
    }

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
