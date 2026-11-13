//! End-to-end package workflows against an isolated registry + cache in a temp dir:
//! publish → add → install → lockfile → upgrade → conflicts → path deps → `--locked`.

use std::path::{Path, PathBuf};

use vpm::edit::{add_dependency, DependencySpec};
use vpm::lockfile::Lockfile;
use vpm::{install, InstallOptions, Locations};

struct World {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    loc: Locations,
}

fn world() -> World {
    let tmp = tempfile::tempdir().unwrap();
    let dir = vpm::relpath::absolute(tmp.path());
    let loc = Locations::under(&dir.join("velt-home"));
    World {
        _tmp: tmp,
        dir,
        loc,
    }
}

impl World {
    /// Create (or overwrite) library `name` at `version` with `deps` and publish it.
    fn publish_lib(&self, name: &str, version: &str, deps: &str, body: &str) {
        let root = self.dir.join("work").join(name);
        if !root.exists() {
            vpm::scaffold::new_package(&self.dir.join("work"), name, true).unwrap();
        }
        let manifest = format!(
            "export const pkg: Package = {{ name: \"{name}\", version: \"{version}\", dependencies: {{ {deps} }} }};"
        );
        std::fs::write(root.join(vpm::manifest::MANIFEST_FILE), manifest).unwrap();
        std::fs::write(root.join("src/lib.velt"), body).unwrap();
        vpm::registry::publish(&root, &self.loc).unwrap();
    }

    fn app(&self) -> PathBuf {
        let root = self.dir.join("app");
        if !root.exists() {
            vpm::scaffold::new_package(&self.dir, "app", false).unwrap();
        }
        root
    }

    fn add(&self, name: &str, version: Option<&str>, path: Option<&str>) {
        let spec = DependencySpec {
            version: version.map(Into::into),
            path: path.map(Into::into),
            ..Default::default()
        };
        add_dependency(&self.app(), name, &spec).unwrap();
    }

    fn install(&self, opts: InstallOptions) -> Result<vpm::Installed, String> {
        install(&self.app(), &self.loc, opts)
    }
}

fn locked_version(root: &Path, name: &str) -> String {
    Lockfile::read(root)
        .unwrap()
        .unwrap()
        .get(name)
        .unwrap()
        .version
        .clone()
}

#[test]
fn publish_install_lock_and_upgrade() {
    let w = world();
    w.publish_lib(
        "lib",
        "1.0.0",
        "",
        "export function v(): i64 { return 100; }\n",
    );
    w.publish_lib(
        "lib",
        "2.0.0",
        "",
        "export function v(): i64 { return 200; }\n",
    );
    w.add("lib", Some("^1.0"), None);

    let first = w.install(InstallOptions::default()).unwrap();
    assert!(first.lock_changed);
    let lock = Lockfile::read(&w.app()).unwrap().unwrap();
    let entry = lock.get("lib").unwrap();
    assert_eq!(
        (entry.version.as_str(), entry.source.as_str()),
        ("1.0.0", "registry")
    );
    assert!(entry.checksum.as_deref().unwrap().starts_with("sha256:"));

    // The graph points the app at the cached copy.
    let lib_dir = first
        .graph
        .dependency_root(&w.app().join("src/main.velt"), "lib")
        .unwrap();
    assert_eq!(lib_dir, w.loc.cache.join("lib-1.0.0"));
    assert!(std::fs::read_to_string(lib_dir.join("src/lib.velt"))
        .unwrap()
        .contains("100"));

    // A newer compatible release does not move the lock...
    w.publish_lib(
        "lib",
        "1.1.0",
        "",
        "export function v(): i64 { return 110; }\n",
    );
    assert!(!w.install(InstallOptions::default()).unwrap().lock_changed);
    assert_eq!(locked_version(&w.app(), "lib"), "1.0.0");
    // ...until an explicit update, or a requirement the locked version no longer satisfies.
    let updated = w
        .install(InstallOptions {
            update: true,
            ..Default::default()
        })
        .unwrap();
    assert!(updated.lock_changed);
    assert_eq!(locked_version(&w.app(), "lib"), "1.1.0");
    w.add("lib", Some("^2"), None);
    w.install(InstallOptions::default()).unwrap();
    assert_eq!(locked_version(&w.app(), "lib"), "2.0.0");
}

