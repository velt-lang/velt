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
        // A home-relative scp path is `/~/` in an ssh URL; an absolute one is the path.
        (
            "ssh://git@github.com/~/o/llama",
            "git@github.com:o/llama.git",
        ),
        ("ssh://git@h.example/srv/x", "git@h.example:/srv/x"),
        (
            "ssh://git@github.com/o/llama",
            "ssh://git@github.com:22/o/llama",
        ),
        ("ssh://git@github.com/~/o/llama", "git@GITHUB.com:o/llama/"),
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
        // Relative to the home, or from the root: two repositories.
        ("git@h.example:/srv/x", "git@h.example:srv/x"),
        ("ssh://git@h.example/srv/x", "git@h.example:srv/x"),
        // `.git` is removed from the last name, not as a whole segment.
        ("https://github.com/a/b", "https://github.com/a/b/.git"),
        ("file:///srv/x", "file:///srv/x/.git"),
    ];
    for (a, b) in different {
        assert_ne!(
            git_identity(a).unwrap(),
            git_identity(b).unwrap(),
            "{a} vs {b}"
        );
    }
}

/// The identity's text: what a lock or a message shows.
#[test]
fn identities_are_spelled_one_way() {
    for (url, identity) in [
        (
            "https://GitHub.com:443/o/llama.git/",
            "https://github.com/o/llama",
        ),
        (
            "git@github.com:o/llama.git",
            "ssh://git@github.com/~/o/llama",
        ),
        ("git@h.example:/srv/x", "ssh://git@h.example/srv/x"),
        // A segment that is only `.git` is a path of its own, kept.
        ("https://github.com/a/b/.git", "https://github.com/a/b/.git"),
        ("file:///srv/x/.git/", "file:///srv/x/.git"),
    ] {
        assert_eq!(git_identity(url).unwrap(), identity, "{url}");
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
        // Double encoding, encoded NUL, overlong UTF-8, `..;`.
        "https://github.com/a/%252e%252e/a/b",
        "https://github.com/a/b%00",
        "https://github.com/a/%c0%ae%c0%ae/b",
        "https://github.com/a/..;/b",
        "https://github.com/a/%zz",
        // Empty segments, which a normal form would erase.
        "https://github.com//a/b",
        "https://github.com/a//b",
        "https://github.com/a/b//",
        "https://github.com/a/b///.git",
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
        "https://github.com/o/duck/releases/download/%252e%252e/%252e%252e/evil/x",
        "https://github.com/o/duck/releases/download/%252F..%252Fevil",
        "https://github.com/o/duck/releases/download/..;/..;/evil/x",
        "https://github.com/o/duck/releases/download/%00/x",
        "https://github.com/o/duck/releases/download/%c0%ae%c0%ae/x",
        "https://github.com/o/duck/releases/download//x",
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
        "https://github.com/%252e%252e/",
        "https://github.com/a/b%00/",
        "https://github.com//x",
        "https://github.com/a//b/",
        "ftp://github.com/o/duck/",
    ] {
        assert!(check_prefix(bad).is_err(), "{bad}");
    }
}

/// A prefix [`check_prefix`] refuses is an error in [`prefix_matches`] too, not a match.
#[test]
fn matching_checks_the_prefix() {
    let url = "https://github.com/o/duck/x.zip";
    for bad in [
        "https://github.com/",
        "https://github.com//o/",
        "http://github.com/o/",
    ] {
        assert!(prefix_matches(bad, url).is_err(), "{bad}");
    }
}

/// Forms refused in a path segment, by all three functions: raw non-ASCII (invisible and
/// look-alike characters), escapes of bytes that can change where a path goes, malformed
/// escapes, and segments judged by their part before `;`.
#[test]
fn unsafe_path_forms_are_refused_everywhere() {
    for form in [
        "\u{200b}b",
        "\u{feff}b",
        "\u{ff0e}\u{ff0e}",
        "\u{2024}",
        "\u{2025}",
        "%ef%bc%8e",
        "%e2%80%a4",
        "%C3%A9",
        "%0a",
        "%0d%0a",
        "%09",
        "%7f",
        "%3b",
        "%3f",
        "%23",
        "%+1",
        "%4",
        ".;",
        ";x",
        "..;x",
    ] {
        let git = format!("https://github.com/a/{form}/b");
        assert!(git_identity(&git).is_err(), "git_identity {git:?}");
        assert!(
            prefix_matches("https://github.com/a/", &git).is_err(),
            "prefix_matches {git:?}"
        );
        let prefix = format!("https://github.com/a/{form}/");
        assert!(check_prefix(&prefix).is_err(), "check_prefix {prefix:?}");
    }
    // A malformed escape says so, even where it would decode to something refused anyway.
    for malformed in ["%+1", "%-1", "%4", "%zz"] {
        let e = git_identity(&format!("https://github.com/a/{malformed}/b")).unwrap_err();
        assert!(e.contains("malformed `%` escape"), "{malformed}: {e}");
    }
    // The refusal shows the URL readably, with the invisible character spelled out.
    let e = git_identity("https://github.com/a/b\u{200b}").unwrap_err();
    assert!(e.contains("https://github.com/a/b\\u{200b}"), "{e}");
    // A space stays: real repository names have one, and it can't change where a path goes.
    let azure = "https://dev.azure.com/org/My%20Project/_git/repo";
    git_identity(azure).unwrap();
    assert!(prefix_matches("https://dev.azure.com/org/My%20Project/", azure).unwrap());
}

/// Hosts with a `%` or a `:` outside `[…]` are refused: `:8443:443` compared as `:8443`.
#[test]
fn hosts_must_be_plain() {
    for host in [
        "github.com:8443:443",
        "github%41.com",
        "[::1",
        "::1]",
        "github.com:443x",
        "github\u{3002}com",
    ] {
        let git = format!("https://{host}/a/b");
        assert!(git_identity(&git).is_err(), "git_identity {git:?}");
        assert!(
            prefix_matches("https://github.com:8443/a/", &git).is_err(),
            "prefix_matches {git:?}"
        );
        let prefix = format!("https://{host}/a/");
        assert!(check_prefix(&prefix).is_err(), "check_prefix {prefix:?}");
    }
    assert!(git_identity("ssh://git@github.com:8443:22/a/b").is_err());
    assert!(git_identity("git@github%41.com:a/b").is_err());
    assert!(git_identity("git@[::1:a/b").is_err());
    assert_ne!(
        git_identity("https://github.com:8443/a/b").unwrap(),
        git_identity("https://github.com/a/b").unwrap()
    );
    git_identity("https://[::1]:8443/a/b").unwrap();
}
