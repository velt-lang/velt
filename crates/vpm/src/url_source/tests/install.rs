//! Installs of url dependencies, served by a local HTTP server: download, hash, cache, lock,
//! `strip` and `path`, sibling packages of one archive, redirects, and what is refused.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use velt_http::{Request, Response, Server};

use crate::install::{install, InstallOptions};
use crate::locations::Locations;
use crate::lockfile::Lockfile;

mod trust;

/// A local server of `files` (path → bytes, or `Redirect` to a URL), logging each request.
struct Serve {
    server: Option<Server>,
    files: Arc<Mutex<HashMap<String, Body>>>,
    log: Arc<Mutex<Vec<String>>>,
}

#[derive(Clone)]
enum Body {
    Bytes(Vec<u8>),
    Redirect(String),
}

impl Serve {
    fn new() -> Serve {
        let files: Arc<Mutex<HashMap<String, Body>>> = Arc::default();
        let log: Arc<Mutex<Vec<String>>> = Arc::default();
        let (f, l) = (files.clone(), log.clone());
        let handler = Arc::new(move |req: Request| {
            l.lock().unwrap().push(req.path.clone());
            match f.lock().unwrap().get(&req.path).cloned() {
                Some(Body::Bytes(b)) => Response::bytes(200, "application/octet-stream", b),
                Some(Body::Redirect(to)) => Response::text(302, "").with_header("Location", &to),
                None => Response::text(404, "not found"),
            }
        });
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let server = Server::start(listener, handler, 1 << 20).unwrap();
        Serve {
            server: Some(server),
            files,
            log,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.server.as_ref().unwrap().addr())
    }

    fn put(&self, path: &str, body: Body) {
        self.files.lock().unwrap().insert(path.to_string(), body);
    }

    fn requests(&self) -> usize {
        self.log.lock().unwrap().len()
    }

    fn stop(&mut self) {
        if let Some(s) = self.server.take() {
            s.stop();
        }
    }
}

/// A `.tar.gz` of `files` (path → contents; a contents starting `-> ` is a symbolic link).
fn tar_gz(files: &[(&str, &str)]) -> Vec<u8> {
    let mut b = tar::Builder::new(vec![]);
    for (path, contents) in files {
        let mut h = tar::Header::new_gnu();
        match contents.strip_prefix("-> ") {
            Some(target) => {
                h.set_entry_type(tar::EntryType::Symlink);
                h.set_size(0);
                b.append_link(&mut h, path, target).unwrap();
            }
            None => {
                h.set_size(contents.len() as u64);
                h.set_mode(0o644);
                b.append_data(&mut h, path, contents.as_bytes()).unwrap();
            }
        }
    }
    let mut gz = flate2::write::GzEncoder::new(vec![], flate2::Compression::default());
    gz.write_all(&b.into_inner().unwrap()).unwrap();
    gz.finish().unwrap()
}

fn manifest(name: &str, version: &str, rest: &str) -> String {
    format!("export const pkg: Package = {{ name: \"{name}\", version: \"{version}\"{rest} }};")
}

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

impl Env {
    fn app(&self, deps: &str, rest: &str) -> PathBuf {
        let app = self.root.join("app");
        std::fs::create_dir_all(app.join("src")).unwrap();
        std::fs::write(
            app.join("package.velt"),
            manifest(
                "app",
                "0.1.0",
                &format!(", dependencies: {{ {deps} }}{rest}"),
            ),
        )
        .unwrap();
        app
    }

    fn install(&self, app: &Path) -> Result<crate::Installed, String> {
        install(app, &self.loc, InstallOptions::default())
    }
}

fn hex(bytes: &[u8]) -> String {
    velt_toolchain::install::sha256_hex(bytes)
}

/// The release archive of `vue` 3.6.0: one top directory, the package at `packages/vue`.
fn vue() -> Vec<u8> {
    tar_gz(&[
        (
            "vue-3.6.0/packages/vue/package.velt",
            &manifest("vue", "3.6.0", ""),
        ),
        (
            "vue-3.6.0/packages/vue/src/lib.velt",
            "export const v = 3;\n",
        ),
        ("vue-3.6.0/README.md", "vue"),
    ])
}

