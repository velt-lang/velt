//! Packages with native code against an isolated local registry (hand-made bundles, no cargo):
//! publish → install for a target → `velt.lock.json` pins every target → tampering is caught → a
//! missing target without cargo gets the documented message → targets can be added, never
//! replaced. One test: it sets `$VELT_CARGO` for the whole process.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use vpm::edit::{add_dependency, DependencySpec};
use vpm::lockfile::Lockfile;
use vpm::native::{library_files, NativeMeta, NativeOrigin};
use vpm::{install, InstallOptions, Locations};

const LINUX: &str = "x86_64-unknown-linux-gnu";
const MAC: &str = "aarch64-apple-darwin";
const WINDOWS: &str = "x86_64-pc-windows-msvc";

/// The shared library `bundle_dir` writes for `content`.
fn library_of(content: &str) -> Vec<u8> {
    let exports = BTreeMap::from([("velt_db_open".into(), "(string)->IoResult<u64>".into())]);
    vpm::native::exports::sample_library("db", &exports, content)
}

fn bundle_dir(dir: &Path, target: &str, content: &str) -> PathBuf {
    let b = dir.join(target);
    std::fs::create_dir_all(b.join("shared")).unwrap();
    std::fs::create_dir_all(b.join("static")).unwrap();
    let (shared, import_lib) = library_files("db", target);
    let (shared, import_lib) = (
        format!("shared/{shared}"),
        import_lib.map(|f| format!("shared/{f}")),
    );
    std::fs::write(b.join(&shared), library_of(content)).unwrap();
    let exports = BTreeMap::from([("velt_db_open".into(), "(string)->IoResult<u64>".into())]);
    if let Some(import_lib) = &import_lib {
        let dll = shared.trim_start_matches("shared/");
        let names = vpm::native::exports::exported_names(&library_of(content)).unwrap();
        let imports: Vec<(String, String)> = names.into_iter().map(|n| (n.clone(), n)).collect();
        let lib = vpm::native::exports::sample_import_library(dll, &imports);
        std::fs::write(b.join(import_lib), lib).unwrap();
    }
    let object = vpm::native::exports::sample_object("db", &exports, content);
    std::fs::write(b.join("static/db.o"), object).unwrap();
    let meta = NativeMeta {
        package: "db".into(),
        version: "1.0.0".into(),
        target: target.into(),
        abi: 1,
        shared,
        import_lib,
        static_obj: Some("static/db.o".into()),
        exports: BTreeMap::from([("velt_db_open".into(), "(string)->IoResult<u64>".into())]),
    };
    std::fs::write(b.join("native.json"), meta.to_json()).unwrap();
    b
}

