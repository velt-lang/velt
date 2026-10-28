//! What module resolution looked at: while [`record`] runs a function, the file system calls
//! that the module loader and the npm resolver make through this module ([`is_file`],
//! [`is_dir`], [`read_dir`], [`read_to_string`], [`canonicalize`]) are noted in a [`Seen`], so a build can tell
//! later whether anything that decided how its imports resolved has changed (the compiler's
//! build stamp, #1612): a path probed is a file, a directory or missing as it was, a directory
//! lists the same entries, a file read has the same contents, a link leads where it led.
//!
//! Outside [`record`] the calls only do what `std` does. Notes are per thread: resolution runs
//! on the thread that loads the program.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs::DirEntry;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use sha2::{Digest, Sha256};

/// What a path was when it was probed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    /// Something else (a device, a socket).
    Other,
    Missing,
}

impl Kind {
    /// What `path` is now (symbolic links followed, as `Path::is_file` does).
    pub fn of(path: &Path) -> Kind {
        match std::fs::metadata(path) {
            Ok(m) if m.is_file() => Kind::File,
            Ok(m) if m.is_dir() => Kind::Dir,
            Ok(_) => Kind::Other,
            Err(_) => Kind::Missing,
        }
    }

    /// One letter, for text formats.
    pub fn letter(self) -> char {
        match self {
            Kind::File => 'f',
            Kind::Dir => 'd',
            Kind::Other => 'o',
            Kind::Missing => '-',
        }
    }

    /// The kind [`Kind::letter`] wrote.
    pub fn from_letter(c: char) -> Option<Kind> {
        Some(match c {
            'f' => Kind::File,
            'd' => Kind::Dir,
            'o' => Kind::Other,
            '-' => Kind::Missing,
            _ => return None,
        })
    }
}

/// A file read while recording.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Read {
    /// Size and modification time ([`stat`]) just before it was read: when they are still the
    /// file's, so are its contents (unless it changed within the time's granularity).
    pub stat: Option<(u64, u128)>,
    /// [`content_hash`] of what was read.
    pub hash: String,
}

/// Everything noted while recording, by absolute path.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Seen {
    /// Every path probed, with what it was.
    pub probed: BTreeMap<PathBuf, Kind>,
    /// Every directory listed, with the [`listing_hash`] of its entries then.
    pub listed: BTreeMap<PathBuf, String>,
    /// Every file read.
    pub read: BTreeMap<PathBuf, Read>,
    /// Every path resolved ([`canonicalize`]), with the [`content_hash`] of where it led
    /// ([`resolved_hash`]).
    pub resolved: BTreeMap<PathBuf, String>,
}

thread_local! {
    static SEEN: RefCell<Option<Seen>> = const { RefCell::new(None) };
}

/// Run `f`, noting what it looks at through this module.
pub fn record<T>(f: impl FnOnce() -> T) -> (T, Seen) {
    let outer = SEEN.with(|s| s.borrow_mut().replace(Seen::default()));
    let out = f();
    let seen = SEEN.with(|s| std::mem::replace(&mut *s.borrow_mut(), outer));
    (out, seen.unwrap_or_default())
}

/// Note something about `path` when recording.
fn note(path: &Path, add: impl FnOnce(&mut Seen, PathBuf)) {
    SEEN.with(|s| {
        if let Some(seen) = s.borrow_mut().as_mut() {
            add(seen, crate::relpath::absolute(path));
        }
    });
}

fn recording() -> bool {
    SEEN.with(|s| s.borrow().is_some())
}

/// What `path` is, noted.
pub fn kind(path: &Path) -> Kind {
    let kind = Kind::of(path);
    note(path, |seen, p| {
        seen.probed.entry(p).or_insert(kind);
    });
    kind
}

/// `path.is_file()`, noted.
pub fn is_file(path: &Path) -> bool {
    kind(path) == Kind::File
}

/// `path.is_dir()`, noted.
pub fn is_dir(path: &Path) -> bool {
    kind(path) == Kind::Dir
}

/// The entries of `dir` (those that could be read), noted.
pub fn read_dir(dir: &Path) -> std::io::Result<Vec<DirEntry>> {
    let entries: Vec<DirEntry> = match std::fs::read_dir(dir) {
        Ok(entries) => entries.flatten().collect(),
        Err(e) => {
            kind(dir);
            return Err(e);
        }
    };
    if recording() {
        let hash = hash_names(entries.iter().map(entry_name).collect());
        note(dir, |seen, p| {
            seen.listed.entry(p).or_insert(hash);
        });
    }
    Ok(entries)
}