fn dep(url: &str, archive: &[u8], rest: &str) -> String {
    format!(
        "vue: {{ url: \"{url}\", sha256: \"{}\"{rest} }}",
        hex(archive)
    )
}

#[test]
fn an_archive_is_downloaded_checked_cached_and_locked() {
    let e = env();
    let mut s = Serve::new();
    let archive = vue();
    s.put("/v3.6.0/vue.tar.gz", Body::Bytes(archive.clone()));
    let url = s.url("/v3.6.0/vue.tar.gz");
    let app = e.app(
        &dep(&url, &archive, ", strip: 1, path: \"packages/vue\""),
        "",
    );
    let installed = e.install(&app).unwrap();
    let vue_dir = &installed.graph.packages()[0].dependencies["vue"];
    assert_eq!(
        std::fs::read_to_string(vue_dir.join("src/lib.velt")).unwrap(),
        "export const v = 3;\n"
    );
    assert!(vue_dir.starts_with(e.loc.cache.join("url").join(hex(&archive))));
    let lock = Lockfile::read(&app).unwrap().unwrap();
    let locked = lock.get("vue").unwrap();
    assert_eq!(
        locked.source,
        format!(
            "url+{url}?strip=1&path=packages/vue#sha256:{}",
            hex(&archive)
        )
    );
    assert_eq!(
        locked.checksum.as_deref(),
        Some(crate::git::tree_checksum(vue_dir).unwrap().as_str())
    );
    assert_eq!(locked.version, "3.6.0");
    // Installed again: from the cache, with the server gone.
    let downloads = s.requests();
    s.stop();
    e.install(&app).unwrap();
    assert_eq!(downloads, 1);
    // An edited tree in the cache is a checksum error, not used.
    std::fs::write(vue_dir.join("src/lib.velt"), "export const v = 4;\n").unwrap();
    let err = e.install(&app).unwrap_err();
    assert!(
        err.contains("checksum mismatch") && err.contains("delete `"),
        "{err}"
    );
}

#[test]
fn another_archive_at_the_url_is_an_error() {
    let e = env();
    let s = Serve::new();
    s.put("/vue.tar.gz", Body::Bytes(vue()));
    let other = tar_gz(&[("package.velt", &manifest("vue", "9.9.9", ""))]);
    let app = e.app(&dep(&s.url("/vue.tar.gz"), &other, ""), "");
    let err = e.install(&app).unwrap_err();
    assert!(
        err.contains("has SHA-256") && err.contains("the archive changed"),
        "{err}"
    );
    // Nothing of it was kept.
    assert!(!e.loc.cache.join("url").join(hex(&vue())).exists());
    assert!(!e.loc.cache.join("url").join(hex(&other)).exists());
    // A URL that doesn't exist says so.
    let app = e.app(&dep(&s.url("/missing.tar.gz"), &other, ""), "");
    let err = e.install(&app).unwrap_err();
    assert!(err.contains("doesn't exist (HTTP 404)"), "{err}");
}

#[test]
fn redirects_are_followed_only_to_https_or_this_machine() {
    let e = env();
    let s = Serve::new();
    let archive = vue();
    s.put("/real/vue.tar.gz", Body::Bytes(archive.clone()));
    s.put(
        "/latest/vue.tar.gz",
        Body::Redirect(s.url("/real/vue.tar.gz")),
    );
    s.put(
        "/away/vue.tar.gz",
        Body::Redirect("http://example.com/vue.tar.gz".into()),
    );
    let app = e.app(
        &dep(
            &s.url("/latest/vue.tar.gz"),
            &archive,
            ", strip: 1, path: \"packages/vue\"",
        ),
        "",
    );
    e.install(&app).unwrap();
    // The lock keeps the URL as written, not where it led.
    let lock = Lockfile::read(&app).unwrap().unwrap();
    assert!(lock
        .get("vue")
        .unwrap()
        .source
        .contains("/latest/vue.tar.gz"));
    let e = env();
    let app = e.app(&dep(&s.url("/away/vue.tar.gz"), &archive, ""), "");
    let err = e.install(&app).unwrap_err();
    assert!(err.contains("plain http:// to another machine"), "{err}");
}

