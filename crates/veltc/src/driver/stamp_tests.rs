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
        outside: "-",
        generator_inputs: &[],
        c_libs: &[],
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
    let (sm, seen) = build(dir, &[("main.velt", "a"), ("lib.velt", "b")]);
    let key = Key::of(&[b"opts"]);
    let stamp = render_of(&sm, &seen, &[]);
    assert_eq!(check(&stamp, &key, LATER), Some(2));
    assert_eq!(
        check(&stamp, &Key::of(&[b"other"]), LATER),
        None,
        "other options"
    );

    // Same contents, new time: still unchanged.
    std::fs::write(dir.join("lib.velt"), "b").unwrap();
    assert_eq!(check(&stamp, &key, LATER), Some(2));
    // Another content: changed.
    std::fs::write(dir.join("lib.velt"), "c").unwrap();
    assert_eq!(check(&stamp, &key, LATER), None);
    std::fs::write(dir.join("lib.velt"), "b").unwrap();
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
    std::fs::remove_file(dir.join("main.velt")).unwrap();
    assert_eq!(check(&stamp, &key, LATER), None);
}

#[test]
fn a_file_changed_after_it_was_read_is_a_change() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let lib = dir.join("lib.velt");
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
    let (sm, seen) = build(tmp.path(), &[("main.velt", "a")]);
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
    let (sm, seen) = build(tmp.path(), &[("main.velt", "a")]);
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
    let root = tmp.path().join("harness.velt");
    sm.add(&root, "generated");
    let key = Key::of(&[b"opts"]);
    let built = Built {
        seen: &Seen::default(),
        virtual_root: Some(&root),
        objects: 1,
        link_inputs: &[],
        std_objects: &[],
        outside: "-",
        generator_inputs: &[],
        c_libs: &[],
    };
    let stamp = render(&key, &sm, &built);
    assert!(!stamp.contains("harness.velt"));
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
        outside: "-",
        generator_inputs: &[],
        c_libs: &[],
    };
    let stamp = render(&key, &SourceMap::new(), &built);
    let path = tmp.path().join("app.build-stamp");
    std::fs::write(&path, &stamp).unwrap();
    let (objects, std, _) = read(&path, &key).unwrap();
    assert_eq!((objects, std), (1, vec![vpm::relpath::absolute(&member)]));
    set(&member, std::time::SystemTime::now());
    assert_eq!(read(&path, &key), None, "touched");
    set(&member, old);
    assert!(read(&path, &key).is_some());
    std::fs::remove_file(&member).unwrap();
    assert_eq!(read(&path, &key), None, "removed");
}

/// A build after a change reuses the hashes the old stamp has for the files that still have the
/// size and time it records, and hashes every other file it reads: one edited since, one the old
/// stamp is not newer than (rewritten within its time's granularity), one just written.
#[test]
fn a_rebuild_hashes_only_files_that_may_have_changed() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let (main, lib) = (dir.join("main.velt"), dir.join("lib.velt"));
    std::fs::write(&main, "a").unwrap();
    std::fs::write(&lib, "b").unwrap();
    set_time(&main, 1_000_000);
    set_time(&lib, 1_000_000);
    let (sm, seen) = probe::record(|| {
        let mut sm = SourceMap::new();
        for path in [&main, &lib] {
            sm.add(path, probe::read_to_string(path).unwrap());
        }
        sm
    });
    let stamp_path = dir.join("app.build-stamp");
    std::fs::write(&stamp_path, render_of(&sm, &seen, &[])).unwrap();
    set_time(&stamp_path, 2_000_000);
    let abs = |p: &Path| vpm::relpath::absolute(p);
    // Marked, to tell a reused hash from one computed again.
    let marked = || {
        let mut known = known(&stamp_path);
        for read in known.values_mut() {
            read.hash = format!("old {}", read.hash);
        }
        known
    };
    assert_eq!(marked().len(), 2);
    let hashes = |known| {
        let ((), seen) = probe::record_reusing(known, || {
            probe::read_to_string(&main).unwrap();
            probe::read_to_string(&lib).unwrap();
        });
        (
            seen.read[&abs(&main)].hash.clone(),
            seen.read[&abs(&lib)].hash.clone(),
        )
    };
    let old = |text: &str| format!("old {}", probe::content_hash(text.as_bytes()));
    assert_eq!(hashes(marked()), (old("a"), old("b")), "unchanged");

    // Edited (same size): hashed again.
    std::fs::write(&main, "c").unwrap();
    set_time(&main, 1_000_001);
    assert_eq!(hashes(marked()), (probe::content_hash(b"c"), old("b")));
    // Just written, with the size and time the old stamp has for it: too new to trust.
    std::fs::write(&main, "d").unwrap();
    let mut fresh = marked();
    fresh.get_mut(&abs(&main)).unwrap().stat = probe::stat(&main);
    assert_eq!(hashes(fresh), (probe::content_hash(b"d"), old("b")));
    // A stamp not newer than the files (written within their time's granularity): nothing in
    // it is trusted.
    set_time(&stamp_path, 1_000_000);
    assert!(known(&stamp_path).is_empty());
    assert_eq!(
        hashes(known(&stamp_path)),
        (probe::content_hash(b"d"), probe::content_hash(b"b"))
    );
    // No stamp, or one of another format: nothing known.
    std::fs::write(&stamp_path, "velt-build-stamp 0\nfile x 1 1\tlib.velt\n").unwrap();
    assert!(known(&stamp_path).is_empty());
    assert!(known(&dir.join("none.build-stamp")).is_empty());
}

