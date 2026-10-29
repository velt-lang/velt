use std::time::{Duration, SystemTime};

use super::*;

fn small(i: u32) -> Vec<u8> {
    vec![i as u8; 100 + i as usize]
}

#[test]
fn stores_and_finds_small_and_large_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = Cache::new(tmp.path(), DEFAULT_LIMIT);
    let key = Key::of(&[b"code", b"target"]);
    assert_eq!(cache.get_key("cl", &key), None);
    assert!(cache.put_key("cl", &key, b"compiled"));
    assert_eq!(cache.get_key("cl", &key).as_deref(), Some(&b"compiled"[..]));
    // Kinds are separate.
    assert_eq!(cache.get_key("llvm", &key), None);
    let large = vec![7; LARGE + 1];
    assert!(cache.put_key("llvm", &key, &large));
    assert_eq!(cache.get_key("llvm", &key), Some(large));
    assert_eq!(cache.written(), 8 + LARGE as u64 + 1);
    // Raw keys: a 32-byte one is used as it is, others hashed.
    cache.put("cl", b"short", b"y");
    assert_eq!(cache.get("cl", b"short").as_deref(), Some(&b"y"[..]));
    // Another process (a new `Cache`) sees them.
    let other = Cache::new(tmp.path(), DEFAULT_LIMIT);
    assert_eq!(other.get_key("cl", &key).as_deref(), Some(&b"compiled"[..]));
    assert_eq!(other.usage().entries, 3);
}

#[test]
fn small_entries_share_a_few_files() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = Cache::new(tmp.path(), DEFAULT_LIMIT);
    for i in 0..2000u32 {
        cache.put_key("cl", &Key::of(&[&i.to_le_bytes()]), &small(i % 200));
    }
    let files = files::files_under(tmp.path()).len();
    // 64 shards × (pack, index, lock) at most, and the marker.
    assert!(files <= 3 * pack::SHARDS + 1, "{files} files");
    let usage = cache.usage();
    assert_eq!(usage.entries, 2000);
    let data: u64 = (0..2000).map(|i| small(i % 200).len() as u64).sum();
    assert!(usage.bytes < data * 2, "{usage:?} for {data} bytes of data");
}

#[test]
fn writers_in_parallel_lose_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    std::thread::scope(|scope| {
        for t in 0..4u32 {
            let root = tmp.path();
            scope.spawn(move || {
                // One `Cache` per thread: as separate processes, they share only the files.
                let cache = Cache::new(root, DEFAULT_LIMIT);
                for i in 0..300u32 {
                    let n = t * 1000 + i;
                    cache.put_key("cl", &Key::of(&[&n.to_le_bytes()]), &small(n % 50));
                }
            });
        }
    });
    let cache = Cache::new(tmp.path(), DEFAULT_LIMIT);
    for t in 0..4u32 {
        for i in 0..300u32 {
            let n = t * 1000 + i;
            let found = cache.get_key("cl", &Key::of(&[&n.to_le_bytes()]));
            assert_eq!(found, Some(small(n % 50)), "entry {n}");
        }
    }
}

#[test]
fn a_damaged_pack_is_a_miss_not_wrong_code() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = Cache::new(tmp.path(), DEFAULT_LIMIT);
    let key = Key([3; 32]);
    cache.put_key("cl", &key, b"original value");
    let pack = tmp.path().join(LAYOUT).join("cl/03.pack");
    std::fs::write(&pack, b"different bytes").unwrap();
    assert_eq!(
        Cache::new(tmp.path(), DEFAULT_LIMIT).get_key("cl", &key),
        None
    );
    // A cut-off index record is ignored, and later appends still line up.
    let idx = tmp.path().join(LAYOUT).join("cl/03.idx");
    let mut bytes = std::fs::read(&idx).unwrap();
    bytes.extend_from_slice(&[1, 2, 3]);
    std::fs::write(&idx, bytes).unwrap();
    let fresh = Cache::new(tmp.path(), DEFAULT_LIMIT);
    // Same shard (first byte), another key.
    let mut other = [3; 32];
    other[1] = 9;
    let other = Key(other);
    fresh.put_key("cl", &other, b"next");
    assert_eq!(
        Cache::new(tmp.path(), DEFAULT_LIMIT)
            .get_key("cl", &other)
            .as_deref(),
        Some(&b"next"[..])
    );
}

