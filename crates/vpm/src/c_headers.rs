//! `c-headers.json` (docs/internals/design/c-ffi.md "Checking declarations against C
//! headers"): what a package's author checked. For each target (as zig names it,
//! `x86_64-linux-gnu`) and each C library, a digest of each `declare c` function that passed the
//! check against its headers there, and of each of the package's own headers the check read. It
//! is generated (`velt native headers`, and `velt publish` before it packs), committed for git
//! dependencies, and shipped with the package. A build that fetched the package compares its
//! declarations and the headers it has with it, so users never run clang. It holds the value
//! of each `declare c const` per target too, which such a build compiles in, with the package's
//! own headers those were read from, which it compares as it does a library's.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// The record's file name, at the package root.
pub const FILE: &str = "c-headers.json";

/// The contents of `c-headers.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CHeaders {
    /// zig target → C library → what was checked there.
    pub checked: BTreeMap<String, BTreeMap<String, LibraryRecord>>,
    /// zig target → the C constants read there.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub constants: BTreeMap<String, ConstantsRecord>,
}

/// The C constants of a package on one target.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConstantsRecord {
    /// The package's own headers the values were read from, with what they include, as
    /// [`LibraryRecord::headers`].
    pub headers: BTreeMap<String, String>,
    /// C constant → what its author declared and what the headers gave.
    pub values: BTreeMap<String, CConstant>,
}

/// What was checked of one C library on one target.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryRecord {
    /// A digest per declaration: a program loads some of a package's modules, so it compiles
    /// some of these, and each must be here.
    pub declarations: BTreeSet<String>,
    /// The package's own headers the check read, with what they include: `/`-separated path in
    /// the package → digest.
    pub headers: BTreeMap<String, String>,
    /// A digest of the library's `defines`, which change what its headers declare
    /// ([`defines_digest`]); empty, and left out, for a library without any.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub defines: String,
}

/// The digest of a library's `defines` as a record holds it: empty when it has none, so the record
/// of a library without them is the same as before they were recorded.
pub fn defines_digest(defines: &[(String, String)]) -> String {
    use sha2::{Digest, Sha256};
    if defines.is_empty() {
        return String::new();
    }
    let mut h = Sha256::new();
    for (name, value) in defines {
        for part in [name, value] {
            h.update((part.len() as u64).to_le_bytes());
            h.update(part);
        }
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// A `declare c const` as its author's check saw it on one target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CConstant {
    /// The Velt type declared (`i32`, `Ptr<void> | null`).
    pub declared: String,
    /// Its C type, as [`Probe::c_type`]: what the value is checked against again.
    pub c_type: String,
    pub value: CConstantValue,
}

/// The value of a C constant. Numbers are text: JSON's aren't exact over 2^53.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CConstantValue {
    /// In decimal.
    Int(String),
    /// `f64::to_bits`, in hexadecimal.
    Float(String),
    Bool(bool),
    /// In hexadecimal.
    Pointer(String),
}

/// What clang says about a name in a package's headers, as the compiler remembers it while the
/// headers don't change (not part of the record).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Probe {
    /// Its C type, typedefs resolved (`unsigned int`, `void (*)(void *)`).
    pub c_type: String,
    /// `None`: clang can't evaluate it, or it is of a type no constant crosses as.
    pub value: Option<CConstantValue>,
}

impl CHeaders {
    /// The record of the package at `root`; `None` when it has none.
    pub fn read(root: &Path) -> Result<Option<CHeaders>, String> {
        let path = root.join(FILE);
        if !path.is_file() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
        crate::json_file::parse(&text, &path.display().to_string()).map(Some)
    }

    /// Write it to the package at `root`.
    pub fn write(&self, root: &Path) -> Result<(), String> {
        crate::json_file::write(&root.join(FILE), self)
    }

    /// What is recorded for `library` on `target`.
    pub fn library(&self, target: &str, library: &str) -> Option<&LibraryRecord> {
        self.checked.get(target)?.get(library)
    }

    /// The record of constant `name` on `target`.
    pub fn constant(&self, target: &str, name: &str) -> Option<&CConstant> {
        self.constants.get(target)?.values.get(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A record made before C constants reads as one without any, and one without any writes
    /// nothing for them, so a package's file doesn't change.
    #[test]
    fn constants_are_optional_in_the_record() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(FILE), "{\"checked\":{}}").unwrap();
        let old = CHeaders::read(tmp.path()).unwrap().unwrap();
        assert!(old.constants.is_empty());
        old.write(tmp.path()).unwrap();
        let text = std::fs::read_to_string(tmp.path().join(FILE)).unwrap();
        assert!(!text.contains("constants"), "{text}");
        let mut record = CHeaders::default();
        let constant = CConstant {
            declared: "Ptr<void>".into(),
            c_type: "void (*)(void *)".into(),
            value: CConstantValue::Pointer("ffffffffffffffff".into()),
        };
        record
            .constants
            .entry("x86_64-linux-gnu".into())
            .or_default()
            .values
            .insert("T".into(), constant.clone());
        record.write(tmp.path()).unwrap();
        let back = CHeaders::read(tmp.path()).unwrap().unwrap();
        assert_eq!(back, record);
        assert_eq!(back.constant("x86_64-linux-gnu", "T"), Some(&constant));
        assert_eq!(back.constant("x86_64-linux-gnu", "U"), None);
        assert_eq!(back.constant("aarch64-macos", "T"), None);
    }

    #[test]
    fn the_record_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        assert_eq!(CHeaders::read(dir).unwrap(), None);
        let mut r = CHeaders::default();
        let library = LibraryRecord {
            declarations: BTreeSet::from(["ab12".to_string()]),
            headers: BTreeMap::from([("native/sqlite3.h".to_string(), "cd34".to_string())]),
            defines: defines_digest(&[("X".into(), "1".into())]),
        };
        r.checked
            .entry("x86_64-linux-gnu".into())
            .or_default()
            .insert("sqlite3".into(), library.clone());
        r.write(dir).unwrap();
        let back = CHeaders::read(dir).unwrap().unwrap();
        assert_eq!(back.library("x86_64-linux-gnu", "sqlite3"), Some(&library));
        assert_eq!(back.library("x86_64-windows-gnu", "sqlite3"), None);
        std::fs::write(dir.join(FILE), "{").unwrap();
        assert!(CHeaders::read(dir).unwrap_err().contains(FILE));
    }

    /// A library's `defines` digest differs with any name or value; none at all is empty.
    #[test]
    fn defines_have_a_digest() {
        let d = |pairs: &[(&str, &str)]| {
            let pairs: Vec<(String, String)> = (pairs.iter())
                .map(|(n, v)| (n.to_string(), v.to_string()))
                .collect();
            defines_digest(&pairs)
        };
        assert_eq!(d(&[]), "");
        let answer = d(&[("ANSWER", "40")]);
        assert_eq!(answer.len(), 64);
        assert_ne!(answer, d(&[("ANSWER", "41")]));
        assert_ne!(answer, d(&[("ANSWER", "40"), ("WIDE", "1")]));
        assert_ne!(d(&[("AB", "C")]), d(&[("A", "BC")]));
    }
}
