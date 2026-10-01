//! `std/fs`: async file-system operations as leaf `VeltFut`s (run on tokio's blocking pool once
//! first polled; result slot types are given per function), plus blocking `*_sync` variants in
//! `sync.rs`, and chunked reader/writer handles in `stream.rs`.
//!
//! Every argument is copied at the call, so the caller keeps ownership of its strings and may drop
//! them right away. Data arguments are strings.

mod ops;
pub mod stream;
pub mod sync;

pub use ops::VeltStat;

use crate::str::VeltStr;
use crate::task::leaf::blocking_leaf;
use crate::task::VeltFut;
use ops::{data_arg, path_arg};

/// `readFile(path)` → result slot `IoResult<VeltStr>` (`INVALID_DATA` if not UTF-8).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_read_file(path: *const VeltStr) -> *mut VeltFut {
    let p = path_arg(path);
    blocking_leaf(move || ops::read_text(p))
}

/// `readFileBytes(path)` → `IoResult<VeltBytes>`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_read_file_bytes(path: *const VeltStr) -> *mut VeltFut {
    let p = path_arg(path);
    blocking_leaf(move || ops::read_bytes(p))
}

/// `writeFile(path, data)` (string or bytes) → `IoResult<()>`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_write_file(
    path: *const VeltStr,
    data: *const VeltStr,
) -> *mut VeltFut {
    let (p, d) = (path_arg(path), data_arg(data));
    blocking_leaf(move || ops::write(p, d, false))
}

/// `appendFile(path, data)` (string or bytes) → `IoResult<()>`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_append_file(
    path: *const VeltStr,
    data: *const VeltStr,
) -> *mut VeltFut {
    let (p, d) = (path_arg(path), data_arg(data));
    blocking_leaf(move || ops::write(p, d, true))
}

/// `readDir(path)` → `IoResult<VeltStrArray>` of sorted entry names.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_read_dir(path: *const VeltStr) -> *mut VeltFut {
    let p = path_arg(path);
    blocking_leaf(move || ops::read_dir(p))
}

/// `stat(path)` → `IoResult<VeltStat>`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_stat(path: *const VeltStr) -> *mut VeltFut {
    let p = path_arg(path);
    blocking_leaf(move || ops::stat(p))
}

/// `mkdir(path, { recursive })` → `IoResult<()>`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_mkdir(path: *const VeltStr, recursive: u8) -> *mut VeltFut {
    let p = path_arg(path);
    blocking_leaf(move || ops::mkdir(p, recursive != 0))
}

/// `rm(path, { recursive })` → `IoResult<()>`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_remove(path: *const VeltStr, recursive: u8) -> *mut VeltFut {
    let p = path_arg(path);
    blocking_leaf(move || ops::remove(p, recursive != 0))
}

/// `rename(from, to)` → `IoResult<()>`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_rename(
    from: *const VeltStr,
    to: *const VeltStr,
) -> *mut VeltFut {
    let (f, t) = (path_arg(from), path_arg(to));
    blocking_leaf(move || ops::rename(f, t))
}

/// `copyFile(from, to)` → `IoResult<()>`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_copy_file(
    from: *const VeltStr,
    to: *const VeltStr,
) -> *mut VeltFut {
    let (f, t) = (path_arg(from), path_arg(to));
    blocking_leaf(move || ops::copy(f, t))
}

/// `exists(path)` → result slot `u8` (never fails).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_exists(path: *const VeltStr) -> *mut VeltFut {
    let p = path_arg(path);
    blocking_leaf(move || ops::exists(p))
}
