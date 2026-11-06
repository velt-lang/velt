//! What the cache may vouch for: a URL only once its bytes hashed to the archive, a raw tree only
//! while it is what was extracted; plain http only from manifests the root controls; and the
//! checks whose breaks no other test saw.

use super::*;
use crate::url_source::{fetch, Verified};

/// A URL nothing listens on: an `https` download from it fails at once.
fn closed_https(path: &str) -> String {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    format!("https://127.0.0.1:{port}{path}")
}

fn raw(e: &Env, archive: &[u8]) -> PathBuf {
    e.loc.cache.join("url").join(hex(archive))
}

#[test]
fn an_archive_cached_from_one_url_does_not_vouch_for_another() {
    // The root allows `duck` from a prefix. Another of its dependencies names the same hash from
    // a URL anyone controls; once that archive is cached, `duck` from the allowed URL must not be
    // taken from it: the allowed URL never served those bytes.
    let e = env();
    let s = Serve::new();
    let evil = tar_gz(&[
        ("a/package.velt", &manifest("aevil", "1.0.0", "")),
        (
            "duck/package.velt",
            &manifest("duck", "1.0.0", ", native: { c: {} }"),
        ),
        ("duck/src/lib.velt", "\n"),
    ]);
    s.put("/evil.tgz", Body::Bytes(evil.clone()));
    let good = closed_https("/o/duck/releases/download/v1/duck.tar.gz");
    let prefix = good.split("/v1/").next().unwrap().to_string() + "/";
    let aevil = format!(
        "aevil: {{ url: \"{}\", sha256: \"{}\", path: \"a\" }}",
        s.url("/evil.tgz"),
        hex(&evil)
    );
    e.install(&e.app(&aevil, "")).unwrap();
    let duck = format!(
        "duck: {{ url: \"{good}\", sha256: \"{}\", path: \"duck\" }}",
        hex(&evil)
    );
    let allow = format!(", allowNative: [{{ name: \"duck\", url: \"{prefix}\" }}]");
    let app = e.app(&format!("{aevil}, {duck}"), &allow);
    let err = e.install(&app).unwrap_err();
    assert!(err.contains(&format!("cannot download {good}")), "{err}");
    let record = Verified::read(&raw(&e, &evil)).unwrap();
    assert!(!record.knows(&good));
}

#[test]
fn a_changed_cached_tree_never_reaches_a_new_lock() {
    let e = env();
    let s = Serve::new();
    let archive = vue();
    s.put("/vue.tar.gz", Body::Bytes(archive.clone()));
    let url = s.url("/vue.tar.gz");
    let app = e.app(
        &dep(&url, &archive, ", strip: 1, path: \"packages/vue\""),
        "",
    );
    e.install(&app).unwrap();
    let lib = raw(&e, &archive).join("vue-3.6.0/packages/vue/src/lib.velt");
    std::fs::write(&lib, "export const v = 667;\n").unwrap();
    // With no lock to compare with, the raw tree is read against its checksum at extraction.
    std::fs::remove_file(app.join(crate::lockfile::LOCK_FILE)).unwrap();
    let err = e.install(&app).unwrap_err();
    assert!(err.contains("was changed after it was extracted"), "{err}");
    // And `velt add` in another project on that cache.
    let err = super::super::super::entry_for(&e.loc, &url, Some("packages/vue")).unwrap_err();
    assert!(err.contains("was changed after it was extracted"), "{err}");
}

#[test]
fn plain_http_only_from_a_manifest_the_root_controls() {
    let e = env();
    let s = Serve::new();
    let inner = vue();
    s.put("/vue.tar.gz", Body::Bytes(inner.clone()));
    let wrapper = tar_gz(&[(
        "w/package.velt",
        &manifest(
            "w",
            "1.0.0",
            &format!(
                ", dependencies: {{ vue: {{ url: \"{}\", sha256: \"{}\", strip: 1, path: \"packages/vue\" }} }}",
                s.url("/vue.tar.gz"),
                hex(&inner)
            ),
        ),
    )]);
    s.put("/w.tgz", Body::Bytes(wrapper.clone()));
    let w = format!(
        "w: {{ url: \"{}\", sha256: \"{}\", path: \"w\" }}",
        s.url("/w.tgz"),
        hex(&wrapper)
    );
    let err = e.install(&e.app(&w, "")).unwrap_err();
    assert!(
        err.contains("dependency `vue` of `w`: plain http")
            && err.contains("only in a manifest this package controls"),
        "{err}"
    );
}

