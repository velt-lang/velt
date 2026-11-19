//! The directory operations that WebAssembly builds still call in the runtime (std/fs/rt.velt):
//! listing a directory (plain and with each entry's type), making one and removing a file, a
//! link or a tree, with their `*_sync` functions. A native build has them as std code
//! (std/fs/tree/, #2196), so velt_rt no longer compiles them; they moved here from velt_rt's
//! fs/ops.rs and fs/sync.rs unchanged.

use super::ops::{at, name_text, path_arg, unit};
use crate::bytes::VeltBytes;
use crate::result::IoResult;
use crate::str::VeltStr;
use crate::str_array::VeltStrArray;
use std::fs;
use std::io;
use std::path::PathBuf;

/// `readDir(path, { withFileTypes })` result: `{ VeltStrArray names; VeltBytes kinds; }` (48
/// bytes), the sorted names and, at the same index, each entry's [`kind`].
#[repr(C)]
#[derive(Debug)]
pub struct VeltDirents {
    /// Entry names (not paths), sorted.
    pub names: VeltStrArray,
    /// One [`kind`] per name.
    pub kinds: VeltBytes,
}

/// What a directory entry is (`VeltDirents::kinds`), as `std/fs/dirent.velt`'s `Dirent` reads it.
pub mod kind {
    /// Anything else (a FIFO, socket or device).
    pub const OTHER: u8 = 0;
    /// A regular file.
    pub const FILE: u8 = 1;
    /// A directory.
    pub const DIR: u8 = 2;
    /// A symbolic link (not followed).
    pub const SYMLINK: u8 = 3;
}

/// `readDir(path)`: entry names (not paths), sorted for deterministic output.
pub fn read_dir(path: PathBuf) -> IoResult<VeltStrArray> {
    let r = fs::read_dir(&path).and_then(|entries| {
        let mut names = entries
            .map(|e| e.map(|e| name_text(e.file_name())))
            .collect::<io::Result<Vec<String>>>()?;
        names.sort_unstable();
        Ok(names)
    });
    let r = r.map_err(at("scandir", &path));
    IoResult::from_io(r, VeltStrArray::from_strings)
}

/// The [`kind`] of an entry's type (which describes a symlink itself, not its target).
fn kind_of(t: fs::FileType) -> u8 {
    if t.is_symlink() {
        kind::SYMLINK
    } else if t.is_dir() {
        kind::DIR
    } else if t.is_file() {
        kind::FILE
    } else {
        kind::OTHER
    }
}

/// `readDir(path, { withFileTypes: true })`: sorted names with their types, which come with the
/// listing.
pub fn read_dir_typed(path: PathBuf) -> IoResult<VeltDirents> {
    let r = fs::read_dir(&path).and_then(|entries| {
        let mut items = entries
            .map(|e| e.and_then(|e| Ok((name_text(e.file_name()), kind_of(e.file_type()?)))))
            .collect::<io::Result<Vec<(String, u8)>>>()?;
        items.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        Ok(items)
    });
    let r = r.map_err(at("scandir", &path));
    IoResult::from_io(r, |items| {
        let kinds = items.iter().map(|(_, k)| *k).collect();
        VeltDirents {
            names: VeltStrArray::from_strings(items.into_iter().map(|(n, _)| n)),
            kinds: VeltBytes::from_vec(kinds),
        }
    })
}

/// `mkdir(path, { recursive })`.
pub fn mkdir(path: PathBuf, recursive: bool) -> IoResult<()> {
    let r = if recursive {
        fs::create_dir_all(&path)
    } else {
        fs::create_dir(&path)
    };
    unit(r.map_err(at("mkdir", &path)))
}

/// `rm(path, { recursive })`: files, symlinks, empty directories, or whole trees if `recursive`.
pub fn remove(path: PathBuf, recursive: bool) -> IoResult<()> {
    let r = fs::symlink_metadata(&path)
        .map_err(at("lstat", &path))
        .and_then(|m| {
            if !m.is_dir() {
                fs::remove_file(&path).map_err(at("unlink", &path))
            } else if recursive {
                fs::remove_dir_all(&path).map_err(at("rm", &path))
            } else {
                fs::remove_dir(&path).map_err(at("rmdir", &path))
            }
        });
    unit(r)
}

/// `readDirSync(path)`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_read_dir_sync(
    path: *const VeltStr,
    out: *mut IoResult<VeltStrArray>,
) {
    read_dir(path_arg(path)).write_to(out);
}

/// `readDirSync(path, { withFileTypes: true })`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_read_dir_typed_sync(
    path: *const VeltStr,
    out: *mut IoResult<VeltDirents>,
) {
    read_dir_typed(path_arg(path)).write_to(out);
}

/// `mkdirSync(path, { recursive })`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_mkdir_sync(
    path: *const VeltStr,
    recursive: u8,
    out: *mut IoResult<()>,
) {
    mkdir(path_arg(path), recursive != 0).write_to(out);
}

/// `rmSync(path, { recursive })`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_remove_sync(
    path: *const VeltStr,
    recursive: u8,
    out: *mut IoResult<()>,
) {
    remove(path_arg(path), recursive != 0).write_to(out);
}
