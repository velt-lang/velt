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
    // 64 shards × (pack, index, lock) at most.
    assert!(files <= 3 * pack::SHARDS, "{files} files");
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