#[test]
fn redirects_stay_on_https_and_relative_locations_resolve() {
    let from = "https://h.example/a/b/x.tgz";
    let local = "http://127.0.0.1:9/x.tgz";
    let e = fetch::check_hop(from, local).unwrap_err();
    assert!(e.contains("stays on https"), "{e}");
    fetch::check_hop("http://127.0.0.1:9/a", local).unwrap();
    fetch::check_hop(from, "https://cdn.example/x.tgz").unwrap();
    for (location, resolved) in [
        ("https://cdn.example/x", "https://cdn.example/x"),
        ("//cdn.example/x", "https://cdn.example/x"),
        ("/y.tgz", "https://h.example/y.tgz"),
        ("y.tgz", "https://h.example/a/b/y.tgz"),
    ] {
        assert_eq!(
            fetch::resolve(from, location).as_deref(),
            Some(resolved),
            "{location}"
        );
    }
    assert_eq!(fetch::resolve(from, "?q"), None);
    // A host-relative `Location` from a real server.
    let env = env();
    let s = Serve::new();
    let archive = vue();
    s.put("/real/vue.tar.gz", Body::Bytes(archive.clone()));
    s.put("/latest", Body::Redirect("/real/vue.tar.gz".into()));
    let app = env.app(
        &dep(
            &s.url("/latest"),
            &archive,
            ", strip: 1, path: \"packages/vue\"",
        ),
        "",
    );
    env.install(&app).unwrap();
}

#[test]
fn velt_add_never_replaces_an_entry_silently() {
    let spec = |sha: &str| super::super::super::UrlSpec {
        url: "https://h.example/json.tgz".into(),
        sha256: sha.repeat(64),
        strip: 0,
        subdir: None,
    };
    let text = |deps: &str| manifest("app", "0.1.0", &format!(", dependencies: {{ {deps} }}"));
    let check = |deps: &str, found| {
        let m = crate::manifest::Manifest::parse(&text(deps)).unwrap();
        super::super::super::check_replace(&m, "json", &found)
    };
    check("", spec("a")).unwrap();
    let e = check("json: { path: \"../json\" }", spec("a")).unwrap_err();
    assert!(
        e.contains("already has a dependency `json` (path ../json)"),
        "{e}"
    );
    let pinned = format!(
        "json: {{ url: \"https://h.example/json.tgz\", sha256: \"{}\" }}",
        "a".repeat(64)
    );
    check(&pinned, spec("a")).unwrap();
    let e = check(&pinned, spec("b")).unwrap_err();
    assert!(e.contains("remove that entry first"), "{e}");
}

#[test]
fn the_download_limit_is_the_one_given() {
    let s = Serve::new();
    s.put("/big", Body::Bytes(vec![b'x'; 4096]));
    assert!(fetch::fetch_within(&s.url("/big"), 1024).is_err());
    assert_eq!(
        fetch::fetch_within(&s.url("/big"), 8192).unwrap().len(),
        4096
    );
}

#[test]
fn extracting_where_an_equal_tree_already_is_takes_it() {
    let tmp = tempfile::tempdir().unwrap();
    let raw = tmp.path().join("raw");
    let archive = tar_gz(&[("a/x", "1"), ("a/l", "-> x")]);
    let first = super::super::super::unpack(&archive, &raw, "u").unwrap();
    let again = super::super::super::unpack(&archive, &raw, "u").unwrap();
    assert_eq!(
        (first.clone(), again),
        (vec!["a/l".to_string()], vec!["a/l".to_string()])
    );
}

#[test]
fn one_archive_with_another_strip_or_path_is_another_source() {
    let e = env();
    let s = Serve::new();
    let both = tar_gz(&[
        ("r/a/package.velt", &manifest("vue", "1.0.0", "")),
        ("r/b/package.velt", &manifest("vue", "1.0.0", "")),
    ]);
    s.put("/both.tgz", Body::Bytes(both.clone()));
    let local = |name: &str, path: &str| {
        let dir = e.root.join(name);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let deps = format!(
            ", dependencies: {{ vue: {{ url: \"{}\", sha256: \"{}\", strip: 1, path: \"{path}\" }} }}",
            s.url("/both.tgz"),
            hex(&both)
        );
        std::fs::write(dir.join("package.velt"), manifest(name, "1.0.0", &deps)).unwrap();
    };
    local("x", "a");
    local("y", "b");
    let app = e.app("x: { path: \"../x\" }, y: { path: \"../y\" }", "");
    let err = e.install(&app).unwrap_err();
    assert!(err.contains("`vue` is asked for from two sources"), "{err}");
}

