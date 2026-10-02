use std::path::{Path, PathBuf};

use super::*;
use crate::lockfile::{LockedPackage, REGISTRY_SOURCE};

struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    loc: Locations,
}

fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let root = crate::relpath::absolute(tmp.path());
    let loc = Locations::under(&root.join("home"));
    Env {
        _tmp: tmp,
        root,
        loc,
    }
}

fn write_package(dir: &Path, name: &str, version: &str, deps: &str) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let manifest = format!(
        "export const pkg: Package = {{ name: \"{name}\", version: \"{version}\", dependencies: {{ {deps} }} }};"
    );
    std::fs::write(dir.join(crate::manifest::MANIFEST_FILE), manifest).unwrap();
    std::fs::write(dir.join("src/lib.vlt"), format!("// {name} {version}\n")).unwrap();
}

impl Env {
    fn publish(&self, name: &str, version: &str, deps: &str) {
        let dir = self.root.join("src-pkgs").join(format!("{name}-{version}"));
        write_package(&dir, name, version, deps);
        crate::registry::publish(&dir, &self.loc).unwrap();
    }

    fn resolve(&self, deps: &str, lock: Option<&Lockfile>) -> Result<Resolution, String> {
        let app = self.root.join("app");
        write_package(&app, "app", "0.1.0", deps);
        let manifest = Manifest::from_dir(&app).unwrap();
        resolve(&app, &manifest, &self.loc, lock)
    }
}

fn versions(r: &Resolution) -> Vec<String> {
    r.packages
        .values()
        .map(|p| format!("{} {}", p.name, p.version))
        .collect()
}

#[test]
fn picks_highest_compatible_and_transitive() {
    let e = env();
    e.publish("lib", "1.0.0", "");
    e.publish("lib", "1.4.2", "util: \"0.2\", ");
    e.publish("lib", "2.0.0", "");
    e.publish("util", "0.2.1", "");
    e.publish("util", "0.3.0", "");
    let r = e.resolve("lib: \"^1.0\", ", None).unwrap();
    assert_eq!(versions(&r), ["lib 1.4.2", "util 0.2.1"]);
    assert_eq!(r.root_dependencies, ["lib"]);
    assert_eq!(r.packages["lib"].dependencies, ["util"]);
    assert!(matches!(r.packages["lib"].source, Source::Registry { .. }));
}

#[test]
fn backtracks_to_an_older_version() {
    let e = env();
    e.publish("util", "1.0.0", "");
    e.publish("util", "2.0.0", "");
    e.publish("lib", "1.0.0", "util: \"^1\", ");
    e.publish("lib", "1.1.0", "util: \"^2\", ");
    // app pins util ^1, so lib 1.1.0 (needing util ^2) must be rejected in favour of lib 1.0.0.
    let r = e.resolve("util: \"^1\", lib: \"^1\", ", None).unwrap();
    assert_eq!(versions(&r), ["lib 1.0.0", "util 1.0.0"]);
}

#[test]
fn conflict_lists_requirement_chains() {
    let e = env();
    e.publish("b", "1.0.0", "");
    e.publish("b", "2.0.0", "");
    e.publish("c", "1.0.0", "b: \"^2.0\", ");
    let err = e.resolve("b: \"^1.0\", c: \"1\", ", None).unwrap_err();
    assert!(err.contains("conflicting requirements for `b`"), "{err}");
    assert!(err.contains("app 0.1.0 requires `b ^1.0`"), "{err}");
    assert!(
        err.contains("app 0.1.0 → c 1.0.0 requires `b ^2.0`"),
        "{err}"
    );
}

#[test]
fn missing_package_and_version() {
    let e = env();
    e.publish("lib", "1.0.0", "");
    let err = e.resolve("nope: \"1\", ", None).unwrap_err();
    assert!(err.contains("`nope` is not in the registry"), "{err}");
    let err = e.resolve("lib: \"^3\", ", None).unwrap_err();
    assert!(
        err.contains("no version of `lib` matches `lib ^3`"),
        "{err}"
    );
    assert!(err.contains("available: 1.0.0"), "{err}");
}

#[test]
fn prefers_locked_version_while_it_satisfies() {
    let e = env();
    e.publish("lib", "1.0.0", "");
    e.publish("lib", "1.1.0", "");
    let lock = Lockfile::new(vec![LockedPackage {
        name: "lib".into(),
        version: "1.0.0".into(),
        source: REGISTRY_SOURCE.into(),
        checksum: None,
        dependencies: vec![],
        native: Default::default(),
    }]);
    assert_eq!(
        versions(&e.resolve("lib: \"^1.0\", ", Some(&lock)).unwrap()),
        ["lib 1.0.0"]
    );
    // The requirement moved past the locked version: the lock is ignored for `lib`.
    assert_eq!(
        versions(&e.resolve("lib: \"^1.1\", ", Some(&lock)).unwrap()),
        ["lib 1.1.0"]
    );
}

#[test]
fn path_dependencies_and_their_deps() {
    let e = env();
    e.publish("util", "0.1.0", "");
    write_package(&e.root.join("mylib"), "mylib", "0.5.0", "util: \"0.1\", ");
    let r = e.resolve("mylib: { path: \"../mylib\" }, ", None).unwrap();
    assert_eq!(versions(&r), ["mylib 0.5.0", "util 0.1.0"]);
    assert_eq!(
        r.packages["mylib"].source,
        Source::Path {
            dir: e.root.join("mylib")
        }
    );

    let err = e
        .resolve("mylib: { path: \"../mylib\", version: \"^1\" }, ", None)
        .unwrap_err();
    assert!(err.contains("does not match"), "{err}");
    let err = e
        .resolve("other: { path: \"../mylib\" }, ", None)
        .unwrap_err();
    assert!(err.contains("is the package `mylib`"), "{err}");
}