/// A packed entry that fails its checksum is stored again by the next `put` (the reviewer's
/// repro: every 97th byte of the packs flipped left builds missing those entries forever).
#[test]
fn a_damaged_packed_entry_is_stored_again() {
    let tmp = tempfile::tempdir().unwrap();
    let key = Key([5; 32]);
    Cache::new(tmp.path(), DEFAULT_LIMIT).put_key("cl", &key, b"original value");
    let pack = tmp.path().join(LAYOUT).join("cl/05.pack");
    let mut bytes = std::fs::read(&pack).unwrap();
    bytes[3] ^= 0xff;
    std::fs::write(&pack, bytes).unwrap();
    let cache = Cache::new(tmp.path(), DEFAULT_LIMIT);
    assert_eq!(cache.get_key("cl", &key), None);
    assert!(cache.put_key("cl", &key, b"original value"));
    assert_eq!(
        cache.get_key("cl", &key).as_deref(),
        Some(&b"original value"[..])
    );
    let fresh = Cache::new(tmp.path(), DEFAULT_LIMIT);
    assert_eq!(
        fresh.get_key("cl", &key).as_deref(),
        Some(&b"original value"[..])
    );
}

/// A record claiming more than a packed value can hold (up to 4 GiB) is a miss, not an
/// allocation that aborts the build.
#[test]
fn a_damaged_record_length_is_a_miss() {
    let tmp = tempfile::tempdir().unwrap();
    let key = Key([6; 32]);
    Cache::new(tmp.path(), DEFAULT_LIMIT).put_key("cl", &key, b"value");
    let idx = tmp.path().join(LAYOUT).join("cl/06.idx");
    let mut bytes = std::fs::read(&idx).unwrap();
    bytes[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
    std::fs::write(&idx, bytes).unwrap();
    assert_eq!(
        Cache::new(tmp.path(), DEFAULT_LIMIT).get_key("cl", &key),
        None
    );
}

/// A large entry left empty (a power loss after the rename) or with a flipped byte is a miss,
/// is removed, and is stored again.
#[test]
fn a_damaged_large_entry_is_a_miss_and_stored_again() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = Cache::new(tmp.path(), DEFAULT_LIMIT);
    let value = vec![9; LARGE + 10];
    let (truncated, flipped) = (Key([1; 32]), Key([2; 32]));
    for key in [truncated, flipped] {
        assert!(cache.put_key("llvm", &key, &value));
    }
    let path = |key: &Key| files::path(&cache.large_dir("llvm"), key);
    std::fs::write(path(&truncated), b"").unwrap();
    let mut bytes = std::fs::read(path(&flipped)).unwrap();
    bytes[files::HEADER + 100] ^= 1;
    std::fs::write(path(&flipped), bytes).unwrap();
    for key in [truncated, flipped] {
        let cache = Cache::new(tmp.path(), DEFAULT_LIMIT);
        assert_eq!(cache.get_key("llvm", &key), None);
        assert!(cache.put_key("llvm", &key, &value));
        assert_eq!(cache.get_key("llvm", &key), Some(value.clone()));
    }
    // `put` replaces a damaged file it finds without a `get` before it, too.
    std::fs::write(path(&truncated), b"x").unwrap();
    assert!(cache.put_key("llvm", &truncated, &value));
    assert_eq!(cache.get_key("llvm", &truncated), Some(value));
}

/// Set the time of use of every entry: packed ones through a record, large ones by their file.
fn age(cache: &Cache, kind: &str, key: &Key, hours_ago: u64) {
    let when = SystemTime::now() - Duration::from_secs(3600 * hours_ago);
    let large = files::path(&cache.large_dir(kind), key);
    if large.exists() {
        let f = std::fs::OpenOptions::new().write(true).open(large).unwrap();
        f.set_modified(when).unwrap();
    } else {
        cache.packs(kind).set_time(key, minutes(when));
    }
}

