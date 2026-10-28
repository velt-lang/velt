//! Small entries, packed: each kind's entries are spread over [`SHARDS`] shards by the first byte
//! of their key, and a shard is two append-only files:
//! - `<nn>.pack`: the values, one after the other;
//! - `<nn>.idx`: one [`RECORD`]-byte record per value stored or used: its key, where it is in the
//!   pack, its length, a checksum of it, and when it was stored or last used (minutes since 1970).
//!   The last record of a key wins.
//!
//! One file per entry wasted half the disk: most compiled functions are a few hundred bytes to a
//! few kilobytes, and every file takes at least a whole block.
//!
//! Processes append under an exclusive lock of `<nn>.lock`, a file never replaced. Garbage
//! collection rewrites a shard under the same lock, renaming the new pack, then the new index, into
//! place. Reads take no lock: a reader whose index and pack come from different rewrites (or that
//! sees half of an append) finds a checksum that does not match, which is a miss, never wrong code.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::SystemTime;

use crate::{checksum, Key, LARGE, TOUCH_EVERY};

/// Shards per kind.
pub(crate) const SHARDS: usize = 64;
/// Bytes per index record: key (32), offset (8), length (4), checksum (8), time (4).
pub(crate) const RECORD: usize = 56;

/// Where a value is, and when it was last used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub offset: u64,
    pub len: u32,
    pub check: u64,
    /// Minutes since 1970.
    pub time: u32,
}

/// One shard as this process knows it.
#[derive(Default)]
struct Shard {
    /// The index, read on first use (later appends by other processes are not seen: misses).
    entries: Option<HashMap<[u8; 32], Entry>>,
    /// The pack, opened on first read.
    pack: Option<File>,
}

/// The packed entries of one kind.
pub(crate) struct Packs {
    dir: PathBuf,
    shards: Vec<Mutex<Shard>>,
}

impl Packs {
    pub(crate) fn new(dir: PathBuf) -> Packs {
        Packs {
            dir,
            shards: (0..SHARDS).map(|_| Mutex::default()).collect(),
        }
    }

    /// The value under `key`, if this process can see it; marks it used when its time is old.
    /// One that fails its check is forgotten, so that `put` stores it again (the last record of
    /// a key wins).
    pub(crate) fn get(&self, key: &Key) -> Option<Vec<u8>> {
        let n = shard_of(key);
        let mut shard = self.shards[n]
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let entries = shard
            .entries
            .get_or_insert_with(|| read_index(&self.idx(n)).unwrap_or_default());
        let entry = *entries.get(&key.0)?;
        if shard.pack.is_none() {
            shard.pack = File::open(self.pack(n)).ok();
        }
        // Packed values are under `LARGE` bytes: a damaged record asks for no more.
        let value = (entry.len as usize) < LARGE;
        let value = value
            .then(|| vec![0; entry.len as usize])
            .and_then(|mut value| {
                read_at(shard.pack.as_ref()?, &mut value, entry.offset).ok()?;
                (checksum(&value) == entry.check).then_some(value)
            });
        let Some(value) = value else {
            if let Some(entries) = &mut shard.entries {
                entries.remove(&key.0);
            }
            return None;
        };
        let now = now_minutes();
        if now.saturating_sub(entry.time) * 60 > TOUCH_EVERY.as_secs() as u32 {
            let touched = Entry { time: now, ..entry };
            if self.append(n, None, &[(key.0, touched)]).is_ok() {
                if let Some(entries) = &mut shard.entries {
                    entries.insert(key.0, touched);
                }
            }
        }
        Some(value)
    }

    /// Store `value` under `key` (nothing to do when this process already sees it).
    pub(crate) fn put(&self, key: &Key, value: &[u8]) -> bool {
        let n = shard_of(key);
        let mut shard = self.shards[n]
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let entries = shard
            .entries
            .get_or_insert_with(|| read_index(&self.idx(n)).unwrap_or_default());
        if entries.contains_key(&key.0) {
            return true;
        }
        let entry = Entry {
            offset: 0, // Set by `append`, under the lock.
            len: value.len() as u32,
            check: checksum(value),
            time: now_minutes(),
        };
        match self.append(n, Some(value), &[(key.0, entry)]) {
            Ok(entry) => {
                if let Some(entries) = &mut shard.entries {
                    entries.insert(key.0, entry);
                }
                true
            }
            Err(_) => false,
        }
    }

