//! Blocking file-system operations producing C-layout results. Shared by the async ABI (run on
//! tokio's blocking pool) and the `*_sync` ABI (run on the calling thread).

use crate::result::{fs_error, IoResult};
use crate::str::VeltStr;
use std::ffi::OsString;
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

pub(crate) fn unit(r: io::Result<()>) -> IoResult<()> {
    IoResult::from_io(r, |()| ())
}

/// Node's error message for a failed `syscall` on `path` (`ENOENT: …, open 'x'`).
pub(crate) fn at<'a>(
    syscall: &'static str,
    path: &'a Path,
) -> impl FnOnce(io::Error) -> io::Error + 'a {
    move |e| fs_error(e, syscall, path, None)
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

/// An entry name as text: no copy when it is UTF-8 (one U+FFFD per invalid sequence otherwise).
pub(crate) fn name_text(name: OsString) -> String {
    name.into_string()
        .unwrap_or_else(|n| n.to_string_lossy().into_owned())
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

/// `realpath(path)`: the absolute path of `path` with every symlink, `.` and `..` resolved
/// (Node's `fs.realpath`); a path that doesn't exist is `ENOENT`. Windows' extended-length
/// prefix (`\\?\`) is dropped where the path doesn't need it, as Node gives `C:\dir\file`.
pub fn realpath(path: PathBuf) -> IoResult<VeltStr> {
    let r = fs::canonicalize(&path).map_err(at("realpath", &path));
    IoResult::from_io(r, |p| {
        VeltStr::from_vec(name_text(plain_path(p).into_os_string()).into_bytes())
    })
}

/// `\\?\C:\x` as `C:\x` and `\\?\UNC\server\share` as `\\server\share` (what `canonicalize`
/// returns on Windows); any other path as it is.
fn plain_path(p: PathBuf) -> PathBuf {
    let Some(s) = p.to_str() else {
        return p;
    };
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    match s.strip_prefix(r"\\?\") {
        // MAX_PATH counts UTF-16 units, not UTF-8 bytes.
        Some(rest)
            if rest.as_bytes().get(1) == Some(&b':') && rest.encode_utf16().count() < 260 =>
        {
            PathBuf::from(rest)
        }
        _ => p,
    }
}

/// `readlink(path)`: a symlink's target, as stored in the link. Something other than a link is
/// `EINVAL` everywhere, as on Linux (Windows reports "not a reparse point").
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

/// `symlink(target, path)`: a link at `path` to `target` (relative to the link's directory).
pub fn symlink(target: PathBuf, path: PathBuf) -> IoResult<()> {
    let r = make_symlink(&target, &path);
    unit(r.map_err(|e| fs_error(e, "symlink", &target, Some(&path))))
}

#[cfg(unix)]
fn make_symlink(target: &Path, path: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, path)
}

/// Windows has file and directory links; as in Node, the type follows what `target` is now (a
/// file link when it does not exist). A relative target is stored with `\` separators, as Node's
/// `preprocessSymlinkDestination` does: Windows does not resolve `/` in a link's relative
/// target. Creating a link needs Developer Mode or the `SeCreateSymbolicLinkPrivilege` (`EACCES`
/// otherwise).
#[cfg(windows)]
fn make_symlink(target: &Path, path: &Path) -> io::Result<()> {
    let target = if target.is_relative() {
        PathBuf::from(target.to_string_lossy().replace('/', "\\"))
    } else {
        target.to_path_buf()
    };
    let resolved = match path.parent() {
        Some(dir) if target.is_relative() => dir.join(&target),
        _ => target.clone(),
    };
    if resolved.is_dir() {
        std::os::windows::fs::symlink_dir(&target, path)
    } else {
        std::os::windows::fs::symlink_file(&target, path)
    }
}

/// WASI's `symlink_path` is not stable in Rust yet.
#[cfg(not(any(unix, windows)))]
fn make_symlink(_target: &Path, _path: &Path) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
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

#[cfg(test)]
mod tests {
    use super::plain_path;
    use std::path::PathBuf;

    #[test]
    fn windows_extended_prefixes_are_dropped_where_not_needed() {
        let plain = |s: &str| plain_path(PathBuf::from(s)).to_string_lossy().into_owned();
        assert_eq!(plain(r"\\?\C:\dir\f.txt"), r"C:\dir\f.txt");
        assert_eq!(plain(r"\\?\UNC\server\share\f"), r"\\server\share\f");
        assert_eq!(plain("/private/var/f"), "/private/var/f");
        // A path too long for the plain form keeps its prefix, as does a volume GUID path.
        let long = format!(r"\\?\C:\{}", "d".repeat(300));
        assert_eq!(plain(&long), long);
        // Under 260 UTF-16 units but more UTF-8 bytes: plain.
        let wide = format!(r"\\?\C:\{}", "é".repeat(200));
        assert_eq!(plain(&wide), wide[4..]);
        let volume = r"\\?\Volume{0b1c}\f";
        assert_eq!(plain(volume), volume);
    }
}
