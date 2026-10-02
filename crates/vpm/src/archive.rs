//! The package archive exchanged with a remote registry: the package's files (see [`contents`])
//! in one byte stream,
//!
//! ```text
//! VELTPKG1\n
//! <relative path>\n<byte length>\n<bytes>      (repeated, paths sorted)
//! ```
//!
//! so its [`checksum`] equals [`contents::checksum`] of the directory it was packed from or is
//! unpacked into. Unpacking accepts only `package.vlt`, files under `src/` and under the native
//! crate directory its `package.vlt` names (no `..`, no absolute paths), so an archive cannot write
//! outside its destination. Native bundles ([`crate::native`]) use the same format with their own
//! path rule ([`entries_with`]).

use std::path::Path;

use sha2::{Digest, Sha256};

use crate::contents;
use crate::manifest::{legacy, LEGACY_MANIFEST_FILE, MANIFEST_FILE, SRC_DIR};

const MAGIC: &[u8] = b"VELTPKG1\n";

/// One file of an archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// `/`-separated path relative to the package root.
    pub path: String,
    /// Contents.
    pub bytes: Vec<u8>,
}

/// Pack the package rooted at `root`.
pub fn pack(root: &Path) -> Result<Vec<u8>, String> {
    pack_files(root, &contents::list_files(root)?)
}

/// Pack `files` (relative to `root`, `/`-separated, sorted).
pub fn pack_files(root: &Path, files: &[String]) -> Result<Vec<u8>, String> {
    let mut out = MAGIC.to_vec();
    for rel in files {
        let bytes = std::fs::read(root.join(rel))
            .map_err(|e| format!("cannot read `{}`: {e}", root.join(rel).display()))?;
        out.extend_from_slice(format!("{rel}\n{}\n", bytes.len()).as_bytes());
        out.extend_from_slice(&bytes);
    }
    Ok(out)
}

/// Parse and validate a package archive.
pub fn entries(archive: &[u8]) -> Result<Vec<Entry>, String> {
    let entries = entries_with(archive, |_| true)?;
    let native = entries
        .iter()
        .find(|e| e.path == MANIFEST_FILE)
        .and_then(|e| std::str::from_utf8(&e.bytes).ok())
        .and_then(contents::native_dir_of);
    for e in &entries {
        let allowed = e.path == MANIFEST_FILE
            || e.path.starts_with(&format!("{SRC_DIR}/"))
            || native
                .as_ref()
                .is_some_and(|dir| e.path.starts_with(&format!("{dir}/")));
        if e.path == LEGACY_MANIFEST_FILE {
            return Err(format!(
                "cannot use this package archive: {}",
                legacy::REPUBLISH
            ));
        }
        if !allowed {
            return Err(format!("archive contains a disallowed path `{}`", e.path));
        }
    }
    Ok(entries)
}

/// Parse an archive whose paths are safe relative paths that `allowed` accepts.
pub fn entries_with(archive: &[u8], allowed: impl Fn(&str) -> bool) -> Result<Vec<Entry>, String> {
    let mut rest = archive
        .strip_prefix(MAGIC)
        .ok_or("not a Velt package archive")?;
    let mut entries = vec![];
    while !rest.is_empty() {
        let (path, after) = line(rest)?;
        let (len, after) = line(after)?;
        let len: usize = len.parse().map_err(|_| "corrupt archive: bad length")?;
        if after.len() < len {
            return Err("corrupt archive: truncated file".into());
        }
        if !safe_path(path) || !allowed(path) {
            return Err(format!("archive contains a disallowed path `{path}`"));
        }
        entries.push(Entry {
            path: path.to_string(),
            bytes: after[..len].to_vec(),
        });
        rest = &after[len..];
    }
    Ok(entries)
}

fn line(bytes: &[u8]) -> Result<(&str, &[u8]), String> {
    let end = bytes
        .iter()
        .position(|&b| b == b'\n')
        .ok_or("corrupt archive: missing newline")?;
    let text = std::str::from_utf8(&bytes[..end]).map_err(|_| "corrupt archive: bad path")?;
    Ok((text, &bytes[end + 1..]))
}

fn safe_path(path: &str) -> bool {
    !path.contains('\\')
        && !path.contains(':')
        && path
            .split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
}

/// The content checksum of a package archive (same as [`contents::checksum`] of the package).
pub fn checksum(archive: &[u8]) -> Result<String, String> {
    checksum_of(entries(archive)?)
}

/// The content checksum of parsed entries (the hash [`contents::checksum`] computes).
pub fn checksum_of(mut entries: Vec<Entry>) -> Result<String, String> {
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let mut hasher = Sha256::new();
    for e in &entries {
        hasher.update(e.path.as_bytes());
        hasher.update([0]);
        hasher.update((e.bytes.len() as u64).to_le_bytes());
        hasher.update(&e.bytes);
    }
    let hex: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(format!("sha256:{hex}"))
}

/// Write the package archive's files under `dest` (created).
pub fn unpack(archive: &[u8], dest: &Path) -> Result<(), String> {
    write_entries(entries(archive)?, dest)
}

/// Write validated entries under `dest` (created).
pub fn write_entries(entries: Vec<Entry>, dest: &Path) -> Result<(), String> {
    for e in entries {
        let path = dest.join(&e.path);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|err| format!("cannot create `{}`: {err}", dir.display()))?;
        }
        std::fs::write(&path, &e.bytes)
            .map_err(|err| format!("cannot write `{}`: {err}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_with_matching_checksum() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path().join("p");
        std::fs::create_dir_all(pkg.join("src/sub")).unwrap();
        std::fs::write(
            pkg.join("package.vlt"),
            "export const pkg: Package = { name: \"p\", version: \"1.0.0\" };",
        )
        .unwrap();
        std::fs::write(pkg.join("src/lib.vlt"), "export function f() {}\n").unwrap();
        std::fs::write(pkg.join("src/sub/x.vlt"), "").unwrap();
        let archive = pack(&pkg).unwrap();
        assert_eq!(
            checksum(&archive).unwrap(),
            contents::checksum(&pkg).unwrap()
        );
        let out = tmp.path().join("out");
        unpack(&archive, &out).unwrap();
        assert_eq!(
            contents::checksum(&out).unwrap(),
            contents::checksum(&pkg).unwrap()
        );
    }

    #[test]
    fn rejects_bad_archives() {
        assert!(entries(b"nope").is_err());
        for path in [
            "../x",
            "/etc/passwd",
            "src/../../x",
            "target/x",
            "src//x",
            "native/x",
        ] {
            let bad = [MAGIC, format!("{path}\n1\nx").as_bytes()].concat();
            assert!(entries(&bad).unwrap_err().contains("disallowed"), "{path}");
        }
        // A native crate directory is allowed only when the manifest names it.
        let manifest =
            "export const pkg: Package = { name: \"p\", version: \"1.0.0\", native: {} };";
        let with_native = [
            MAGIC,
            b"native/Cargo.toml\n0\n",
            format!("package.vlt\n{}\n{manifest}", manifest.len()).as_bytes(),
        ]
        .concat();
        assert_eq!(entries(&with_native).unwrap().len(), 2);
        let legacy = [MAGIC, b"velt.toml\n0\n"].concat();
        assert!(entries(&legacy)
            .unwrap_err()
            .contains("must publish a new version"));
        let truncated = [MAGIC, b"package.vlt\n10\nabc"].concat();
        assert!(entries(&truncated).unwrap_err().contains("truncated"));
    }
}
