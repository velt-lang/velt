//! The build cache: compiled code shared by every build on the machine, whatever the project or
//! the compiler version, so nothing is compiled twice.
//!
//! - Entries are keyed by content: the key is a hash of everything the value depends on (the
//!   code, the target and settings, the version of whatever compiled it), so an entry is never
//!   stale, and two compiler versions (projects can pin different ones) keep their entries side
//!   by side instead of overwriting each other's.
//! - Layout: `<root>/v3/<kind>/`. Entries under [`LARGE`] bytes (most compiled functions) are
//!   packed into a few files per kind (`pack`); larger ones (objects) are a file each (`files`).
//!   Writers in several processes at once are safe, and readers never see half an entry.
//! - Size: the cache stays under a limit (default [`DEFAULT_LIMIT`], `$VELT_BUILD_CACHE_SIZE`),
//!   counted as the disk space its files take (whole blocks). A hit marks its entry used (at
//!   most once per [`TOUCH_EVERY`]), and [`Cache::collect_garbage_if_due`] removes the entries
//!   used least recently once the cache outgrows the limit, down to three quarters of it, and
//!   the directories of other layout versions.
//! - Ownership: the root holds a [`MARKER`] (`CACHEDIR.TAG`, which backup tools skip) from the
//!   first write on. [`Cache::clear`] and garbage collection act only on a root that has it, and
//!   remove only what the cache made, so a `$VELT_BUILD_CACHE` naming a directory with other
//!   files in it (or the home directory) loses none of them.
//! - Trust: entries are used as they are (as sccache does), so the directory must be writable
//!   only by users whose code may run in the builds.
//!
//! Nothing here can fail a build: an entry that cannot be read is a miss, and one that cannot be
//! written is just not cached.

mod files;
mod pack;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

use pack::Packs;

/// The layout version: a change of the layout or of how keys are made starts a new directory
/// (garbage collection removes the others).
const LAYOUT: &str = "v3";
/// The file that marks a directory as a build cache (<https://bford.info/cachedir/>).
pub const MARKER: &str = "CACHEDIR.TAG";
const MARKER_TEXT: &str = "Signature: 8a477f597d28d172789f06886806bc55\n\
# This directory is Velt's build cache (velt clean --cache empties it).\n";
/// What the cache makes in its root besides layout directories (`v<n>`), which `clear` removes.
const OWNED: [&str; 2] = ["warm", MARKER];
/// Entries of at least this many bytes are a file each; smaller ones are packed.
pub const LARGE: usize = 16 << 10;
/// Default size limit: 2 GiB.
pub const DEFAULT_LIMIT: u64 = 2 << 30;
/// A hit refreshes its entry's time of use when that is older than this.
pub const TOUCH_EVERY: Duration = Duration::from_secs(60 * 60);
/// How often [`Cache::collect_garbage_if_due`] looks at the cache's size.
const GC_EVERY: Duration = Duration::from_secs(60 * 60);
/// Temporary files older than this are left over from a build that stopped while writing.
const STALE_TEMP: Duration = Duration::from_secs(60 * 60);

/// A key: the SHA-256 of what an entry depends on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key(pub [u8; 32]);

impl Key {
    /// The key of `parts`, each length-prefixed (so `["ab", "c"]` and `["a", "bc"]` differ).
    pub fn of(parts: &[&[u8]]) -> Key {
        let mut h = KeyHasher::new();
        for p in parts {
            h.part(p);
        }
        h.finish()
    }

    /// Lowercase hex.
    pub fn hex(&self) -> String {
        hex(&self.0)
    }
}

/// Builds a [`Key`] from parts.
#[derive(Clone, Default)]
pub struct KeyHasher(Sha256);

impl KeyHasher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one part, length-prefixed.
    pub fn part(&mut self, bytes: &[u8]) -> &mut Self {
        self.0.update((bytes.len() as u64).to_le_bytes());
        self.0.update(bytes);
        self
    }

    pub fn finish(self) -> Key {
        Key(self.0.finalize().into())
    }
}

/// Entries and the disk space they take.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// Entries in the cache.
    pub entries: u64,
    /// Bytes its files take on disk.
    pub bytes: u64,
}

/// The cache under one root directory.
pub struct Cache {
    /// `<root>`.
    root: PathBuf,
    /// `<root>/v3`.
    dir: PathBuf,
    /// The size limit in bytes.
    limit: u64,
    /// Bytes this process stored.
    written: AtomicU64,
    /// The packed entries of each kind used so far.
    packs: Mutex<HashMap<String, Arc<Packs>>>,
    /// This process made sure the root has its marker.
    marked: std::sync::atomic::AtomicBool,
}

impl std::fmt::Debug for Cache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cache")
            .field("dir", &self.dir)
            .field("limit", &self.limit)
            .finish_non_exhaustive()
    }
}

impl Cache {
    /// The cache under `root` (created on the first write), kept under `limit` bytes.
    pub fn new(root: &Path, limit: u64) -> Cache {
        Cache {
            root: root.to_path_buf(),
            dir: root.join(LAYOUT),
            limit,
            written: AtomicU64::new(0),
            packs: Mutex::default(),
            marked: Default::default(),
        }
    }