#[test]
fn the_host_is_part_of_the_key() {
    let browser = BuildOptions {
        input: "app.velt".into(),
        target: Some("wasm32-unknown-unknown".into()),
        ..Default::default()
    };
    let host = |condition: &str| crate::driver::HostBuild {
        program: "app".into(),
        package: "host-edge".into(),
        root: "/p/host-edge".into(),
        config: vpm::host::HostConfig {
            target: "wasm32-unknown-unknown".into(),
            condition: condition.into(),
            entry: "./src/entry.mjs".into(),
            scaffold: Default::default(),
        },
    };
    let edge = BuildOptions {
        host: Some(host("edge")),
        ..browser.clone()
    };
    let other = BuildOptions {
        host: Some(host("other")),
        ..browser.clone()
    };
    let key = |o: &BuildOptions| options_key(o, Backend::Llvm).0;
    assert_ne!(key(&browser), key(&edge));
    // Another condition loads other variants.
    assert_ne!(key(&edge), key(&other));
}

/// An unchanged build isn't checked, and the check finds the C libraries: its link gets them from
/// the stamp.
#[test]
fn the_c_libraries_are_handed_back() {
    let dir = tempfile::tempdir().unwrap();
    let sm = SourceMap::new();
    let key = Key::of(&[b"opts"]);
    for libs in [
        vec![],
        vec!["sqlite3".to_string(), "framework:Accelerate".to_string()],
    ] {
        let built = Built {
            seen: &Seen::default(),
            virtual_root: None,
            objects: 1,
            link_inputs: &[],
            std_objects: &[],
            outside: "-",
            generator_inputs: &[],
            c_libs: &libs,
        };
        let path = dir.path().join("a.build-stamp");
        write(&path, &render(&key, &sm, &built)).unwrap();
        assert_eq!(read(&path, &key).unwrap().2, libs);
    }
}

/// A directory is written once, in the table, and its entries name it by number; reading the
/// stamp gives every path whole again.
#[test]
fn directories_are_written_once_and_read_back_whole() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = vpm::relpath::absolute(tmp.path());
    let (sm, seen) = build(&dir, &[("a.velt", "1"), ("b.velt", "2"), ("c.velt", "3")]);
    let text = render_of(&sm, &seen, &[]);
    let shown = dir.display().to_string();
    let table: Vec<&str> = text
        .lines()
        .filter(|l| *l == format!("d {shown}"))
        .collect();
    assert_eq!(table.len(), 1, "{text}");
    assert!(
        !text.contains(&format!("\t{shown}{}", std::path::MAIN_SEPARATOR)),
        "{text}"
    );
    let whole = entry_lines(&text);
    assert!(
        whole.contains(&format!("probe f\t{}", dir.join("a.velt").display()))
            || whole
                .iter()
                .any(|l| l.ends_with(&dir.join("a.velt").display().to_string())),
        "{whole:?}"
    );
    assert_eq!(check(&text, &Key::of(&[b"opts"]), LATER), Some(2));
    // `compose` of the whole lines gives the same entries back.
    let again = compose(&Key::of(&[b"opts"]), 2, &whole);
    assert_eq!(entry_lines(&again), whole);
}

#[test]
fn entries_of_directories_in_turn_expand_to_their_own_paths() {
    // The directory's text stays in the buffer through a run of entries; a run of another
    // directory between two of one must not keep it.
    let tmp = tempfile::tempdir().unwrap();
    let dir = vpm::relpath::absolute(tmp.path());
    for sub in ["a", "b"] {
        std::fs::create_dir(dir.join(sub)).unwrap();
    }
    let files = [
        ("a/1.velt", "1"),
        ("b/1.velt", "2"),
        ("a/2.velt", "3"),
        ("b/2.velt", "4"),
    ];
    let (sm, seen) = build(&dir, &files);
    let text = render_of(&sm, &seen, &[]);
    assert_eq!(check(&text, &Key::of(&[b"opts"]), LATER), Some(2), "{text}");
    // Each entry is its own file: an edit to any one of them is seen.
    for (name, _) in files {
        let path = dir.join(name);
        let before = std::fs::read(&path).unwrap();
        std::fs::write(&path, "edited").unwrap();
        assert_eq!(check(&text, &Key::of(&[b"opts"]), LATER), None, "{name}");
        std::fs::write(&path, before).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn a_path_with_a_line_break_makes_a_stamp_that_never_matches() {
    // `d ` after a line break would be another row of the directory table, and every later
    // `~n/` would read another directory.
    let tmp = tempfile::tempdir().unwrap();
    let dir = vpm::relpath::absolute(tmp.path());
    let odd = "q\nd \nend 0";
    std::fs::create_dir(dir.join(odd)).unwrap();
    let (sm, seen) = build(&dir, &[("a.velt", "1"), ("q\nd \nend 0/b.velt", "2")]);
    let text = render_of(&sm, &seen, &[]);
    assert!(!text.lines().any(|l| l == "d " || l == "end 0"), "{text}");
    assert_eq!(check(&text, &Key::of(&[b"opts"]), LATER), None, "{text}");
    for c in ['\r', '\n'] {
        let mut s = String::new();
        write_entries(&mut s, &[("file h".into(), format!("/x{c}/a"))]);
        assert!(!s.contains("end "), "{c:?}: {s}");
    }
}