#[test]
fn garbage_collection_removes_the_least_recently_used() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = Cache::new(tmp.path(), DEFAULT_LIMIT);
    let keys: Vec<Key> = (0..10u8).map(|i| Key::of(&[&[i]])).collect();
    for (i, key) in keys.iter().enumerate() {
        // Half small, half large; entry i was used 20 - i hours ago.
        let value = if i % 2 == 0 {
            vec![1; LARGE - 1]
        } else {
            vec![2; 4 * LARGE]
        };
        cache.put_key("k", key, &value);
        age(&cache, "k", key, 20 - i as u64);
    }
    let (before, after) = cache.collect_garbage(u64::MAX);
    assert_eq!(after, before, "under the limit: nothing removed");
    assert_eq!(before.entries, 10);
    let (_, after) = cache.collect_garbage(before.bytes / 2);
    assert!(
        after.bytes < before.bytes && after.entries < 10,
        "{after:?}"
    );
    let fresh = Cache::new(tmp.path(), DEFAULT_LIMIT);
    let left: Vec<bool> = keys
        .iter()
        .map(|k| fresh.get_key("k", k).is_some())
        .collect();
    let first_kept = left.iter().position(|&l| l).unwrap();
    assert!(
        first_kept > 0 && left[first_kept..].iter().all(|&l| l),
        "{left:?}"
    );

    // A hit marks an entry used (as the reads above did): it then outlives newer ones.
    for (i, key) in keys.iter().enumerate().skip(first_kept) {
        age(&fresh, "k", key, 20 - i as u64);
    }
    let oldest_kept = keys[first_kept];
    age(&fresh, "k", &oldest_kept, 30);
    assert!(fresh.get_key("k", &oldest_kept).is_some());
    let usage = fresh.usage();
    let (_, after) = fresh.collect_garbage(usage.bytes * 2 / 3);
    assert!(after.entries < usage.entries, "{after:?}");
    let fresh = Cache::new(tmp.path(), DEFAULT_LIMIT);
    assert!(fresh.get_key("k", &oldest_kept).is_some(), "used last");
    fresh.clear().unwrap();
    assert_eq!(fresh.usage().entries, 0);
}

#[test]
fn collection_removes_old_layouts() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("v1/cl/ab")).unwrap();
    std::fs::write(tmp.path().join("v1/cl/ab/cd"), b"old").unwrap();
    let cache = Cache::new(tmp.path(), DEFAULT_LIMIT);
    cache.put("k", b"a", b"1");
    cache.collect_garbage(DEFAULT_LIMIT);
    assert!(!tmp.path().join("v1").exists());
    assert_eq!(cache.get("k", b"a").as_deref(), Some(&b"1"[..]));
}

/// `clear` and garbage collection touch only what the cache made: a directory without the
/// marker is refused whole, and other files beside the cache's are kept.
#[test]
fn clear_and_collection_remove_only_the_caches_own_files() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("cache");
    std::fs::create_dir_all(root.join("v1")).unwrap();
    std::fs::write(root.join("notes.txt"), b"mine").unwrap();
    std::fs::write(root.join("v1/x"), b"mine too").unwrap();
    let cache = Cache::new(&root, DEFAULT_LIMIT);
    assert!(cache.clear().is_err(), "no marker: not a cache");
    cache.collect_garbage(0);
    assert!(root.join("notes.txt").exists() && root.join("v1/x").exists());

    // A directory with other files in it is used, but never adopted.
    cache.put("k", b"a", b"1");
    assert_eq!(cache.get("k", b"a").as_deref(), Some(&b"1"[..]));
    assert!(!root.join(MARKER).exists());
    assert!(cache.clear().is_err());
    assert!(root.join("v1/x").exists());

    // Adopted, a root keeps what the cache did not make when it is cleared.
    let shared = tmp.path().join("shared");
    let cache = Cache::new(&shared, DEFAULT_LIMIT);
    cache.put("k", b"a", b"1");
    assert!(shared.join(MARKER).exists());
    std::fs::write(shared.join("notes.txt"), b"mine").unwrap();
    cache.clear().unwrap();
    assert_eq!(cache.get("k", b"a"), None);
    assert!(
        shared.join("notes.txt").exists(),
        "a file the cache did not make"
    );
    assert!(!shared.join(MARKER).exists() && !shared.join(LAYOUT).exists());
    // An empty root goes too.
    let alone = tmp.path().join("alone");
    let cache = Cache::new(&alone, DEFAULT_LIMIT);
    cache.put("k", b"a", b"1");
    cache.clear().unwrap();
    assert!(!alone.exists());
    cache.clear().unwrap();
}

/// File entries are plain files handed out by path, checked before that: a damaged one is a
/// miss and is stored again. Garbage collection counts and removes them like other entries.
#[test]
fn file_entries_are_handed_out_by_path() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = Cache::new(&tmp.path().join("cache"), DEFAULT_LIMIT);
    let src = tmp.path().join("std.o");
    let object = vec![0x7f; 3 * LARGE];
    std::fs::write(&src, &object).unwrap();
    let key = Key([4; 32]);
    assert_eq!(cache.path_of("std", &key), None);
    assert!(cache.put_file("std", &key, &src));
    let path = cache.path_of("std", &key).unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        object,
        "the file as it was, no header"
    );
    assert_eq!(cache.usage().entries, 1);

    // Damaged: a miss, removed, then stored again.
    std::fs::write(&path, b"truncated").unwrap();
    assert_eq!(cache.path_of("std", &key), None);
    assert!(!path.exists());
    assert!(cache.put_file("std", &key, &src));
    assert_eq!(cache.path_of("std", &key), Some(path.clone()));

    // `touch` keeps a used file over an older one.
    let old = Key([5; 32]);
    assert!(cache.put_file("std", &old, &src));
    let long_ago = SystemTime::now() - Duration::from_secs(3600 * 30);
    for k in [key, old] {
        let p = files::sidecar(&files::path(&cache.files_dir("std"), &k));
        let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
        f.set_modified(long_ago).unwrap();
    }
    cache.touch("std", &key);
    let usage = cache.usage();
    // Room for one of the two entries.
    cache.collect_garbage(usage.bytes * 9 / 10);
    assert!(cache.path_of("std", &key).is_some(), "touched");
    assert_eq!(cache.path_of("std", &old), None, "used longest ago");
    assert!(!files::sidecar(&files::path(&cache.files_dir("std"), &old)).exists());
}

