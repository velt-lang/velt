use super::*;

/// Spellings of one repository have one identity; different repositories, schemes or paths
/// don't.
#[test]
fn git_urls_compare_in_one_normal_form() {
    let same = [
        ("https://github.com/o/llama", "https://github.com/o/llama"),
        ("https://github.com/o/llama", "https://GitHub.com/o/llama"),
        (
            "https://github.com/o/llama",
            "https://github.com/o/llama.git",
        ),
        ("https://github.com/o/llama", "https://github.com/o/llama/"),
        (
            "https://github.com/o/llama",
            "https://github.com/o/llama.git/",
        ),
        (
            "https://github.com/o/llama",
            "https://github.com:443/o/llama",
        ),
        ("ssh://git@github.com/o/llama", "git@github.com:o/llama.git"),
        (
            "ssh://git@github.com/o/llama",
            "ssh://git@github.com:22/o/llama",
        ),
        ("ssh://git@github.com/o/llama", "git@GITHUB.com:o/llama/"),
        ("file:///srv/repos/llama", "file:///srv/repos/llama.git"),
    ];
    for (a, b) in same {
        assert_eq!(
            git_identity(a).unwrap(),
            git_identity(b).unwrap(),
            "{a} vs {b}"
        );
    }
    let different = [
        (
            "https://github.com/o/llama",
            "https://github.com/o/llama-evil",
        ),
        ("https://github.com/o/llama", "https://github.com/O/llama"),
        ("https://github.com/o/llama", "https://github.com/o/llama/x"),
        (
            "https://github.com/o/llama",
            "https://github.com:8443/o/llama",
        ),
        ("https://github.com/o/llama", "ssh://git@github.com/o/llama"),
        ("https://github.com/o/llama", "https://gitlab.com/o/llama"),
        (
            "https://github.com/o/llama",
            "https://github.com.evil/o/llama",
        ),
        (
            "ssh://git@github.com/o/llama",
            "ssh://evil@github.com/o/llama",
        ),
    ];
    for (a, b) in different {
        assert_ne!(
            git_identity(a).unwrap(),
            git_identity(b).unwrap(),
            "{a} vs {b}"
        );
    }
}

#[test]
fn git_urls_that_cant_be_compared_safely_are_refused() {
    for url in [
        "https://github.com/o/../evil/llama",
        "https://github.com/o/./llama",
        "https://github.com/o/%2e%2e/evil",
        "https://github.com/o%2Fllama",
        "https://github.com/o/llama%5c",
        "https://github.com\\o\\llama",
        "https://user:token@github.com/o/llama",
        "https://user@github.com/o/llama",
        "git@github.com:o/../evil",
        "-oProxyCommand=evil",
        "github.com/o/llama",
    ] {
        assert!(git_identity(url).is_err(), "{url}");
    }
}

#[test]
fn prefixes_match_on_a_segment_boundary() {
    let prefix = "https://github.com/o/duck/releases/download/";
    let cases = [
        (
            "https://github.com/o/duck/releases/download/v1/duck.tar.gz",
            true,
        ),
        (
            "https://GITHUB.com/o/duck/releases/download/v1/duck.tar.gz",
            true,
        ),
        (
            "https://github.com:443/o/duck/releases/download/v1/x.zip",
            true,
        ),
        (
            "https://github.com/o/duck/releases/download/v1/x.zip?raw=1",
            true,
        ),
        (
            "https://github.com/o/duck/releases/download-evil/x.zip",
            false,
        ),
        ("https://github.com/o/duck/releases/x.zip", false),
        (
            "https://github.com/o/duck-evil/releases/download/v1/x.zip",
            false,
        ),
        (
            "https://github.com.evil/o/duck/releases/download/v1/x.zip",
            false,
        ),
        (
            "https://github.com:8443/o/duck/releases/download/v1/x.zip",
            false,
        ),
    ];
    for (url, matches) in cases {
        assert_eq!(prefix_matches(prefix, url).unwrap(), matches, "{url}");
    }
    // Without a trailing `/`, the last segment still has to end.
    let bare = "https://github.com/o/duck";
    assert!(prefix_matches(bare, "https://github.com/o/duck").unwrap());
    assert!(prefix_matches(bare, "https://github.com/o/duck/x.zip").unwrap());
    assert!(!prefix_matches(bare, "https://github.com/o/duck-evil/x.zip").unwrap());
}

#[test]
fn urls_that_escape_a_prefix_are_refused() {
    let prefix = "https://github.com/o/duck/releases/download/";
    for url in [
        "https://github.com/o/duck/releases/download/../../../evil/x.zip",
        "https://github.com/o/duck/releases/download/%2e%2e/%2e%2e/evil/x.zip",
        "https://github.com/o/duck/releases/download/..%2f..%2fevil/x.zip",
        "https://github.com/o/duck/releases/download/v1\\..\\..\\x.zip",
        "https://github.com/o/duck/releases/download/./x.zip",
        "https://token@github.com/o/duck/releases/download/v1/x.zip",
        "https://u:p@github.com/o/duck/releases/download/v1/x.zip",
        "http://github.com/o/duck/releases/download/v1/x.zip",
    ] {
        assert!(prefix_matches(prefix, url).is_err(), "{url}");
    }
}

#[test]
fn prefixes_must_name_a_path_under_an_https_host() {
    for ok in [
        "https://github.com/o/",
        "https://github.com/o/duck/releases/download/",
        "https://sqlite.org/2026",
    ] {
        check_prefix(ok).unwrap();
    }
    for bad in [
        "https://github.com/",
        "https://github.com",
        "https:///o/duck",
        "http://github.com/o/duck/",
        "https://github.com/o/duck/?x=1",
        "https://github.com/o/duck/#x",
        "https://u:p@github.com/o/duck/",
        "https://github.com/o/../duck/",
        "https://github.com/o/%2E%2E/",
        "ftp://github.com/o/duck/",
    ] {
        assert!(check_prefix(bad).is_err(), "{bad}");
    }
}