#[test]
fn locked_mode_refuses_changes() {
    let w = world();
    w.publish_lib("lib", "1.0.0", "", "");
    w.add("lib", Some("1"), None);
    let err = w
        .install(InstallOptions {
            locked: true,
            ..Default::default()
        })
        .unwrap_err();
    assert!(err.contains("--locked"), "{err}");
    w.install(InstallOptions::default()).unwrap();
    w.install(InstallOptions {
        locked: true,
        ..Default::default()
    })
    .unwrap();
    w.publish_lib("other", "1.0.0", "", "");
    w.add("other", Some("1"), None);
    let err = w
        .install(InstallOptions {
            locked: true,
            ..Default::default()
        })
        .unwrap_err();
    assert!(err.contains("needs to be updated"), "{err}");
}

#[test]
fn conflicting_transitive_requirements() {
    let w = world();
    w.publish_lib("base", "1.0.0", "", "");
    w.publish_lib("base", "2.0.0", "", "");
    w.publish_lib("mid", "1.0.0", "base: \"^2\"", "");
    w.add("base", Some("^1"), None);
    w.add("mid", Some("^1"), None);
    let err = w.install(InstallOptions::default()).unwrap_err();
    assert!(err.contains("conflicting requirements for `base`"), "{err}");
    assert!(
        err.contains("app 0.1.0 → mid 1.0.0 requires `base ^2`"),
        "{err}"
    );
    assert!(
        !w.app().join("velt.lock.json").exists(),
        "a failed install must not write the lockfile"
    );
}

#[test]
fn path_dependency_with_registry_dependency() {
    let w = world();
    w.publish_lib("util", "0.3.0", "", "");
    vpm::scaffold::new_package(&w.dir, "local", true).unwrap();
    vpm::edit::add_dependency(
        &w.dir.join("local"),
        "util",
        &DependencySpec {
            version: Some("0.3".into()),
            path: None,
            ..Default::default()
        },
    )
    .unwrap();
    w.add("local", None, Some("../local"));

    let installed = w.install(InstallOptions::default()).unwrap();
    let lock = Lockfile::read(&w.app()).unwrap().unwrap();
    let local = lock.get("local").unwrap();
    assert_eq!(
        (local.source.as_str(), local.checksum.as_deref()),
        ("path+../local", None)
    );
    assert_eq!(local.dependencies, ["util"]);
    assert_eq!(lock.get("util").unwrap().version, "0.3.0");

    let main = w.app().join("src/main.velt");
    assert_eq!(
        installed.graph.dependency_root(&main, "local").unwrap(),
        w.dir.join("local")
    );
    // `util` is a dependency of `local`, not of the app.
    assert!(installed
        .graph
        .dependency_root(&main, "util")
        .unwrap_err()
        .contains("not a dependency"));
    let local_file = w.dir.join("local/src/lib.velt");
    assert_eq!(
        installed
            .graph
            .dependency_root(&local_file, "util")
            .unwrap(),
        w.loc.cache.join("util-0.3.0")
    );
}

#[test]
fn tampered_registry_is_detected() {
    let w = world();
    w.publish_lib("lib", "1.0.0", "", "export const A: i64 = 1;\n");
    w.add("lib", Some("1"), None);
    w.install(InstallOptions::default()).unwrap();
    std::fs::write(w.loc.registry.join("lib/1.0.0/src/lib.velt"), "evil").unwrap();
    std::fs::remove_dir_all(w.loc.cache.join("lib-1.0.0")).unwrap();
    let err = w.install(InstallOptions::default()).unwrap_err();
    assert!(err.contains("checksum mismatch"), "{err}");
}