/// A file entry's own size and time stay as they were stored while it is used: a stamp that
/// names it as an input stays valid. The time of use is its sidecar's.
#[test]
fn using_a_file_entry_leaves_the_file_as_it_is() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = Cache::new(&tmp.path().join("cache"), DEFAULT_LIMIT);
    let src = tmp.path().join("member.o");
    std::fs::write(&src, vec![1; LARGE]).unwrap();
    let key = Key([6; 32]);
    assert!(cache.put_file("std", &key, &src));
    let path = cache.path_of("std", &key).unwrap();
    let long_ago = SystemTime::now() - Duration::from_secs(3600 * 30);
    for p in [path.clone(), files::sidecar(&path)] {
        let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
        f.set_modified(long_ago).unwrap();
    }
    assert!(cache.path_of("std", &key).is_some());
    cache.touch("std", &key);
    let time = |p: &std::path::Path| std::fs::metadata(p).unwrap().modified().unwrap();
    assert_eq!(time(&path), long_ago, "the file keeps its time");
    assert!(
        time(&files::sidecar(&path)) > long_ago,
        "its use is on the sidecar"
    );
}

/// A reader looking up entries while a writer stores them never removes one half-way through:
/// every entry stored is there afterwards.
#[test]
fn a_reader_during_a_store_does_not_undo_it() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("cache");
    let src = tmp.path().join("member.o");
    std::fs::write(&src, vec![7; 4 * LARGE]).unwrap();
    let keys: Vec<Key> = (0..200u32).map(|i| Key::of(&[&i.to_le_bytes()])).collect();
    let done = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            // One `Cache` per thread: as separate processes, they share only the files.
            let cache = Cache::new(&root, DEFAULT_LIMIT);
            for key in &keys {
                assert!(cache.put_file("std", key, &src));
            }
            done.store(true, Ordering::Relaxed);
        });
        scope.spawn(|| {
            let cache = Cache::new(&root, DEFAULT_LIMIT);
            while !done.load(Ordering::Relaxed) {
                for key in &keys {
                    if let Some(p) = cache.path_of("std", key) {
                        assert_eq!(std::fs::read(p).unwrap().len(), 4 * LARGE);
                    }
                }
            }
        });
    });
    let cache = Cache::new(&root, DEFAULT_LIMIT);
    let missing = keys
        .iter()
        .filter(|k| cache.path_of("std", k).is_none())
        .count();
    assert_eq!(missing, 0, "entries undone by the reader");
}

#[test]
fn collects_only_when_due() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = Cache::new(tmp.path(), DEFAULT_LIMIT);
    assert_eq!(cache.collect_garbage_if_due(), None, "nothing written");
    cache.put("k", b"a", b"1");
    assert!(cache.collect_garbage_if_due().is_some());
    cache.put("k", b"b", b"2");
    assert_eq!(cache.collect_garbage_if_due(), None, "collected recently");
}

#[test]
fn keys_tell_parts_apart() {
    assert_ne!(Key::of(&[b"ab", b"c"]), Key::of(&[b"a", b"bc"]));
    assert_eq!(Key::of(&[b"a"]), Key::of(&[b"a"]));
    assert_eq!(Key([0xab; 32]).hex().len(), 64);
}

#[test]
fn sizes() {
    assert_eq!(parse_size("500M"), Some(500 << 20));
    assert_eq!(parse_size("2G"), Some(2 << 30));
    assert_eq!(parse_size("1.5gb"), Some(3 << 29));
    assert_eq!(parse_size("1024"), Some(1024));
    assert_eq!(parse_size("0"), Some(0));
    assert_eq!(parse_size("lots"), None);
    assert_eq!(parse_size("-1G"), None);
}
