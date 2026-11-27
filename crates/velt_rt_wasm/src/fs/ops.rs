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
            // Node's `mtimeMs` (`sec * 1e3 + nsec / 1e6`), as std's native `stat` computes it: the
            // same float for the same file (#3785); `as_secs_f64() * 1000.0` differs in the last
            // bits for about one time in four.
            .map_or(0.0, |d| {
                d.as_secs() as f64 * 1000.0 + d.subsec_nanos() as f64 / 1e6
            }),
        is_file: m.is_file() as u8,
        is_dir: m.is_dir() as u8,
        is_symlink: m.file_type().is_symlink() as u8,
    }
}

#[cfg(test)]
mod tests {
    use super::stats_of;
    use std::fs;
    use std::time::{Duration, UNIX_EPOCH};

    /// A file's `mtime_ms` is Node's `sec * 1e3 + nsec / 1e6` to the last bit (#3785): 40
    /// modification times 1.2345 ms apart, of which `as_secs_f64() * 1000.0` gets 10 wrong.
    #[test]
    fn mtime_ms_is_nodes_float() {
        let path = std::env::temp_dir().join(format!("velt-rt-wasm-mtime-{}", std::process::id()));
        let file = fs::File::create(&path).expect("create a temporary file");
        let mut wrong = Vec::new();
        for k in 0..40u32 {
            let at = UNIX_EPOCH + Duration::new(1_795_626_242, 919_527_600 + k * 1_234_500);
            file.set_modified(at).expect("set the modification time");
            // What the file system kept (it may round the nanoseconds).
            let kept = fs::metadata(&path)
                .and_then(|m| m.modified())
                .expect("read the modification time back")
                .duration_since(UNIX_EPOCH)
                .expect("after the epoch");
            let node = kept.as_secs() as f64 * 1e3 + kept.subsec_nanos() as f64 / 1e6;
            let got = stats_of(fs::metadata(&path).expect("stat")).mtime_ms;
            if got.to_bits() != node.to_bits() {
                wrong.push((kept, got, node));
            }
        }
        drop(file);
        let _ = fs::remove_file(&path);
        assert!(wrong.is_empty(), "mtime_ms differs from Node's: {wrong:?}");
    }
}
