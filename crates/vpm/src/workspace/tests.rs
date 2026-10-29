use std::path::{Path, PathBuf};

use super::*;
use crate::install::{install, InstallOptions};
use crate::locations::Locations;
use crate::lockfile::{Lockfile, LOCK_FILE};

struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    loc: Locations,
}

fn env(workspace: &str) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let root = relpath::absolute(tmp.path()).join("repo");
    std::fs::create_dir_all(&root).unwrap();
    let loc = Locations::under(&relpath::absolute(tmp.path()).join("home"));
    std::fs::write(
        root.join(WORKSPACE_FILE),
        format!("export const workspace: Workspace = {workspace};\n"),
    )
    .unwrap();
    Env {
        _tmp: tmp,
        root,
        loc,
    }
}

fn write_package(dir: &Path, name: &str, fields: &str) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join(MANIFEST_FILE),
        format!(
            "export const pkg: Package = {{ name: \"{name}\", version: \"0.1.0\", {fields} }};"
        ),
    )
    .unwrap();
    std::fs::write(dir.join("src/lib.vlt"), format!("// {name}\n")).unwrap();
}

impl Env {
    fn member(&self, rel: &str, name: &str, deps: &str) -> PathBuf {
        let dir = self.root.join(rel);
        write_package(&dir, name, &format!("dependencies: {{ {deps} }}"));
        dir
    }

    fn publish(&self, name: &str, version: &str) {
        let dir = self
            .root
            .join("../published")
            .join(format!("{name}-{version}"));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join(MANIFEST_FILE),
            format!("export const pkg: Package = {{ name: \"{name}\", version: \"{version}\" }};"),
        )
        .unwrap();
        std::fs::write(dir.join("src/lib.vlt"), "\n").unwrap();
        crate::registry::publish(&dir, &self.loc).unwrap();
    }

    fn install(&self, dir: &Path) -> Result<crate::Installed, String> {
        install(dir, &self.loc, InstallOptions::default())
    }

    fn lock(&self) -> Lockfile {
        Lockfile::read(&self.root)
            .unwrap()
            .expect("a lockfile at the workspace root")
    }
}

fn graph_names(installed: &crate::Installed) -> Vec<&str> {
    installed
        .graph
        .packages()
        .iter()
        .map(|p| p.name.as_str())
        .collect()
}

