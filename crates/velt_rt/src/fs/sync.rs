//! Blocking `std/fs` variants (`readFileSync` ...): same operations and result layouts as the async
//! ABI, run on the calling thread. Each writes its `IoResult` to `out` and returns nothing:
//! `std/fs.vlt` declares a struct result, which lowers to exactly that (rt_abi_async.md §3.1).

use super::ops::{self, data_arg, path_arg, VeltStat};
use crate::bytes::VeltBytes;
use crate::result::IoResult;
use crate::str::VeltStr;
use crate::str_array::VeltStrArray;

/// `readFileSync(path)` (UTF-8 text).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_read_file_sync(
    path: *const VeltStr,
    out: *mut IoResult<VeltStr>,
) {
    ops::read_text(path_arg(path)).write_to(out);
}

/// `readFileSync(path)` as bytes.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_read_file_bytes_sync(
    path: *const VeltStr,
    out: *mut IoResult<VeltBytes>,
) {
    ops::read_bytes(path_arg(path)).write_to(out);
}

/// `writeFileSync(path, data)`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_write_file_sync(
    path: *const VeltStr,
    data: *const VeltStr,
    out: *mut IoResult<()>,
) {
    ops::write(path_arg(path), data_arg(data), false).write_to(out);
}

/// `appendFileSync(path, data)`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_append_file_sync(
    path: *const VeltStr,
    data: *const VeltStr,
    out: *mut IoResult<()>,
) {
    ops::write(path_arg(path), data_arg(data), true).write_to(out);
}

/// `readDirSync(path)`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_read_dir_sync(
    path: *const VeltStr,
    out: *mut IoResult<VeltStrArray>,
) {
    ops::read_dir(path_arg(path)).write_to(out);
}

/// `statSync(path)`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_stat_sync(path: *const VeltStr, out: *mut IoResult<VeltStat>) {
    ops::stat(path_arg(path)).write_to(out);
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

/// `existsSync(path)`: 1 if the path exists.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_exists_sync(path: *const VeltStr) -> u8 {
    ops::exists(path_arg(path))
}
