use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;
use crate::install::{install, InstallOptions};
use crate::lockfile::Lockfile;
use crate::manifest::Manifest;

fn spec(url: &str, reference: GitRef, subdir: Option<&str>) -> GitSpec {
    GitSpec {
        url: url.into(),
        reference,
        subdir: subdir.map(str::to_string),
    }
}

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

#[test]
fn sources_round_trip_through_the_lockfile() {
    for s in [
        spec(
            "https://github.com/signalxjs/core",
            GitRef::Branch("main".into()),
            Some("packages/velt/velt"),
        ),
        spec("https://github.com/acme/json", GitRef::Default, None),
        spec(
            "git@github.com:acme/http.git",
            GitRef::Tag("v0.3.1".into()),
            None,
        ),
        spec("file:///tmp/repo", GitRef::Rev(COMMIT.into()), Some("lib")),
    ] {
        let source = s.source(COMMIT);
        assert_eq!(
            GitSpec::parse_source(&source),
            Some((s, COMMIT.to_string())),
            "{source}"
        );
    }
    assert_eq!(
        spec("https://h/r", GitRef::Branch("main".into()), Some("a/b")).source(COMMIT),
        format!("git+https://h/r?branch=main&path=a/b#{COMMIT}")
    );
    assert_eq!(GitSpec::parse_source("registry"), None);
    assert_eq!(GitSpec::parse_source("git+https://h/r#main"), None);
    assert_eq!(
        GitSpec::parse_source(&format!("git+https://h/r?x=1#{COMMIT}")),
        None
    );
}

#[test]
fn urls_and_commits_are_checked() {
    for ok in [
        "https://github.com/a/b",
        "http://localhost:8080/a.git",
        "ssh://git@host/a/b",
        "git@github.com:a/b.git",
        "file:///srv/repo",
    ] {
        assert!(check_url(ok).is_ok(), "{ok}");
    }
    for bad in [
        "github.com/a/b",
        "-uhttps://x",
        "ext::sh -c id",
        "https://h/r#x",
        "https://h/a b",
        "/srv/repo",
    ] {
        assert!(check_url(bad).is_err(), "{bad}");
    }
    assert!(is_commit(COMMIT));
    assert!(!is_commit("0123456"));
    assert!(!is_commit(&COMMIT.to_uppercase()));
}

