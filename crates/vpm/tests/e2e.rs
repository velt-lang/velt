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
        std::fs::write(root.join("src/lib.vlt"), body).unwrap();
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
        .dependency_root(&w.app().join("src/main.vlt"), "lib")
        .unwrap();
    assert_eq!(lib_dir, w.loc.cache.join("lib-1.0.0"));
    assert!(std::fs::read_to_string(lib_dir.join("src/lib.vlt"))
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

    let main = w.app().join("src/main.vlt");
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
    let local_file = w.dir.join("local/src/lib.vlt");
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
    std::fs::write(w.loc.registry.join("lib/1.0.0/src/lib.vlt"), "evil").unwrap();
    std::fs::remove_dir_all(w.loc.cache.join("lib-1.0.0")).unwrap();
    let err = w.install(InstallOptions::default()).unwrap_err();
    assert!(err.contains("checksum mismatch"), "{err}");
}