#[test]
fn strip_and_path_choose_a_directory_whose_links_stay_inside_it() {
    let e = env();
    let s = Serve::new();
    let two_tops = tar_gz(&[
        ("a/package.velt", &manifest("vue", "1.0.0", "")),
        ("b/x", ""),
    ]);
    s.put("/two.tar.gz", Body::Bytes(two_tops.clone()));
    let app = e.app(&dep(&s.url("/two.tar.gz"), &two_tops, ", strip: 1"), "");
    let err = e.install(&app).unwrap_err();
    assert!(
        err.contains("`strip: 1` needs one directory at level 1, but it has 2"),
        "{err}"
    );
    let app = e.app(&dep(&s.url("/two.tar.gz"), &two_tops, ", path: \"c\""), "");
    let err = e.install(&app).unwrap_err();
    assert!(err.contains("it has no directory `c`"), "{err}");
    // A link that stays in the archive but leaves the chosen directory.
    let leaky = tar_gz(&[
        ("pkg/package.velt", &manifest("vue", "1.0.0", "")),
        ("pkg/src/secret.velt", "-> ../../other/secret.velt"),
        ("other/secret.velt", "x"),
    ]);
    s.put("/leaky.tar.gz", Body::Bytes(leaky.clone()));
    let app = e.app(&dep(&s.url("/leaky.tar.gz"), &leaky, ", path: \"pkg\""), "");
    let err = e.install(&app).unwrap_err();
    assert!(err.contains("points out of that part"), "{err}");
}

#[test]
fn a_path_dependency_is_another_directory_of_the_same_archive() {
    let e = env();
    let s = Serve::new();
    let mono = tar_gz(&[
        (
            "repo/packages/vue/package.velt",
            &manifest(
                "vue",
                "3.6.0",
                ", dependencies: { shared: { path: \"../shared\" } }",
            ),
        ),
        ("repo/packages/vue/src/lib.velt", "\n"),
        (
            "repo/packages/shared/package.velt",
            &manifest("shared", "3.6.0", ""),
        ),
        ("repo/packages/shared/src/lib.velt", "\n"),
    ]);
    s.put("/mono.tar.gz", Body::Bytes(mono.clone()));
    let app = e.app(
        &dep(
            &s.url("/mono.tar.gz"),
            &mono,
            ", strip: 1, path: \"packages/vue\"",
        ),
        "",
    );
    e.install(&app).unwrap();
    let lock = Lockfile::read(&app).unwrap().unwrap();
    let shared = &lock.get("shared").unwrap().source;
    assert!(
        shared.starts_with("url+") && shared.contains("?strip=1&path=packages/shared#sha256:"),
        "{shared}"
    );
    // Downloaded once for both.
    assert_eq!(s.requests(), 1);
    // A path out of the archive is refused.
    let out = tar_gz(&[(
        "vue/package.velt",
        &manifest(
            "vue",
            "1.0.0",
            ", dependencies: { x: { path: \"../../x\" } }",
        ),
    )]);
    s.put("/out.tar.gz", Body::Bytes(out.clone()));
    let app = e.app(&dep(&s.url("/out.tar.gz"), &out, ", path: \"vue\""), "");
    let err = e.install(&app).unwrap_err();
    assert!(err.contains("leads out of the archive"), "{err}");
}

#[test]
fn native_code_from_an_archive_needs_an_entry_naming_its_source() {
    let e = env();
    let s = Serve::new();
    let native = tar_gz(&[
        (
            "vue/package.velt",
            &manifest("vue", "1.0.0", ", native: { c: {} }"),
        ),
        ("vue/src/lib.velt", "\n"),
    ]);
    s.put("/n.tar.gz", Body::Bytes(native.clone()));
    for allow in ["", ", allowNative: [\"vue\"]"] {
        let app = e.app(&dep(&s.url("/n.tar.gz"), &native, ", path: \"vue\""), allow);
        let err = e.install(&app).unwrap_err();
        // A local http URL can't be an allowNative prefix (https only), so nothing is
        // suggested; an https one is (`allow_native::tests::sources`).
        assert!(
            err.contains("comes from an archive whose URL allowNative can't name")
                && !err.contains("allowNative: ["),
            "{allow}: {err}"
        );
    }
}

