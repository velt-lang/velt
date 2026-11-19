//! Blocking `std/fs` variants (`readDirSync` ...): same operations and result layouts as the async
//! ABI, run on the calling thread. Each writes its `IoResult` to `out` and returns nothing:
//! `std/fs/rt.velt` declares a struct result, which lowers to exactly that (rt_abi_async.md §3.1).

use super::ops::{self, path_arg, VeltDirents};
use crate::result::IoResult;
use crate::str::VeltStr;
use crate::str_array::VeltStrArray;

/// `readDirSync(path)`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_read_dir_sync(
    path: *const VeltStr,
    out: *mut IoResult<VeltStrArray>,
) {
    ops::read_dir(path_arg(path)).write_to(out);
}

/// `readDirSync(path, { withFileTypes: true })`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_read_dir_typed_sync(
    path: *const VeltStr,
    out: *mut IoResult<VeltDirents>,
) {
    ops::read_dir_typed(path_arg(path)).write_to(out);
}

/// `readlinkSync(path)`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_readlink_sync(
    path: *const VeltStr,
    out: *mut IoResult<VeltStr>,
) {
    ops::readlink(path_arg(path)).write_to(out);
}

/// `realpathSync(path)`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_realpath_sync(
    path: *const VeltStr,
    out: *mut IoResult<VeltStr>,
) {
    ops::realpath(path_arg(path)).write_to(out);
}

/// `symlinkSync(target, path)`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_symlink_sync(
    target: *const VeltStr,
    path: *const VeltStr,
    out: *mut IoResult<()>,
) {
    ops::symlink(path_arg(target), path_arg(path)).write_to(out);
}

/// `mkdirSync(path, { recursive })`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_mkdir_sync(
    path: *const VeltStr,
    recursive: u8,
    out: *mut IoResult<()>,
) {
    ops::mkdir(path_arg(path), recursive != 0).write_to(out);
}

/// `rmSync(path, { recursive })`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_remove_sync(
    path: *const VeltStr,
    recursive: u8,
    out: *mut IoResult<()>,
) {
    ops::remove(path_arg(path), recursive != 0).write_to(out);
}

/// `renameSync(from, to)`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_rename_sync(
    from: *const VeltStr,
    to: *const VeltStr,
    out: *mut IoResult<()>,
) {
    ops::rename(path_arg(from), path_arg(to)).write_to(out);
}

/// `copyFileSync(from, to)`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_copy_file_sync(
    from: *const VeltStr,
    to: *const VeltStr,
    out: *mut IoResult<()>,
) {
    ops::copy(path_arg(from), path_arg(to)).write_to(out);
}
