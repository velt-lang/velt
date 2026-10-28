//! Large entries (at least [`crate::LARGE`] bytes, such as LLVM objects), one file each:
//! `<kind>/large/<first 2 hex digits>/<rest of the hex key>`. A file's modification time is when
//! it was last used. Writes go to a temporary file renamed into place, so readers see a whole
//! entry or none.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use crate::{Key, TOUCH_EVERY};

/// The file of `key` under `dir`.
pub(crate) fn path(dir: &Path, key: &Key) -> PathBuf {
    let hex = key.hex();
    dir.join(&hex[..2]).join(&hex[2..])
}

/// The entry under `key`, marking it used.
pub(crate) fn get(dir: &Path, key: &Key) -> Option<Vec<u8>> {
    let path = path(dir, key);
    let mut file = fs::File::open(&path).ok()?;
    let meta = file.metadata().ok()?;
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.read_to_end(&mut bytes).ok()?;
    touch_if_old(&path, &meta);
    Some(bytes)
}

/// Store `value` under `key`.
pub(crate) fn put(dir: &Path, key: &Key, value: &[u8]) -> bool {
    let path = path(dir, key);
    path.exists() || write_atomically(&path, value).is_ok()
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
    let written = fs::File::create(&tmp).and_then(|mut f| f.write_all(bytes));
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
