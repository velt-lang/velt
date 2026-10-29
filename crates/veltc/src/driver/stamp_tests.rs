use std::time::{Duration, SystemTime};

use super::*;

/// A build of `files` in `dir`: each probed and read, `dir` listed, and a package looked for
/// in `dir/node_modules` (as an npm import does).
fn build(dir: &Path, files: &[(&str, &str)]) -> (SourceMap, Seen) {
    for (name, src) in files {
        std::fs::write(dir.join(name), src).unwrap();
    }
    let (sm, seen) = probe::record(|| {
        let mut sm = SourceMap::new();
        probe::read_dir(dir).unwrap();
        probe::is_file(&dir.join("node_modules/pkg/package.json"));
        for (name, _) in files {
            let path = dir.join(name);
            assert!(probe::is_file(&path));
            let src = probe::read_to_string(&path).unwrap();
            sm.add(path, src);
        }
        sm
    });
    (sm, seen)
}

fn render_of(sm: &SourceMap, seen: &Seen, link_inputs: &[PathBuf]) -> String {
    let built = Built {
        seen,
        virtual_root: None,
        objects: 2,
        link_inputs,
        std_objects: &[],
    };
    render(&Key::of(&[b"opts"]), sm, &built)
}

/// Long after every file of a test was written.
const LATER: Option<u128> = Some(u128::MAX);

fn set_time(path: &Path, secs: u64) -> u128 {
    let time = SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
    let file = std::fs::File::options().write(true).open(path).unwrap();
    file.set_modified(time).unwrap();
    u128::from(secs) * 1_000_000_000
}

#[test]
fn unchanged_inputs_match() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let (sm, seen) = build(dir, &[("main.vlt", "a"), ("lib.vlt", "b")]);
    let key = Key::of(&[b"opts"]);
    let stamp = render_of(&sm, &seen, &[]);
    assert_eq!(check(&stamp, &key, LATER), Some(2));
    assert_eq!(
        check(&stamp, &Key::of(&[b"other"]), LATER),
        None,
        "other options"
    );

    // Same contents, new time: still unchanged.
    std::fs::write(dir.join("lib.vlt"), "b").unwrap();
    assert_eq!(check(&stamp, &key, LATER), Some(2));
    // Another content: changed.
    std::fs::write(dir.join("lib.vlt"), "c").unwrap();
    assert_eq!(check(&stamp, &key, LATER), None);
    std::fs::write(dir.join("lib.vlt"), "b").unwrap();
    assert_eq!(check(&stamp, &key, LATER), Some(2));

    // A new entry in a directory resolution listed: changed.
    std::fs::write(dir.join("lib.ts"), "").unwrap();
    assert_eq!(check(&stamp, &key, LATER), None);
    std::fs::remove_file(dir.join("lib.ts")).unwrap();
    assert_eq!(check(&stamp, &key, LATER), Some(2));
    // A path resolution found missing exists now: changed (with `dir`'s listing as it is now,
    // to see the probe alone).
    let pkg = dir.join("node_modules/pkg");
    std::fs::create_dir_all(&pkg).unwrap();
    let mut seen = seen;
    let now = probe::listing_hash(dir).unwrap();
    seen.listed.insert(vpm::relpath::absolute(dir), now);
    let stamp = render_of(&sm, &seen, &[]);
    assert_eq!(check(&stamp, &key, LATER), Some(2));
    std::fs::write(pkg.join("package.json"), "{}").unwrap();
    assert_eq!(check(&stamp, &key, LATER), None);
    // A deleted file: changed.
    std::fs::remove_file(pkg.join("package.json")).unwrap();
    std::fs::remove_file(dir.join("main.vlt")).unwrap();
    assert_eq!(check(&stamp, &key, LATER), None);
}