/// A registry nothing listens on: asking it fails at once (port 1 refuses connections), as with
/// the network down or the registry's host unreachable.
const UNREACHABLE: &str = "http://127.0.0.1:1";

impl World {
    /// [`World::install`] with the registry unreachable (the same registry directory and cache).
    fn install_offline(&self, opts: InstallOptions) -> Result<vpm::Installed, String> {
        let loc = Locations {
            remote: Some(UNREACHABLE.to_string()),
            ..self.loc.clone()
        };
        install(&self.app(), &loc, opts)
    }

    /// Rewrite the registry's index entry of `name` `version` with `edit`.
    fn edit_index(&self, name: &str, version: &str, edit: impl Fn(&mut serde_json::Value)) {
        let path = self.loc.registry.join(name).join("index.json");
        let mut index: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let entry = index["versions"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|v| v["version"] == version)
            .unwrap();
        edit(entry);
        std::fs::write(&path, index.to_string()).unwrap();
    }
}

/// With `velt.lock.json` covering package.velt and every locked package in the cache under its
/// locked checksum, installing asks no registry (#2840): offline, or with the registry down,
/// a locked project still installs and builds. What isn't locked or cached needs the registry,
/// and the error says why and what to do.
#[test]
fn a_locked_and_cached_project_installs_without_the_registry() {
    let w = world();
    w.publish_lib("util", "1.0.0", "", "export const U: i64 = 1;\n");
    w.publish_lib("lib", "1.0.0", "util: \"^1\"", "export const L: i64 = 1;\n");
    w.add("lib", Some("^1"), None);
    w.install(InstallOptions::default()).unwrap();
    let lock = std::fs::read_to_string(w.app().join("velt.lock.json")).unwrap();

    for locked in [false, true] {
        let offline = w
            .install_offline(InstallOptions {
                locked,
                ..Default::default()
            })
            .unwrap();
        assert!(!offline.lock_changed);
        assert_eq!(
            offline
                .graph
                .dependency_root(&w.app().join("src/main.velt"), "lib")
                .unwrap(),
            w.loc.cache.join("lib-1.0.0")
        );
    }
    let read_lock = || std::fs::read_to_string(w.app().join("velt.lock.json")).unwrap();
    assert_eq!(read_lock(), lock);

    // A locked package missing from the cache needs the registry.
    std::fs::remove_dir_all(w.loc.cache.join("util-1.0.0")).unwrap();
    let err = w.install_offline(InstallOptions::default()).unwrap_err();
    assert!(
        err.starts_with(&format!(
            "cannot reach the registry {UNREACHABLE} to read the index of `util`"
        )),
        "{err}"
    );
    assert!(
        err.contains("`util` needs the registry: velt.lock.json pins 1.0.0, which package.velt no longer accepts or which isn't in the cache")
            && err.contains("run `velt install` once the registry can be reached"),
        "{err}"
    );
    w.install(InstallOptions::default()).unwrap();

    // So does a dependency the lock doesn't pin yet; the lock stays as it was.
    w.publish_lib("other", "1.0.0", "", "export const O: i64 = 1;\n");
    w.add("other", Some("1"), None);
    let err = w.install_offline(InstallOptions::default()).unwrap_err();
    assert!(
        err.contains("`other` needs the registry: velt.lock.json doesn't pin it yet"),
        "{err}"
    );
    assert_eq!(read_lock(), lock);
}

/// A locked version from the cache is the only candidate tried first; when package.velt asks for
/// something it can't meet together with the other locked versions, the registry's other
/// versions are still found.
#[test]
fn a_cached_locked_version_that_no_longer_fits_is_re_resolved() {
    let w = world();
    w.publish_lib("util", "1.0.0", "", "export const U: i64 = 1;\n");
    w.add("util", Some("^1"), None);
    w.install(InstallOptions::default()).unwrap();
    w.publish_lib("util", "1.1.0", "", "export const U: i64 = 2;\n");
    w.publish_lib(
        "lib",
        "1.0.0",
        "util: \"^1.1\"",
        "export const L: i64 = 1;\n",
    );
    w.add("lib", Some("1"), None);
    w.install(InstallOptions::default()).unwrap();
    assert_eq!(locked_version(&w.app(), "util"), "1.1.0");
}

