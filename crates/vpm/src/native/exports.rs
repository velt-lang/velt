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
/// `velt_native_init_<pkg>`, signature records, and `<pkg>_*` functions that each have one.
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
/// another DLL's (`free` in the C runtime) for the executable that links it.
pub fn check_import_library(
    bytes: &[u8],
    dll: &str,
    exports: &BTreeSet<String>,
) -> Result<(), String> {
    use object::read::coff::{ImportFile, ImportName};
    let archive = object::read::archive::ArchiveFile::parse(bytes)
        .map_err(|e| format!("cannot read the import library: {e}"))?;
    let stem = dll.strip_suffix(".dll").unwrap_or(dll);
    let descriptors = [
        format!("__IMPORT_DESCRIPTOR_{stem}"),
        "__NULL_IMPORT_DESCRIPTOR".to_string(),
        format!("\x7f{stem}_NULL_THUNK_DATA"),
    ];
    let mut imported = BTreeSet::new();
    let mut problems = vec![];
    for member in archive.members() {
        let member = member.map_err(|e| format!("cannot read the import library: {e}"))?;
        let data = member
            .data(bytes)
            .map_err(|e| format!("cannot read the import library: {e}"))?;
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
                imported.insert(symbol);
            }
            // The import descriptor and thunk objects define only their fixed names.
            Err(_) => {
                let object = object::File::parse(data)
                    .map_err(|e| format!("cannot read the import library: {e}"))?;
                for symbol in object.symbols() {
                    if symbol.is_undefined() || symbol.is_local() {
                        continue;
                    }
                    let name = String::from_utf8_lossy(symbol.name_bytes().unwrap_or_default())
                        .into_owned();
                    if !descriptors.contains(&name) {
                        problems.push(format!("the import library defines `{name}`"));
                    }
                }
            }
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
        if symbol.is_undefined() || symbol.is_local() {
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

/// One symbol of a [`SampleElf`]: name, `st_info` (binding << 4 | type), `st_other`
/// (visibility), section index (1: `.rodata`; `0xfff1`: absolute) and offset into `.rodata`.
#[doc(hidden)]
pub type SampleSymbol = (String, u8, u8, u16, u64);

/// A minimal x86-64 ELF file for tests that need valid bundles without cargo: a shared library
/// (its symbols in `.dynsym`) or a relocatable object (`.symtab`). It cannot be loaded or linked.
#[doc(hidden)]
#[derive(Clone, Debug, Default)]
pub struct SampleElf {
    /// A relocatable object instead of a shared library.
    pub object: bool,
    /// The symbols.
    pub symbols: Vec<SampleSymbol>,
    /// The bytes of the `.rodata` section.
    pub rodata: Vec<u8>,
    /// `.rodata`'s address (default: its file offset).
    pub rodata_address: Option<u64>,
}

/// `STB_GLOBAL` `STT_FUNC`, as `st_info`.
#[doc(hidden)]
pub const GLOBAL_FUNC: u8 = (1 << 4) | 2;

impl SampleElf {
    /// What [`read`] sees in a library of `package` built with the SDK: the init function, each
    /// export and its signature record, with `note` as unreferenced data (so that samples with
    /// different notes differ).
    pub fn library(package: &str, exports: &BTreeMap<String, String>, note: &str) -> SampleElf {
        let mut elf = SampleElf::default();
        elf.symbols
            .push((init_symbol(package), GLOBAL_FUNC, 0, 1, 0));
        for (name, sig) in exports {
            let at = elf.rodata.len() as u64;
            elf.rodata.extend_from_slice(sig.as_bytes());
            elf.rodata.push(0);
            elf.symbols.push((name.clone(), GLOBAL_FUNC, 0, 1, 0));
            // STB_GLOBAL STT_OBJECT
            elf.symbols
                .push((format!("{SIG_PREFIX}{name}"), (1 << 4) | 1, 0, 1, at));
        }
        elf.rodata.extend_from_slice(note.as_bytes());
        elf.rodata.push(0);
        elf
    }

    /// The prelinked object of a library of `package`: the init function and the exports.
    pub fn static_object(
        package: &str,
        exports: &BTreeMap<String, String>,
        note: &str,
    ) -> SampleElf {
        let mut elf = SampleElf {
            object: true,
            ..SampleElf::default()
        };
        let names = std::iter::once(init_symbol(package)).chain(exports.keys().cloned());
        elf.symbols = names.map(|n| (n, GLOBAL_FUNC, 0, 1, 0)).collect();
        elf.rodata = format!("{note}\0").into_bytes();
        elf
    }

    /// The file's bytes.
    pub fn bytes(&self) -> Vec<u8> {
        const RODATA: usize = 64;
        let rodata_address = self.rodata_address.unwrap_or(RODATA as u64);
        let mut strings = vec![0u8];
        let mut symtab = vec![0u8; 24];
        for (name, info, other, shndx, offset) in &self.symbols {
            let name_at = strings.len() as u32;
            strings.extend_from_slice(name.as_bytes());
            strings.push(0);
            symtab.extend_from_slice(&name_at.to_le_bytes());
            symtab.push(*info);
            symtab.push(*other);
            symtab.extend_from_slice(&shndx.to_le_bytes());
            let value = if *shndx == 1 {
                rodata_address.wrapping_add(*offset)
            } else {
                *offset
            };
            symtab.extend_from_slice(&value.to_le_bytes());
            symtab.extend_from_slice(&0u64.to_le_bytes());
        }
        let (strtab_name, symtab_name, symtab_type) = if self.object {
            (".strtab", ".symtab", 2u64) // SHT_SYMTAB
        } else {
            (".dynstr", ".dynsym", 11u64) // SHT_DYNSYM
        };
        let shstrtab = format!("\0.rodata\0{strtab_name}\0{symtab_name}\0.shstrtab\0").into_bytes();
        let align = |n: usize| n.div_ceil(8) * 8;
        let rodata_at = RODATA;
        let strings_at = align(rodata_at + self.rodata.len());
        let symtab_at = align(strings_at + strings.len());
        let shstrtab_at = align(symtab_at + symtab.len());
        let sh_at = align(shstrtab_at + shstrtab.len());
        let mut out = vec![0u8; sh_at];
        // ELF header: 64-bit, little endian, ET_DYN or ET_REL, x86-64, 5 sections, names in 4.
        out[..16].copy_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let kind: u16 = if self.object { 1 } else { 3 };
        out[16..18].copy_from_slice(&kind.to_le_bytes());
        out[18..20].copy_from_slice(&62u16.to_le_bytes());
        out[20..24].copy_from_slice(&1u32.to_le_bytes());
        out[40..48].copy_from_slice(&(sh_at as u64).to_le_bytes());
        out[52..54].copy_from_slice(&64u16.to_le_bytes());
        out[58..60].copy_from_slice(&64u16.to_le_bytes());
        out[60..62].copy_from_slice(&5u16.to_le_bytes());
        out[62..64].copy_from_slice(&4u16.to_le_bytes());
        for (at, data) in [
            (rodata_at, &self.rodata),
            (strings_at, &strings),
            (symtab_at, &symtab),
            (shstrtab_at, &shstrtab),
        ] {
            out[at..at + data.len()].copy_from_slice(data);
        }
        // Section headers: name, type, flags, addr, offset, size, link, info, align, entsize (the
        // fields of an `Elf64_Shdr`, in their sizes below). `info` of the symbol table: the first
        // non-local symbol.
        let (ro, st, sy, sh) = (
            rodata_at as u64,
            strings_at as u64,
            symtab_at as u64,
            shstrtab_at as u64,
        );
        let symtab_name_at = 9 + strtab_name.len() as u64 + 1;
        let shstrtab_name_at = symtab_name_at + symtab_name.len() as u64 + 1;
        let headers: [[u64; 10]; 5] = [
            [0; 10],
            [
                1,
                1,
                2,
                rodata_address,
                ro,
                self.rodata.len() as u64,
                0,
                0,
                1,
                0,
            ],
            [9, 3, 2, 0, st, strings.len() as u64, 0, 0, 1, 0],
            [
                symtab_name_at,
                symtab_type,
                2,
                0,
                sy,
                symtab.len() as u64,
                2,
                1,
                8,
                24,
            ],
            [
                shstrtab_name_at,
                3,
                0,
                0,
                sh,
                shstrtab.len() as u64,
                0,
                0,
                1,
                0,
            ],
        ];
        const SIZES: [usize; 10] = [4, 4, 8, 8, 8, 8, 4, 4, 8, 8];
        for header in headers {
            for (value, size) in header.iter().zip(SIZES) {
                out.extend_from_slice(&value.to_le_bytes()[..size]);
            }
        }
        out
    }
}

/// [`SampleElf::library`]'s bytes.
#[doc(hidden)]
pub fn sample_library(package: &str, exports: &BTreeMap<String, String>, note: &str) -> Vec<u8> {
    SampleElf::library(package, exports, note).bytes()
}

/// [`SampleElf::static_object`]'s bytes.
#[doc(hidden)]
pub fn sample_object(package: &str, exports: &BTreeMap<String, String>, note: &str) -> Vec<u8> {
    SampleElf::static_object(package, exports, note).bytes()
}

/// A Windows import library for `dll`: one short import member per name of `imports`
/// (`(symbol, import name)`), as `link.exe` writes them for x64.
#[doc(hidden)]
pub fn sample_import_library(dll: &str, imports: &[(String, String)]) -> Vec<u8> {
    let mut out = b"!<arch>\n".to_vec();
    let mut member = |name: &str, data: &[u8]| {
        let header = format!(
            "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
            name,
            0,
            0,
            0,
            644,
            data.len()
        );
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(data);
        if data.len() % 2 == 1 {
            out.push(b'\n');
        }
    };
    for (symbol, import) in imports {
        let export_as = symbol != import;
        let mut names = format!("{symbol}\0{dll}\0").into_bytes();
        if export_as {
            names.extend_from_slice(format!("{import}\0").as_bytes());
        }
        let mut data = vec![];
        data.extend_from_slice(&0u16.to_le_bytes()); // Sig1
        data.extend_from_slice(&0xffffu16.to_le_bytes()); // Sig2
        data.extend_from_slice(&0u16.to_le_bytes()); // Version
        data.extend_from_slice(&0x8664u16.to_le_bytes()); // Machine: AMD64
        data.extend_from_slice(&0u32.to_le_bytes()); // TimeDateStamp
        data.extend_from_slice(&(names.len() as u32).to_le_bytes()); // SizeOfData
        data.extend_from_slice(&0u16.to_le_bytes()); // OrdinalOrHint
                                                     // Type: code (0); name type: IMPORT_OBJECT_NAME (1) or _EXPORTAS (4).
        let name_type: u16 = if export_as { 4 } else { 1 };
        data.extend_from_slice(&(name_type << 2).to_le_bytes());
        data.extend_from_slice(&names);
        member("import/", &data);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_exports() -> BTreeMap<String, String> {
        BTreeMap::from([("db_open".to_string(), "(string)->u64".to_string())])
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
                e.contains("`free` does not start with `db_`"),
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
        // `db_open` mapped to the C runtime's `free` (IMPORT_OBJECT_NAME_EXPORTAS).
        let mut renamed = own.clone();
        renamed.iter_mut().find(|(s, _)| s == "db_open").unwrap().1 = "free".into();
        let e =
            check_import_library(&sample_import_library(dll, &renamed), dll, &names).unwrap_err();
        assert!(e.contains("`db_open` is imported as `free`"), "{e}");
        // Another DLL's.
        let e = check_import_library(&sample_import_library("ucrtbase.dll", &own), dll, &names)
            .unwrap_err();
        assert!(e.contains("is imported from `ucrtbase.dll`"), "{e}");
        // Missing and extra imports.
        let mut changed = own.clone();
        changed.retain(|(s, _)| s != "db_open");
        changed.push(("free".into(), "free".into()));
        let e =
            check_import_library(&sample_import_library(dll, &changed), dll, &names).unwrap_err();
        assert!(
            e.contains("`db_open` is exported by the DLL but not imported"),
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