#[test]
fn manifests_check_the_git_fields() {
    let read = |dep: &str| {
        Manifest::parse(&format!(
            "export const pkg: Package = {{ name: \"app\", version: \"0.1.0\", dependencies: {{ x: {dep} }} }};"
        ))
    };
    let ok = read(r#"{ git: "https://h/r", branch: "main", path: "packages/x" }"#).unwrap();
    assert_eq!(
        ok.dependencies["x"].git(),
        Some(spec(
            "https://h/r",
            GitRef::Branch("main".into()),
            Some("packages/x")
        ))
    );
    for (dep, message) in [
        (r#"{ branch: "main" }"#, "`branch` needs a `git` repository"),
        (
            r#"{ git: "https://h/r", branch: "a", tag: "b" }"#,
            "`branch` and `tag` both name a commit",
        ),
        (
            r#"{ git: "https://h/r", rev: "abc123" }"#,
            "`rev` must be a full commit hash",
        ),
        (
            r#"{ git: "https://h/r", path: "../x" }"#,
            "with `git`, `path` is a `/`-separated directory inside the repository",
        ),
        (r#"{ git: "h/r" }"#, "is not a git repository URL"),
    ] {
        let err = read(dep).unwrap_err();
        assert!(err.contains(message), "{dep}: {err}");
    }
    // Written back as read, for `velt add` and `velt manifest --json`.
    let text = ok.to_vlt();
    assert!(
        text.contains(r#"x: { git: "https://h/r", branch: "main", path: "packages/x" }"#),
        "{text}"
    );
    assert_eq!(ok.to_json()["dependencies"]["x"]["branch"], "main");
}

/// A repository made with the `git` command, in a temporary directory.
struct Repo {
    dir: PathBuf,
}

impl Repo {
    fn new(root: &Path) -> Repo {
        let dir = root.join("upstream");
        std::fs::create_dir_all(&dir).unwrap();
        let repo = Repo { dir };
        repo.git(&["init", "-q", "-b", "main"]);
        repo
    }

    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Commit a package `name` at `subdir` whose `src/lib.vlt` says `says`; returns the commit.
    fn commit_package(&self, subdir: &str, name: &str, says: &str) -> String {
        let pkg = self.dir.join(subdir);
        std::fs::create_dir_all(pkg.join("src")).unwrap();
        std::fs::write(
            pkg.join("package.vlt"),
            format!("export const pkg: Package = {{ name: \"{name}\", version: \"0.1.0\" }};"),
        )
        .unwrap();
        std::fs::write(
            pkg.join("src/lib.vlt"),
            format!("export const says = \"{says}\";\n"),
        )
        .unwrap();
        // A file beside the package, which the checksum covers too.
        std::fs::write(self.dir.join("README.md"), says).unwrap();
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", says]);
        self.git(&["rev-parse", "HEAD"])
    }

    fn url(&self) -> String {
        format!("file://{}", self.dir.display())
    }
}

struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    loc: Locations,
    upstream: Repo,
}

fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let root = crate::relpath::absolute(tmp.path());
    let loc = Locations::under(&root.join("home"));
    let upstream = Repo::new(&root);
    Env {
        _tmp: tmp,
        root,
        loc,
        upstream,
    }
}

impl Env {
    fn app(&self, dep: &str) -> PathBuf {
        let app = self.root.join("app");
        std::fs::create_dir_all(app.join("src")).unwrap();
        std::fs::write(
            app.join("package.vlt"),
            format!("export const pkg: Package = {{ name: \"app\", version: \"0.1.0\", dependencies: {{ {dep} }} }};"),
        )
        .unwrap();
        app
    }

    fn install(&self, app: &Path, update: bool) -> Result<crate::Installed, String> {
        let opts = InstallOptions {
            update,
            ..Default::default()
        };
        install(app, &self.loc, opts)
    }

    /// What the installed `lib` says, from its checkout.
    fn says(&self, installed: &crate::Installed) -> String {
        let lib = &installed.graph.packages()[0].dependencies["lib"];
        std::fs::read_to_string(lib.join("src/lib.vlt")).unwrap()
    }
}

#[test]
fn a_branch_is_locked_to_a_commit_until_update() {
    let e = env();
    let first = e.upstream.commit_package("packages/lib", "lib", "one");
    let app = e.app(&format!(
        "lib: {{ git: \"{}\", branch: \"main\", path: \"packages/lib\" }}",
        e.upstream.url()
    ));
    let installed = e.install(&app, false).unwrap();
    assert!(e.says(&installed).contains("one"));
    let lock = Lockfile::read(&app).unwrap().unwrap();
    let locked = lock.get("lib").unwrap();
    let (locked_spec, commit) = GitSpec::parse_source(&locked.source).unwrap();
    assert_eq!(commit, first);
    assert_eq!(locked_spec.subdir.as_deref(), Some("packages/lib"));
    assert!(locked.checksum.as_deref().unwrap().starts_with("sha256:"));

    // The branch moves on; the lock keeps the build where it was, without asking the
    // repository (it is gone).
    let second = e.upstream.commit_package("packages/lib", "lib", "two");
    let moved = e.upstream.dir.with_file_name("moved");
    std::fs::rename(&e.upstream.dir, &moved).unwrap();
    let installed = e.install(&app, false).unwrap();
    assert!(!installed.lock_changed);
    assert!(e.says(&installed).contains("one"));
    std::fs::rename(&moved, &e.upstream.dir).unwrap();

    // `velt update` moves to the branch's newest commit.
    let installed = e.install(&app, true).unwrap();
    assert!(installed.lock_changed);
    assert!(e.says(&installed).contains("two"));
    let lock = Lockfile::read(&app).unwrap().unwrap();
    assert!(lock.get("lib").unwrap().source.ends_with(&second));
}

#[test]
fn tags_revs_and_the_default_branch() {
    let e = env();
    let first = e.upstream.commit_package("", "lib", "one");
    e.upstream.git(&["tag", "-a", "v1", "-m", "v1"]);
    e.upstream.commit_package("", "lib", "two");
    let url = e.upstream.url();
    for (dep, says) in [
        (format!("lib: {{ git: \"{url}\", tag: \"v1\" }}"), "one"),
        (
            format!("lib: {{ git: \"{url}\", rev: \"{first}\" }}"),
            "one",
        ),
        (format!("lib: {{ git: \"{url}\" }}"), "two"),
    ] {
        let app = e.app(&dep);
        let _ = std::fs::remove_file(app.join("velt.lock.json"));
        let installed = e.install(&app, false).unwrap();
        assert!(e.says(&installed).contains(says), "{dep}");
    }
}

#[test]
fn wrong_packages_refs_and_checkouts_are_errors() {
    let e = env();
    let commit = e.upstream.commit_package("lib", "lib", "one");
    let url = e.upstream.url();
    let app = e.app(&format!("other: {{ git: \"{url}\", path: \"lib\" }}"));
    let err = e.install(&app, false).unwrap_err();
    assert!(
        err.contains("the package at") && err.contains("is `lib`"),
        "{err}"
    );
    let app = e.app(&format!(
        "lib: {{ git: \"{url}\", branch: \"nope\", path: \"lib\" }}"
    ));
    let err = e.install(&app, false).unwrap_err();
    assert!(err.contains("has no branch `nope`"), "{err}");

    // A checkout changed in the cache no longer matches the lock.
    let app = e.app(&format!("lib: {{ git: \"{url}\", path: \"lib\" }}"));
    e.install(&app, false).unwrap();
    let checkout = repository_dir(&e.loc, &url).join(&commit);
    std::fs::write(checkout.join("README.md"), "tampered").unwrap();
    let err = e.install(&app, false).unwrap_err();
    assert!(err.contains("checksum mismatch for commit"), "{err}");
}

#[test]
fn a_git_dependency_needs_a_version_to_be_published() {
    let e = env();
    let app = e.app(&format!("lib: {{ git: \"{}\" }}", e.upstream.url()));
    std::fs::write(app.join("src/lib.vlt"), "\n").unwrap();
    let err = crate::registry::publish(&app, &e.loc).unwrap_err();
    assert!(
        err.contains("dependency `lib` has only a `git` repository"),
        "{err}"
    );
}
