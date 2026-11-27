//! Packages with native code against an isolated local registry (hand-made bundles of C
//! libraries, no zig): publish → install for a target → `velt.lock.json` pins every target →
//! tampering is caught → a missing target gets the documented message → targets can be added,
//! never replaced → a Rust crate's library (the old native ABI) is refused. One test: the steps
//! build on each other.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use vpm::edit::{add_dependency, DependencySpec};
use vpm::graph::{CBundle, PackageGraph};
use vpm::lockfile::Lockfile;
use vpm::native::{c_library_files, NativeLib, NativeMeta, NativeOrigin};
use vpm::{install, InstallOptions, Locations};

const LINUX: &str = "x86_64-unknown-linux-gnu";
const MAC: &str = "aarch64-apple-darwin";
const WINDOWS: &str = "x86_64-pc-windows-msvc";

/// A bundle of package `db`'s C library `db` for `target`, as `velt native build` writes it,
/// each file holding `content`.
fn bundle_dir(dir: &Path, target: &str, content: &str) -> PathBuf {
    let b = dir.join(target);
    std::fs::create_dir_all(b.join("shared")).unwrap();
    std::fs::create_dir_all(b.join("static")).unwrap();
    let files = c_library_files("db", "db", target);
    let all = [
        Some(&files.shared),
        files.import_lib.as_ref(),
        files.static_lib.as_ref(),
    ];
    for file in all.into_iter().flatten() {
        std::fs::write(b.join(file), content).unwrap();
    }
    let meta = NativeMeta {
        package: "db".into(),
        version: "1.0.0".into(),
        target: target.into(),
        abi: 0,
        exports: BTreeMap::new(),
        c: BTreeMap::from([("db".into(), files)]),
    };
    std::fs::write(b.join("native.json"), meta.to_json()).unwrap();
    b
}

/// The prebuilt bundle `db` has in `graph`.
fn prebuilt(graph: &PackageGraph) -> Option<&NativeLib> {
    graph
        .packages()
        .iter()
        .find(|p| p.name == "db")
        .and_then(|p| match &p.c_bundle {
            Some(CBundle::Prebuilt(lib)) => Some(&**lib),
            _ => None,
        })
}

/// The shared library of `lib`.
fn shared(lib: &NativeLib) -> PathBuf {
    lib.dir.join(&lib.meta.c["db"].shared)
}

