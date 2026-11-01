use std::path::{Path, PathBuf};

use velt_build_cache::{Cache, KeyHasher};
use velt_common::FileId;

use super::*;

fn prefix(compiler: &str) -> KeyHasher {
    let mut h = KeyHasher::new();
    h.part(compiler.as_bytes());
    h
}

fn dump(m: &ast::Module) -> String {
    velt_syntax::dump(m)
}

fn fresh(src: &str, file: FileId) -> String {
    dump(&parse_source(file, src, false).0)
}

const A: &str = "export function f(a: i64): i64 {\n  return a + 1;\n}\n";
/// `A` edited without changing its length.
const B: &str = "export function g(a: i64): i64 {\n  return a - 1;\n}\n";

#[test]
fn a_hit_decodes_the_parse_for_this_build_s_file_id() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = Cache::new(tmp.path(), u64::MAX);
    let p = prefix("c1");
    let (cold, diags) = parse_cached(&cache, &p, FileId(2), A, false);
    assert!(diags.is_empty());
    assert_eq!(dump(&cold), fresh(A, FileId(2)));
    assert!(cache.get_key(KIND, &key(&p, A, false)).is_some(), "stored");
    // Another process (its own view of the cache), another file id.
    let other = Cache::new(tmp.path(), u64::MAX);
    let (warm, _) = parse_cached(&other, &p, FileId(7), A, false);
    assert_eq!(dump(&warm), fresh(A, FileId(7)));
}

#[test]
fn an_edited_file_another_compiler_or_plain_ts_is_another_key() {
    let p = prefix("c1");
    assert_ne!(key(&p, A, false), key(&p, B, false));
    assert_ne!(key(&p, A, false), key(&p, A, true));
    assert_ne!(key(&p, A, false), key(&prefix("c2"), A, false));
    let tmp = tempfile::tempdir().unwrap();
    let cache = Cache::new(tmp.path(), u64::MAX);
    parse_cached(&cache, &p, FileId(1), A, false);
    let (edited, _) = parse_cached(&cache, &p, FileId(1), B, false);
    assert_eq!(dump(&edited), fresh(B, FileId(1)));
}

#[test]
fn a_parse_with_errors_is_not_stored() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = Cache::new(tmp.path(), u64::MAX);
    let p = prefix("c1");
    let bad = "export function f( {\n";
    let (_, diags) = parse_cached(&cache, &p, FileId(1), bad, false);
    assert!(!diags.is_empty());
    assert!(cache.get_key(KIND, &key(&p, bad, false)).is_none());
    // Reported again on the next build.
    let (_, again) = parse_cached(&cache, &p, FileId(1), bad, false);
    assert_eq!(again.len(), diags.len());
}

#[test]
fn an_entry_that_does_not_decode_is_parsed_and_stored_again() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = Cache::new(tmp.path(), u64::MAX);
    let p = prefix("c1");
    let k = key(&p, A, false);
    // A value of another format under the key (the cache's own check passes).
    cache.put_key(KIND, &k, b"not an ast");
    let (m, _) = parse_cached(&cache, &p, FileId(1), A, false);
    assert_eq!(dump(&m), fresh(A, FileId(1)));
}

/// Every file under `dir` with extension `ext`.
fn files_in(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = e.path();
        if path.is_dir() {
            files_in(&path, ext, out);
        } else if path.extension().is_some_and(|x| x == ext) {
            out.push(path);
        }
    }
}

#[test]
fn a_damaged_entry_on_disk_is_a_miss() {
    let tmp = tempfile::tempdir().unwrap();
    let p = prefix("c1");
    parse_cached(&Cache::new(tmp.path(), u64::MAX), &p, FileId(1), A, false);
    let mut packs = vec![];
    files_in(tmp.path(), "pack", &mut packs);
    assert_eq!(packs.len(), 1, "{packs:?}");
    let mut bytes = std::fs::read(&packs[0]).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0x20;
    std::fs::write(&packs[0], &bytes).unwrap();
    let cache = Cache::new(tmp.path(), u64::MAX);
    assert!(
        cache.get_key(KIND, &key(&p, A, false)).is_none(),
        "fails its check"
    );
    let (m, _) = parse_cached(&cache, &p, FileId(1), A, false);
    assert_eq!(dump(&m), fresh(A, FileId(1)));
}