#[test]
fn a_file_changed_after_it_was_read_is_a_change() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let lib = dir.join("lib.vlt");
    std::fs::write(&lib, "aaaa").unwrap();
    let read_at = set_time(&lib, 1_000_000);
    let (sm, seen) = probe::record(|| {
        let mut sm = SourceMap::new();
        sm.add(&lib, probe::read_to_string(&lib).unwrap());
        sm
    });
    // Rewritten, same size, while the build ran: the stamp written after it must not take
    // the new size and time for those of what was read.
    std::fs::write(&lib, "bbbb").unwrap();
    set_time(&lib, 1_000_001);
    let key = Key::of(&[b"opts"]);
    let stamp = render_of(&sm, &seen, &[]);
    assert_eq!(check(&stamp, &key, LATER), None);
    // Rewritten within the granularity of its time (the same time as when it was read): a
    // stamp not newer than the file does not trust the time.
    set_time(&lib, 1_000_000);
    assert_eq!(check(&stamp, &key, Some(read_at)), None, "racily clean");
    assert_eq!(check(&stamp, &key, None), None);
    std::fs::write(&lib, "aaaa").unwrap();
    set_time(&lib, 1_000_000);
    assert_eq!(check(&stamp, &key, Some(read_at)), Some(2));
}

#[test]
fn a_stamp_cut_short_misses() {
    let tmp = tempfile::tempdir().unwrap();
    let (sm, seen) = build(tmp.path(), &[("main.vlt", "a")]);
    let key = Key::of(&[b"opts"]);
    let stamp = render_of(&sm, &seen, &[]);
    assert_eq!(check(&stamp, &key, LATER), Some(2));
    assert!(stamp.ends_with("end 4\n"), "{stamp}");
    for (cut, _) in stamp
        .match_indices('\n')
        .filter(|(i, _)| i + 1 < stamp.len())
    {
        assert_eq!(check(&stamp[..=cut], &key, LATER), None, "cut after {cut}");
    }
    assert_eq!(check(&format!("{stamp}file x\ty\n"), &key, LATER), None);
    assert_eq!(check("", &key, LATER), None);

    // Written whole, in place of the old stamp.
    let path = tmp.path().join("main.build-stamp");
    std::fs::write(&path, "old").unwrap();
    write(&path, &stamp).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), stamp);
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 2);
}

#[test]
fn link_inputs_keep_their_size_and_time() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("member.o");
    std::fs::write(&lib, "o").unwrap();
    let (sm, seen) = build(tmp.path(), &[("main.vlt", "a")]);
    let key = Key::of(&[b"opts"]);
    let stamp = render_of(&sm, &seen, std::slice::from_ref(&lib));
    assert_eq!(check(&stamp, &key, LATER), Some(2));
    std::fs::write(&lib, "oo").unwrap();
    assert_eq!(check(&stamp, &key, LATER), None);
    std::fs::remove_file(&lib).unwrap();
    assert_eq!(check(&stamp, &key, LATER), None);
}

#[test]
fn a_generated_root_is_left_to_the_key() {
    let tmp = tempfile::tempdir().unwrap();
    let mut sm = SourceMap::new();
    let root = tmp.path().join("harness.vlt");
    sm.add(&root, "generated");
    let key = Key::of(&[b"opts"]);
    let built = Built {
        seen: &Seen::default(),
        virtual_root: Some(&root),
        objects: 1,
        link_inputs: &[],
        std_objects: &[],
    };
    let stamp = render(&key, &sm, &built);
    assert!(!stamp.contains("harness.vlt"));
    assert_eq!(check(&stamp, &key, LATER), Some(1));
}

/// The std cache's objects are recorded and handed back for the fast path's link; one rewritten
/// (touched) or removed makes the stamp stale, so the build links (and compiles) again.
#[test]
fn std_objects_are_linked_again_and_must_stay_as_they_were() {
    let tmp = tempfile::tempdir().unwrap();
    let member = tmp.path().join("member.o");
    std::fs::write(&member, b"object").unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    let set = |p: &std::path::Path, t| {
        let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
        f.set_modified(t).unwrap();
    };
    set(&member, old);
    let key = Key::of(&[b"opts"]);
    let built = Built {
        seen: &Seen::default(),
        virtual_root: None,
        objects: 1,
        link_inputs: &[],
        std_objects: std::slice::from_ref(&member),
    };
    let stamp = render(&key, &SourceMap::new(), &built);
    let path = tmp.path().join("app.build-stamp");
    std::fs::write(&path, &stamp).unwrap();
    let (objects, std) = read(&path, &key).unwrap();
    assert_eq!((objects, std), (1, vec![vpm::relpath::absolute(&member)]));
    set(&member, std::time::SystemTime::now());
    assert_eq!(read(&path, &key), None, "touched");
    set(&member, old);
    assert!(read(&path, &key).is_some());
    std::fs::remove_file(&member).unwrap();
    assert_eq!(read(&path, &key), None, "removed");
}
