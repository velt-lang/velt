//! `c-headers.json` (docs/internals/design/c-ffi.md "Checking declarations against C
//! headers"): what a package's author checked. For each target (as zig names it,
//! `x86_64-linux-gnu`) and each C library, a digest of each `declare c` function that passed the
//! check against its headers there, and of each of the package's own headers the check read. It
//! is generated (`velt native headers`, and `velt publish` before it packs), committed for git
//! dependencies, and shipped with the package. A build that fetched the package compares its
//! declarations and the headers it has with it, so users never run clang.

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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_record_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        assert_eq!(CHeaders::read(dir).unwrap(), None);
        let mut r = CHeaders::default();
        let library = LibraryRecord {
            declarations: BTreeSet::from(["ab12".to_string()]),
            headers: BTreeMap::from([("native/sqlite3.h".to_string(), "cd34".to_string())]),
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
}
