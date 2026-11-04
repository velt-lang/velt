//! `c-headers.json` (docs/internals/design/c-ffi.md "Checking declarations against C
//! headers"): what a package's author checked. For each target (as zig names it,
//! `x86_64-linux-gnu`) and each C library, a digest of the `declare c` functions that passed
//! the check against its headers there. It is generated (`velt native headers`, and `velt
//! publish` before it packs), committed for git dependencies, and shipped with the package. A
//! build that fetched the package compares its declarations with it, so users never run clang.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// The record's file name, at the package root.
pub const FILE: &str = "c-headers.json";

/// The contents of `c-headers.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CHeaders {
    /// zig target → C library → the digest of its declarations checked there.
    pub checked: BTreeMap<String, BTreeMap<String, String>>,
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

    /// The digest recorded for `library` on `target`.
    pub fn digest(&self, target: &str, library: &str) -> Option<&str> {
        self.checked.get(target)?.get(library).map(String::as_str)
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
        r.checked
            .entry("x86_64-linux-gnu".into())
            .or_default()
            .insert("sqlite3".into(), "ab12".into());
        r.write(dir).unwrap();
        let back = CHeaders::read(dir).unwrap().unwrap();
        assert_eq!(back.digest("x86_64-linux-gnu", "sqlite3"), Some("ab12"));
        assert_eq!(back.digest("x86_64-windows-gnu", "sqlite3"), None);
        std::fs::write(dir.join(FILE), "{").unwrap();
        assert!(CHeaders::read(dir).unwrap_err().contains(FILE));
    }
}