    /// The size limit in bytes.
    pub fn limit(&self) -> u64 {
        self.limit
    }

    /// The entry of `kind` under `key` (a raw key: hashed when not 32 bytes long).
    pub fn get(&self, kind: &str, key: &[u8]) -> Option<Vec<u8>> {
        self.get_key(kind, &raw_key(key))
    }

    /// The entry of `kind` under `key`, marking it used.
    pub fn get_key(&self, kind: &str, key: &Key) -> Option<Vec<u8>> {
        self.packs(kind)
            .get(key)
            .or_else(|| files::get(&self.large_dir(kind), key))
    }

    /// Store `value` as the entry of `kind` under `key` (a raw key: hashed when not 32 bytes).
    pub fn put(&self, kind: &str, key: &[u8], value: &[u8]) {
        self.put_key(kind, &raw_key(key), value);
    }

    /// Store `value` as `key`'s entry of `kind`; `false` if it could not be stored.
    pub fn put_key(&self, kind: &str, key: &Key, value: &[u8]) -> bool {
        self.mark();
        let stored = if value.len() < LARGE {
            self.packs(kind).put(key, value)
        } else {
            files::put(&self.large_dir(kind), key, value)
        };
        if stored {
            self.written
                .fetch_add(value.len() as u64, Ordering::Relaxed);
        }
        stored
    }

    /// Write the root's marker, once per process, before the first entry.
    fn mark(&self) {
        if self.marked.swap(true, Ordering::Relaxed) {
            return;
        }
        let marker = self.root.join(MARKER);
        if !marker.exists() {
            let _ = fs::create_dir_all(&self.root);
            let _ = fs::write(marker, MARKER_TEXT);
        }
    }

    /// Whether the root is a build cache (has its marker).
    pub fn is_cache(&self) -> bool {
        self.root.join(MARKER).is_file()
    }

    /// Bytes this process stored.
    pub fn written(&self) -> u64 {
        self.written.load(Ordering::Relaxed)
    }

    fn packs(&self, kind: &str) -> Arc<Packs> {
        let mut packs = self.packs.lock().unwrap_or_else(PoisonError::into_inner);
        let packs = packs
            .entry(kind.to_string())
            .or_insert_with(|| Arc::new(Packs::new(self.dir.join(kind))));
        Arc::clone(packs)
    }

    fn large_dir(&self, kind: &str) -> PathBuf {
        self.dir.join(kind).join("large")
    }