#[test]
fn native_packages_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = vpm::relpath::absolute(tmp.path());
    let loc = Locations::under(&dir.join("velt-home"));

    // The library: a C library from `sources`, for two targets.
    vpm::scaffold::new_package(&dir, "db", true).unwrap();
    let lib = dir.join("db");
    std::fs::write(
        lib.join("package.velt"),
        format!("export const pkg: Package = {{ name: \"db\", version: \"1.0.0\", native: {{ targets: [\"{LINUX}\", \"{MAC}\"], c: {{ db: {{ sources: [\"native/db.c\"], unchecked: true }} }} }} }};"),
    )
    .unwrap();
    std::fs::create_dir_all(lib.join("native")).unwrap();
    std::fs::write(lib.join("native/db.c"), "int db_open(void) { return 1; }\n").unwrap();
    let artifacts = dir.join("artifacts");
    let linux = bundle_dir(&artifacts, LINUX, "linux v1");

    // Every listed target is required.
    let only_linux = BTreeMap::from([(LINUX.to_string(), linux.clone())]);
    let e = vpm::registry::publish_with_native(&lib, &loc, &only_linux).unwrap_err();
    assert!(
        e.contains("no native library for aarch64-apple-darwin"),
        "{e}"
    );
    // A bundle must match its target.
    let wrong = BTreeMap::from([
        (LINUX.to_string(), linux.clone()),
        (MAC.to_string(), linux.clone()),
    ]);
    let e = vpm::registry::publish_with_native(&lib, &loc, &wrong).unwrap_err();
    assert!(
        e.contains("not of `db 1.0.0` for aarch64-apple-darwin"),
        "{e}"
    );

    let mac = bundle_dir(&artifacts, MAC, "mac v1");
    let both = BTreeMap::from([(LINUX.to_string(), linux.clone()), (MAC.to_string(), mac)]);
    let entry = vpm::registry::publish_with_native(&lib, &loc, &both).unwrap();
    assert_eq!(entry.native.len(), 2);
    let index = vpm::registry::read_index(&loc, "db").unwrap().unwrap();
    assert_eq!(index.versions[0].native, entry.native);
    assert_eq!(index.versions[0].native_abi, Some(0));
    // The C sources are published (for builds from source).
    assert!(loc
        .registry_package("db", &semver::Version::new(1, 0, 0))
        .join("native/db.c")
        .is_file());

    // An app installs it for Linux.
    vpm::scaffold::new_package(&dir, "app", false).unwrap();
    let app = dir.join("app");
    let spec = DependencySpec {
        version: Some("1.0".into()),
        path: None,
        ..Default::default()
    };
    add_dependency(&app, "db", &spec).unwrap();
    let for_linux = InstallOptions {
        target: Some(LINUX.into()),
        ..Default::default()
    };
    // Native code is installed only when the app allows it (`allowNative`), naming the line.
    let e = install(&app, &loc, for_linux.clone()).unwrap_err();
    assert!(
        e.contains("package `db` has native code, which runs with no safety net")
            && e.contains("allowNative: [\"db\"]"),
        "{e}"
    );
    let manifest = std::fs::read_to_string(app.join("package.velt")).unwrap();
    std::fs::write(
        app.join("package.velt"),
        manifest.replacen("name: \"app\",", "name: \"app\", allowNative: [\"db\"],", 1),
    )
    .unwrap();
    let installed = install(&app, &loc, for_linux.clone()).unwrap();
    let native = prebuilt(&installed.graph).unwrap();
    assert_eq!(native.origin, NativeOrigin::Prebuilt);
    assert_eq!(std::fs::read(shared(native)).unwrap(), b"linux v1");
    let static_lib = native.meta.c["db"].static_lib.as_ref().unwrap();
    assert!(native.dir.join(static_lib).is_file());
    // The lockfile pins both targets.
    let lock = Lockfile::read(&app).unwrap().unwrap();
    assert_eq!(lock.get("db").unwrap().native, entry.native);
    // It records what the program allows.
    assert_eq!(lock.allow_native, ["db"]);

    // Without a target nothing native is provided.
    let plain = install(&app, &loc, InstallOptions::default()).unwrap();
    assert!(prebuilt(&plain.graph).is_none());

    // A tampered cache entry is replaced by a verified copy.
    std::fs::write(shared(native), "evil").unwrap();
    let again = install(&app, &loc, for_linux.clone()).unwrap();
    let native = prebuilt(&again.graph).unwrap();
    assert_eq!(std::fs::read(shared(native)).unwrap(), b"linux v1");

    // A tampered registry copy is refused, and nothing unverified lands in the cache.
    let cached = native.dir.clone();
    std::fs::remove_dir_all(&cached).unwrap();
    let published = loc.registry_native("db", &semver::Version::new(1, 0, 0), LINUX);
    let library = published.join(&native.meta.c["db"].shared);
    std::fs::write(&library, "evil").unwrap();
    let e = install(&app, &loc, for_linux.clone()).unwrap_err();
    assert!(
        e.contains("checksum mismatch for the x86_64-unknown-linux-gnu native library"),
        "{e}"
    );
    assert!(!cached.exists());
    std::fs::write(&library, "linux v1").unwrap();

    // No library for Windows: the documented message.
    let for_windows = InstallOptions {
        target: Some(WINDOWS.into()),
        ..Default::default()
    };
    let e = install(&app, &loc, for_windows.clone()).unwrap_err();
    assert!(
        e.starts_with("`db 1.0.0` has no prebuilt native library for x86_64-pc-windows-msvc (published: aarch64-apple-darwin, x86_64-unknown-linux-gnu)"),
        "{e}"
    );
    // Its sources are only built on request.
    assert!(e.contains("Set VELT_NATIVE_FROM_SOURCE=1"), "{e}");

    // The author adds Windows later; published targets are never replaced.
    let windows = bundle_dir(&artifacts, WINDOWS, "windows v1");
    let add = BTreeMap::from([(WINDOWS.to_string(), windows)]);
    vpm::registry::add_native(&lib, &loc, &add).unwrap();
    vpm::registry::add_native(&lib, &loc, &add).unwrap(); // identical: a no-op
    let changed = bundle_dir(&dir.join("changed"), LINUX, "linux v2");
    let e = vpm::registry::add_native(&lib, &loc, &BTreeMap::from([(LINUX.to_string(), changed)]))
        .unwrap_err();
    assert!(e.contains("never replaced"), "{e}");

    // `--locked` keeps exactly the locked libraries (the new target is not picked up)...
    let locked = InstallOptions {
        locked: true,
        target: Some(WINDOWS.into()),
        ..Default::default()
    };
    assert!(install(&app, &loc, locked)
        .unwrap_err()
        .contains("no prebuilt native library"));
    // ...while a normal install adds it to the lockfile.
    let installed = install(&app, &loc, for_windows).unwrap();
    assert!(installed.lock_changed);
    let native = prebuilt(&installed.graph).unwrap();
    assert_eq!(std::fs::read(shared(native)).unwrap(), b"windows v1");

    // A Rust crate's library (the old native ABI, which needed the runtime's function table) is
    // refused before download (once the index is asked: the locked version in the cache asks
    // none).
    std::fs::remove_dir_all(loc.cache.join("db-1.0.0")).unwrap();
    let index_path = loc.registry.join("db/index.json");
    let text = std::fs::read_to_string(&index_path).unwrap();
    std::fs::write(
        &index_path,
        text.replace("\"native_abi\": 0", "\"native_abi\": 3"),
    )
    .unwrap();
    let e = install(&dir.join("app"), &loc, for_linux).unwrap_err();
    assert!(
        e.contains("`db 1.0.0` has a Rust crate's native library (native ABI 3), which velt no longer supports"),
        "{e}"
    );

    // A path dependency's C sources build where a program links them: nothing prebuilt.
    std::fs::write(app.join("package.velt"), "export const pkg: Package = { name: \"app\", version: \"0.1.0\", allowNative: [\"db\"], dependencies: { db: { path: \"../db\" } } };").unwrap();
    let installed = install(
        &app,
        &loc,
        InstallOptions {
            target: Some(LINUX.into()),
            ..Default::default()
        },
    )
    .unwrap();
    let db = installed.graph.packages().iter().find(|p| p.name == "db");
    assert!(matches!(
        db.and_then(|p| p.c_bundle.as_ref()),
        Some(CBundle::FromSource(_))
    ));
}
