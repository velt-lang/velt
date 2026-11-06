//! The one extractor for archives fetched from a URL (`.tar.gz` and `.zip`, chosen by their first
//! bytes, never by name): docs/internals/design/c-ffi.md "Where libraries come from" and #2367
//! security rule 3. Archives come from anywhere, so the extractor is the riskiest code in the
//! package manager, and it refuses rather than guesses:
//!
//! - paths that are absolute, hold `..`, a backslash, NUL or another control character, or a
//!   character or name Windows can't hold (`:`, `CON`, a trailing dot or space): one archive gives
//!   the same tree on every OS ([`names`]);
//! - two entries whose paths differ only by case or Unicode normalisation, an entry written twice,
//!   a path that is both a file and a directory, and a path under a link;
//! - links (symbolic and hard) resolving outside the tree, through the archive's other links;
//! - devices, FIFOs, sparse files, encrypted or zip64 entries, and a zip whose local header
//!   disagrees with its central directory or whose entries overlap;
//! - more than [`Limits`] allow: unpacked size, entry count and compression ratio, enforced while
//!   decompressing.
//!
//! The tree is read whole ([`read`]) before anything is written, then written into a staging
//! directory beside the destination and renamed into place ([`extract`]), so a refused archive
//! leaves nothing behind. Modes and timestamps are ignored. Symbolic links are written as links,
//! or, on Windows, as files holding their target text (as git checks out with
//! `core.symlinks=false`), so [`crate::git::tree_checksum`] gives one archive one checksum on every
//! OS.

mod names;
mod tar;
mod tree;
mod zip;

#[cfg(test)]
mod tests;

use std::io::Read;
use std::path::{Path, PathBuf};

pub use tree::{Node, Tree};

/// What an archive may unpack to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The bytes of every file together.
    pub total: u64,
    /// Entries of any kind (directories and links too).
    pub entries: usize,
    /// Unpacked bytes per archive byte, past the first MiB: deflate reaches about 1000, real
    /// source archives 5-20.
    pub ratio: u64,
    /// The bytes of every path, and of every directory above one, together: deep paths with new
    /// prefixes cost memory and time no file size counts.
    pub names: usize,
}

impl Limits {
    /// 512 MiB, 100,000 entries, a ratio of 200, 64 MiB of names.
    pub const DEFAULT: Limits = Limits {
        total: 512 << 20,
        entries: 100_000,
        ratio: 200,
        names: 64 << 20,
    };

    /// `Err` when `unpacked` bytes from an archive of `packed` bytes are more than allowed.
    fn check_size(&self, unpacked: u64, packed: usize) -> Result<(), String> {
        if unpacked > self.total {
            return Err(format!("it unpacks to more than {} MiB", self.total >> 20));
        }
        let allowed = (packed as u64)
            .saturating_mul(self.ratio)
            .saturating_add(1 << 20);
        if unpacked > allowed {
            return Err(format!(
                "it unpacks to more than {} times its size (a damaged or hostile archive)",
                self.ratio
            ));
        }
        Ok(())
    }
}

/// How symbolic links are written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Links {
    /// As symbolic links.
    Symbolic,
    /// As files holding the target's text.
    Files,
}

impl Links {
    /// What git does here: links on Unix, files on Windows.
    pub(crate) const NATIVE: Links = if cfg!(windows) {
        Links::Files
    } else {
        Links::Symbolic
    };
}

/// The format of `archive`, from its first bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    TarGz,
    Zip,
}

fn format(archive: &[u8]) -> Result<Format, String> {
    match archive {
        [0x1f, 0x8b, ..] => Ok(Format::TarGz),
        [b'P', b'K', 3, 4, ..] | [b'P', b'K', 5, 6, ..] => Ok(Format::Zip),
        _ => Err("it is neither a .tar.gz nor a .zip archive (by its first bytes)".to_string()),
    }
}

/// The tree in `archive`, checked, within `limits`.
pub fn read(archive: &[u8], limits: Limits) -> Result<Tree, String> {
    let mut tree = Tree::new(limits);
    match format(archive)? {
        Format::TarGz => tar::read(archive, limits, &mut tree)?,
        Format::Zip => zip::read(archive, limits, &mut tree)?,
    }
    tree.finish()?;
    Ok(tree)
}

/// Extract `archive` into `dest`, which must not exist yet, within [`Limits::DEFAULT`].
pub fn extract(archive: &[u8], dest: &Path) -> Result<(), String> {
    extract_within(archive, dest, Limits::DEFAULT, Links::NATIVE)
}