    /// The kinds on disk.
    fn kinds(&self) -> Vec<String> {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return vec![];
        };
        entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect()
    }

    /// When this process stored anything and the last look at the cache's size was more than
    /// an hour ago (or the process alone stored a tenth of the limit): [`Self::collect_garbage`].
    pub fn collect_garbage_if_due(&self) -> Option<(Usage, Usage)> {
        let written = self.written();
        if written == 0 {
            return None;
        }
        let stamp = self.dir.join("gc-stamp");
        let last = fs::metadata(&stamp).and_then(|m| m.modified()).ok();
        let due = last.is_none_or(|t| t.elapsed().unwrap_or_default() > GC_EVERY)
            || written > self.limit / 10;
        if !due {
            return None;
        }
        let _ = fs::write(&stamp, b"");
        Some(self.collect_garbage(self.limit))
    }

    /// Remove other layout versions and leftover temporary files; then, if the cache takes more
    /// than `limit` bytes, the entries used least recently until it takes at most three quarters
    /// of it. Packs that hold much unreferenced data are rewritten. Returns the usage before and
    /// after.
    pub fn collect_garbage(&self, limit: u64) -> (Usage, Usage) {
        if !self.is_cache() {
            return (Usage::default(), Usage::default());
        }
        self.remove_other_layouts();
        let before = self.usage();
        // (time of last use in minutes, bytes) of every entry.
        let mut items: Vec<(u32, u64)> = Vec::new();
        let now = SystemTime::now();
        for kind in self.kinds() {
            // A shard rewrite that stopped (or whose rename failed) leaves its temporary files.
            for path in files::files_in(&self.dir.join(&kind)) {
                let old = fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .is_ok_and(|t| now.duration_since(t).unwrap_or_default() > STALE_TEMP);
                if files::is_temp(&path) && old {
                    let _ = fs::remove_file(&path);
                }
            }
            for (_, entries) in self.packs(&kind).scan() {
                let record = pack::RECORD as u64;
                let sizes = entries
                    .values()
                    .map(|e| (e.time, u64::from(e.len) + record));
                items.extend(sizes);
            }
            for path in files::files_under(&self.large_dir(&kind)) {
                let Ok(meta) = fs::metadata(&path) else {
                    continue;
                };
                let used = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                if files::is_temp(&path) {
                    if now.duration_since(used).unwrap_or_default() > STALE_TEMP {
                        let _ = fs::remove_file(&path);
                    }
                    continue;
                }
                items.push((minutes(used), files::disk_size(&meta)));
            }
        }
        let mut cutoff = 0;
        if before.bytes > limit {
            // What the files take beyond their entries (partly used blocks, index files) stays
            // whatever is removed, so the entries must fit in the rest.
            let mut left: u64 = items.iter().map(|i| i.1).sum();
            let overhead = before.bytes.saturating_sub(left);
            let target = (limit / 4 * 3).saturating_sub(overhead);
            items.sort_unstable();
            for (time, size) in items {
                if left <= target {
                    break;
                }
                left = left.saturating_sub(size);
                cutoff = time + 1;
            }
        }
        for kind in self.kinds() {
            let packs = self.packs(&kind);
            for n in 0..pack::SHARDS {
                let _ = packs.compact(n, cutoff);
            }
            for path in files::files_under(&self.large_dir(&kind)) {
                let used = fs::metadata(&path).and_then(|m| m.modified());
                if used.is_ok_and(|t| minutes(t) < cutoff) {
                    let _ = fs::remove_file(&path);
                }
            }
        }
        (before, self.usage())
    }

    /// Directories of other layout versions under the root (`v1`, ...).
    fn remove_other_layouts(&self) {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if is_layout(&name) && name != LAYOUT && e.file_type().is_ok_and(|t| t.is_dir()) {
                let _ = fs::remove_dir_all(e.path());
            }
        }
    }

    /// Entries and the disk space of the cache.
    pub fn usage(&self) -> Usage {
        let mut usage = Usage::default();
        for kind in self.kinds() {
            let packs = self.packs(&kind);
            usage.bytes += packs.disk_bytes();
            usage.entries += packs
                .scan()
                .iter()
                .map(|(_, e)| e.len() as u64)
                .sum::<u64>();
            for path in files::files_under(&self.large_dir(&kind)) {
                if files::is_temp(&path) {
                    continue;
                }
                if let Ok(meta) = fs::metadata(&path) {
                    usage.entries += 1;
                    usage.bytes += files::disk_size(&meta);
                }
            }
        }
        usage
    }

    /// Remove every entry (all layout versions) and what else the cache made in its root, then
    /// the root if nothing else is left in it. Refuses a root without the cache's marker: it may
    /// be any directory (`$VELT_BUILD_CACHE`), and its files are not the cache's to delete.
    pub fn clear(&self) -> std::io::Result<()> {
        if !self.root.exists() {
            return Ok(());
        }
        if !self.is_cache() {
            return Err(std::io::Error::other(format!(
                "{} is not a build cache (it has no {MARKER}); not removing anything",
                self.root.display()
            )));
        }
        for e in fs::read_dir(&self.root)?.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !(is_layout(&name) || OWNED.contains(&name.as_str())) {
                continue;
            }
            match e.file_type()?.is_dir() {
                true => fs::remove_dir_all(e.path())?,
                false => fs::remove_file(e.path())?,
            }
        }
        let _ = fs::remove_dir(&self.root);
        self.packs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        self.marked.store(false, Ordering::Relaxed);
        Ok(())
    }
}

/// Whether `name` is a layout directory's (`v1`, `v2`, ...).
fn is_layout(name: &str) -> bool {
    name.len() > 1 && name.starts_with('v') && name[1..].bytes().all(|b| b.is_ascii_digit())
}

/// The first 8 bytes of the value's SHA-256.
fn checksum(value: &[u8]) -> u64 {
    let key = Key::of(&[value]);
    u64::from_le_bytes(key.0[..8].try_into().unwrap_or_default())
}

/// `key` as a [`Key`]: itself when it is 32 bytes (already a hash), else its hash.
fn raw_key(key: &[u8]) -> Key {
    match <[u8; 32]>::try_from(key) {
        Ok(k) => Key(k),
        Err(_) => Key::of(&[key]),
    }
}

/// Minutes since 1970.
fn minutes(t: SystemTime) -> u32 {
    let secs = t
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    (secs / 60) as u32
}

/// Lowercase hex of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(DIGITS[usize::from(b >> 4)] as char);
        s.push(DIGITS[usize::from(b & 15)] as char);
    }
    s
}

/// A size such as `500M`, `2G`, `1.5GB`, `0` or `1048576` (bytes) in bytes; `None` if it is not
/// one.
pub fn parse_size(text: &str) -> Option<u64> {
    let t = text.trim().to_ascii_uppercase();
    let t = t.strip_suffix('B').unwrap_or(&t);
    let (num, unit) = match t.char_indices().find(|(_, c)| c.is_ascii_alphabetic()) {
        Some((i, _)) => (&t[..i], &t[i..]),
        None => (t, ""),
    };
    let scale: u64 = match unit {
        "" => 1,
        "K" | "KI" => 1 << 10,
        "M" | "MI" => 1 << 20,
        "G" | "GI" => 1 << 30,
        "T" | "TI" => 1 << 40,
        _ => return None,
    };
    let n: f64 = num.trim().parse().ok()?;
    (n.is_finite() && n >= 0.0).then_some((n * scale as f64) as u64)
}

#[cfg(test)]
mod tests;
