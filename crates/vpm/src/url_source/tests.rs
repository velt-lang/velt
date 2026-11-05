//! The url source's lock form and fields; installs over a local HTTP server are in `install`.

mod install;

use super::*;

fn spec(strip: u32, subdir: Option<&str>) -> UrlSpec {
    UrlSpec {
        url: "https://github.com/o/vue/releases/download/v3.6.0/vue-3.6.0.tar.gz".into(),
        sha256: "ab".repeat(32),
        strip,
        subdir: subdir.map(String::from),
    }
}

#[test]
fn sources_round_trip_through_the_lockfile() {
    let plain = spec(0, None);
    assert_eq!(
        plain.source(),
        format!("url+{}#sha256:{}", plain.url, plain.sha256)
    );
    let full = spec(1, Some("packages/vue"));
    assert_eq!(
        full.source(),
        format!(
            "url+{}?strip=1&path=packages/vue#sha256:{}",
            full.url, full.sha256
        )
    );
    // Delimiters in the URL (a query, a fragment) are escaped, and come back.
    let odd = UrlSpec {
        url: "https://h.example/get?file=a.tar.gz&v=1".into(),
        ..spec(0, Some("a#b"))
    };
    for s in [plain, full, odd] {
        assert_eq!(
            UrlSpec::parse_source(&s.source()),
            Some(s.clone()),
            "{}",
            s.source()
        );
    }
    for bad in [
        "url+https://h/x.tar.gz",
        "url+https://h/x.tar.gz#sha256:short",
        "url+https://h/x.tar.gz#abab",
        "url+https://h/x.tar.gz?strip=x#sha256:"
            .to_string()
            .as_str(),
        "url+https://h/x.tar.gz?other=1#sha256:",
        "git+https://h/x#0123",
    ] {
        let bad = match bad.ends_with("sha256:") {
            true => format!("{bad}{}", "a".repeat(64)),
            false => bad.to_string(),
        };
        assert_eq!(UrlSpec::parse_source(&bad), None, "{bad}");
    }
}

fn detailed(fields: &[(&str, &str)]) -> DetailedDependency {
    let mut d = DetailedDependency::default();
    for (k, v) in fields {
        let v = Some(v.to_string());
        match *k {
            "url" => d.url = v,
            "sha256" => d.sha256 = v,
            "path" => d.path = v,
            "git" => d.git = v,
            "rev" => d.rev = v,
            "strip" => d.strip = v.and_then(|s| s.parse().ok()),
            _ => unreachable!(),
        }
    }
    d
}

#[test]
fn fields_are_checked() {
    let url = "https://github.com/o/p/releases/download/v1/p.tar.gz";
    let hash = "0".repeat(64);
    check_fields("p", &detailed(&[("url", url), ("sha256", &hash)])).unwrap();
    check_fields(
        "p",
        &detailed(&[
            ("url", url),
            ("sha256", &hash),
            ("strip", "1"),
            ("path", "a/b"),
        ]),
    )
    .unwrap();
    // Plain http only to this machine.
    check_fields(
        "p",
        &detailed(&[("url", "http://127.0.0.1:8080/p.tar.gz"), ("sha256", &hash)]),
    )
    .unwrap();
    for (fields, why) in [
        (vec![("url", url)], "needs its `sha256`"),
        (
            vec![("url", url), ("sha256", "ABAB")],
            "64 lowercase hex digits",
        ),
        (
            vec![("url", url), ("sha256", &"A".repeat(64))],
            "64 lowercase hex digits",
        ),
        (vec![("sha256", &hash)], "`sha256` needs a `url`"),
        (vec![("strip", "1")], "`strip` needs a `url`"),
        (
            vec![("url", url), ("sha256", &hash), ("git", "https://h/r")],
            "keep one",
        ),
        (
            vec![("url", url), ("sha256", &hash), ("rev", &"a".repeat(40))],
            "are for `git`",
        ),
        (
            vec![("url", "http://example.com/p.tar.gz"), ("sha256", &hash)],
            "plain http://",
        ),
        (
            vec![("url", "ftp://example.com/p.tar.gz"), ("sha256", &hash)],
            "a ftp:// URL",
        ),
        (
            vec![
                ("url", "https://u:p@example.com/p.tar.gz"),
                ("sha256", &hash),
            ],
            "credentials",
        ),
        (
            vec![
                ("url", "https://example.com/a/%2e%2e/p.tar.gz"),
                ("sha256", &hash),
            ],
            "%2e",
        ),
        (
            vec![("url", url), ("sha256", &hash), ("path", "../x")],
            "inside the archive",
        ),
        (
            vec![("url", url), ("sha256", &hash), ("path", "/x")],
            "inside the archive",
        ),
        (
            vec![("url", url), ("sha256", &hash), ("path", "a\\b")],
            "inside the archive",
        ),
    ] {
        let e = check_fields("p", &detailed(&fields)).unwrap_err();
        assert!(e.contains(why), "{fields:?}: expected {why:?} in {e}");
    }
}

#[test]
fn a_manifest_reads_the_url_fields() {
    let text = |dep: &str| {
        format!("export const pkg: Package = {{ name: \"app\", version: \"0.1.0\", dependencies: {{ vue: {dep} }} }};")
    };
    let hash = "0".repeat(64);
    let ok = text(&format!(
        "{{ url: \"https://h.example/o/v.tar.gz\", sha256: \"{hash}\", strip: 1, path: \"packages/vue\" }}"
    ));
    let m = crate::manifest::Manifest::parse(&ok).unwrap();
    let got = m.dependencies["vue"].url_spec().unwrap();
    assert_eq!(
        (got.strip, got.subdir.as_deref()),
        (1, Some("packages/vue"))
    );
    // Written back the same way.
    let lit = crate::manifest::write::dependency_lit(&m.dependencies["vue"]);
    assert!(
        lit.contains("strip: 1") && lit.contains(&format!("sha256: \"{hash}\"")),
        "{lit}"
    );
    for (strip, why) in [
        ("\"1\"", "whole number"),
        ("17", "from 0 to 16"),
        ("1.5", "whole number"),
    ] {
        let bad = text(&format!(
            "{{ url: \"https://h.example/v.tar.gz\", sha256: \"{hash}\", strip: {strip} }}"
        ));
        let e = crate::manifest::Manifest::parse(&bad).unwrap_err();
        assert!(e.contains(why), "{strip}: {e}");
    }
}