fn std_sources() -> Vec<(String, bool)> {
    let std = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
    let mut files = vec![];
    files_in(&std, "vlt", &mut files);
    files_in(&std, "ts", &mut files);
    files.sort();
    assert!(files.len() > 100, "{} std files", files.len());
    let ts = |p: &Path| p.extension().is_some_and(|x| x == "ts");
    (files.iter())
        .map(|f| (std::fs::read_to_string(f).unwrap(), ts(f)))
        .collect()
}

/// Several processes storing the same entries at once (each with its own view of the cache, as
/// separate `velt` processes have): every parse is right, and every entry is whole afterwards.
#[test]
fn concurrent_writers_store_whole_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let p = prefix("c1");
    let sources: Vec<(String, bool)> = std_sources().into_iter().take(60).collect();
    std::thread::scope(|s| {
        for t in 0..6 {
            let (root, p, sources) = (tmp.path(), &p, &sources);
            s.spawn(move || {
                let cache = Cache::new(root, u64::MAX);
                velt_syntax::with_parser_stack(|| {
                    for i in 0..sources.len() {
                        let (src, ts) = &sources[(i + t * 7) % sources.len()];
                        let (m, _) = parse_cached(&cache, p, FileId(1), src, *ts);
                        assert!(dump(&m) == dump(&parse_source(FileId(1), src, *ts).0));
                    }
                });
            });
        }
    });
    let cache = Cache::new(tmp.path(), u64::MAX);
    for (src, ts) in &sources {
        let (_, diags) = parse_source(FileId(1), src, *ts);
        let stored = cache.get_key(KIND, &key(&p, src, *ts));
        assert_eq!(stored.is_some(), diags.is_empty());
    }
}

/// Every std file comes out of the cache as it parses, spans included.
#[test]
fn every_std_file_round_trips_through_the_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = Cache::new(tmp.path(), u64::MAX);
    let p = prefix("c1");
    velt_syntax::with_parser_stack(|| {
        for (src, ts) in &std_sources() {
            let (cold, diags) = parse_cached(&cache, &p, FileId(3), src, *ts);
            let (warm, again) = parse_cached(&cache, &p, FileId(5), src, *ts);
            assert!(diags.is_empty() && again.is_empty());
            assert!(dump(&warm) == dump(&parse_source(FileId(5), src, *ts).0));
            assert!(dump(&cold) == dump(&parse_source(FileId(3), src, *ts).0));
        }
    });
}

/// A module of file 0 is stored as parsed under another id: its empty span at 0 (here the
/// module's own) is a span of the file in a build that decodes it as another file, not the
/// dummy.
#[test]
fn an_entry_written_for_file_0_decodes_for_any_file() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = Cache::new(tmp.path(), u64::MAX);
    let p = prefix("c1");
    let src = "";
    let (first, _) = parse_cached(&cache, &p, FileId(0), src, false);
    assert_eq!(dump(&first), fresh(src, FileId(0)));
    let (other, _) = parse_cached(&cache, &p, FileId(7), src, false);
    assert_eq!(other.span.file, FileId(7));
    assert_eq!(dump(&other), fresh(src, FileId(7)));
}

/// A std file the editor has unsaved changes in (the language server's overlay) is parsed: its
/// entry is neither read nor written.
#[test]
fn an_unsaved_buffer_is_neither_read_nor_stored() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = Cache::new(tmp.path(), u64::MAX);
    let p = prefix("c1");
    // An entry under A's key that decodes to B: reading it would show.
    let planted = velt_syntax::encode_module(&parse_source(FileId(1), B, false).0, FileId(1));
    cache.put_key(KIND, &key(&p, A, false), &planted);
    let (m, _) = parse_in(Some(&cache), &p, FileId(1), A, false, false);
    assert_eq!(dump(&m), fresh(A, FileId(1)));
    // Not stored.
    let (m, _) = parse_in(Some(&cache), &p, FileId(1), B, false, false);
    assert_eq!(dump(&m), fresh(B, FileId(1)));
    assert!(Cache::new(tmp.path(), u64::MAX)
        .get_key(KIND, &key(&p, B, false))
        .is_none());
    // The same source read from disk does use the cache.
    let (m, _) = parse_in(Some(&cache), &p, FileId(1), A, false, true);
    assert_eq!(dump(&m), fresh(B, FileId(1)));
}
