//! The real docs website builds from the repository (`docs/site/pages.txt`, `std/`), and every
//! page listed exists.

use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

#[test]
fn repository_site_builds() {
    let root = root();
    let out = tempfile::tempdir().expect("temp dir");
    let pages = root.join("docs/site/pages.txt");
    let n = velt_doc::site::build_site(&pages, &root.join("std"), out.path()).expect("site");
    assert!(n > 10, "{n} pages");
    for page in [
        "index.html",
        "book/tour.html",
        "reference/types.html",
        "std/fs.html",
        "tooling/webassembly.html",
        "std/index.html",
        "std/std.fs.html",
    ] {
        assert!(out.path().join(page).is_file(), "{page}");
    }
    let index = std::fs::read_to_string(out.path().join("index.html")).expect("index");
    assert!(index.contains("href=\"book/tour.html\""), "{index}");
    let tour = std::fs::read_to_string(out.path().join("book/tour.html")).expect("tour");
    assert!(tour.contains("href=\"../style.css\""), "{tour}");
    // std's `/** */` doc comments reach the API pages.
    let fs = std::fs::read_to_string(out.path().join("std/std.fs.html")).expect("std.fs page");
    assert!(
        fs.contains("Reads a UTF-8 text file (<code>EILSEQ</code> if it is not valid UTF-8)."),
        "readFile's doc comment"
    );
    let search = std::fs::read_to_string(out.path().join("search-index.js")).expect("search");
    assert!(
        search.contains("\"readFile\",\"function\",\"std/fs\""),
        "search index"
    );
}
