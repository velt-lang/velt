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
//! deliberate.
//!
//! A build looks only at the tree's directories ([`TreeCheck::Directories`]): their times change
//! when a file is added, removed or renamed, and checking them costs a few `stat` calls where
//! every file's costs one per file (+39% on a no-op build with a Vue-sized dependency, against a
//! ±3% budget). An edit in place is then caught by the next `velt install`, `update` or `add`,
//! which look at every file. Stamps live outside the trees (`<cache>/stamps/`), so they are never
//! part of what they describe.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::locations::Locations;

/// How much of a stamped tree is looked at to decide that it is unchanged.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TreeCheck {
    /// Every file's metadata: any edit is caught (`velt install`, `update`, `add`).
    #[default]
    Files,
    /// Only the directories': a file added, removed or renamed is caught, an edit in place
    /// only by the next [`TreeCheck::Files`]. What a build uses, so that a fetched dependency
    /// costs a no-op build a few `stat` calls rather than one per file.
    Directories,
}

/// What a stamp records.
struct Stamp {
    checksum: String,
    files: String,
    dirs: String,
    /// The tree's directories, relative (`.` for its root).
    dir_list: Vec<String>,
}

impl Stamp {
    fn read(path: &Path) -> Option<Stamp> {
        let text = std::fs::read_to_string(path).ok()?;
        let mut lines = text.lines();
        Some(Stamp {
            checksum: lines.next()?.to_string(),
            files: lines.next()?.to_string(),
            dirs: lines.next()?.to_string(),
            dir_list: lines.map(String::from).collect(),
        })
    }

    fn write(&self, path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let text = format!(
            "{}\n{}\n{}\n{}\n",
            self.checksum,
            self.files,
            self.dirs,
            self.dir_list.join("\n")
        );
        let _ = std::fs::write(path, text);
    }
}

/// The [`crate::git::tree_checksum`] of `dir`, from its stamp while the tree is unchanged, as
/// far as `depth` looks. With `expected` (the lock's), a different checksum is an error that
/// `mismatch` words. `check` (what else a tree must pass, such as its links staying inside) runs
/// whenever the tree is read in full: a stamped tree passed it when it was stamped, and a change
/// that could make it fail changes the fingerprint.
pub(crate) fn checksum(
    loc: &Locations,
    dir: &Path,
    (expected, depth): (Option<&str>, TreeCheck),
    mismatch: impl FnOnce(&str, &str) -> String,
    check: impl FnOnce() -> Result<(), String>,
) -> Result<String, String> {
    let path = stamp_path(loc, dir);
    let stamp = Stamp::read(&path);
    if let (TreeCheck::Directories, Some(stamp)) = (depth, &stamp) {
        if dirs_fingerprint(dir, &stamp.dir_list).as_deref() == Ok(stamp.dirs.as_str()) {
            return compare(stamp.checksum.clone(), expected, mismatch);
        }
    }
    let (files, dir_list) = fingerprint(dir)?;
    let actual = match stamp.filter(|s| s.files == files) {
        Some(s) => s.checksum,
        None => {
            check()?;
            crate::git::tree_checksum(dir)?
        }
    };
    let actual = compare(actual, expected, mismatch)?;
    // Only a checksum the lock agrees with (or that nothing locked yet) is stamped; a failed
    // write only costs the next build a full read.
    let dirs = dirs_fingerprint(dir, &dir_list)?;
    let stamp = Stamp {
        checksum: actual.clone(),
        files,
        dirs,
        dir_list,
    };
    stamp.write(&path);
    Ok(actual)
}

fn compare(
    actual: String,
    expected: Option<&str>,
    mismatch: impl FnOnce(&str, &str) -> String,
) -> Result<String, String> {
    match expected {
        Some(expected) if actual != expected => Err(mismatch(expected, &actual)),
        _ => Ok(actual),
    }
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
pub(crate) fn fingerprint(dir: &Path) -> Result<(String, Vec<String>), String> {
    let mut hasher = Sha256::new();
    let mut dirs = vec![".".to_string()];
    walk(dir, "", &mut hasher, &mut dirs)?;
    Ok((hex(hasher), dirs))
}

/// A hash of the times of `dir`'s directories `dirs` (relative): a directory's times change
/// when an entry is added, removed or renamed in it.
fn dirs_fingerprint(dir: &Path, dirs: &[String]) -> Result<String, String> {
    let mut hasher = Sha256::new();
    for rel in dirs {
        let path = dir.join(rel);
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
        if !meta.is_dir() {
            return Err(format!("`{}` is no longer a directory", path.display()));
        }
        hasher.update(rel.as_bytes());
        hasher.update([0]);
        hasher.update(times(&meta));
    }
    Ok(hex(hasher))
}

fn hex(hasher: Sha256) -> String {
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn walk(dir: &Path, rel: &str, hasher: &mut Sha256, dirs: &mut Vec<String>) -> Result<(), String> {
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
            let name = entry.file_name().to_string_lossy().into_owned();
            let child = match rel.is_empty() {
                true => name,
                false => format!("{rel}/{name}"),
            };
            dirs.push(child.clone());
            // A file moved between directories changes their times, so no level marker is needed.
            walk(&entry.path(), &child, hasher, dirs)?;
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
