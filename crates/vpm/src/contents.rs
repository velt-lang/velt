//! The contents of a package as distributed: `velt.toml` plus everything under `src/`.
//! Listing, copying and content-hashing all use the same file set so a checksum computed on a
//! source tree matches the one computed on its registry or cache copy.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::manifest::{MANIFEST_FILE, SRC_DIR};

/// Package files relative to `root`, `/`-separated, sorted (deterministic hashing).
pub fn list_files(root: &Path) -> Result<Vec<String>, String> {
    let manifest = root.join(MANIFEST_FILE);
    if !manifest.is_file() {
        return Err(format!("`{}` has no {MANIFEST_FILE}", root.display()));
    }
    let mut files = vec![MANIFEST_FILE.to_string()];
    let src = root.join(SRC_DIR);
    if src.is_dir() {
        collect(&src, SRC_DIR, &mut files)?;
    }
    files.sort();
    Ok(files)
}

fn collect(dir: &Path, rel: &str, out: &mut Vec<String>) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read `{}`: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot read `{}`: {e}", dir.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let child = format!("{rel}/{name}");
        if path.is_dir() {
            collect(&path, &child, out)?;
        } else {
            out.push(child);
        }
    }
    Ok(())
}

/// `sha256:<hex>` over the package files: for each file, its relative path, a NUL, its length
/// and its bytes. Line endings are hashed as stored.
pub fn checksum(root: &Path) -> Result<String, String> {
    let mut hasher = Sha256::new();
    for rel in list_files(root)? {
        let bytes = read(&root.join(&rel))?;
        hasher.update(rel.as_bytes());
        hasher.update([0]);
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    let hex: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(format!("sha256:{hex}"))
}

/// Copy the package files of `from` into `to` (created; must not contain other files that matter).
pub fn copy_package(from: &Path, to: &Path) -> Result<(), String> {
    for rel in list_files(from)? {
        let dest: PathBuf = to.join(&rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create `{}`: {e}", parent.display()))?;
        }
        std::fs::copy(from.join(&rel), &dest)
            .map_err(|e| format!("cannot copy to `{}`: {e}", dest.display()))?;
    }
    Ok(())
}

fn read(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("cannot read `{}`: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    #[test]
    fn lists_hashes_and_copies() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        write(&a, "velt.toml", "[package]");
        write(&a, "src/lib.vlt", "export function f() {}");
        write(&a, "src/sub/x.vlt", "");
        write(&a, "target/junk", "ignored");
        assert_eq!(
            list_files(&a).unwrap(),
            ["src/lib.vlt", "src/sub/x.vlt", "velt.toml"]
        );

        let b = tmp.path().join("b");
        copy_package(&a, &b).unwrap();
        assert!(!b.join("target").exists());
        let sum = checksum(&a).unwrap();
        assert!(sum.starts_with("sha256:") && sum.len() == 7 + 64);
        assert_eq!(checksum(&b).unwrap(), sum);

        write(&b, "src/lib.vlt", "export function g() {}");
        assert_ne!(checksum(&b).unwrap(), sum);
    }

    #[test]
    fn missing_manifest_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(list_files(tmp.path()).unwrap_err().contains("velt.toml"));
    }
}
