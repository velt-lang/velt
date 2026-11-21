//! An installed release's std is left read-only (#2943): a release never changes, so nothing
//! should edit it, and a check by compilation units can trust it as installed (take its files
//! as the snapshot kept beside std's compiled unit says, without a `stat` of each). Everything
//! that removes or replaces an installed toolchain clears the bit first ([`remove_tree`], and
//! `chmod -R u+w` in the install scripts): a read-only directory can't lose its entries on
//! Unix. On Windows, NTFS deletes read-only files anyway; FAT volumes and network shares may
//! not, so the bit is cleared there too.

use std::io;
use std::path::Path;

/// The directory under a toolchain prefix that is left read-only.
pub const STD_DIR: &str = "std";

/// Make `dir` and everything below it read-only (`on`) or writable again. On Unix a file keeps
/// whether it is executable (0444 or 0555; 0644 or 0755 again) and a directory becomes 0555
/// (0755 again); on Windows a file takes the read-only attribute, which directories ignore.
/// Writable again goes top down, so each directory can be changed below; read-only goes bottom
/// up, so it is still listable while it is walked.
pub fn set_tree(dir: &Path, on: bool) -> io::Result<()> {
    let meta = std::fs::symlink_metadata(dir)?;
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    if meta.is_dir() {
        if !on {
            set_one(dir, &meta, false)?;
        }
        for entry in std::fs::read_dir(dir)? {
            set_tree(&entry?.path(), on)?;
        }
        if on {
            set_one(dir, &meta, true)?;
        }
        return Ok(());
    }
    set_one(dir, &meta, on)
}

/// [`set_tree`] for the std directory of toolchain prefix `prefix`.
pub fn set_std(prefix: &Path, on: bool) -> io::Result<()> {
    set_tree(&prefix.join(STD_DIR), on)
}

/// Remove directory `dir` and everything below it, read-only or not ([`set_tree`]).
pub fn remove_tree(dir: &Path) -> io::Result<()> {
    match set_tree(dir, false) {
        // Only the tree itself missing ends it: an entry gone mid-walk (another remover) still
        // leaves the rest to remove.
        Err(e) if e.kind() == io::ErrorKind::NotFound && !dir.exists() => return Err(e),
        // Removing reports what is still in the way.
        _ => {}
    }
    std::fs::remove_dir_all(dir)
}

#[cfg(unix)]
fn set_one(path: &Path, meta: &std::fs::Metadata, on: bool) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let exec = meta.is_dir() || meta.permissions().mode() & 0o111 != 0;
    let mode = match (on, exec) {
        (true, true) => 0o555,
        (true, false) => 0o444,
        (false, true) => 0o755,
        (false, false) => 0o644,
    };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_one(path: &Path, meta: &std::fs::Metadata, on: bool) -> io::Result<()> {
    if meta.is_dir() {
        return Ok(());
    }
    let mut p = meta.permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    p.set_readonly(on);
    std::fs::set_permissions(path, p)
}

#[cfg(test)]
#[path = "read_only_tests.rs"]
mod tests;
