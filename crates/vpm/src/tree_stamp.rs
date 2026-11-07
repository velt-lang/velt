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
//! ±3% budget). An edit in place is then caught whenever the program is loaded again, by a build
//! that is no longer a no-op and by `velt check`, before anything is read
//! ([`crate::PackageGraph::verify_fetched`], every file), and by `velt install`,
//! `update` and `add`, which look at every file; `velt install --locked` reads every byte
//! ([`TreeCheck::Bytes`]). An edit to a file the program never reads leaves a no-op build a
//! no-op. Stamps live outside the trees (`<cache>/stamps/`), so they are never part of what they
//! describe.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::locations::Locations;

/// How much of a stamped tree is looked at to decide that it is unchanged.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TreeCheck {
    /// Every file's metadata: any edit is caught (`velt install`, `update`, `add`).
    #[default]
    Files,
    /// Every byte, whatever a stamp says (`velt install --locked`, the check CI can rely on:
    /// a stamp is a file anything running as the user could have written).
    Bytes,
    /// Only the directories': a file added, removed or renamed is caught, an edit in place
    /// only by the next [`TreeCheck::Files`]. What a build uses, so that a fetched dependency
    /// costs a no-op build a few `stat` calls rather than one per file.
    Directories,
}

/// How much of each fetched tree an install looks at: every byte for `velt install --locked`,
/// the check CI relies on (a stamp is a file anything running as the user could have written);
/// otherwise what `opts` asks (a build keeps its directories-only check, `--locked` or not).
pub(crate) fn for_install(opts: &crate::install::InstallOptions) -> TreeCheck {
    match (opts.locked, opts.tree_check) {
        (true, TreeCheck::Files) => TreeCheck::Bytes,
        (_, check) => check,
    }
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
    expected: (Option<&str>, TreeCheck),
    mismatch: impl FnOnce(&str, &str) -> String,
    check: impl FnOnce() -> Result<(), String>,
) -> Result<String, String> {
    checksum_in(&loc.cache, dir, expected, mismatch, check)
}

/// [`checksum`] with stamps under `cache` (`Locations::cache`).
fn checksum_in(
    cache: &Path,
    dir: &Path,
    (expected, depth): (Option<&str>, TreeCheck),
    mismatch: impl FnOnce(&str, &str) -> String,
    check: impl FnOnce() -> Result<(), String>,
) -> Result<String, String> {
    let path = stamp_path_in(cache, dir);
    let stamp = match depth {
        TreeCheck::Bytes => None,
        _ => Stamp::read(&path),
    };
    if let (TreeCheck::Directories, Some(stamp)) = (depth, &stamp) {
        if dirs_fingerprint(dir, &stamp.dir_list).as_deref() == Ok(stamp.dirs.as_str()) {
            return compare(stamp.checksum.clone(), expected, mismatch);
        }
    }
    // Both fingerprints before the tree is read: a change while it is read is then a change
    // since the stamp, not one the stamp says was checked.
    let (files, dir_list, newest) = fingerprint(dir)?;
    let dirs = dirs_fingerprint(dir, &dir_list)?;
    let actual = match stamp.filter(|s| s.files == files) {
        Some(s) => s.checksum,
        None => {
            check()?;
            crate::git::tree_checksum(dir)?
        }
    };
    let actual = compare(actual, expected, mismatch)?;
    // Only a checksum the lock agrees with (or that nothing locked yet) is stamped, and only
    // when no entry changed within the times' resolution of now (as git's "racily clean"
    // rule): an edit in that window would leave the times as they were stamped. A failed write
    // only costs the next build a full read.
    if !is_racy(newest) {
        let stamp = Stamp {
            checksum: actual.clone(),
            files,
            dirs,
            dir_list,
        };
        stamp.write(&path);
    }
    Ok(actual)
}

/// How close to now an entry's time may be for the tree to be stamped: FAT and HFS+ keep times
/// to 2 s.
const RACY: std::time::Duration = std::time::Duration::from_secs(3);