#[test]
fn native_packages_end_to_end() {
    std::env::set_var("VELT_CARGO", "velt-test-no-such-cargo");
    let tmp = tempfile::tempdir().unwrap();
    let dir = vpm::relpath::absolute(tmp.path());
    let loc = Locations::under(&dir.join("velt-home"));

    // The library: `native` for two targets.
    vpm::scaffold::new_package(&dir, "db", true).unwrap();
    let lib = dir.join("db");
    std::fs::write(
        lib.join("package.vlt"),
        format!("export const pkg: Package = {{ name: \"db\", version: \"1.0.0\", native: {{ targets: [\"{LINUX}\", \"{MAC}\"] }} }};"),
    )
    .unwrap();
    std::fs::create_dir_all(lib.join("native/src")).unwrap();
    std::fs::write(lib.join("native/Cargo.toml"), "[package]\nname = \"db\"\n").unwrap();
    std::fs::write(lib.join("native/src/lib.rs"), "").unwrap();
    std::fs::write(lib.join("native/Cargo.lock"), "version = 4\n").unwrap();
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
    assert_eq!(index.versions[0].native_abi, Some(1));
    // The native crate's sources are published (for builds from source).
    assert!(loc
        .registry_package("db", &semver::Version::new(1, 0, 0))
        .join("native/src/lib.rs")
        .is_file());

    // An app installs it for Linux.
    vpm::scaffold::new_package(&dir, "app", false).unwrap();
    let app = dir.join("app");
    let spec = DependencySpec {
        version: Some("1.0".into()),
        path: None,
    };
    add_dependency(&app, "db", &spec).unwrap();
    let for_linux = InstallOptions {
        target: Some(LINUX.into()),
        ..Default::default()
    };
    let installed = install(&app, &loc, for_linux.clone()).unwrap();
    let (pkg, native) = installed.graph.natives().next().unwrap();
    assert_eq!(pkg.name, "db");
    assert_eq!(native.origin, NativeOrigin::Prebuilt);
    assert_eq!(
        native.meta.exports["velt_db_open"],
        "(string)->IoResult<u64>"
    );
    assert_eq!(
        std::fs::read(native.shared_lib()).unwrap(),
        library_of("linux v1")
    );
    assert!(native.static_obj().unwrap().is_file());
    // The lockfile pins both targets.
    let lock = Lockfile::read(&app).unwrap().unwrap();
    assert_eq!(lock.get("db").unwrap().native, entry.native);

    // Without a target nothing native is provided.
    let plain = install(&app, &loc, InstallOptions::default()).unwrap();
    assert_eq!(plain.graph.natives().count(), 0);

    // A tampered cache entry is replaced by a verified copy.
    std::fs::write(native.shared_lib(), "evil").unwrap();
    let again = install(&app, &loc, for_linux.clone()).unwrap();
    let (_, native) = again.graph.natives().next().unwrap();
    assert_eq!(
        std::fs::read(native.shared_lib()).unwrap(),
        library_of("linux v1")
    );

    // A tampered registry copy is refused, and nothing unverified lands in the cache.
    let cached = native.dir.clone();
    std::fs::remove_dir_all(&cached).unwrap();
    let published = loc.registry_native("db", &semver::Version::new(1, 0, 0), LINUX);
    std::fs::write(published.join("shared/libvelt_native_db.so"), "evil").unwrap();
    let e = install(&app, &loc, for_linux.clone()).unwrap_err();
    assert!(
        e.contains("checksum mismatch for the x86_64-unknown-linux-gnu native library"),
        "{e}"
    );
    assert!(!cached.exists());
    std::fs::write(published.join("shared/libvelt_native_db.so"), "linux v1").unwrap();

    // No library for Windows, no cargo: the documented message.
    let for_windows = InstallOptions {
        target: Some(WINDOWS.into()),
        ..Default::default()
    };
    let e = install(&app, &loc, for_windows.clone()).unwrap_err();
    assert!(
        e.starts_with("`db 1.0.0` has no prebuilt native library for x86_64-pc-windows-msvc (published: aarch64-apple-darwin, x86_64-unknown-linux-gnu)"),
        "{e}"
    );
    assert!(e.contains("https://rustup.rs"), "{e}");

    // With cargo installed, a registry package's sources are still only built on request.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let fake = dir.join("fake-cargo");
        std::fs::write(&fake, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::env::set_var("VELT_CARGO", &fake);
        let e = install(&app, &loc, for_windows.clone()).unwrap_err();
        assert!(e.contains("set VELT_NATIVE_FROM_SOURCE=1"), "{e}");
        std::env::set_var("VELT_CARGO", "velt-test-no-such-cargo");
    }

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
    let (_, native) = installed.graph.natives().next().unwrap();
    assert_eq!(
        std::fs::read(native.shared_lib()).unwrap(),
        library_of("windows v1")
    );

    // A library needing a newer runtime table is refused before download.
    let index_path = loc.registry.join("db/index.json");
    let text = std::fs::read_to_string(&index_path).unwrap();
    std::fs::write(
        &index_path,
        text.replace("\"native_abi\": 1", "\"native_abi\": 99"),
    )
    .unwrap();
    let e = install(&dir.join("app"), &loc, for_linux).unwrap_err();
    assert!(
        e.contains("needs Velt native ABI 99; this velt provides 1"),
        "{e}"
    );

    // A path dependency with native code needs cargo to build it.
    std::fs::write(app.join("package.vlt"), "export const pkg: Package = { name: \"app\", version: \"0.1.0\", dependencies: { db: { path: \"../db\" } } };").unwrap();
    let e = install(
        &app,
        &loc,
        InstallOptions {
            target: Some(LINUX.into()),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(e.contains("has native code, which needs cargo"), "{e}");
}
