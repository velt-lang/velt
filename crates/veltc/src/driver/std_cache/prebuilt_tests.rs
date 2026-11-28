//! Prebuilt hook modules' finder on stand-ins: what is linked, what is a miss, and which
//! builds look objects up.

use super::*;

/// A finder over the cache in `dir` that links what it finds.
fn finder(dir: &Path) -> Finder {
    let cache = velt_build_cache::Cache::new(&dir.join("cache"), u64::MAX);
    Finder {
        cache: Some(Box::leak(Box::new(cache))),
        manifests: dir.join("cache/warm/hooks"),
        shipped: None,
        base: KeyHasher::new(),
        std_root: dir.join("std"),
        target: String::new(),
        cpu: None,
        linking: true,
        seen: Mutex::default(),
    }
}

/// An object found by the check and gone from the cache by the link (another build's
/// garbage collection, `velt clean --cache`) is still linked: sema has called it by then.
#[test]
fn an_object_gone_from_the_cache_by_the_link_is_still_linked() {
    let tmp = tempfile::tempdir().unwrap();
    let finder = finder(tmp.path());
    let hook = prebuilt::of_symbol("velt_hook_prebuilt_probe_text").unwrap();
    let (h, symbol) = (&HOOKS[hook], prebuilt::symbol(&HOOKS[hook]));
    let line = entry_line(
        &symbol,
        &[vir::Ty::F64, vir::Ty::I64, vir::Ty::Ptr],
        vir::Ty::Unit,
    );
    // What a fill stores: the object, and a manifest naming it (no std files).
    let key = finder.lookup_key(h.module_entry);
    let object_key = object_key(&key, &[]);
    let built = tmp.path().join("built.o");
    std::fs::write(&built, b"object bytes").unwrap();
    assert!(finder.cache.unwrap().put_file(KIND, &object_key, &built));
    let manifest = Manifest {
        object: object_key.hex(),
        entries: vec![line],
        files: vec![],
        tls: vec![],
        statics: vec![],
    };
    write_atomically(&finder.manifest_path(&key), &manifest.render()).unwrap();

    assert!(finder.linked(hook));
    finder.cache.unwrap().clear().unwrap();
    assert_eq!(finder.cache.unwrap().path_of(KIND, &object_key), None);
    let program = vir::Program {
        externs: vec![vir::ExternFn {
            symbol,
            params: vec![vir::Ty::F64, vir::Ty::I64, vir::Ty::Ptr],
            ret: vir::Ty::Unit,
            noreturn: false,
            host: None,
            c_decl: None,
            variadic: false,
            c_abi: None,
        }],
        ..vir::Program::default()
    };
    let found = finder.found();
    let (objects, tls) = linked_objects(&program, &found).unwrap();
    assert!(tls.is_empty());
    let place = super::super::Place {
        dir: tmp.path().join("app.std"),
        ext: "o".into(),
    };
    let placed = put(&place, &objects).unwrap();
    assert_eq!(placed, [place.dir.join("hook0.o")]);
    assert_eq!(std::fs::read(&placed[0]).unwrap(), b"object bytes");
    // The pin goes with the build.
    let pinned = found[0].1.object.clone();
    drop(found);
    drop(finder);
    assert!(!pinned.exists());
}

/// Set the time `dir` last changed to `by` ago.
fn backdate(dir: &Path, by: std::time::Duration) {
    #[cfg(unix)]
    let f = std::fs::File::open(dir).unwrap();
    #[cfg(windows)]
    let f = {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_WRITE_ATTRIBUTES, FILE_FLAG_BACKUP_SEMANTICS (a directory).
        (std::fs::OpenOptions::new().access_mode(0x100))
            .custom_flags(0x0200_0000)
            .open(dir)
            .unwrap()
    };
    f.set_modified(std::time::SystemTime::now() - by).unwrap();
}

/// A pin directory a killed build left goes once it is older than any build runs; a fresh
/// one (a build running now) and other directories stay.
#[test]
fn a_stale_pin_directory_is_swept() {
    let tmp = tempfile::tempdir().unwrap();
    let day = std::time::Duration::from_secs(24 * 60 * 60);
    let dir = |name: &str, age| {
        let d = tmp.path().join(name);
        std::fs::create_dir(&d).unwrap();
        std::fs::write(d.join("object"), b"object bytes").unwrap();
        backdate(&d, age);
        d
    };
    let stale = dir("velt-hook-4242-0", day * 2);
    let fresh = dir("velt-hook-4243-0", day / 2);
    let other = dir("other-4242-0", day * 2);
    sweep_stale_pins(tmp.path(), std::time::SystemTime::now());
    assert!(!stale.exists());
    assert!(fresh.join("object").exists());
    assert!(other.join("object").exists());
}

