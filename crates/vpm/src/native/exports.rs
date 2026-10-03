//! What a built native library exports: read from the shared library (ELF, Mach-O or PE) with
//! the `object` crate, so it works for any target, not only the host's. The SDK's `#[export]`
//! emits a `velt_sig_<name>` data symbol holding each function's NUL-terminated signature.
//!
//! A prebuilt bundle is checked against its metadata with the same reader
//! (`native::bundle::check_exports`): the shared library here, the Windows import library with
//! [`check_import_library`] and the prelinked object with [`check_static_object`].

use std::collections::{BTreeMap, BTreeSet};

use object::{Object, ObjectSection, ObjectSymbol};

use crate::native::{export_prefix, init_symbol, SIG_PREFIX};

/// Exported symbols a Rust `cdylib` may carry besides the crate's own.
pub const TOOLCHAIN_EXPORTS: &[&str] = &["rust_eh_personality", "_fltused", "__rust_probestack"];

/// The exports of library `bytes` with their signatures; checks the naming rules for `package`:
/// `velt_native_init_<pkg>`, signature records, and `velt_<pkg>__*` functions that each have one.
pub fn read(bytes: &[u8], package: &str) -> Result<BTreeMap<String, String>, String> {
    let file = object::File::parse(bytes).map_err(|e| format!("cannot read the library: {e}"))?;
    let mut names = BTreeSet::new();
    let mut sigs = BTreeMap::new();
    for (name, address) in exported_symbols(&file)? {
        if let Some(func) = name.strip_prefix(SIG_PREFIX) {
            sigs.insert(func.to_string(), read_c_string(&file, address)?);
        } else {
            names.insert(name);
        }
    }
    check(names, sigs, package)
}

/// Every symbol of library `bytes` another module can bind to, by name (signature records
/// included).
pub fn exported_names(bytes: &[u8]) -> Result<BTreeSet<String>, String> {
    let file = object::File::parse(bytes).map_err(|e| format!("cannot read the library: {e}"))?;
    Ok(exported_symbols(&file)?
        .into_iter()
        .map(|(n, _)| n)
        .collect())
}

/// `(name, address)` of every symbol the library exports. For ELF that is every defined dynamic
/// symbol with global, weak or unique binding and default or protected visibility, whatever its
/// type (IFUNC, TLS, common and absolute symbols bind like functions, and `exports()` skips
/// them). Mach-O names lose their leading `_`.
fn exported_symbols(file: &object::File) -> Result<Vec<(String, u64)>, String> {
    let name_of = |raw: &[u8]| {
        let raw = String::from_utf8_lossy(raw).into_owned();
        match raw.strip_prefix('_') {
            Some(n) if file.format() == object::BinaryFormat::MachO => n.to_string(),
            _ => raw,
        }
    };
    if file.format() == object::BinaryFormat::Elf {
        let mut out = vec![];
        for symbol in file.dynamic_symbols() {
            let object::SymbolFlags::Elf { st_info, st_other } = symbol.flags() else {
                continue;
            };
            // STB_GLOBAL, STB_WEAK, STB_GNU_UNIQUE; STV_DEFAULT, STV_PROTECTED.
            let bindable = matches!(st_info >> 4, 1 | 2 | 10) && matches!(st_other & 3, 0 | 3);
            if symbol.is_undefined() || !bindable {
                continue;
            }
            let name = symbol
                .name_bytes()
                .map_err(|e| format!("cannot read the library's exports: {e}"))?;
            out.push((name_of(name), symbol.address()));
        }
        return Ok(out);
    }
    let exports = file
        .exports()
        .map_err(|e| format!("cannot read the library's exports: {e}"))?;
    Ok(exports
        .iter()
        .map(|e| (name_of(e.name()), e.address()))
        .collect())
}

