//! Blocking file-system operations producing C-layout results. Shared by the async ABI (run on
//! tokio's blocking pool) and the `*_sync` ABI (run on the calling thread).

use crate::bytes::VeltBytes;
use crate::result::{invalid_utf8, IoResult};
use crate::str::VeltStr;
use crate::str_array::VeltStrArray;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

/// `fs.stat` result: `{ u64 size; f64 mtime_ms; u8 is_file; u8 is_dir; }` — size 24, align 8.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VeltStat {
    /// Size in bytes.
    pub size: u64,
    /// Last modification, milliseconds since the Unix epoch (0 if unavailable).
    pub mtime_ms: f64,
    /// 1 if a regular file.
    pub is_file: u8,
    /// 1 if a directory.
    pub is_dir: u8,
}

/// Owned path from a Velt string argument (copied: async operations outlive the call).
///
/// # Safety
/// `s` must point to a valid `VeltStr`.
pub unsafe fn path_arg(s: *const VeltStr) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy((*s).as_bytes()).into_owned())
}

/// Owned copy of a string argument's bytes.
///
/// # Safety
/// `s` must point to a valid `VeltStr`.
pub unsafe fn data_arg(s: *const VeltStr) -> Vec<u8> {
    (*s).as_bytes().to_vec()
}

fn unit(r: io::Result<()>) -> IoResult<()> {
    IoResult::from_io(r, |()| ())
}

/// `readFile(path)` as UTF-8 text.
pub fn read_text(path: PathBuf) -> IoResult<VeltStr> {
    let r = fs::read(&path).and_then(|b| match String::from_utf8(b) {
        Ok(s) => Ok(s.into_bytes()),
        Err(_) => Err(invalid_utf8("file")),
    });
    IoResult::from_io(r, VeltStr::from_vec)
}

/// `readFile(path)` as bytes.
pub fn read_bytes(path: PathBuf) -> IoResult<VeltBytes> {
    IoResult::from_io(fs::read(&path), VeltBytes::from_vec)
}

/// `writeFile` (truncate) / `appendFile` (create if missing).
pub fn write(path: PathBuf, data: Vec<u8>, append: bool) -> IoResult<()> {
    if !append {
        return unit(fs::write(&path, data));
    }
    let r = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| f.write_all(&data));
    unit(r)
}

/// `readDir(path)`: entry names (not paths), sorted for deterministic output.
pub fn read_dir(path: PathBuf) -> IoResult<VeltStrArray> {
    let r = fs::read_dir(&path).and_then(|entries| {
        let mut names = entries
            .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect::<io::Result<Vec<String>>>()?;
        names.sort();
        Ok(names)
    });
    IoResult::from_io(r, VeltStrArray::from_strings)
}

/// `stat(path)` (follows symlinks).
pub fn stat(path: PathBuf) -> IoResult<VeltStat> {
    IoResult::from_io(fs::metadata(&path), |m| VeltStat {
        size: m.len(),
        mtime_ms: m
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0.0, |d| d.as_secs_f64() * 1000.0),
        is_file: m.is_file() as u8,
        is_dir: m.is_dir() as u8,
    })
}

/// `mkdir(path, { recursive })`.
pub fn mkdir(path: PathBuf, recursive: bool) -> IoResult<()> {
    unit(if recursive {
        fs::create_dir_all(&path)
    } else {
        fs::create_dir(&path)
    })
}

/// `rm(path, { recursive })`: files, symlinks, empty directories, or whole trees if `recursive`.
pub fn remove(path: PathBuf, recursive: bool) -> IoResult<()> {
    let r = fs::symlink_metadata(&path).and_then(|m| {
        if !m.is_dir() {
            fs::remove_file(&path)
        } else if recursive {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_dir(&path)
        }
    });
    unit(r)
}

/// `rename(from, to)` (replaces an existing file at `to`).
pub fn rename(from: PathBuf, to: PathBuf) -> IoResult<()> {
    unit(fs::rename(from, to))
}

/// `copyFile(from, to)`.
pub fn copy(from: PathBuf, to: PathBuf) -> IoResult<()> {
    unit(fs::copy(from, to).map(|_| ()))
}

/// `exists(path)`: 1 if anything (file, directory, valid symlink) is there.
pub fn exists(path: PathBuf) -> u8 {
    path.try_exists().unwrap_or(false) as u8
}