    /// Under the shard's lock: append `value` (if any) to the pack, then `records` to the index
    /// (a stored value's record gets its offset). Returns the last record written.
    fn append(
        &self,
        n: usize,
        value: Option<&[u8]>,
        records: &[([u8; 32], Entry)],
    ) -> std::io::Result<Entry> {
        fs::create_dir_all(&self.dir)?;
        let _lock = self.lock(n)?;
        let mut last = records[0].1;
        let mut out = Vec::with_capacity(records.len() * RECORD);
        let offset = match value {
            Some(value) => {
                let mut pack = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(self.pack(n))?;
                let offset = pack.metadata()?.len();
                pack.write_all(value)?;
                Some(offset)
            }
            None => None,
        };
        for (key, entry) in records {
            last = Entry {
                offset: offset.unwrap_or(entry.offset),
                ..*entry
            };
            encode(&mut out, key, &last);
        }
        // Opened for writing, not appending: on Windows, an append-only handle cannot truncate.
        let mut idx = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.idx(n))?;
        // A record cut short by a crash would misalign every later one.
        let mut len = idx.metadata()?.len();
        if len % RECORD as u64 != 0 {
            len -= len % RECORD as u64;
            idx.set_len(len)?;
        }
        idx.seek(SeekFrom::Start(len))?;
        idx.write_all(&out)?;
        Ok(last)
    }

    /// The live entries of every shard (for garbage collection and usage).
    pub(crate) fn scan(&self) -> Vec<(usize, HashMap<[u8; 32], Entry>)> {
        (0..SHARDS)
            .filter_map(|n| read_index(&self.idx(n)).map(|e| (n, e)))
            .collect()
    }

    /// Bytes the shard files take.
    pub(crate) fn disk_bytes(&self) -> u64 {
        (0..SHARDS)
            .flat_map(|n| [self.pack(n), self.idx(n)])
            .filter_map(|p| fs::metadata(p).ok())
            .map(|m| crate::files::disk_size(&m))
            .sum()
    }

    /// Under the shard's lock: rewrite shard `n` keeping its entries used at or after `cutoff`
    /// (minutes), when that removes any or when a quarter of its pack is no longer referenced.
    /// Returns whether it was rewritten.
    pub(crate) fn compact(&self, n: usize, cutoff: u32) -> std::io::Result<bool> {
        if !self.idx(n).exists() {
            return Ok(false);
        }
        let _lock = self.lock(n)?;
        let entries = read_index(&self.idx(n)).unwrap_or_default();
        let pack_len = fs::metadata(self.pack(n)).map_or(0, |m| m.len());
        let keep: Vec<_> = entries
            .iter()
            .filter(|(_, e)| e.time >= cutoff && e.offset + u64::from(e.len) <= pack_len)
            .collect();
        let live: u64 = keep.iter().map(|(_, e)| u64::from(e.len)).sum();
        let idx_len = fs::metadata(self.idx(n)).map_or(0, |m| m.len());
        let wasted = pack_len - live.min(pack_len) > pack_len / 4
            || idx_len > (keep.len() * RECORD * 2) as u64;
        if keep.len() == entries.len() && !wasted {
            return Ok(false);
        }
        let mut old = File::open(self.pack(n))?;
        let mut old_bytes = Vec::with_capacity(pack_len as usize);
        old.read_to_end(&mut old_bytes)?;
        let (mut pack, mut idx) = (Vec::with_capacity(live as usize), Vec::new());
        for (key, e) in keep {
            let value = &old_bytes[e.offset as usize..(e.offset + u64::from(e.len)) as usize];
            if checksum(value) != e.check {
                continue;
            }
            let moved = Entry {
                offset: pack.len() as u64,
                ..*e
            };
            pack.extend_from_slice(value);
            encode(&mut idx, key, &moved);
        }
        let tmp_pack = self
            .dir
            .join(format!("{n:02}.pack.{}.tmp", std::process::id()));
        let tmp_idx = self
            .dir
            .join(format!("{n:02}.idx.{}.tmp", std::process::id()));
        fs::write(&tmp_pack, &pack)?;
        fs::write(&tmp_idx, &idx)?;
        fs::rename(&tmp_pack, self.pack(n))?;
        fs::rename(&tmp_idx, self.idx(n))?;
        // This process re-reads the shard on its next use.
        *self.shards[n]
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Shard::default();
        Ok(true)
    }

    /// Record `time` as the time of use of `key`'s entry (tests).
    #[cfg(test)]
    pub(crate) fn set_time(&self, key: &Key, time: u32) {
        let n = shard_of(key);
        let entry = read_index(&self.idx(n)).unwrap_or_default()[&key.0];
        self.append(n, None, &[(key.0, Entry { time, ..entry })])
            .unwrap();
        *self.shards[n]
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Shard::default();
    }

    fn lock(&self, n: usize) -> std::io::Result<File> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.dir.join(format!("{n:02}.lock")))?;
        file.lock()?;
        Ok(file)
    }

    fn pack(&self, n: usize) -> PathBuf {
        self.dir.join(format!("{n:02}.pack"))
    }

    fn idx(&self, n: usize) -> PathBuf {
        self.dir.join(format!("{n:02}.idx"))
    }
}

fn shard_of(key: &Key) -> usize {
    usize::from(key.0[0]) % SHARDS
}

/// The entries of an index file (the last record of each key), `None` if it cannot be read.
fn read_index(path: &Path) -> Option<HashMap<[u8; 32], Entry>> {
    let bytes = fs::read(path).ok()?;
    let mut entries = HashMap::with_capacity(bytes.len() / RECORD);
    for r in bytes.as_chunks::<RECORD>().0 {
        let mut key = [0; 32];
        key.copy_from_slice(&r[..32]);
        let entry = Entry {
            offset: u64::from_le_bytes(r[32..40].try_into().ok()?),
            len: u32::from_le_bytes(r[40..44].try_into().ok()?),
            check: u64::from_le_bytes(r[44..52].try_into().ok()?),
            time: u32::from_le_bytes(r[52..56].try_into().ok()?),
        };
        entries.insert(key, entry);
    }
    Some(entries)
}

fn encode(out: &mut Vec<u8>, key: &[u8; 32], e: &Entry) {
    out.extend_from_slice(key);
    out.extend_from_slice(&e.offset.to_le_bytes());
    out.extend_from_slice(&e.len.to_le_bytes());
    out.extend_from_slice(&e.check.to_le_bytes());
    out.extend_from_slice(&e.time.to_le_bytes());
}

fn now_minutes() -> u32 {
    crate::minutes(SystemTime::now())
}

#[cfg(unix)]
fn read_at(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
}

#[cfg(windows)]
fn read_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_read(buf, offset)? {
            0 => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            n => {
                buf = &mut buf[n..];
                offset += n as u64;
            }
        }
    }
    Ok(())
}
