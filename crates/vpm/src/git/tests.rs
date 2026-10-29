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
        // Git allows the lock's delimiters in branch and directory names.
        spec(
            "https://h/r",
            GitRef::Branch("feat&path=x#1%".into()),
            Some("a=b/c?d"),
        ),
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
    assert_eq!(
        GitSpec::parse_source(&format!("git+https://h/r?branch=a%2#{COMMIT}")),
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
    // Credentials would be printed and written to the lock.
    for secret in [
        "https://user:s3cretTOKEN@github.com/a/b",
        "https://ghp_TOKEN@github.com/a/b",
        "ssh://git:pw@host/a/b",
    ] {
        let err = check_url(secret).unwrap_err();
        assert!(err.contains("can't hold credentials"), "{secret}: {err}");
    }
    assert!(check_url("ssh://git@github.com/a/b").is_ok());
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
        (
            r#"{ git: "https://h/r", path: "C:/x" }"#,
            "with `git`, `path` is a `/`-separated directory inside the repository",
        ),
        (
            r#"{ git: "https://h/r", rev: "0123456789ABCDEF0123456789abcdef01234567" }"#,
            "in lowercase hex (40 digits, or 64 for SHA-256)",
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

/// `file:///…` for `path`, with `/` separators (`file:///C:/…` on Windows), so it can be written
/// in a string literal of a manifest.
fn file_url(path: &Path) -> String {
    let path = path.display().to_string().replace('\\', "/");
    match path.starts_with('/') {
        true => format!("file://{path}"),
        false => format!("file:///{path}"),
    }
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
        file_url(&self.dir)
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

#[test]
fn a_path_dependency_of_a_git_package_is_locked_with_it() {
    let e = env();
    e.upstream.commit_package("sib", "sib", "sibling");
    let lib = e.upstream.dir.join("lib");
    std::fs::create_dir_all(lib.join("src")).unwrap();
    std::fs::write(
        lib.join("package.vlt"),
        "export const pkg: Package = { name: \"lib\", version: \"0.1.0\", dependencies: { sib: { path: \"../sib\" } } };",
    )
    .unwrap();
    std::fs::write(lib.join("src/lib.vlt"), "\n").unwrap();
    e.upstream.git(&["add", "-A"]);
    e.upstream.git(&["commit", "-q", "-m", "lib"]);
    let commit = e.upstream.git(&["rev-parse", "HEAD"]);
    let url = e.upstream.url();
    let app = e.app(&format!("lib: {{ git: \"{url}\", path: \"lib\" }}"));
    e.install(&app, false).unwrap();
    let lock = Lockfile::read(&app).unwrap().unwrap();
    let sib = lock.get("sib").unwrap();
    assert_eq!(
        sib.source,
        format!("git+{url}?rev={commit}&path=sib#{commit}")
    );
    assert!(sib.checksum.is_some());
    // Without the repository, the lock still holds.
    std::fs::rename(&e.upstream.dir, e.root.join("gone")).unwrap();
    assert!(!e.install(&app, false).unwrap().lock_changed);
}

#[test]
fn native_code_from_a_repository_is_not_built_unasked() {
    let e = env();
    let sib = e.upstream.dir.join("sib");
    std::fs::create_dir_all(sib.join("src")).unwrap();
    std::fs::create_dir_all(sib.join("native/src")).unwrap();
    std::fs::write(
        sib.join("package.vlt"),
        "export const pkg: Package = { name: \"sib\", version: \"0.1.0\", native: {} };",
    )
    .unwrap();
    std::fs::write(sib.join("src/lib.vlt"), "\n").unwrap();
    // Never run: the install must refuse before cargo would.
    std::fs::write(sib.join("native/build.rs"), "fn main() {}").unwrap();
    e.upstream.commit_package("lib", "lib", "x");
    std::fs::write(
        e.upstream.dir.join("lib/package.vlt"),
        "export const pkg: Package = { name: \"lib\", version: \"0.1.0\", dependencies: { sib: { path: \"../sib\" } } };",
    )
    .unwrap();
    e.upstream.git(&["add", "-A"]);
    e.upstream.git(&["commit", "-q", "-m", "native"]);
    let app = e.app(&format!(
        "lib: {{ git: \"{}\", path: \"lib\" }}",
        e.upstream.url()
    ));
    let opts = InstallOptions {
        target: Some("x86_64-unknown-linux-gnu".into()),
        ..Default::default()
    };
    let err = install(&app, &e.loc, opts).unwrap_err();
    assert!(
        err.contains("`sib` from") && err.contains("VELT_NATIVE_FROM_SOURCE=1"),
        "{err}"
    );
}

#[test]
fn paths_never_lead_out_of_the_repository() {
    let e = env();
    e.upstream.commit_package("lib", "lib", "x");
    std::fs::write(
        e.upstream.dir.join("lib/package.vlt"),
        "export const pkg: Package = { name: \"lib\", version: \"0.1.0\", dependencies: { out: { path: \"../../outside\" } } };",
    )
    .unwrap();
    e.upstream.git(&["add", "-A"]);
    e.upstream.git(&["commit", "-q", "-m", "out"]);
    let url = e.upstream.url();
    let app = e.app(&format!("lib: {{ git: \"{url}\", path: \"lib\" }}"));
    let err = e.install(&app, false).unwrap_err();
    assert!(
        err.contains("its dependency `out` has a `path` that leads out of the repository"),
        "{err}"
    );

    // A symbolic link in the repository to a package elsewhere is refused too.
    #[cfg(unix)]
    {
        let elsewhere = e.root.join("elsewhere");
        std::fs::create_dir_all(elsewhere.join("src")).unwrap();
        std::fs::write(
            elsewhere.join("package.vlt"),
            "export const pkg: Package = { name: \"link\", version: \"0.1.0\" };",
        )
        .unwrap();
        std::os::unix::fs::symlink(&elsewhere, e.upstream.dir.join("link")).unwrap();
        e.upstream.git(&["add", "-A"]);
        e.upstream.git(&["commit", "-q", "-m", "link"]);
        let app = e.app(&format!("link: {{ git: \"{url}\", path: \"link\" }}"));
        let _ = std::fs::remove_file(app.join("velt.lock.json"));
        let err = e.install(&app, false).unwrap_err();
        // Refused when it is checked out, before anything reads the package.
        assert!(
            err.contains("has a symbolic link `link` that points out of the repository"),
            "{err}"
        );
    }
}

#[cfg(unix)]
#[test]
fn links_must_resolve_inside_the_checkout() {
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    let tree = tmp.path().join("tree");
    std::fs::create_dir_all(tree.join("lib/src")).unwrap();
    std::fs::create_dir_all(tree.join("d")).unwrap();
    std::fs::write(tree.join("lib/src/real.vlt"), "\n").unwrap();
    // Inside: a sibling, a parent's child, a link to a directory link, one that dangles inside.
    symlink("real.vlt", tree.join("lib/src/same.vlt")).unwrap();
    symlink("../lib", tree.join("d/lib")).unwrap();
    symlink("d/lib/src", tree.join("srclink")).unwrap();
    symlink("not-yet", tree.join("lib/dangling")).unwrap();
    assert_eq!(check_links(&tree), Ok(()));

    let refused = |name: &str, target: &str| {
        let link = tree.join(name);
        symlink(target, &link).unwrap();
        let err = check_links(&tree).unwrap_err();
        std::fs::remove_file(&link).unwrap();
        assert!(
            err.contains(&format!("`{name}` that points out of the repository")),
            "{err}"
        );
    };
    refused("lib/src/leak.vlt", "/etc/passwd");
    refused("lib/src/up.vlt", "../../../outside/lib.vlt");
    // `d/up` is the checkout's root, so `../x` through it leaves the checkout, though its text
    // alone (`d/up/../x`) looks as if it stays inside.
    symlink("..", tree.join("d/up")).unwrap();
    refused("chain", "d/up/../x");
    // A loop never ends inside.
    symlink("loop2", tree.join("loop1")).unwrap();
    refused("loop2", "loop1");
}

#[cfg(unix)]
#[test]
fn a_repository_with_a_link_out_of_it_is_refused() {
    let e = env();
    e.upstream.commit_package("lib", "lib", "x");
    let outside = e.root.join("outside.vlt");
    std::fs::write(&outside, "export const says = \"secret\";\n").unwrap();
    std::os::unix::fs::symlink(&outside, e.upstream.dir.join("lib/src/leak.vlt")).unwrap();
    e.upstream.git(&["add", "-A"]);
    e.upstream.git(&["commit", "-q", "-m", "leak"]);
    let app = e.app(&format!(
        "lib: {{ git: \"{}\", path: \"lib\" }}",
        e.upstream.url()
    ));
    let err = e.install(&app, false).unwrap_err();
    assert!(
        err.contains("has a symbolic link `lib/src/leak.vlt` that points out of the repository"),
        "{err}"
    );
    // Nothing was put in the cache.
    let commit = e.upstream.git(&["rev-parse", "HEAD"]);
    assert!(!repository_dir(&e.loc, &e.upstream.url())
        .join(commit)
        .exists());
}
