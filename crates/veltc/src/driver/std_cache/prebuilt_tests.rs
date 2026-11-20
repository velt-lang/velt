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
    };
    write_atomically(&finder.manifest_path(&key), &manifest.render()).unwrap();
    assert!(!finder.linked(hook));
    assert!(finder.found().is_empty());
}

/// Debug builds take no prebuilt objects where LLVM's code calls the outline-atomics helpers,
/// which their links may lack (arm64 Linux, #3300: the bundled linker's debug link failed with
/// undefined `__aarch64_cas4_acq`); elsewhere they do.
#[test]
fn debug_links_take_objects_except_where_atomics_call_helpers() {
    for target in ["aarch64-unknown-linux-gnu", "aarch64-unknown-linux-musl"] {
        assert!(!debug_link_takes(target), "{target}");
    }
    for target in [
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "x86_64-unknown-linux-gnu",
        "x86_64-unknown-linux-musl",
        "x86_64-pc-windows-msvc",
    ] {
        assert!(debug_link_takes(target), "{target}");
    }
}