/// The sweep runs when its marker is missing or older than an hour, and renews the marker;
/// a build within the hour lists no temporary directory.
#[test]
fn the_sweep_runs_at_most_once_an_hour() {
    let tmp = tempfile::tempdir().unwrap();
    let swept = tmp.path().join("warm/hooks").join(SWEPT);
    let now = std::time::SystemTime::now();
    assert!(sweep_due(&swept, now));
    assert!(!sweep_due(&swept, now));
    let later = now + SWEEP_EVERY / 2;
    assert!(!sweep_due(&swept, later));
    let past = now + SWEEP_EVERY + std::time::Duration::from_secs(1);
    assert!(sweep_due(&swept, past));
    assert!(!sweep_due(&swept, past));
}

/// An object gone before the check is a miss: the build loads the module.
#[test]
fn an_object_gone_before_the_check_is_a_miss() {
    let tmp = tempfile::tempdir().unwrap();
    let finder = finder(tmp.path());
    let hook = prebuilt::of_symbol("velt_hook_prebuilt_probe_text").unwrap();
    let key = finder.lookup_key(HOOKS[hook].module_entry);
    let manifest = Manifest {
        object: object_key(&key, &[]).hex(),
        entries: vec![],
        files: vec![],
        tls: vec![],
        statics: vec![],
    };
    write_atomically(&finder.manifest_path(&key), &manifest.render()).unwrap();
    assert!(!finder.linked(hook));
    assert!(finder.found().is_empty());
}

/// The lookup takes the object's key the manifest records, not one hashed again from its files:
/// a std file that changed since the fill is still a miss, and a file copied with the same
/// contents (another time) still finds the object.
#[test]
fn a_changed_std_file_is_a_miss_and_a_copy_of_the_same_is_not() {
    let check = |change: fn(&Path)| {
        let tmp = tempfile::tempdir().unwrap();
        let finder = finder(tmp.path());
        let hook = prebuilt::of_symbol("velt_hook_prebuilt_probe_text").unwrap();
        let key = finder.lookup_key(HOOKS[hook].module_entry);
        let std_file = tmp.path().join("probe.velt");
        std::fs::write(&std_file, "export function f() {}\n").unwrap();
        let files = vec![file_entry(&std_file, "export function f() {}\n")];
        let object_key = object_key(&key, &files);
        let built = tmp.path().join("built.o");
        std::fs::write(&built, b"object bytes").unwrap();
        assert!(finder.cache.unwrap().put_file(KIND, &object_key, &built));
        let manifest = Manifest {
            object: object_key.hex(),
            entries: vec![],
            files,
            tls: vec![],
        };
        write_atomically(&finder.manifest_path(&key), &manifest.render()).unwrap();
        change(&std_file);
        finder.linked(hook)
    };
    assert!(check(|_| {}));
    assert!(!check(
        |f| std::fs::write(f, "export function g() {}\n").unwrap()
    ));
    assert!(check(|f| {
        let t = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        let file = std::fs::File::options().write(true).open(f).unwrap();
        file.set_modified(t).unwrap();
    }));
}

/// A manifest's object line is the key's lowercase hex; anything else finds nothing.
#[test]
fn an_object_line_reads_back_as_its_key() {
    let key = Key::of(&[b"probe"]);
    assert_eq!(key_of_hex(&key.hex()), Some(key));
    let key = Key([0xab; 32]);
    assert_eq!(key_of_hex(&key.hex()), Some(key));
    assert_eq!(key_of_hex(&key.hex()[1..]), None);
    assert_eq!(key_of_hex(&key.hex().to_uppercase()), None);
    assert_eq!(key_of_hex(&"g".repeat(64)), None);
}

/// A debug build for arm64 Linux looks prebuilt objects up like one for x86-64 Linux (#3315: its
/// link gets the outline-atomics helpers the objects call, `crate::link`), where this machine's
/// std root and settings let a finder be made, as in this repository's tests.
#[test]
fn a_debug_build_for_arm64_linux_has_a_finder_where_x86_64_linux_has() {
    let finder = |target: &str| {
        Finder::for_build(&BuildOptions {
            target: Some(target.into()),
            ..Default::default()
        })
        .is_some()
    };
    let x86 = finder("x86_64-unknown-linux-gnu");
    assert_eq!(finder("aarch64-unknown-linux-gnu"), x86);
    if !x86 {
        eprintln!("note: no finder for x86-64 Linux here; the comparison says little");
    }
}