/// `std::fs::read_to_string(path)`, noted: its size and time before reading and the hash of
/// what was read.
pub fn read_to_string(path: &Path) -> std::io::Result<String> {
    if !recording() {
        return std::fs::read_to_string(path);
    }
    let before = stat(path);
    let read = std::fs::read_to_string(path);
    match &read {
        Ok(text) => {
            let hash = content_hash(text.as_bytes());
            note(path, |seen, p| {
                seen.read.entry(p).or_insert(Read { stat: before, hash });
            });
        }
        Err(_) => {
            kind(path);
        }
    }
    read
}

/// `std::fs::canonicalize(path)`, noted.
pub fn canonicalize(path: &Path) -> std::io::Result<PathBuf> {
    let real = std::fs::canonicalize(path);
    if recording() {
        let hash = hash_resolved(real.as_deref().ok());
        note(path, |seen, p| {
            seen.resolved.entry(p).or_insert(hash);
        });
    }
    real
}

/// The hash a [`Seen::resolved`] path records of where it leads now.
pub fn resolved_hash(path: &Path) -> String {
    hash_resolved(std::fs::canonicalize(path).ok().as_deref())
}

fn hash_resolved(real: Option<&Path>) -> String {
    let text = real.map_or_else(String::new, |p| p.to_string_lossy().into_owned());
    content_hash(text.as_bytes())
}

/// The hash a [`Read`] records of contents `bytes`.
pub fn content_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// The hash a [`Seen::listed`] directory records of its entries (names and whether each is a
/// directory); `None` when `dir` cannot be listed.
pub fn listing_hash(dir: &Path) -> Option<String> {
    let entries = std::fs::read_dir(dir).ok()?;
    Some(hash_names(
        entries.flatten().map(|e| entry_name(&e)).collect(),
    ))
}

fn entry_name(e: &DirEntry) -> (String, bool) {
    let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
    (e.file_name().to_string_lossy().into_owned(), is_dir)
}

fn hash_names(mut names: Vec<(String, bool)>) -> String {
    names.sort();
    let mut text = String::new();
    for (name, is_dir) in names {
        text.push_str(&name);
        text.push(if is_dir { '/' } else { '\0' });
    }
    content_hash(text.as_bytes())
}

/// Size and modification time (nanoseconds since 1970) of `path`.
pub fn stat(path: &Path) -> Option<(u64, u128)> {
    let meta = std::fs::metadata(path).ok()?;
    let time = meta.modified().ok()?;
    let nanos = time.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_nanos();
    Some((meta.len(), nanos))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_only_inside_record() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("a.ts"), "a").unwrap();
        assert!(is_file(&dir.join("a.ts")));
        let ((), seen) = record(|| {
            assert!(is_file(&dir.join("a.ts")));
            assert!(!is_file(&dir.join("b.ts")));
            assert!(!is_dir(&dir.join("a.ts")));
            assert_eq!(read_dir(dir).unwrap().len(), 1);
            assert_eq!(read_to_string(&dir.join("a.ts")).unwrap(), "a");
            assert!(read_to_string(&dir.join("c.ts")).is_err());
        });
        let abs = |n: &str| crate::relpath::absolute(&dir.join(n));
        assert_eq!(seen.probed[&abs("a.ts")], Kind::File);
        assert_eq!(seen.probed[&abs("b.ts")], Kind::Missing);
        assert_eq!(seen.probed[&abs("c.ts")], Kind::Missing);
        assert_eq!(seen.listed[&abs("")], listing_hash(dir).unwrap());
        let read = &seen.read[&abs("a.ts")];
        assert_eq!(read.hash, content_hash(b"a"));
        assert_eq!(read.stat, stat(&dir.join("a.ts")));
        let ((), linked) = record(|| {
            canonicalize(&dir.join("a.ts")).unwrap();
        });
        assert_eq!(
            linked.resolved[&abs("a.ts")],
            resolved_hash(&dir.join("a.ts"))
        );
        // A new entry changes the listing; nothing is recorded any more.
        std::fs::write(dir.join("b.ts"), "").unwrap();
        assert_ne!(seen.listed[&abs("")], listing_hash(dir).unwrap());
        let ((), again) = record(|| ());
        assert_eq!(again, Seen::default());
    }
}
