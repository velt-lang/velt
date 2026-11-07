//! What the cache may vouch for: a URL only once its bytes hashed to the archive, a raw tree only
//! while it is what was extracted; plain http only from manifests the root controls; and the
//! checks whose breaks no other test saw.

use super::*;
use crate::url_source::{fetch, UrlSpec, Verified};

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
    // A scheme only at the start makes a `Location` absolute.
    assert_eq!(
        fetch::resolve(from, "/y.tgz?u=https://x.example/z").as_deref(),
        Some("https://h.example/y.tgz?u=https://x.example/z")
    );
    assert_eq!(
        fetch::resolve(from, "y?u=https://x").as_deref(),
        Some("https://h.example/a/b/y?u=https://x")
    );
    // GitHub's release asset redirect: a signed query with escapes that a manifest's URL may
    // not hold is the server's, and followed; its path and host are still checked.
    let asset = "https://release-assets.githubusercontent.com/github-production-release-asset/50805212/1f0fe4d4-8d6b-4e3b-a1a6-0b4f5f2f0f0f?sp=r&sv=2018-11-09&sr=b&spr=https&se=2026-11-06T20%3A47%3A07Z&rscd=attachment%3B+filename%3Dripgrep-14.1.1-x86_64-unknown-linux-musl.tar.gz.sha256&rsct=application%2Foctet-stream&skoid=96c2d410-5711-43a1-aedd-ab1947aa7ab0&sktid=398a6654-997b-47e9-b12b-9515b896b4de&skt=2026-11-06T19%3A46%3A26Z&ske=2026-11-06T20%3A47%3A07Z&sks=b&skv=2018-11-09&sig=Wz6%2FXq%2BmR8e1Zt0vU3k%3D&jwt=eyJhbGciOiJIUzI1NiJ9.e30.x&response-content-disposition=attachment%3B%20filename%3Dripgrep.tar.gz&response-content-type=application%2Foctet-stream";
    fetch::check_hop(
        "https://github.com/BurntSushi/ripgrep/releases/download/14.1.1/ripgrep.tar.gz",
        asset,
    )
    .unwrap();
    for path in ["/a%2Fb/x", "/a%3Bb/x", "/../x"] {
        let e = fetch::check_hop(from, &format!("https://cdn.example{path}?q=1"));
        assert!(e.is_err(), "{path}");
    }
    // A host-relative `Location` from a real server.
    let env = env();
    let s = Serve::new();
    let archive = vue();
    s.put("/real/vue.tar.gz", Body::Bytes(archive.clone()));
    s.put(
        "/latest",
        Body::Redirect("/real/vue.tar.gz?x=a%3Bb%2Fc#part".into()),
    );
    let app = env.app(
        &dep(
            &s.url("/latest#first"),
            &archive,
            ", strip: 1, path: \"packages/vue\"",
        ),
        "",
    );
    env.install(&app).unwrap();
    // No fragment went on the wire, from the manifest's URL or the hop's.
    let log = s.log.lock().unwrap().clone();
    assert!(log.iter().all(|p| !p.contains('#')), "{log:?}");
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
fn a_failed_extraction_is_reported_even_beside_a_tree_with_a_record() {
    // Extracting where a tree already is fails (the lock makes it no concurrent install's), and
    // the tree that's there isn't taken for the one asked for.
    let tmp = tempfile::tempdir().unwrap();
    let raw = tmp.path().join("raw");
    let archive = tar_gz(&[("a/x", "1"), ("a/l", "-> x")]);
    super::super::super::unpack(&archive, &raw, "u", false).unwrap();
    let e = super::super::super::unpack(&archive, &raw, "u", false).unwrap_err();
    assert!(e.contains("can't be used"), "{e}");
    assert_eq!(Verified::read(&raw).unwrap().links, vec!["a/l".to_string()]);
}

#[test]
fn a_raw_tree_is_never_seen_without_its_record() {
    // A second install that found the raw tree before its record would take it for one no URL
    // vouches for. A thread watches while the tree is extracted and hashed.
    let tmp = tempfile::tempdir().unwrap();
    let raw = tmp.path().join("raw");
    let files: Vec<(String, String)> = (0..2000)
        .map(|i| {
            (
                format!("a/src/f{i}.velt"),
                format!("export const v{i} = {i};\n"),
            )
        })
        .collect();
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    let archive = tar_gz(&refs);
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watcher = {
        let (raw, done) = (raw.clone(), done.clone());
        std::thread::spawn(move || {
            let mut seen = 0;
            while !done.load(std::sync::atomic::Ordering::Relaxed) || seen == 0 {
                if raw.is_dir() {
                    assert!(Verified::read(&raw).is_some(), "a raw tree with no record");
                    seen += 1;
                }
            }
        })
    };
    super::super::super::unpack(&archive, &raw, "u", false).unwrap();
    done.store(true, std::sync::atomic::Ordering::Relaxed);
    watcher.join().unwrap();
}

