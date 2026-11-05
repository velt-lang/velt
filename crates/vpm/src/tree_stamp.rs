//! The tree checksum of a fetched tree (a git checkout, an extracted archive), without reading
//! the whole tree on every build. A fetched source is checked against the lock on every use
//! (#2367 section 1), and [`crate::git::tree_checksum`] reads every byte: a Vue-sized dependency
//! made a no-op `velt build` take nine times its instructions.
//!
//! A stamp records the checksum with a fingerprint of the tree's metadata: each file's path,
//! type, size and modification time, and on Unix its inode change time, which changes on every
//! write and can't be set back by the file's owner. While the fingerprint matches, the stamp's
//! checksum stands for the tree; any edit, even one that restores the size and the modification
//! time, changes the fingerprint on Unix, and the tree is hashed again. On Windows, which has no
//! inode change time, an edit that restores both the length and the modification time would go
//! unseen until the stamp is removed: the cache is the user's own, and such an edit is
//! deliberate. Stamps live outside the trees
//! (`<cache>/stamps/`), so they are never part of what they describe.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::locations::Locations;

/// The [`crate::git::tree_checksum`] of `dir`, from its stamp while the tree is unchanged.
/// With `expected` (the lock's), a different checksum is an error that `mismatch` words.
pub(crate) fn checksum(
    loc: &Locations,
    dir: &Path,
    expected: Option<&str>,
    mismatch: impl FnOnce(&str, &str) -> String,
) -> Result<String, String> {
    checksum_checked(loc, dir, expected, mismatch, || Ok(()))
}

/// [`checksum`], running `check` (what else a tree must pass, such as its links staying inside)
/// whenever the tree is read in full: a stamped tree passed it when it was stamped, and an edit
/// that could make it fail changes the fingerprint.
pub(crate) fn checksum_checked(
    loc: &Locations,
    dir: &Path,
    expected: Option<&str>,
    mismatch: impl FnOnce(&str, &str) -> String,
    check: impl FnOnce() -> Result<(), String>,
) -> Result<String, String> {
    let stamp = stamp_path(loc, dir);
    let print = fingerprint(dir)?;
    let stamped = std::fs::read_to_string(&stamp).ok().and_then(|text| {
        let (sum, fp) = text.split_once('\n')?;
        (fp.trim_end() == print).then(|| sum.to_string())
    });
    let actual = match stamped {
        Some(sum) => sum,
        None => {
            check()?;
            crate::git::tree_checksum(dir)?
        }
    };
    if let Some(expected) = expected {
        if actual != expected {
            return Err(mismatch(expected, &actual));
        }
    }
    // Only a checksum the lock agrees with (or that nothing locked yet) is stamped; a failed
    // write only costs the next build a full read.
    if let Some(parent) = stamp.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&stamp, format!("{actual}\n{print}\n"));
    Ok(actual)
}

/// `<cache>/stamps/<sha256 of the tree's path>`.
fn stamp_path(loc: &Locations, dir: &Path) -> PathBuf {
    let hash = Sha256::digest(dir.to_string_lossy().as_bytes());
    let name: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    loc.cache.join("stamps").join(name)
}

/// A hash of every entry's name, type, size and times, in the order the directories list them:
/// that order is stable while a directory is unchanged, and if it does change, the fingerprint
/// differs and the tree is read in full once, which is only slower. Bytes are hashed as they
/// are, with no text formatting or sorting: this runs on every build.
pub(crate) fn fingerprint(dir: &Path) -> Result<String, String> {
    let mut hasher = Sha256::new();
    walk(dir, &mut hasher)?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn walk(dir: &Path, hasher: &mut Sha256) -> Result<(), String> {
    let read =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read `{}`: {e}", dir.display()))?;
    for entry in read {
        let entry = entry.map_err(|e| format!("cannot read `{}`: {e}", dir.display()))?;
        // `DirEntry::metadata` doesn't follow a link.
        let meta = entry
            .metadata()
            .map_err(|e| format!("cannot read `{}`: {e}", entry.path().display()))?;
        let kind: u8 = match () {
            _ if meta.file_type().is_symlink() => b'l',
            _ if meta.is_dir() => b'd',
            _ => b'f',
        };
        hasher.update(entry.file_name().as_encoded_bytes());
        hasher.update([0, kind]);
        // Redundant on Unix, where every write changes the inode change time; where there is
        // none (Windows), an edit that restores the modification time still changes the size
        // unless it keeps the length too.
        hasher.update(meta.len().to_le_bytes());
        hasher.update(times(&meta));
        if kind == b'd' {
            // A file moved between directories changes their times, so no level marker is needed.
            walk(&entry.path(), hasher)?;
        }
    }
    Ok(())
}

/// The modification time, and on Unix the inode change time, in nanoseconds.
fn times(meta: &std::fs::Metadata) -> [u8; 32] {
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    #[cfg(unix)]
    let changed = {
        use std::os::unix::fs::MetadataExt;
        (meta.ctime() as i128) * 1_000_000_000 + meta.ctime_nsec() as i128
    };
    #[cfg(not(unix))]
    let changed = 0i128;
    let mut out = [0; 32];
    out[..16].copy_from_slice(&modified.to_le_bytes());
    out[16..].copy_from_slice(&changed.to_le_bytes());
    out
}

#[cfg(test)]
mod tests;
