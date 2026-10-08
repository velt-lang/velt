//! Blocking file-system operations producing C-layout results. Shared by the async ABI (run on
//! tokio's blocking pool) and the `*_sync` ABI (run on the calling thread).

use crate::bytes::VeltBytes;
use crate::result::{fs_error, invalid_utf8, op_error, IoResult};
use crate::str::VeltStr;
use crate::str_array::VeltStrArray;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
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

/// What a directory entry is (`VeltDirents::kinds`), as `std/fs.vlt`'s `Dirent` reads it.
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

fn unit(r: io::Result<()>) -> IoResult<()> {
    IoResult::from_io(r, |()| ())
}

/// Node's error message for a failed `syscall` on `path` (`ENOENT: …, open 'x'`).
fn at<'a>(syscall: &'static str, path: &'a Path) -> impl FnOnce(io::Error) -> io::Error + 'a {
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

/// An entry name as text: no copy when it is UTF-8 (one U+FFFD per invalid sequence otherwise).
fn name_text(name: OsString) -> String {
    name.into_string()
        .unwrap_or_else(|n| n.to_string_lossy().into_owned())
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

/// `readDir(path, { withFileTypes: true })`: sorted names with their types. The type comes with
/// the listing (`d_type` from `readdir` on Linux and macOS, the find data on Windows); only an
/// entry whose file system reports no type (`DT_UNKNOWN`) costs an `lstat`, which
/// `DirEntry::file_type` makes then.
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

/// `Stats` of metadata (`is_symlink` is only ever set for `lstat`'s).
fn stats_of(m: fs::Metadata) -> VeltStat {
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
/// file link when it does not exist). Creating one needs Developer Mode or the
/// `SeCreateSymbolicLinkPrivilege` (`EACCES` otherwise).
#[cfg(windows)]
fn make_symlink(target: &Path, path: &Path) -> io::Result<()> {
    let resolved = match path.parent() {
        Some(dir) if target.is_relative() => dir.join(target),
        _ => target.to_path_buf(),
    };
    if resolved.is_dir() {
        std::os::windows::fs::symlink_dir(target, path)
    } else {
        std::os::windows::fs::symlink_file(target, path)
    }
}

/// WASI's `symlink_path` is not stable in Rust yet.
#[cfg(not(any(unix, windows)))]
fn make_symlink(_target: &Path, _path: &Path) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
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
            if is_dir_link(&m) {
                fs::remove_dir(&path).map_err(at("rmdir", &path))
            } else if !m.is_dir() {
                fs::remove_file(&path).map_err(at("unlink", &path))
            } else if recursive {
                fs::remove_dir_all(&path).map_err(at("rm", &path))
            } else {
                fs::remove_dir(&path).map_err(at("rmdir", &path))
            }
        });
    unit(r)
}

/// A Windows directory symlink or junction, which is removed as a directory (the link only,
/// never its target). Unix links are files.
#[cfg(windows)]
fn is_dir_link(m: &fs::Metadata) -> bool {
    use std::os::windows::fs::FileTypeExt;
    m.file_type().is_symlink_dir()
}

#[cfg(not(windows))]
fn is_dir_link(_m: &fs::Metadata) -> bool {
    false
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

/// `exists(path)`: 1 if anything (file, directory, valid symlink) is there.
pub fn exists(path: PathBuf) -> u8 {
    path.try_exists().unwrap_or(false) as u8
}