#[test]
fn strip_does_not_go_through_a_link() {
    let e = env();
    let s = Serve::new();
    let linked = tar_gz(&[("top", "-> .")]);
    s.put("/l.tgz", Body::Bytes(linked.clone()));
    let app = e.app(&dep(&s.url("/l.tgz"), &linked, ", strip: 1"), "");
    let err = e.install(&app).unwrap_err();
    assert!(
        err.contains("`strip: 1` needs one directory at level 1, but it has 1"),
        "{err}"
    );
}

/// A cached tree whose package directory became a link out of the archive, with a lock.
#[cfg(unix)]
#[test]
fn a_cached_directory_that_became_a_link_out_is_refused() {
    let e = env();
    let s = Serve::new();
    let archive = vue();
    s.put("/vue.tar.gz", Body::Bytes(archive.clone()));
    let app = e.app(
        &dep(
            &s.url("/vue.tar.gz"),
            &archive,
            ", strip: 1, path: \"packages/vue\"",
        ),
        "",
    );
    e.install(&app).unwrap();
    let pkg = raw(&e, &archive).join("vue-3.6.0/packages/vue");
    let outside = e.root.join("outside-vue");
    std::fs::rename(&pkg, &outside).unwrap();
    std::os::unix::fs::symlink(&outside, &pkg).unwrap();
    let err = e.install(&app).unwrap_err();
    assert!(
        err.contains("`path` leads out of the archive (through a link)"),
        "{err}"
    );
}

/// A sibling package's directory that became a link out of the archive.
#[cfg(unix)]
#[test]
fn a_sibling_through_a_link_out_of_the_archive_is_refused() {
    let e = env();
    let s = Serve::new();
    let mono = tar_gz(&[
        (
            "repo/vue/package.velt",
            &manifest(
                "vue",
                "1.0.0",
                ", dependencies: { shared: { path: \"../shared\" } }",
            ),
        ),
        ("repo/shared/package.velt", &manifest("shared", "1.0.0", "")),
    ]);
    s.put("/mono.tgz", Body::Bytes(mono.clone()));
    let app = e.app(
        &dep(&s.url("/mono.tgz"), &mono, ", strip: 1, path: \"vue\""),
        "",
    );
    e.install(&app).unwrap();
    let shared = raw(&e, &mono).join("repo/shared");
    let outside = e.root.join("outside-shared");
    std::fs::rename(&shared, &outside).unwrap();
    std::os::unix::fs::symlink(&outside, &shared).unwrap();
    let err = e.install(&app).unwrap_err();
    assert!(
        err.contains("its dependency `shared` has a `path` that leads out of the archive"),
        "{err}"
    );
}

#[test]
fn velt_add_checks_its_path_and_a_candidate_its_name() {
    let e = env();
    let s = Serve::new();
    let archive = vue();
    s.put("/vue.tar.gz", Body::Bytes(archive.clone()));
    for bad in ["../x", "/x", "a/./b", "."] {
        let err =
            super::super::super::entry_for(&e.loc, &s.url("/vue.tar.gz"), Some(bad)).unwrap_err();
        assert!(err.contains("`--path` with a URL"), "{bad}: {err}");
    }
    // An archive whose package is named otherwise than the dependency.
    let other = tar_gz(&[("p/package.velt", &manifest("other", "1.0.0", ""))]);
    s.put("/other.tgz", Body::Bytes(other.clone()));
    let app = e.app(&dep(&s.url("/other.tgz"), &other, ", path: \"p\""), "");
    let err = e.install(&app).unwrap_err();
    assert!(err.contains("is `other`"), "{err}");
}

#[test]
fn an_archive_announced_past_512_mib_is_refused_before_it_is_read() {
    use std::io::{Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/big.tgz", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (mut c, _) = listener.accept().unwrap();
        let mut buf = [0u8; 1024];
        let _ = c.read(&mut buf);
        let _ = c.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 600000000\r\n\r\n");
    });
    let e = fetch::fetch_bytes(&url).unwrap_err();
    assert!(
        e.contains(&format!("exceeds the limit of {}", 512 << 20)),
        "{e}"
    );
    server.join().unwrap();
}
