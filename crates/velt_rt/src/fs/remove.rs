//! `rm` on Windows, the one directory operation the runtime keeps (std/fs/tree/win32.velt calls
//! it). macOS and Linux remove in std (std/fs/tree/posix.velt, #2196); Windows keeps this until
//! std opens each directory relative to its parent's handle, as `remove_dir_all` does here, so
//! that a directory swapped for a junction during the walk is never entered.
//!
//! The function is exported on every system (the symbol tables list one set of names); off
//! Windows nothing calls it and it reports "unsupported".

use super::ops::{path_arg, unit};
use crate::result::IoResult;
use crate::str::VeltStr;
use std::io;
use std::path::PathBuf;

/// `rm(path, { recursive })`: files, symlinks, empty directories, or whole trees if `recursive`.
/// A directory symlink or junction is removed as a directory (the link only, never its target).
#[cfg(windows)]
fn remove(path: PathBuf, recursive: bool) -> io::Result<()> {
    use super::ops::at;
    use std::fs;
    use std::os::windows::fs::FileTypeExt;
    let m = fs::symlink_metadata(&path).map_err(at("lstat", &path))?;
    if m.file_type().is_symlink_dir() {
        fs::remove_dir(&path).map_err(at("rmdir", &path))
    } else if !m.is_dir() {
        fs::remove_file(&path).map_err(at("unlink", &path))
    } else if recursive {
        fs::remove_dir_all(&path).map_err(at("rm", &path))
    } else {
        fs::remove_dir(&path).map_err(at("rmdir", &path))
    }
}

#[cfg(not(windows))]
fn remove(_path: PathBuf, _recursive: bool) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

/// `rmSync(path, { recursive })` on Windows.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_remove_sync(
    path: *const VeltStr,
    recursive: u8,
    out: *mut IoResult<()>,
) {
    unit(remove(path_arg(path), recursive != 0)).write_to(out);
}