/// A Windows import library (`bytes`) must import exactly `exports` (the DLL's export names), each
/// by its own name from `dll`: an import library could otherwise map a package's function to
/// another DLL's (`free` in the C runtime) for the executable that links it. The archive's symbol
/// map, which the linker searches instead of the members, must point each name at a member that
/// defines it.
pub fn check_import_library(
    bytes: &[u8],
    dll: &str,
    exports: &BTreeSet<String>,
) -> Result<(), String> {
    use object::read::coff::{ImportFile, ImportName};
    let bad = |e: object::read::Error| format!("cannot read the import library: {e}");
    let archive = object::read::archive::ArchiveFile::parse(bytes).map_err(bad)?;
    let stem = dll.strip_suffix(".dll").unwrap_or(dll);
    let descriptors = [
        format!("__IMPORT_DESCRIPTOR_{stem}"),
        "__NULL_IMPORT_DESCRIPTOR".to_string(),
        format!("\x7f{stem}_NULL_THUNK_DATA"),
    ];
    let mut imported = BTreeSet::new();
    let mut problems = vec![];
    // Member (by its data's range) → the names it defines.
    let mut defined: BTreeMap<(u64, u64), BTreeSet<String>> = BTreeMap::new();
    for member in archive.members() {
        let member = member.map_err(bad)?;
        let data = member.data(bytes).map_err(bad)?;
        let names = defined.entry(member.file_range()).or_default();
        match ImportFile::parse(data) {
            Ok(import) => {
                let symbol = String::from_utf8_lossy(import.symbol()).into_owned();
                let from = String::from_utf8_lossy(import.dll()).into_owned();
                if !from.eq_ignore_ascii_case(dll) {
                    problems.push(format!("`{symbol}` is imported from `{from}`, not `{dll}`"));
                }
                match import.import() {
                    ImportName::Name(name) if name == symbol.as_bytes() => {}
                    ImportName::Name(name) => problems.push(format!(
                        "`{symbol}` is imported as `{}`",
                        String::from_utf8_lossy(name)
                    )),
                    ImportName::Ordinal(n) => {
                        problems.push(format!("`{symbol}` is imported by ordinal {n}"))
                    }
                }
                names.insert(format!("__imp_{symbol}"));
                names.insert(symbol.clone());
                imported.insert(symbol);
            }
            // The import descriptor and thunk objects define only their fixed names.
            Err(_) => {
                let object = object::File::parse(data).map_err(bad)?;
                for symbol in object.symbols() {
                    if symbol.is_undefined() || symbol.is_local() {
                        continue;
                    }
                    let name = String::from_utf8_lossy(symbol.name_bytes().unwrap_or_default())
                        .into_owned();
                    if !descriptors.contains(&name) {
                        problems.push(format!("the import library defines `{name}`"));
                    }
                    names.insert(name);
                }
            }
        }
    }
    for symbol in archive.symbols().map_err(bad)?.into_iter().flatten() {
        let symbol = symbol.map_err(bad)?;
        let name = String::from_utf8_lossy(symbol.name()).into_owned();
        let target = archive.member(symbol.offset()).ok().map(|m| m.file_range());
        match target.and_then(|t| defined.get(&t)) {
            Some(names) if names.contains(&name) => {}
            Some(_) => problems.push(format!(
                "the symbol map points `{name}` at a member that does not define it"
            )),
            None => problems.push(format!(
                "the symbol map points `{name}` at offset {} where no member starts",
                symbol.offset().0
            )),
        }
    }
    for name in exports.difference(&imported) {
        problems.push(format!("`{name}` is exported by the DLL but not imported"));
    }
    for name in imported.difference(exports) {
        problems.push(format!(
            "`{name}` is imported but the DLL does not export it"
        ));
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "the import library does not match `{dll}`:\n  {}",
            problems.join("\n  ")
        ))
    }
}

/// The prelinked object (`bytes`) of `package` may define, as non-local symbols, only `exports`,
/// the init function and toolchain names: a release build links it into the executable, where any
/// other global (`free`) would replace the C library's.
pub fn check_static_object(
    bytes: &[u8],
    package: &str,
    exports: &BTreeMap<String, String>,
) -> Result<(), String> {
    let file =
        object::File::parse(bytes).map_err(|e| format!("cannot read the object file: {e}"))?;
    let macho = file.format() == object::BinaryFormat::MachO;
    let init = init_symbol(package);
    let mut extra = BTreeSet::new();
    for symbol in file.symbols() {
        // A Mach-O tentative definition (a common symbol) is N_UNDF with a size in n_value; it
        // wins over a dylib's definition (`-commons ignore_dylibs`), so it is one.
        let common = macho && symbol.is_undefined() && symbol.address() != 0;
        if (symbol.is_undefined() && !common) || symbol.is_local() {
            continue;
        }
        let raw = String::from_utf8_lossy(
            symbol
                .name_bytes()
                .map_err(|e| format!("cannot read the object file: {e}"))?,
        )
        .into_owned();
        let name = match raw.strip_prefix('_') {
            Some(n) if macho => n.to_string(),
            _ => raw,
        };
        let allowed = exports.contains_key(&name)
            || name == init
            || TOOLCHAIN_EXPORTS.contains(&name.as_str());
        if !allowed {
            extra.insert(name);
        }
    }
    if extra.is_empty() {
        Ok(())
    } else {
        let names: Vec<String> = extra.into_iter().collect();
        Err(format!(
            "the object file defines global symbols that are not the package's exports: `{}`",
            names.join("`, `")
        ))
    }
}