/// Whether `newest` (the latest time of any entry, in nanoseconds since the epoch) is within
/// [`RACY`] of now.
fn is_racy(newest: u128) -> bool {
    #[cfg(test)]
    if tests::NO_RACY_WINDOW.with(std::cell::Cell::get) {
        return false;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    #[cfg(test)]
    let now = now + tests::CLOCK_AHEAD.with(std::cell::Cell::get).as_nanos();
    newest + RACY.as_nanos() > now
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
#[cfg(test)]
fn stamp_path(loc: &Locations, dir: &Path) -> PathBuf {
    stamp_path_in(&loc.cache, dir)
}

fn stamp_path_in(cache: &Path, dir: &Path) -> PathBuf {
    let hash = Sha256::digest(dir.to_string_lossy().as_bytes());
    let name: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    cache.join("stamps").join(name)
}

/// A fetched tree (a git checkout, an archive's package directory) with the checksum its lock
/// holds, checked file by file whenever the program is loaded again ([`Fetched::verify`]): that
/// reads files anyway, and an edit in place must not reach it. A no-op build looks only at
/// directories ([`TreeCheck::Directories`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fetched {
    pub dir: PathBuf,
    pub checksum: String,
    /// What to delete to fetch the tree again: the git checkout, or the archive's raw tree
    /// (`dir` is a directory inside it, which the archive's record would still vouch for).
    pub fetched_at: PathBuf,
    /// `Locations::cache`, where the stamps are.
    pub cache: PathBuf,
}

/// The fetched trees of `packages`: each git checkout (its checksum covers the whole checkout,
/// whichever package of it is used) and each archive's package directory.
pub(crate) fn fetched<'r>(
    loc: &Locations,
    packages: impl Iterator<Item = &'r crate::resolve::ResolvedPackage>,
) -> Vec<Fetched> {
    use crate::resolve::Source;
    let mut out: Vec<Fetched> = packages
        .filter_map(|p| match &p.source {
            Source::Git {
                spec,
                commit,
                checksum,
                ..
            } => {
                let dir = crate::git::checkout_dir(loc, &spec.url, commit);
                Some((dir.clone(), dir, checksum))
            }
            Source::Url {
                spec,
                dir,
                checksum,
            } => Some((
                dir.clone(),
                crate::url_source::raw_dir(loc, &spec.sha256),
                checksum,
            )),
            Source::Registry { .. } | Source::Path { .. } => None,
        })
        .map(|(dir, fetched_at, checksum)| Fetched {
            dir,
            checksum: checksum.clone(),
            fetched_at,
            cache: loc.cache.clone(),
        })
        .collect();
    out.sort_by(|a, b| a.dir.cmp(&b.dir));
    out.dedup();
    out
}

impl Fetched {
    /// `Err` unless the tree still has its locked checksum, every file's metadata compared
    /// with the stamp (and the bytes read when it differs).
    pub fn verify(&self) -> Result<(), String> {
        let mismatch = |expected: &str, actual: &str| {
            format!(
                "`{}` changed since it was fetched: velt.lock.json has {expected}, it hashes to {actual} (delete `{}` to fetch it again)",
                self.dir.display(),
                self.fetched_at.display()
            )
        };
        let links = || crate::git::check_links(&self.dir);
        let expected = (Some(self.checksum.as_str()), TreeCheck::Files);
        checksum_in(&self.cache, &self.dir, expected, mismatch, links).map(drop)
    }
}

/// A hash of every entry's name, type, size and times, in the order the directories list them:
/// that order is stable while a directory is unchanged, and if it does change, the fingerprint
/// differs and the tree is read in full once, which is only slower. Bytes are hashed as they
/// are, with no text formatting or sorting: this runs on every build.
pub(crate) fn fingerprint(dir: &Path) -> Result<(String, Vec<String>, u128), String> {
    let mut hasher = Sha256::new();
    let mut dirs = vec![".".to_string()];
    let mut newest = 0;
    walk(dir, "", &mut hasher, (&mut dirs, &mut newest))?;
    let root = std::fs::symlink_metadata(dir)
        .map_err(|e| format!("cannot read `{}`: {e}", dir.display()))?;
    newest = newest.max(latest(&root));
    Ok((hex(hasher), dirs, newest))
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

fn walk(
    dir: &Path,
    rel: &str,
    hasher: &mut Sha256,
    (dirs, newest): (&mut Vec<String>, &mut u128),
) -> Result<(), String> {
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
        *newest = (*newest).max(latest(&meta));
        if kind == b'd' {
            let name = entry.file_name().to_string_lossy().into_owned();
            let child = match rel.is_empty() {
                true => name,
                false => format!("{rel}/{name}"),
            };
            dirs.push(child.clone());
            // A file moved between directories changes their times, so no level marker is needed.
            walk(&entry.path(), &child, hasher, (dirs, newest))?;
        }
    }
    Ok(())
}

/// The later of the modification and (on Unix) inode change times, in nanoseconds.
fn latest(meta: &std::fs::Metadata) -> u128 {
    let t = times(meta);
    let modified = u128::from_le_bytes(t[..16].try_into().unwrap_or_default());
    let changed = i128::from_le_bytes(t[16..].try_into().unwrap_or_default()).max(0) as u128;
    modified.max(changed)
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
