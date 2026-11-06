//! `allowNative` entries bound to a source (#2367, crate::manifest::allow_native).

use super::*;
use crate::manifest::{AllowNative, AllowedFrom};

#[test]
fn entries_name_a_package_and_where_it_is_allowed_from() {
    let m = read(&with(
        "allowNative: [\"sqlite\", { name: \"llama\", git: \"https://github.com/o/llama\" }, { name: \"duck\", url: \"https://github.com/o/duck/releases/download/\" }]",
    ));
    assert_eq!(
        m.allow_native,
        [
            AllowNative::named("sqlite"),
            AllowNative {
                name: "llama".into(),
                from: AllowedFrom::Git("https://github.com/o/llama".into()),
            },
            AllowNative {
                name: "duck".into(),
                from: AllowedFrom::Url("https://github.com/o/duck/releases/download/".into()),
            },
        ]
    );
    let text = m.to_velt();
    for entry in [
        "\"sqlite\"",
        "{ name: \"llama\", git: \"https://github.com/o/llama\" }",
        "{ name: \"duck\", url: \"https://github.com/o/duck/releases/download/\" }",
    ] {
        assert!(text.contains(entry), "{entry}: {text}");
    }
    assert_eq!(read(&text), m, "{text}");
    let json = m.to_json();
    assert_eq!(
        json["allowNative"],
        serde_json::json!([
            "sqlite",
            { "name": "llama", "git": "https://github.com/o/llama" },
            { "name": "duck", "url": "https://github.com/o/duck/releases/download/" },
        ])
    );
}

#[test]
fn entries_are_checked_where_they_are_written() {
    for (entry, message, at) in [
        (
            "{ git: \"https://github.com/o/llama\" }",
            "an `allowNative` entry needs a `name`",
            "{ git: \"https://github.com/o/llama\" }",
        ),
        (
            "{ name: \"llama\" }",
            "needs exactly one of `git` (a repository) and `url` (a URL prefix)",
            "{ name: \"llama\" }",
        ),
        (
            "{ name: \"x\", git: \"https://h/o/x\", url: \"https://h/o/x/\" }",
            "needs exactly one of `git`",
            "{ name: \"x\", git: \"https://h/o/x\", url: \"https://h/o/x/\" }",
        ),
        (
            "{ name: \"x\", url: \"https://github.com/\" }",
            "names a whole host",
            "\"https://github.com/\"",
        ),
        (
            "{ name: \"x\", url: \"http://github.com/o/x/\" }",
            "is not an https URL",
            "\"http://github.com/o/x/\"",
        ),
        (
            "{ name: \"x\", url: \"https://github.com/o/x/releases/download/../../../evil/\" }",
            "`..` path segment",
            "\"https://github.com/o/x/releases/download/../../../evil/\"",
        ),
        (
            "{ name: \"x\", git: \"https://github.com/o/%2e%2e/x\" }",
            "contains `%2e`",
            "\"https://github.com/o/%2e%2e/x\"",
        ),
        (
            "{ name: \"x\", git: \"https://u:token@github.com/o/x\" }",
            "credentials",
            "\"https://u:token@github.com/o/x\"",
        ),
        (
            "{ name: \"x\", gti: \"https://github.com/o/x\" }",
            "unknown key `gti`",
            "gti",
        ),
        ("1", "`allowNative` entries must be package names", "1"),
    ] {
        let src = with(&format!("allowNative: [{entry}]"));
        let all = errors(&src);
        assert!(
            all.iter()
                .any(|(m, span)| m.contains(message) && span == at),
            "{entry}: {all:?}"
        );
    }
}