#[test]
fn two_urls_of_one_archive_are_one_source() {
    // `x` and `y` (local packages: only a manifest the root controls may name plain http) ask
    // for `vue` from two URLs serving the same bytes: one tree, so not two sources. Another
    // archive is two sources.
    let e = env();
    let s = Serve::new();
    let archive = vue();
    s.put("/a/vue.tar.gz", Body::Bytes(archive.clone()));
    s.put("/mirror/vue.tar.gz", Body::Bytes(archive.clone()));
    let local = |name: &str, url: &str, archive: &[u8]| {
        let dir = e.root.join(name);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let deps = format!(
            ", dependencies: {{ vue: {{ url: \"{url}\", sha256: \"{}\", strip: 1, path: \"packages/vue\" }} }}",
            hex(archive)
        );
        std::fs::write(dir.join("package.velt"), manifest(name, "1.0.0", &deps)).unwrap();
        std::fs::write(dir.join("src/lib.velt"), "\n").unwrap();
    };
    local("x", &s.url("/a/vue.tar.gz"), &archive);
    local("y", &s.url("/mirror/vue.tar.gz"), &archive);
    let deps = "x: { path: \"../x\" }, y: { path: \"../y\" }";
    let app = e.app(deps, "");
    e.install(&app).unwrap();
    // `y` asking for another archive is two sources.
    let other = tar_gz(&[
        ("v/packages/vue/package.velt", &manifest("vue", "3.6.1", "")),
        ("v/packages/vue/src/lib.velt", "\n"),
    ]);
    s.put("/other/vue.tar.gz", Body::Bytes(other.clone()));
    local("y", &s.url("/other/vue.tar.gz"), &other);
    let err = e.install(&app).unwrap_err();
    assert!(err.contains("`vue` is asked for from two sources"), "{err}");
}

#[test]
fn velt_add_finds_the_package_and_writes_its_hash() {
    let e = env();
    let s = Serve::new();
    let archive = vue();
    s.put("/vue.tar.gz", Body::Bytes(archive.clone()));
    let url = s.url("/vue.tar.gz");
    // The package is inside the one top directory: `strip: 1` is found.
    let (name, spec) = super::super::entry_for(&e.loc, &url, Some("packages/vue")).unwrap();
    assert_eq!(name, "vue");
    assert_eq!(
        (spec.sha256.as_str(), spec.strip, spec.subdir.as_deref()),
        (hex(&archive).as_str(), 1, Some("packages/vue"))
    );
    // Without `--path` there is no package at the top: the error says what to give.
    let err = super::super::entry_for(&e.loc, &url, None).unwrap_err();
    assert!(
        err.contains("name the package's directory with `--path`"),
        "{err}"
    );
    // A flat archive needs no `strip`.
    let flat = tar_gz(&[
        ("package.velt", &manifest("flat", "0.1.0", "")),
        ("src/lib.velt", "\n"),
    ]);
    s.put("/flat.zip", Body::Bytes(flat.clone()));
    let (name, spec) = super::super::entry_for(&e.loc, &s.url("/flat.zip"), None).unwrap();
    assert_eq!((name.as_str(), spec.strip, spec.subdir), ("flat", 0, None));
    // What `velt add` writes installs, and downloads nothing more.
    let app = e.app("", "");
    let (_, spec) = super::super::entry_for(&e.loc, &url, Some("packages/vue")).unwrap();
    let dep = crate::edit::DependencySpec {
        path: spec.subdir.clone(),
        url: Some(spec.url.clone()),
        sha256: Some(spec.sha256.clone()),
        strip: Some(spec.strip),
        ..Default::default()
    };
    crate::edit::add_dependency(&app, "vue", &dep).unwrap();
    let text = std::fs::read_to_string(app.join("package.velt")).unwrap();
    assert!(
        text.contains(&format!("sha256: \"{}\"", hex(&archive))) && text.contains("strip: 1"),
        "{text}"
    );
    let before = s.requests();
    e.install(&app).unwrap();
    assert_eq!(s.requests(), before);
}
