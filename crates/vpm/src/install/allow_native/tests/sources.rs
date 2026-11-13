//! Which source each kind of entry allows, the `unchecked` note on a refusal by source, and the
//! notes an install prints (through real installs, not only the functions that write them).

use super::*;
use crate::install::{install, InstallOptions};
use crate::locations::Locations;

fn git_source(url: &str) -> Source {
    Source::Git {
        spec: GitSpec {
            url: url.to_string(),
            reference: GitRef::Rev("abc".to_string()),
            subdir: None,
        },
        commit: "abc".to_string(),
        checksum: String::new(),
        dir: PathBuf::new(),
        npm: BTreeMap::new(),
    }
}

fn url_source(url: &str) -> Source {
    Source::Url {
        spec: crate::url_source::UrlSpec {
            url: url.to_string(),
            sha256: "a".repeat(64),
            strip: 0,
            subdir: None,
        },
        checksum: String::new(),
        dir: PathBuf::new(),
    }
}

/// An archive is suggested by its URL's directory, the narrowest prefix that allows it.
#[test]
fn an_archive_is_suggested_by_its_directory() {
    let lib = dir_with(&manifest_text("lib", "native: { c: {} }"));
    let url = "https://github.com/o/lib/releases/download/v1/lib-1.tar.gz";
    let e = run_from(url_source(url), "", Some(lib.path())).unwrap_err();
    assert!(
        e.contains("allowNative: [{ name: \"lib\", url: \"https://github.com/o/lib/releases/download/v1/\" }]"),
        "{e}"
    );
    // That entry allows it.
    let fields =
        "allowNative: [{ name: \"lib\", url: \"https://github.com/o/lib/releases/download/\" }]";
    run_from(url_source(url), fields, Some(lib.path())).unwrap();
}

/// Every pair of entry kind and source: a bare name the registry and paths, a `git` entry its
/// repository only, a `url` entry archives under its prefix only.
#[test]
fn each_entry_allows_only_its_own_source() {
    let registry = Source::Registry {
        checksum: String::new(),
        native: BTreeMap::new(),
        native_abi: None,
        listed: BTreeMap::new(),
    };
    let path = Source::Path {
        dir: PathBuf::from("/x"),
    };
    let git = git_source("https://github.com/o/llama");
    let other_git = git_source("https://github.com/o/other");
    let named = AllowNative::named("lib");
    let from = |from: AllowedFrom| AllowNative {
        name: "lib".into(),
        from,
    };
    let by_git = from(AllowedFrom::Git("https://github.com/o/llama.git".into()));
    let by_url = from(AllowedFrom::Url("https://github.com/o/".into()));
    let archive = url_source("https://github.com/o/llama/releases/download/v1/x.tar.gz");
    let evil_archive = url_source("https://github.com/o-evil/llama/releases/download/v1/x.tar.gz");
    let cases = [
        (&named, &archive, false),
        (&by_git, &archive, false),
        (&by_url, &archive, true),
        (&by_url, &evil_archive, false),
        (&named, &registry, true),
        (&named, &path, true),
        (&named, &git, false),
        (&by_git, &git, true),
        (&by_git, &other_git, false),
        (&by_git, &registry, false),
        (&by_git, &path, false),
        (&by_url, &registry, false),
        (&by_url, &path, false),
        (&by_url, &git, false),
    ];
    for (entry, source, expected) in cases {
        assert_eq!(
            allows(entry, source).unwrap(),
            expected,
            "{} for {}",
            entry.to_source(),
            describe_source(source)
        );
    }
}

/// A package listed for another source still says its C library has no headers, as an unlisted
/// one does: the decision to add the entry needs to know it.
#[test]
fn a_refusal_by_source_names_unchecked_libraries() {
    let lib = dir_with(&manifest_text(
        "lib",
        "native: { c: { sdk: { system: { default: \"sdk\" }, unchecked: true } } }",
    ));
    let e = run_from(
        git_source("https://github.com/o/lib"),
        "allowNative: [\"lib\"]",
        Some(lib.path()),
    )
    .unwrap_err();
    assert!(
        e.contains("comes from the git repository https://github.com/o/lib")
            && e.contains(
                "; its C library `sdk` is not checked against headers (`unchecked: true`)"
            ),
        "{e}"
    );
}

/// A package directory with native code at `version`.
fn write_native(dir: &Path, name: &str, version: &str) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join(MANIFEST_FILE),
        format!("export const pkg: Package = {{ name: \"{name}\", version: \"{version}\", native: {{ c: {{}} }} }};"),
    )
    .unwrap();
    std::fs::write(dir.join("src/lib.velt"), "\n").unwrap();
}

#[test]
fn an_install_names_native_code_that_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = crate::relpath::absolute(tmp.path());
    let loc = Locations::under(&root.join("home"));
    write_native(&root.join("lib"), "lib", "0.1.0");
    let app = root.join("app");
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::write(
        app.join(MANIFEST_FILE),
        "export const pkg: Package = { name: \"app\", version: \"0.1.0\", allowNative: [\"lib\"], dependencies: { lib: { path: \"../lib\" } } };",
    )
    .unwrap();
    let first = install(&app, &loc, InstallOptions::default()).unwrap();
    assert!(
        !first.notes.iter().any(|n| n.contains("native code of")),
        "{:?}",
        first.notes
    );
    write_native(&root.join("lib"), "lib", "0.2.0");
    let second = install(&app, &loc, InstallOptions::default()).unwrap();
    assert!(
        second
            .notes
            .iter()
            .any(|n| n.starts_with("native code of `lib` changes: 0.1.0 (")
                && n.contains("→ 0.2.0 (")),
        "{:?}",
        second.notes
    );
}

/// A repository whose URL no entry can name is refused without suggesting one: the entry would
/// be refused when added.
#[test]
fn a_repository_no_entry_can_name_gets_no_suggestion() {
    let lib = dir_with(&manifest_text("lib", "native: { c: {} }"));
    for fields in ["", "allowNative: [\"lib\"]"] {
        let e = run_from(
            git_source("https://github.com/o/lib\u{200b}"),
            fields,
            Some(lib.path()),
        )
        .unwrap_err();
        assert!(
            e.contains("comes from a git repository that allowNative can't name")
                && e.contains("https://github.com/o/lib\\u{200b}")
                && !e.contains("allowNative: ["),
            "{fields}: {e}"
        );
    }
}
