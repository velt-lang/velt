//! The whole-file operations that WebAssembly builds still call in the runtime (std/fs/rt.velt):
//! reading, writing and appending whole files, `stat`, `lstat` and `exists`, with their `*_sync`
//! functions. A native build has them as std code (std/fs/whole/, #2196), so velt_rt no longer
//! compiles them; they moved here from velt_rt's fs/ops.rs and fs/sync.rs unchanged.

use super::ops::{at, data_arg, open, path_arg, stats_of, unit, VeltStat};
use crate::bytes::VeltBytes;
use crate::result::{fs_error, invalid_utf8, op_error, IoResult};
use crate::str::VeltStr;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// The whole file, with Node's messages: `open 'x'` for a failed open, `read` (no path) for a
/// failed read; a directory fails the read (`EISDIR: …, read`) on every system.
fn read_all(path: &Path) -> io::Result<Vec<u8>> {
    let mut f = open(path, fs::OpenOptions::new().read(true)).map_err(|e| {
        if e.kind() == io::ErrorKind::IsADirectory {
            op_error(e, "read")
        } else {
            fs_error(e, "open", path, None)
        }
    })?;
    let mut data = vec![];
    f.read_to_end(&mut data).map_err(|e| op_error(e, "read"))?;
    Ok(data)
}

/// `readFile(path)` as UTF-8 text.
pub fn read_text(path: PathBuf) -> IoResult<VeltStr> {
    let r = read_all(&path).and_then(|b| match String::from_utf8(b) {
        Ok(s) => Ok(s.into_bytes()),
        Err(_) => Err(invalid_utf8("file")),
    });
    IoResult::from_io(r, VeltStr::from_vec)
}

/// `readFile(path)` as bytes.
pub fn read_bytes(path: PathBuf) -> IoResult<VeltBytes> {
    IoResult::from_io(read_all(&path), VeltBytes::from_vec)
}

/// `writeFile` (truncate) / `appendFile` (create if missing), with Node's messages: `open 'x'`
/// for a failed open (a directory too), `write` (no path) for a failed write.
pub fn write(path: PathBuf, data: Vec<u8>, append: bool) -> IoResult<()> {
    let mut opts = fs::OpenOptions::new();
    opts.create(true);
    if append {
        opts.append(true);
    } else {
        opts.write(true).truncate(true);
    }
    let r = open(&path, &opts)
        .map_err(at("open", &path))
        .and_then(|mut f| f.write_all(&data).map_err(|e| op_error(e, "write")));
    unit(r)
}

/// `stat(path)` (follows symlinks).
pub fn stat(path: PathBuf) -> IoResult<VeltStat> {
    IoResult::from_io(fs::metadata(&path).map_err(at("stat", &path)), stats_of)
}

/// `lstat(path)`: a symlink's own metadata, not its target's.
pub fn lstat(path: PathBuf) -> IoResult<VeltStat> {
    IoResult::from_io(
        fs::symlink_metadata(&path).map_err(at("lstat", &path)),
        stats_of,
    )
}

/// `exists(path)`: 1 if anything (file, directory, valid symlink) is there.
pub fn exists(path: PathBuf) -> u8 {
    path.try_exists().unwrap_or(false) as u8
}

/// `readFileSync(path)` (UTF-8 text).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_read_file_sync(
    path: *const VeltStr,
    out: *mut IoResult<VeltStr>,
) {
    read_text(path_arg(path)).write_to(out);
}

/// `readFileSync(path)` as bytes.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_read_file_bytes_sync(
    path: *const VeltStr,
    out: *mut IoResult<VeltBytes>,
) {
    read_bytes(path_arg(path)).write_to(out);
}

/// `writeFileSync(path, data)`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_write_file_sync(
    path: *const VeltStr,
    data: *const VeltStr,
    out: *mut IoResult<()>,
) {
    write(path_arg(path), data_arg(data), false).write_to(out);
}

/// `appendFileSync(path, data)`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_append_file_sync(
    path: *const VeltStr,
    data: *const VeltStr,
    out: *mut IoResult<()>,
) {
    write(path_arg(path), data_arg(data), true).write_to(out);
}

/// `statSync(path)`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_stat_sync(path: *const VeltStr, out: *mut IoResult<VeltStat>) {
    stat(path_arg(path)).write_to(out);
}

/// `lstatSync(path)`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_lstat_sync(path: *const VeltStr, out: *mut IoResult<VeltStat>) {
    lstat(path_arg(path)).write_to(out);
}

/// `existsSync(path)`: 1 if the path exists.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_exists_sync(path: *const VeltStr) -> u8 {
    exists(path_arg(path))
}