#[test]
fn a_raw_tree_without_a_record_is_replaced_only_once_its_url_serves_it() {
    let e = env();
    let s = Serve::new();
    let archive = vue();
    s.put("/vue.tar.gz", Body::Bytes(archive.clone()));
    let from = |url: &str| {
        e.app(
            &dep(url, &archive, ", strip: 1, path: \"packages/vue\""),
            "",
        )
    };
    e.install(&from(&s.url("/vue.tar.gz"))).unwrap();
    let raw = raw(&e, &archive);
    std::fs::remove_file(Verified::path_for(&raw)).unwrap();
    let kept = raw.join("vue-3.6.0/packages/vue/package.velt");
    // Its URL is gone, or serves other bytes: the error, and the tree still there.
    let elsewhere = from(&s.url("/moved.tar.gz"));
    let err = e.install(&elsewhere).unwrap_err();
    assert!(err.contains("doesn't exist"), "{err}");
    assert!(kept.is_file());
    s.put("/moved.tar.gz", Body::Bytes(tar_gz(&[("x", "y")])));
    let err = e.install(&elsewhere).unwrap_err();
    assert!(err.contains("has SHA-256"), "{err}");
    assert!(kept.is_file());
    // It serves the archive: replaced, and recorded.
    s.put("/moved.tar.gz", Body::Bytes(archive.clone()));
    e.install(&elsewhere).unwrap();
    assert!(kept.is_file());
    assert!(Verified::read(&raw).unwrap().knows(&s.url("/moved.tar.gz")));
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

#[test]
fn a_build_that_is_no_longer_a_no_op_checks_fetched_files() {
    // What the driver runs before compiling: an edit in place, which a no-op build's directory
    // check doesn't see, is caught here.
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
    let installed = e.install(&app).unwrap();
    installed.graph.verify_fetched().unwrap();
    let lib = raw(&e, &archive).join("vue-3.6.0/packages/vue/src/lib.velt");
    std::fs::write(&lib, "export const v = 6;\n").unwrap();
    let err = installed.graph.verify_fetched().unwrap_err();
    assert!(err.contains("changed since it was fetched"), "{err}");
    // What it says to delete is the raw tree, which the archive's record vouches for: deleting
    // only the package directory inside it would leave the project unable to build.
    let raw = raw(&e, &archive);
    assert!(
        err.contains(&format!("delete `{}` to fetch it again", raw.display())),
        "{err}"
    );
    std::fs::remove_dir_all(&raw).unwrap();
    e.install(&app).unwrap().graph.verify_fetched().unwrap();
}

#[test]
fn concurrent_installs_of_one_archive_download_it_once() {
    // The archive's lock: the first downloads and extracts, the others wait and find its tree.
    let e = env();
    let s = Serve::new();
    let archive = vue();
    s.put("/vue.tar.gz", Body::Bytes(archive.clone()));
    let spec = UrlSpec {
        url: s.url("/vue.tar.gz"),
        sha256: hex(&archive),
        strip: 1,
        subdir: Some("packages/vue".into()),
    };
    let check = crate::tree_stamp::TreeCheck::Files;
    let done: Vec<_> = std::thread::scope(|scope| {
        let runs: Vec<_> = (0..8)
            .map(|_| scope.spawn(|| super::super::super::checkout(&e.loc, &spec, (None, check))))
            .collect();
        runs.into_iter().map(|r| r.join().unwrap()).collect()
    });
    assert!(done.iter().all(Result::is_ok), "{done:?}");
    assert_eq!(s.requests(), 1);
    // A second URL of it recorded by several at once: each URL kept.
    let other = (1..=4)
        .map(|i| format!("{}?mirror={i}", spec.url))
        .collect::<Vec<_>>();
    let loc = &e.loc;
    std::thread::scope(|scope| {
        for url in &other {
            let spec = UrlSpec {
                url: url.clone(),
                ..spec.clone()
            };
            scope.spawn(move || super::super::super::checkout(loc, &spec, (None, check)).unwrap());
        }
    });
    let record = Verified::read(&raw(&e, &archive)).unwrap();
    assert!(other.iter().all(|u| record.knows(u)), "{:?}", record.urls);
}

#[test]
fn concurrent_velt_adds_of_one_archive_take_turns() {
    // `entry_for` extracts under the same lock: the first one's tree is the others' too.
    let e = env();
    let s = Serve::new();
    let archive = vue();
    s.put("/vue.tar.gz", Body::Bytes(archive.clone()));
    let url = s.url("/vue.tar.gz");
    let loc = &e.loc;
    let found: Vec<_> = std::thread::scope(|scope| {
        let runs: Vec<_> = (0..8)
            .map(|_| {
                let url = &url;
                scope.spawn(move || crate::url_source::entry_for(loc, url, Some("packages/vue")))
            })
            .collect();
        runs.into_iter().map(|r| r.join().unwrap()).collect()
    });
    assert!(found.iter().all(Result::is_ok), "{found:?}");
}

#[test]
fn a_credential_in_the_query_is_warned_of_and_an_ordinary_query_is_not() {
    let e = env();
    let s = Serve::new();
    let archive = vue();
    s.put("/vue.tar.gz", Body::Bytes(archive.clone()));
    let install = |query: &str| {
        let url = format!("{}?{query}", s.url("/vue.tar.gz"));
        let app = e.app(
            &dep(&url, &archive, ", strip: 1, path: \"packages/vue\""),
            "",
        );
        let warnings = e.install(&app).unwrap().warnings;
        // Once, when the lock first records the URL, not at each command after it.
        let again = e.install(&app).unwrap().warnings;
        assert!(
            !(again.iter()).any(|w| w.contains("credential")),
            "{again:?}"
        );
        warnings
    };
    for query in [
        "token=abc",
        "v=1&X-Amz-Signature=f00",
        "Private_Token=x",
        "sig=y&sv=2020",
        "api-key=x",
        "deploy_token=x",
        "client_secret=x",
        "v=1;jwt=x",
        "%74oken=x",
    ] {
        let warnings = install(query);
        assert!(
            (warnings.iter()).any(|w| w.contains("looks like a credential") && w.contains("`vue`")),
            "{query}: {warnings:?}"
        );
    }
    for query in ["v=1", "download=1&tokenized=yes", "keys=a"] {
        let warnings = install(query);
        assert!(
            !(warnings.iter()).any(|w| w.contains("credential")),
            "{query}: {warnings:?}"
        );
    }
    // A fragment isn't the query.
    assert_eq!(
        crate::url_source::credential_warning("x", "https://h/a.tgz#token=1"),
        None
    );
}