fn read_c_string(file: &object::File, address: u64) -> Result<String, String> {
    for section in file.sections() {
        let (start, size) = (section.address(), section.size());
        let Some(offset) = address.checked_sub(start).filter(|&o| o < size) else {
            continue;
        };
        let data = section
            .data()
            .map_err(|e| format!("cannot read the library: {e}"))?;
        let rest = usize::try_from(offset)
            .ok()
            .and_then(|o| data.get(o..))
            .unwrap_or_default();
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
            let hint = match crate::native::renamed_export(package, &name) {
                Some(new) => format!("; rename it `{new}`"),
                None => String::new(),
            };
            problems.push(format!(
                "`{name}` does not start with `{prefix}` (every export of package `{package}` must){hint}"
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

// The sample files moved to `native::samples`; tests and other crates use them through here.
#[doc(hidden)]
pub use crate::native::samples::*;

#[cfg(test)]
mod tests {
    use super::*;

    fn db_exports() -> BTreeMap<String, String> {
        BTreeMap::from([("velt_db__open".to_string(), "(string)->u64".to_string())])
    }

    #[test]
    fn every_bindable_elf_symbol_counts() {
        // IFUNC (10), TLS (6), absolute and unique (10 << 4) symbols bind like functions; a
        // hidden (st_other 2), internal (1) or local one doesn't.
        for (info, other, shndx) in [
            ((1 << 4) | 10, 0, 1),
            ((1 << 4) | 6, 0, 1),
            (GLOBAL_FUNC, 0, 0xfff1),
            ((10 << 4) | 2, 0, 1),
            ((2 << 4) | 2, 3, 1), // weak, protected
        ] {
            let mut elf = SampleElf::library("db", &db_exports(), "");
            elf.symbols.push(("free".into(), info, other, shndx, 0));
            let e = read(&elf.bytes(), "db").unwrap_err();
            assert!(
                e.contains("`free` does not start with `velt_db__`"),
                "{info:#x} {shndx:#x}: {e}"
            );
        }
        for (info, other) in [(GLOBAL_FUNC, 2), (GLOBAL_FUNC, 1), (2, 0)] {
            let mut elf = SampleElf::library("db", &db_exports(), "");
            elf.symbols.insert(1, ("free".into(), info, other, 1, 0));
            if info == 2 {
                // Local symbols come first in a symbol table.
                elf.symbols.swap(0, 1);
            }
            assert_eq!(read(&elf.bytes(), "db").unwrap(), db_exports());
        }
    }

    #[test]
    fn section_addresses_near_the_top_do_not_overflow() {
        let mut elf = SampleElf::library("db", &db_exports(), "padding");
        elf.rodata_address = Some(u64::MAX - 4);
        // The signature record is inside the section, whose end is past u64::MAX.
        assert_eq!(read(&elf.bytes(), "db").unwrap(), db_exports());
        let mut elf = SampleElf::library("db", &db_exports(), "");
        elf.symbols[2].4 = 1000; // velt_sig_db_open, outside every section
        let e = read(&elf.bytes(), "db").unwrap_err();
        assert!(e.contains("no section holds the signature record"), "{e}");
    }

    #[test]
    fn import_libraries_import_the_dlls_own_functions() {
        let names = exported_names(&sample_library("db", &db_exports(), "")).unwrap();
        let own: Vec<(String, String)> = names.iter().map(|n| (n.clone(), n.clone())).collect();
        let dll = "velt_native_db.dll";
        check_import_library(&sample_import_library(dll, &own), dll, &names).unwrap();
        // `velt_db__open` mapped to the C runtime's `free` (IMPORT_OBJECT_NAME_EXPORTAS).
        let mut renamed = own.clone();
        renamed
            .iter_mut()
            .find(|(s, _)| s == "velt_db__open")
            .unwrap()
            .1 = "free".into();
        let e =
            check_import_library(&sample_import_library(dll, &renamed), dll, &names).unwrap_err();
        assert!(e.contains("`velt_db__open` is imported as `free`"), "{e}");
        // Another DLL's.
        let e = check_import_library(&sample_import_library("ucrtbase.dll", &own), dll, &names)
            .unwrap_err();
        assert!(e.contains("is imported from `ucrtbase.dll`"), "{e}");
        // Missing and extra imports.
        let mut changed = own.clone();
        changed.retain(|(s, _)| s != "velt_db__open");
        changed.push(("free".into(), "free".into()));
        let e =
            check_import_library(&sample_import_library(dll, &changed), dll, &names).unwrap_err();
        assert!(
            e.contains("`velt_db__open` is exported by the DLL but not imported"),
            "{e}"
        );
        assert!(
            e.contains("`free` is imported but the DLL does not export it"),
            "{e}"
        );
        assert!(check_import_library(b"not an archive", dll, &names).is_err());
    }

    #[test]
    fn static_objects_define_only_the_packages_functions() {
        let exports = db_exports();
        check_static_object(&sample_object("db", &exports, ""), "db", &exports).unwrap();
        let mut elf = SampleElf::static_object("db", &exports, "");
        // A hidden global still binds within the executable.
        elf.symbols.push(("free".into(), GLOBAL_FUNC, 2, 1, 0));
        let e = check_static_object(&elf.bytes(), "db", &exports).unwrap_err();
        assert!(e.contains("not the package's exports: `free`"), "{e}");
        // Undefined references are fine.
        let mut elf = SampleElf::static_object("db", &exports, "");
        elf.symbols.push(("malloc".into(), GLOBAL_FUNC, 0, 0, 0));
        check_static_object(&elf.bytes(), "db", &exports).unwrap();
    }

    /// A relocatable object of `format` defining each of `defined` in `.text` and `common` as a
    /// common (tentative) symbol.
    fn written_object(format: object::BinaryFormat, defined: &[&str], common: &[&str]) -> Vec<u8> {
        use object::write::{Object as Written, Symbol, SymbolSection};
        use object::{Architecture, Endianness, SymbolFlags, SymbolKind, SymbolScope};
        let mut obj = Written::new(format, Architecture::X86_64, Endianness::Little);
        let text = obj.section_id(object::write::StandardSection::Text);
        obj.append_section_data(text, &[0xc3; 16], 1);
        let symbol = |name: &str, section| Symbol {
            name: name.as_bytes().to_vec(),
            value: 0,
            size: 0,
            kind: SymbolKind::Text,
            scope: SymbolScope::Dynamic,
            weak: false,
            section,
            flags: SymbolFlags::None,
        };
        for name in defined {
            obj.add_symbol(symbol(name, SymbolSection::Section(text)));
        }
        for name in common {
            let mut s = symbol(name, SymbolSection::Undefined);
            s.kind = SymbolKind::Data;
            obj.add_common_symbol(s, 8, 8);
        }
        obj.write().unwrap()
    }

    #[test]
    fn import_library_symbol_maps_point_at_their_definitions() {
        let dll = "velt_native_db.dll";
        let imports = [
            ("velt_db__open", Some("velt_db__open")),
            ("velt_native_init_db", Some("velt_native_init_db")),
        ];
        let members: Vec<Vec<u8>> = imports
            .iter()
            .map(|(s, i)| sample_short_import(s, dll, *i))
            .collect();
        let names: BTreeSet<String> = imports.iter().map(|(s, _)| s.to_string()).collect();
        let ok = [("velt_db__open".to_string(), MapTarget::Member(0))];
        check_import_library(&sample_archive(&members, &ok), dll, &names).unwrap();
        // The linker would take `free` from the `velt_db__open` member.
        let wrong = [("free".to_string(), MapTarget::Member(0))];
        let e = check_import_library(&sample_archive(&members, &wrong), dll, &names).unwrap_err();
        assert!(
            e.contains("points `free` at a member that does not define it"),
            "{e}"
        );
        // An offset inside a member's data, where another header could hide.
        let lib = sample_archive(&members, &ok);
        let inside = (lib.len() - 10) as u32;
        let hidden = [("velt_db__open".to_string(), MapTarget::Offset(inside))];
        let e = check_import_library(&sample_archive(&members, &hidden), dll, &names).unwrap_err();
        assert!(e.contains("where no member starts"), "{e}");
    }

    #[test]
    fn ordinal_imports_and_foreign_definitions_are_refused() {
        let dll = "velt_native_db.dll";
        let names = BTreeSet::from(["velt_db__open".to_string()]);
        let by_ordinal = sample_archive(&[sample_short_import("velt_db__open", dll, None)], &[]);
        let e = check_import_library(&by_ordinal, dll, &names).unwrap_err();
        assert!(
            e.contains("`velt_db__open` is imported by ordinal 1"),
            "{e}"
        );
        // A long-format member may define only the import descriptor names.
        let members = [
            sample_short_import("velt_db__open", dll, Some("velt_db__open")),
            written_object(
                object::BinaryFormat::Coff,
                &["__IMPORT_DESCRIPTOR_velt_native_db", "free"],
                &[],
            ),
        ];
        let e = check_import_library(&sample_archive(&members, &[]), dll, &names).unwrap_err();
        assert!(e.contains("the import library defines `free`"), "{e}");
        assert!(!e.contains("`__IMPORT_DESCRIPTOR_velt_native_db`"), "{e}");
    }

    #[test]
    fn common_symbols_are_definitions_in_static_objects() {
        let exports = db_exports();
        // ELF SHN_COMMON.
        let mut elf = SampleElf::static_object("db", &exports, "");
        elf.symbols
            .push(("free".into(), (1 << 4) | 1, 0, 0xfff2, 8));
        let e = check_static_object(&elf.bytes(), "db", &exports).unwrap_err();
        assert!(e.contains("`free`"), "{e}");
        // Mach-O: a tentative `_free` (N_UNDF | N_EXT, n_value = its size).
        let macho = object::BinaryFormat::MachO;
        let good = written_object(macho, &["velt_native_init_db", "velt_db__open"], &[]);
        check_static_object(&good, "db", &exports).unwrap();
        let bad = written_object(macho, &["velt_native_init_db", "velt_db__open"], &["free"]);
        let e = check_static_object(&bad, "db", &exports).unwrap_err();
        assert!(e.contains("not the package's exports: `free`"), "{e}");
    }

    #[test]
    fn a_sample_library_reads_back() {
        let exports = BTreeMap::from([
            (
                "velt_db__open".to_string(),
                "(string)->IoResult<u64>".to_string(),
            ),
            ("velt_db__close".to_string(), "(u64)->void".to_string()),
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
        let sigs = BTreeMap::from([("velt_db__open".to_string(), "(string)->u64".to_string())]);
        let ok = check(
            set(&[
                "velt_native_init_db",
                "velt_db__open",
                "rust_eh_personality",
            ]),
            sigs.clone(),
            "db",
        )
        .unwrap();
        assert_eq!(ok, sigs);

        let e = check(set(&["velt_db__open"]), sigs.clone(), "db").unwrap_err();
        assert!(e.contains("velt_native::package!(db)"), "{e}");

        let e = check(
            set(&[
                "velt_native_init_db",
                "velt_db__open",
                "helper",
                "velt_db__close",
            ]),
            sigs,
            "db",
        )
        .unwrap_err();
        assert!(
            e.contains("`helper` does not start with `velt_db__`"),
            "{e}"
        );
        // An export with the older prefix gets its new name.
        let old = check(
            set(&["velt_native_init_db", "velt_db_open"]),
            BTreeMap::from([("velt_db_open".to_string(), "()->u64".to_string())]),
            "db",
        )
        .unwrap_err();
        assert!(old.contains("rename it `velt_db__open`"), "{old}");
        assert!(
            e.contains("`velt_db__close` has no signature record"),
            "{e}"
        );
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
