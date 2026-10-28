//! Large entries (at least [`crate::LARGE`] bytes, such as LLVM objects), one file each:
//! `<kind>/large/<first 2 hex digits>/<rest of the hex key>`. A file's modification time is when
//! it was last used. Writes go to a temporary file, synced, then renamed into place, so readers
//! see a whole entry or none. A file starts with a [`HEADER`]: the value's length and checksum,
//! checked on every read, so a file a crash left empty or a disk error damaged is a miss (and is
//! removed, so the next build stores the entry again), never wrong code.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use crate::{checksum, Key, TOUCH_EVERY};

/// Bytes before the value: its length (8, little-endian), then its checksum (8).
pub(crate) const HEADER: usize = 16;

/// The file of `key` under `dir`.
pub(crate) fn path(dir: &Path, key: &Key) -> PathBuf {
    let hex = key.hex();
    dir.join(&hex[..2]).join(&hex[2..])
}

/// The entry under `key`, marking it used. A file that fails its check is removed.
pub(crate) fn get(dir: &Path, key: &Key) -> Option<Vec<u8>> {
    let path = path(dir, key);
    let mut file = fs::File::open(&path).ok()?;
    let meta = file.metadata().ok()?;
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.read_to_end(&mut bytes).ok()?;
    let Some(value) = checked(bytes) else {
        drop(file);
        let _ = fs::remove_file(&path);
        return None;
    };
    touch_if_old(&path, &meta);
    Some(value)
}

/// Store `value` under `key`, replacing a file that fails its check.
pub(crate) fn put(dir: &Path, key: &Key, value: &[u8]) -> bool {
    let path = path(dir, key);
    let whole = fs::read(&path).ok().and_then(checked).is_some();
    whole || write_atomically(&path, &with_header(value)).is_ok()
}

/// File entries (`Cache::put_file`): a plain file, which a linker can read as it is, and beside
/// it a sidecar (`<name>.check`) holding the header. The file is written before the sidecar, so
/// a reader that finds them out of step has a miss (and removes both), never wrong code.
pub(crate) fn put_file(dir: &Path, key: &Key, src: &Path) -> bool {
    let path = path(dir, key);
    if checked_file(&path) {
        return true;
    }
    let Ok(value) = fs::read(src) else {
        return false;
    };
    let mut header = Vec::with_capacity(HEADER);
    header.extend_from_slice(&(value.len() as u64).to_le_bytes());
    header.extend_from_slice(&checksum(&value).to_le_bytes());
    write_atomically(&path, &value).is_ok() && write_atomically(&sidecar(&path), &header).is_ok()
}

/// The path of `key`'s file entry when it passes its check (marking it used), else `None`.
pub(crate) fn path_of(dir: &Path, key: &Key) -> Option<PathBuf> {
    let path = path(dir, key);
    if !checked_file(&path) {
        return None;
    }
    touch_if_old(&path, &fs::metadata(&path).ok()?);
    Some(path)
}

/// Mark `key`'s file entry used now (an object a link used, say).
pub(crate) fn touch(dir: &Path, key: &Key) {
    if let Ok(f) = fs::OpenOptions::new().write(true).open(path(dir, key)) {
        let _ = f.set_modified(SystemTime::now());
    }
}

/// Whether the file entry at `path` matches its sidecar; one that does not is removed.
fn checked_file(path: &Path) -> bool {
    let whole = (|| {
        let header = fs::read(sidecar(path)).ok()?;
        let value = fs::read(path).ok()?;
        let mut bytes = header;
        bytes.extend_from_slice(&value);
        checked(bytes)
    })();
    if whole.is_none() && (path.exists() || sidecar(path).exists()) {
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(sidecar(path));
    }
    whole.is_some()
}

/// The sidecar of a file entry.
pub(crate) fn sidecar(path: &Path) -> PathBuf {
    path.with_extension("check")
}

pub(crate) fn is_sidecar(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "check")
}

/// `value` behind its header.
fn with_header(value: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(HEADER + value.len());
    bytes.extend_from_slice(&(value.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&checksum(value).to_le_bytes());
    bytes.extend_from_slice(value);
    bytes
}

/// The value of a file's `bytes`, if its header matches it.
fn checked(mut bytes: Vec<u8>) -> Option<Vec<u8>> {
    let header = bytes.get(..HEADER)?;
    let len = u64::from_le_bytes(header[..8].try_into().ok()?);
    let check = u64::from_le_bytes(header[8..].try_into().ok()?);
    let value = &bytes[HEADER..];
    if value.len() as u64 != len || checksum(value) != check {
        return None;
    }
    bytes.drain(..HEADER);
    Some(bytes)
}

/// Mark an entry used now when its time of use is older than [`TOUCH_EVERY`].
fn touch_if_old(path: &Path, meta: &fs::Metadata) {
    let old = meta
        .modified()
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age > TOUCH_EVERY);
    if old {
        if let Ok(f) = fs::OpenOptions::new().write(true).open(path) {
            let _ = f.set_modified(SystemTime::now());
        }
    }
}

/// Write `bytes` to a temporary file beside `path`, then rename it into place.
fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = dir.join(format!(".{name}.{}.{n}.tmp", std::process::id()));
    // Synced before the rename: otherwise a power loss can leave the renamed file empty.
    let written = fs::File::create(&tmp).and_then(|mut f| {
        f.write_all(bytes)?;
        f.sync_all()
    });
    let renamed = written.and_then(|()| fs::rename(&tmp, path));
    if renamed.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    renamed
}

/// Every file under `dir`, recursively.
pub(crate) fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(d) = pending.pop() {
        let Ok(entries) = fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            match e.file_type() {
                Ok(t) if t.is_dir() => pending.push(e.path()),
                Ok(_) => files.push(e.path()),
                Err(_) => {}
            }
        }
    }
    files
}

/// The files directly in `dir`.
pub(crate) fn files_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return vec![];
    };
    (entries.flatten())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .collect()
}

pub(crate) fn is_temp(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "tmp")
}

/// What a file takes on disk: its allocated blocks (whole blocks, so a 300-byte file takes 4 KiB
/// on most file systems).
pub(crate) fn disk_size(meta: &fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        std::os::unix::fs::MetadataExt::blocks(meta) * 512
    }
    #[cfg(not(unix))]
    {
        meta.len().div_ceil(4096) * 4096
    }
}
