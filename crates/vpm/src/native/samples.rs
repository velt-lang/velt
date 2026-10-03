//! Valid native-library files for tests that need bundles without cargo: a minimal x86-64 ELF
//! shared library or relocatable object ([`SampleElf`]) and a Windows import library
//! ([`sample_import_library`]). They can be read by [`super::exports`] but not loaded or linked.

use std::collections::BTreeMap;

use crate::native::{init_symbol, SIG_PREFIX};

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
    /// What [`super::exports::read`] sees in a library of `package` built with the SDK: the init function, each
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

/// A short import member (`IMPORT_OBJECT_HEADER` + names) importing `symbol` from `dll` as
/// `import` (`None`: by ordinal 1), as `link.exe` writes them for x64.
#[doc(hidden)]
pub fn sample_short_import(symbol: &str, dll: &str, import: Option<&str>) -> Vec<u8> {
    let mut names = format!("{symbol}\0{dll}\0").into_bytes();
    // Name type: IMPORT_OBJECT_ORDINAL (0), _NAME (1) or _EXPORTAS (4).
    let name_type: u16 = match import {
        None => 0,
        Some(i) if i == symbol => 1,
        Some(i) => {
            names.extend_from_slice(format!("{i}\0").as_bytes());
            4
        }
    };
    let mut data = vec![];
    data.extend_from_slice(&0u16.to_le_bytes()); // Sig1
    data.extend_from_slice(&0xffffu16.to_le_bytes()); // Sig2
    data.extend_from_slice(&0u16.to_le_bytes()); // Version
    data.extend_from_slice(&0x8664u16.to_le_bytes()); // Machine: AMD64
    data.extend_from_slice(&0u32.to_le_bytes()); // TimeDateStamp
    data.extend_from_slice(&(names.len() as u32).to_le_bytes()); // SizeOfData
    data.extend_from_slice(&u16::from(import.is_none()).to_le_bytes()); // OrdinalOrHint
    data.extend_from_slice(&(name_type << 2).to_le_bytes()); // Type: code (0)
    data.extend_from_slice(&names);
    data
}

/// Where a [`sample_archive`] symbol map entry points.
#[doc(hidden)]
#[derive(Clone, Copy, Debug)]
pub enum MapTarget {
    /// The header of member `n`.
    Member(usize),
    /// A raw file offset.
    Offset(u32),
}

/// An `ar` archive of `members` with a symbol map (a GNU/COFF first linker member) of `map`.
#[doc(hidden)]
pub fn sample_archive(members: &[Vec<u8>], map: &[(String, MapTarget)]) -> Vec<u8> {
    fn header(name: &str, size: usize) -> Vec<u8> {
        format!("{name:<16}{:<12}{:<6}{:<6}{:<8}{size:<10}`\n", 0, 0, 0, 644).into_bytes()
    }
    let padded = |n: usize| n + n % 2;
    let names_len: usize = map.iter().map(|(n, _)| n.len() + 1).sum();
    let map_len = 4 + 4 * map.len() + names_len;
    let first = 8 + if map.is_empty() {
        0
    } else {
        60 + padded(map_len)
    };
    let mut starts = vec![];
    let mut at = first;
    for m in members {
        starts.push(at as u32);
        at += 60 + padded(m.len());
    }
    let mut out = b"!<arch>\n".to_vec();
    if !map.is_empty() {
        out.extend_from_slice(&header("/", map_len));
        out.extend_from_slice(&(map.len() as u32).to_be_bytes());
        for (_, target) in map {
            let offset = match *target {
                MapTarget::Member(n) => starts[n],
                MapTarget::Offset(o) => o,
            };
            out.extend_from_slice(&offset.to_be_bytes());
        }
        for (name, _) in map {
            out.extend_from_slice(name.as_bytes());
            out.push(0);
        }
        if map_len % 2 == 1 {
            out.push(b'\n');
        }
    }
    for m in members {
        out.extend_from_slice(&header("import/", m.len()));
        out.extend_from_slice(m);
        if m.len() % 2 == 1 {
            out.push(b'\n');
        }
    }
    out
}

/// A Windows import library for `dll`: one short import member per `(symbol, import name)` of
/// `imports`, and a symbol map naming each symbol and its `__imp_` form.
#[doc(hidden)]
pub fn sample_import_library(dll: &str, imports: &[(String, String)]) -> Vec<u8> {
    let members: Vec<Vec<u8>> = imports
        .iter()
        .map(|(symbol, import)| sample_short_import(symbol, dll, Some(import)))
        .collect();
    let map: Vec<(String, MapTarget)> = imports
        .iter()
        .enumerate()
        .flat_map(|(n, (symbol, _))| {
            [
                (format!("__imp_{symbol}"), MapTarget::Member(n)),
                (symbol.clone(), MapTarget::Member(n)),
            ]
        })
        .collect();
    sample_archive(&members, &map)
}
