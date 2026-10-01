//! The package archive exchanged with a remote registry: the package's files (see [`contents`])
//! in one byte stream,
//!
//! ```text
//! VELTPKG1\n
//! <relative path>\n<byte length>\n<bytes>      (repeated, paths sorted)
//! ```
//!
//! so its [`checksum`] equals [`contents::checksum`] of the directory it was packed from or is
//! unpacked into. Unpacking accepts only `velt.toml` and files under `src/` (no `..`, no
//! absolute paths), so an archive cannot write outside its destination.

use std::path::Path;

use sha2::{Digest, Sha256};

use crate::contents;
use crate::manifest::{MANIFEST_FILE, SRC_DIR};

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
    let mut out = MAGIC.to_vec();
    for rel in contents::list_files(root)? {
        let bytes = std::fs::read(root.join(&rel))
            .map_err(|e| format!("cannot read `{}`: {e}", root.join(&rel).display()))?;
        out.extend_from_slice(format!("{rel}\n{}\n", bytes.len()).as_bytes());
        out.extend_from_slice(&bytes);
    }
    Ok(out)
}

/// Parse and validate an archive.
pub fn entries(archive: &[u8]) -> Result<Vec<Entry>, String> {
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
        check_path(path)?;
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

fn check_path(path: &str) -> Result<(), String> {
    let allowed = path == MANIFEST_FILE || path.starts_with(&format!("{SRC_DIR}/"));
    let safe = !path.contains('\\')
        && path
            .split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != "..");
    if allowed && safe {
        Ok(())
    } else {
        Err(format!("archive contains a disallowed path `{path}`"))
    }
}

/// The content checksum of an archive (same as [`contents::checksum`] of the package).
pub fn checksum(archive: &[u8]) -> Result<String, String> {
    let mut entries = entries(archive)?;
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

/// Write the archive's files under `dest` (created).
pub fn unpack(archive: &[u8], dest: &Path) -> Result<(), String> {
    for e in entries(archive)? {
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
            pkg.join("velt.toml"),
            "[package]\nname = \"p\"\nversion = \"1.0.0\"\n",
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
        for path in ["../x", "/etc/passwd", "src/../../x", "target/x", "src//x"] {
            let bad = [MAGIC, format!("{path}\n1\nx").as_bytes()].concat();
            assert!(entries(&bad).unwrap_err().contains("disallowed"), "{path}");
        }
        let truncated = [MAGIC, b"velt.toml\n10\nabc"].concat();
        assert!(entries(&truncated).unwrap_err().contains("truncated"));
    }
}