/// The checksum covers a package, not its index entry: dependencies the index lists for a
/// version must be the ones its own package.velt declares, or nothing is installed (#2840).
#[test]
fn an_index_that_changes_a_packages_dependencies_is_refused() {
    let w = world();
    w.publish_lib("evil", "1.0.0", "", "export const E: i64 = 1;\n");
    w.publish_lib("util", "1.0.0", "", "export const U: i64 = 1;\n");
    w.publish_lib("lib", "1.0.0", "util: \"1\"", "export const L: i64 = 1;\n");
    w.add("lib", Some("^1"), None);
    w.install(InstallOptions::default()).unwrap();
    let lock = std::fs::read_to_string(w.app().join("velt.lock.json")).unwrap();
    let read_lock = || std::fs::read_to_string(w.app().join("velt.lock.json")).unwrap();

    // The index adds `evil` to the locked version.
    w.edit_index("lib", "1.0.0", |e| {
        e["dependencies"] = serde_json::json!({ "util": "1", "evil": "^1" });
    });
    // The cached package is what the lock pins: its own package.velt is followed, so the
    // index isn't asked and `evil` never enters.
    assert!(!w.install(InstallOptions::default()).unwrap().lock_changed);
    // Fetched again, the index's list is checked against the package.
    let uncache = || std::fs::remove_dir_all(w.loc.cache.join("lib-1.0.0")).unwrap();
    for update in [false, true] {
        uncache();
        let err = w
            .install(InstallOptions {
                update,
                ..Default::default()
            })
            .unwrap_err();
        assert_eq!(
            err,
            format!(
                "the registry `{}` lists other dependencies for `lib` 1.0.0 than its package.velt declares: the index has `evil` ^1, `util` 1, the package has `util` 1. The checksum covers the package, not the index, so it was not installed; report this to the registry's operators",
                w.loc.registry.display()
            )
        );
        assert_eq!(read_lock(), lock);
    }

    // A requirement the index changes is refused too; one it only writes differently is not.
    w.edit_index("lib", "1.0.0", |e| {
        e["dependencies"] = serde_json::json!({ "util": "*" });
    });
    uncache();
    let err = w.install(InstallOptions::default()).unwrap_err();
    assert!(
        err.contains("the index has `util` *, the package has `util` 1"),
        "{err}"
    );
    w.edit_index("lib", "1.0.0", |e| {
        e["dependencies"] = serde_json::json!({ "util": "^1" });
    });
    uncache();
    w.install(InstallOptions::default()).unwrap();
    assert_eq!(read_lock(), lock);
}

/// The same version with other content (another registry, or a republished package) is a checksum
/// mismatch under `--locked` too, not "needs to be updated" (#2840).
#[test]
fn locked_reports_a_changed_checksum() {
    let w = world();
    w.publish_lib("lib", "1.0.0", "", "export const A: i64 = 1;\n");
    w.add("lib", Some("1"), None);
    w.install(InstallOptions::default()).unwrap();
    let published = w.loc.registry.join("lib/1.0.0");
    std::fs::write(published.join("src/lib.velt"), "export const A: i64 = 2;\n").unwrap();
    let sum = vpm::contents::checksum(&published).unwrap();
    w.edit_index("lib", "1.0.0", |e| e["checksum"] = sum.clone().into());
    std::fs::remove_dir_all(w.loc.cache.join("lib-1.0.0")).unwrap();
    for locked in [false, true] {
        let err = w
            .install(InstallOptions {
                locked,
                ..Default::default()
            })
            .unwrap_err();
        assert!(
            err.starts_with("checksum mismatch for `lib` 1.0.0: velt.lock.json has sha256:")
                && err.contains(&format!("the registry has {sum}")),
            "--locked {locked}: {err}"
        );
    }
}