#[test]
fn members_share_one_lock_and_each_gets_its_own_graph() {
    let e = env(r#"{ members: ["packages/*", "apps/*"], dependencies: { json: "1.2" } }"#);
    e.publish("json", "1.2.3");
    e.publish("json", "2.0.0");
    e.publish("log", "0.2.0");
    e.member("packages/core", "core", r#"json: "workspace""#);
    e.member("packages/extra", "extra", r#"log: "0.2""#);
    let server = e.member("apps/server", "server", r#"core: "workspace""#);

    let installed = e.install(&server).unwrap();
    assert!(installed.lock_changed);
    assert_eq!(graph_names(&installed), ["server", "core", "json"]);
    let core = &installed.graph.packages()[0].dependencies["core"];
    assert_eq!(core, &e.root.join("packages/core"));
    assert!(!server.join(LOCK_FILE).exists());

    let lock = e.lock();
    let sources: Vec<(&str, &str, &str)> = lock
        .packages
        .iter()
        .map(|p| (p.name.as_str(), p.version.as_str(), p.source.as_str()))
        .collect();
    assert_eq!(
        sources,
        [
            ("core", "0.1.0", "workspace+packages/core"),
            ("extra", "0.1.0", "workspace+packages/extra"),
            ("json", "1.2.3", "registry"),
            ("log", "0.2.0", "registry"),
            ("server", "0.1.0", "workspace+apps/server"),
        ]
    );
    // Another member reuses the lock unchanged.
    let core = e.install(&e.root.join("packages/core")).unwrap();
    assert!(!core.lock_changed);
    assert_eq!(graph_names(&core), ["core", "json"]);
}

#[test]
fn the_whole_workspace_installs_from_its_root() {
    let e = env(r#"{ members: ["packages/*"] }"#);
    e.member("packages/a", "a", "");
    e.member("packages/b", "b", r#"a: "workspace""#);
    let ws = Workspace::load(&e.root).unwrap();
    let installed =
        crate::install::workspace::install(&ws, None, &e.loc, InstallOptions::default()).unwrap();
    assert_eq!(graph_names(&installed), ["a", "b"]);
    assert_eq!(e.lock().packages.len(), 2);
}

#[test]
fn members_conflicting_on_a_version_name_both_chains() {
    let e = env(r#"{ members: ["*"] }"#);
    e.publish("json", "1.2.0");
    e.publish("json", "2.0.0");
    let a = e.member("a", "a", r#"json: "1.2""#);
    e.member("b", "b", r#"json: "2.0""#);
    let err = e.install(&a).unwrap_err();
    assert!(err.contains("a 0.1.0") && err.contains("b 0.1.0"), "{err}");
}

#[test]
fn a_member_with_its_own_lock_is_refused() {
    let e = env(r#"{ members: ["a"] }"#);
    let a = e.member("a", "a", "");
    std::fs::write(a.join(LOCK_FILE), "{\"version\": 1}").unwrap();
    let err = e.install(&a).unwrap_err();
    assert!(
        err.starts_with("a/velt.lock.json: a member of a workspace has no lockfile"),
        "{err}"
    );
}

#[test]
fn a_package_outside_the_members_stays_on_its_own() {
    let e = env(r#"{ members: ["a"] }"#);
    e.member("a", "a", "");
    let alone = e.member("tools/alone", "alone", "");
    let installed = e.install(&alone).unwrap();
    assert_eq!(graph_names(&installed), ["alone"]);
    assert!(alone.join(LOCK_FILE).exists());
    assert!(!e.root.join(LOCK_FILE).exists());
}

#[test]
fn workspace_outside_a_workspace_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let app = relpath::absolute(tmp.path()).join("app");
    write_package(&app, "app", r#"dependencies: { core: "workspace" }"#);
    let loc = Locations::under(&relpath::absolute(tmp.path()).join("home"));
    let err = install(&app, &loc, InstallOptions::default()).unwrap_err();
    assert!(
        err.contains("`app` is not a member of a workspace"),
        "{err}"
    );
}

#[test]
fn load_checks_members_and_their_manifests() {
    let e = env(r#"{ members: ["packages/*", "apps/*"], velt: "0.2" }"#);
    e.member(
        "packages/a",
        "a",
        r#"b: "0.1", json: "workspace", jsno: "workspace""#,
    );
    write_package(
        &e.root.join("packages/b"),
        "b",
        r#"velt: "0.2", registry: "https://r.example.com""#,
    );
    write_package(&e.root.join("packages/b2"), "b", "");
    let err = Workspace::load(&e.root).unwrap_err();
    let lines: Vec<&str> = err.lines().collect();
    assert_eq!(
        lines,
        [
            "velt.workspace.vlt: `apps/*` matches no package (no directory it names has a package.vlt)",
            "two members are named `b`: packages/b and packages/b2",
            "packages/a/package.vlt: `b` is a member of this workspace; write `b: \"workspace\"`",
            "packages/a/package.vlt: `jsno: \"workspace\"`, but no member and no workspace dependency is named `jsno`",
            "packages/a/package.vlt: `json: \"workspace\"`, but no member and no workspace dependency is named `json`",
            "packages/b/package.vlt: `velt` is set by the workspace (velt.workspace.vlt); remove it here",
            "packages/b/package.vlt: `registry` is set by the workspace (velt.workspace.vlt); remove it here",
        ]
    );
}

#[test]
fn unknown_workspace_dependencies_get_a_suggestion() {
    let e = env(r#"{ members: ["*"], dependencies: { json: "1.2" } }"#);
    e.member("a", "a", r#"jsonn: "workspace""#);
    let err = Workspace::load(&e.root).unwrap_err();
    assert!(err.ends_with("(did you mean `json`?)"), "{err}");
}

#[test]
fn excluded_and_root_members() {
    let e = env(r#"{ members: [".", "packages/*", "!packages/old"] }"#);
    write_package(&e.root, "root", "");
    e.member("packages/new", "new", "");
    e.member("packages/old", "old", "");
    let ws = Workspace::load(&e.root).unwrap();
    let names: Vec<&str> = ws.members.iter().map(Member::name).collect();
    assert_eq!(names, ["root", "new"]);
    assert_eq!(
        ws.member_at(&e.root.join("packages/new")).unwrap().name(),
        "new"
    );
    assert!(ws.member("old").is_none());
}

#[test]
fn effective_manifests_resolve_workspace_requirements() {
    let e = env(
        r#"{ members: ["*"], velt: "0.2", registry: "https://r.example.com",
             dependencies: { json: "1.2", util: { path: "vendor/util" } } }"#,
    );
    e.member("a", "a", "");
    e.member(
        "b",
        "b",
        r#"a: "workspace", json: "workspace", util: "workspace", log: "0.3""#,
    );
    let ws = Workspace::load(&e.root).unwrap();
    let b = ws.effective_manifest(ws.member("b").unwrap());
    assert_eq!(b.toolchain.as_deref(), Some("0.2"));
    assert_eq!(b.registry.as_deref(), Some("https://r.example.com"));
    let path = |name: &str| b.dependencies[name].path().map(PathBuf::from);
    assert_eq!(path("a"), Some(e.root.join("a")));
    assert_eq!(path("util"), Some(e.root.join("vendor/util")));
    assert_eq!(b.dependencies["json"].version(), Some("1.2"));
    assert_eq!(b.dependencies["log"].version(), Some("0.3"));
}

#[test]
fn find_root_is_the_nearest_workspace_file() {
    let e = env(r#"{ members: ["a"] }"#);
    let a = e.member("a", "a", "");
    assert_eq!(find_root(&a.join("src")), Some(e.root.clone()));
    assert_eq!(find_root(e.root.parent().unwrap()), None);
}