/// [`extract`] with other limits, and links written as `links` says.
pub(crate) fn extract_within(
    archive: &[u8],
    dest: &Path,
    limits: Limits,
    links: Links,
) -> Result<(), String> {
    let tree = read(archive, limits)?;
    write(&tree, dest, links)
}

/// Write `tree` into a staging directory beside `dest`, then rename it to `dest`.
pub(crate) fn write(tree: &Tree, dest: &Path, links: Links) -> Result<(), String> {
    write_then(tree, (dest, Place::New), links, |_| Ok(()))
}

/// Whether [`write_then`] may take the place of a `dest` already there.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Place {
    /// `dest` must not exist.
    New,
    /// A `dest` already there is moved aside in one rename, the new tree renamed in, and the old
    /// one removed: a reader sees the old tree or the new one, never none.
    Replace,
}

/// [`write`], running `ready` on the staging directory once the tree is written, before it is
/// renamed to `dest`: what is recorded about the tree is there before anyone can see it.
pub(crate) fn write_then(
    tree: &Tree,
    (dest, place): (&Path, Place),
    links: Links,
    ready: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<(), String> {
    let exists = dest.symlink_metadata().is_ok();
    if exists && place == Place::New {
        return Err(format!("`{}` already exists", dest.display()));
    }
    let staging = staging(dest)?;
    let moved = |from: &Path, to: &Path| {
        std::fs::rename(from, to).map_err(|e| {
            format!(
                "cannot move `{}` to `{}`: {e}",
                from.display(),
                to.display()
            )
        })
    };
    let result = write_into(tree, &staging, links)
        .and_then(|()| ready(&staging))
        .and_then(|()| match exists {
            false => moved(&staging, dest),
            true => {
                let mut aside = staging.clone().into_os_string();
                aside.push(".old");
                let aside = PathBuf::from(aside);
                moved(dest, &aside)?;
                let result = moved(&staging, dest);
                match &result {
                    Ok(()) => drop(std::fs::remove_dir_all(&aside)),
                    // Put back what was there.
                    Err(_) => drop(std::fs::rename(&aside, dest)),
                }
                result
            }
        });
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

/// A new, empty directory beside `dest`.
fn staging(dest: &Path) -> Result<PathBuf, String> {
    let parent = dest
        .parent()
        .ok_or_else(|| format!("`{}` has no parent directory", dest.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("cannot create `{}`: {e}", parent.display()))?;
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Unique within the process (a counter) and between processes (the pid); one left behind by
    // a process that died with the same pid is skipped.
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    for _ in 0..1 << 20 {
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let staging = parent.join(format!(".{name}.partial-{}-{n}", std::process::id()));
        match std::fs::create_dir(&staging) {
            Ok(()) => return Ok(staging),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("cannot create `{}`: {e}", staging.display())),
        }
    }
    Err(format!(
        "cannot find a free staging name beside `{}`",
        dest.display()
    ))
}

fn write_into(tree: &Tree, root: &Path, links: Links) -> Result<(), String> {
    // Every directory above a path is a node of its own and paths are sorted, so a directory is
    // made before what is in it, and each is made new (`create_dir`, `create_new`): a name the
    // filesystem takes for one already there (a case or Unicode fold the clash key doesn't
    // know, an 8.3 name) is an error, never a write through what is there.
    for (path, node) in tree.nodes() {
        let out = root.join(path);
        let fail = |e: std::io::Error| format!("cannot write `{}`: {e}", out.display());
        match node {
            Node::Dir => std::fs::create_dir(&out).map_err(fail)?,
            Node::File(bytes) => write_new(&out, bytes).map_err(fail)?,
            Node::Link(target) => match links {
                Links::Files => write_new(&out, target.as_bytes()).map_err(fail)?,
                Links::Symbolic => symlink(target, &out).map_err(fail)?,
            },
        }
    }
    Ok(())
}

/// Write a file that must not exist yet (never through a link that is already there).
fn write_new(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)
}

#[cfg(unix)]
fn symlink(target: &str, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn symlink(target: &str, link: &Path) -> std::io::Result<()> {
    write_new(link, target.as_bytes())
}

/// Read all of `reader`, refusing more than `max` bytes.
fn read_capped(reader: impl Read, max: u64) -> std::io::Result<Option<Vec<u8>>> {
    let mut out = vec![];
    reader.take(max.saturating_add(1)).read_to_end(&mut out)?;
    Ok((out.len() as u64 <= max).then_some(out))
}
