//! `std/fs` (rt_abi_async.md §5) over WASI files: velt_rt's operations compiled from its
//! sources, async variants that run the operation on their first poll (there is no blocking
//! pool on one thread), and the `*_sync` functions of whole.rs and tree.rs. In the browser every
//! operation fails with the error `std` reports there. Renaming, linking and copying are std
//! code on WebAssembly too (std/fs/paths/wasi.velt and js.velt).

#[path = "../../../velt_rt/src/fs/ops.rs"]
mod ops;
mod tree;
mod whole;

pub use ops::VeltStat;
pub use tree::{kind, VeltDirents};

use crate::result::{fs_error, IoResult};
use crate::str::VeltStr;
use crate::task::leaf::ready_leaf;
use crate::task::VeltFut;
use ops::{data_arg, path_arg};
use std::ffi::OsString;
use std::io;
use std::path::Path;

fn unit(r: io::Result<()>) -> IoResult<()> {
    IoResult::from_io(r, |()| ())
}

/// Node's error message for a failed `syscall` on `path` (`ENOENT: …, open 'x'`).
fn at<'a>(syscall: &'static str, path: &'a Path) -> impl FnOnce(io::Error) -> io::Error + 'a {
    move |e| fs_error(e, syscall, path, None)
}

/// An entry name as text: no copy when it is UTF-8 (one U+FFFD per invalid sequence otherwise).
fn name_text(name: OsString) -> String {
    name.into_string()
        .unwrap_or_else(|n| n.to_string_lossy().into_owned())
}

/// `readFileBytes(path)` → `IoResult<VeltBytes>`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_read_file_bytes(path: *const VeltStr) -> *mut VeltFut {
    let p = path_arg(path);
    ready_leaf(move || whole::read_bytes(p))
}

/// `writeFile(path, data)` → `IoResult<()>`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_write_file(
    path: *const VeltStr,
    data: *const VeltStr,
) -> *mut VeltFut {
    let (p, d) = (path_arg(path), data_arg(data));
    ready_leaf(move || whole::write(p, d, false))
}

/// `appendFile(path, data)` → `IoResult<()>`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_append_file(
    path: *const VeltStr,
    data: *const VeltStr,
) -> *mut VeltFut {
    let (p, d) = (path_arg(path), data_arg(data));
    ready_leaf(move || whole::write(p, d, true))
}

/// `readDir(path)` → `IoResult<VeltStrArray>` of sorted names.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_read_dir(path: *const VeltStr) -> *mut VeltFut {
    let p = path_arg(path);
    ready_leaf(move || tree::read_dir(p))
}

/// `stat(path)` → `IoResult<VeltStat>`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_stat(path: *const VeltStr) -> *mut VeltFut {
    let p = path_arg(path);
    ready_leaf(move || whole::stat(p))
}

/// `readDir(path, { withFileTypes: true })` → `IoResult<VeltDirents>`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_read_dir_typed(path: *const VeltStr) -> *mut VeltFut {
    let p = path_arg(path);
    ready_leaf(move || tree::read_dir_typed(p))
}

/// `lstat(path)` → `IoResult<VeltStat>` (a symlink's own metadata).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_lstat(path: *const VeltStr) -> *mut VeltFut {
    let p = path_arg(path);
    ready_leaf(move || whole::lstat(p))
}

/// `mkdir(path, { recursive })` → `IoResult<()>`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_mkdir(path: *const VeltStr, recursive: u8) -> *mut VeltFut {
    let p = path_arg(path);
    ready_leaf(move || tree::mkdir(p, recursive != 0))
}

/// `rm(path, { recursive })` → `IoResult<()>`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_remove(path: *const VeltStr, recursive: u8) -> *mut VeltFut {
    let p = path_arg(path);
    ready_leaf(move || tree::remove(p, recursive != 0))
}

/// `exists(path)` → `u8`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_exists(path: *const VeltStr) -> *mut VeltFut {
    let p = path_arg(path);
    ready_leaf(move || whole::exists(p))
}
