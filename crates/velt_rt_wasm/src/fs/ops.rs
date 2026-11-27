//! What velt_rt_wasm's file-system operations share: the `Stats` layout and the argument
//! copies.

use crate::str::VeltStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// `fs.stat` / `fs.lstat` result: `{ u64 size; f64 mtime_ms; u8 is_file; u8 is_dir;
/// u8 is_symlink; }` — size 24, align 8.
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
    /// 1 if a symbolic link (only from `lstat`: `stat` follows links).
    pub is_symlink: u8,
}

/// Owned path from a Velt string argument (copied: async operations outlive the call).
///
/// # Safety
/// `s` must point to a valid `VeltStr`.
pub unsafe fn path_arg(s: *const VeltStr) -> PathBuf {
    PathBuf::from((*s).text_lossy().into_owned())
}

/// Owned UTF-8 copy of a string argument (one U+FFFD per lone surrogate, #377).
///
/// # Safety
/// `s` must point to a valid `VeltStr`.
pub unsafe fn data_arg(s: *const VeltStr) -> Vec<u8> {
    (*s).to_string_lossy().into_bytes()
}

/// Opens `path` with `opts`. Windows refuses to open a directory as a file with "access
/// denied"; that is EISDIR, as everywhere else.
pub fn open(path: &Path, opts: &fs::OpenOptions) -> io::Result<fs::File> {
    opts.open(path).map_err(|e| {
        if e.kind() == io::ErrorKind::PermissionDenied && path.is_dir() {
            io::Error::from(io::ErrorKind::IsADirectory)
        } else {
            e
        }
    })
}

/// `Stats` of metadata (`is_symlink` is only ever set for `lstat`'s).
pub fn stats_of(m: fs::Metadata) -> VeltStat {
    VeltStat {
        size: m.len(),
        mtime_ms: m
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0.0, |d| d.as_secs_f64() * 1000.0),
        is_file: m.is_file() as u8,
        is_dir: m.is_dir() as u8,
        is_symlink: m.file_type().is_symlink() as u8,
    }
}
