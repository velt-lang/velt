//! Renaming, linking and copying, which WebAssembly builds still call in the runtime
//! (std/fs/rt.velt): `rename`, `copyFile`, `symlink`, `readlink`, `realpath` and their `*_sync`
//! functions. A native build has them as std code (std/fs/paths/, #2196), so velt_rt no longer
//! compiles them; they moved here from velt_rt's fs/ops.rs and fs/sync.rs, without the Windows
//! and Unix parts WebAssembly never compiled.

use super::ops::path_arg;
use super::{at, name_text, unit};
use crate::result::{fs_error, IoResult};
use crate::str::VeltStr;
use std::fs;
use std::io;
use std::path::PathBuf;

/// `realpath(path)`: the absolute path of `path` with every symlink, `.` and `..` resolved
/// (Node's `fs.realpath`); a path that doesn't exist is `ENOENT`.
pub fn realpath(path: PathBuf) -> IoResult<VeltStr> {
    let r = fs::canonicalize(&path).map_err(at("realpath", &path));
    IoResult::from_io(r, |p| {
        VeltStr::from_vec(name_text(p.into_os_string()).into_bytes())
    })
}

/// `readlink(path)`: a symlink's target, as stored in the link. Something other than a link is
/// `EINVAL`, as on Linux.
pub fn readlink(path: PathBuf) -> IoResult<VeltStr> {
    let r = fs::read_link(&path)
        .map_err(|e| match fs::symlink_metadata(&path) {
            Ok(m) if !m.file_type().is_symlink() => io::Error::from(io::ErrorKind::InvalidInput),
            _ => e,
        })
        .map_err(at("readlink", &path));
    IoResult::from_io(r, |t| {
        VeltStr::from_vec(name_text(t.into_os_string()).into_bytes())
    })
}

/// `symlink(target, path)`: WASI's `symlink_path` is not stable in Rust yet.
pub fn symlink(target: PathBuf, path: PathBuf) -> IoResult<()> {
    let e = io::Error::from(io::ErrorKind::Unsupported);
    unit(Err(fs_error(e, "symlink", &target, Some(&path))))
}

/// `rename(from, to)` (replaces an existing file at `to`).
pub fn rename(from: PathBuf, to: PathBuf) -> IoResult<()> {
    unit(fs::rename(&from, &to).map_err(|e| fs_error(e, "rename", &from, Some(&to))))
}

/// `copyFile(from, to)`.
pub fn copy(from: PathBuf, to: PathBuf) -> IoResult<()> {
    let r = fs::copy(&from, &to).map(|_| ());
    unit(r.map_err(|e| fs_error(e, "copyfile", &from, Some(&to))))
}

/// `readlinkSync(path)`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_readlink_sync(
    path: *const VeltStr,
    out: *mut IoResult<VeltStr>,
) {
    readlink(path_arg(path)).write_to(out);
}

/// `realpathSync(path)`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_realpath_sync(
    path: *const VeltStr,
    out: *mut IoResult<VeltStr>,
) {
    realpath(path_arg(path)).write_to(out);
}

/// `symlinkSync(target, path)`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_symlink_sync(
    target: *const VeltStr,
    path: *const VeltStr,
    out: *mut IoResult<()>,
) {
    symlink(path_arg(target), path_arg(path)).write_to(out);
}

/// `renameSync(from, to)`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_rename_sync(
    from: *const VeltStr,
    to: *const VeltStr,
    out: *mut IoResult<()>,
) {
    rename(path_arg(from), path_arg(to)).write_to(out);
}

/// `copyFileSync(from, to)`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_copy_file_sync(
    from: *const VeltStr,
    to: *const VeltStr,
    out: *mut IoResult<()>,
) {
    copy(path_arg(from), path_arg(to)).write_to(out);
}
