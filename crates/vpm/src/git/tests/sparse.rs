//! Sparse checkouts (#3786, `git::sparse`): a dependency with a `path` fetches and checks out
//! that directory alone.

use std::collections::BTreeMap;

use super::*;
use crate::git::sparse::{fetch_keeping, leads, outside, Leads, Listing};

/// An upstream with history: two packages (`packages/lib` depends on its sibling
/// `packages/util` by path), and a large file beside them that changes in every commit. The
/// repository serves partial fetches when `partial`. Returns the last commit.
fn monorepo(e: &Env, partial: bool) -> String {
    let dir = &e.upstream.dir;
    let write = |rel: &str, text: &str| {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write(
        "packages/lib/package.velt",
        "export const pkg: Package = { name: \"lib\", version: \"0.1.0\", dependencies: { util: { path: \"../util\" } } };",
    );
    write(
        "packages/lib/src/lib.velt",
        "export const says = \"lib\";\n",
    );
    write(
        "packages/util/package.velt",
        "export const pkg: Package = { name: \"util\", version: \"0.1.0\" };",
    );
    write(
        "packages/util/src/lib.velt",
        "export const says = \"util\";\n",
    );
    for round in ["one", "two", "three"] {
        write("docs/big.txt", &round.repeat(1000));
        e.upstream.git(&["add", "-A"]);
        e.upstream.git(&["commit", "-q", "-m", round]);
    }
    if partial {
        e.upstream
            .git(&["config", "uploadpack.allowFilter", "true"]);
    }
    e.upstream.git(&["rev-parse", "HEAD"])
}

/// The files below `root`, `/`-separated and sorted.
fn files_of(root: &Path) -> Vec<String> {
    let mut out = vec![];
    collect(root, "", &mut out).unwrap();
    out.sort();
    out
}

fn lib_dep(e: &Env) -> String {
    format!(
        "lib: {{ git: \"{}\", branch: \"main\", path: \"packages/lib\" }}",
        e.upstream.url()
    )
}

#[test]
fn a_path_dependency_checks_out_its_directory_alone() {
    for partial in [true, false] {
        let e = env();
        let commit = monorepo(&e, partial);
        let url = e.upstream.url();
        let app = e.app(&lib_dep(&e));
        let installed = e.install(&app, false).unwrap();
        assert!(e.says(&installed).contains("lib"));
        // Each package has its own checkout, holding its directory at its place in the
        // repository and nothing else of the commit.
        let lib = checkout_dir(&e.loc, &url, &commit, Some("packages/lib"));
        assert_eq!(
            files_of(&lib),
            ["packages/lib/package.velt", "packages/lib/src/lib.velt"],
            "partial: {partial}"
        );
        let util = checkout_dir(&e.loc, &url, &commit, Some("packages/util"));
        assert_eq!(
            files_of(&util),
            ["packages/util/package.velt", "packages/util/src/lib.velt"]
        );
        assert!(!checkout_dir(&e.loc, &url, &commit, None).exists());
        // The lock says the checkouts are sparse, and each checksum is its checkout's.
        let lock = Lockfile::read(&app).unwrap().unwrap();
        let locked = lock.get("lib").unwrap();
        assert_eq!(
            locked.source,
            format!("git+{url}?branch=main&path=packages/lib&sparse=1#{commit}")
        );
        assert_eq!(locked.checksum, Some(tree_checksum(&lib).unwrap()));
        let sibling = lock.get("util").unwrap();
        assert_eq!(
            sibling.source,
            format!("git+{url}?rev={commit}&path=packages/util&sparse=1#{commit}")
        );
        assert_eq!(sibling.checksum, Some(tree_checksum(&util).unwrap()));
        // Without the repository, the lock still holds.
        std::fs::rename(&e.upstream.dir, e.root.join("gone")).unwrap();
        assert!(!e.install(&app, false).unwrap().lock_changed);
    }
}

/// The checksum is the same whether the server leaves files out of the fetch or sends them all.
#[test]
fn a_sparse_checkout_is_the_same_from_any_server() {
    let sums: Vec<Option<String>> = [true, false]
        .into_iter()
        .map(|partial| {
            let e = env();
            monorepo(&e, partial);
            let app = e.app(&lib_dep(&e));
            e.install(&app, false).unwrap();
            let lock = Lockfile::read(&app).unwrap().unwrap();
            lock.get("lib").unwrap().checksum.clone()
        })
        .collect();
    assert!(sums[0].is_some());
    assert_eq!(sums[0], sums[1]);
}

/// The fetch takes one commit of the history, and of its files only the path's.
#[test]
fn the_fetch_is_shallow_and_leaves_other_files_out() {
    let e = env();
    let commit = monorepo(&e, true);
    let url = e.upstream.url();
    let dir = e.root.join("checkout");
    let repo = e.root.join("kept");
    fetch_keeping(&url, &commit, "packages/lib", &dir, Some(&repo)).unwrap();
    let run = |args: &[&str]| git(Some(&repo), args).unwrap();
    assert_eq!(run(&["rev-list", "--count", &commit]).trim(), "1");
    let listed = run(&["rev-list", "--objects", "--missing=print", &commit]);
    let missing: Vec<&str> = listed.lines().filter(|l| l.starts_with('?')).collect();
    // `docs/big.txt` and the two files of `packages/util`.
    assert_eq!(missing.len(), 3, "{listed}");
    let big = e
        .upstream
        .git(&["rev-parse", &format!("{commit}:docs/big.txt")]);
    assert!(missing.contains(&format!("?{big}").as_str()), "{listed}");
}

/// A lock written before sparse checkouts names no `sparse`: its checksum is the whole tree's,
/// and the whole tree is what is checked out for it.
#[test]
fn an_old_lock_keeps_its_whole_checkout() {
    let e = env();
    let commit = monorepo(&e, true);
    let url = e.upstream.url();
    let app = e.app(&lib_dep(&e));
    e.install(&app, false).unwrap();
    // The lock as it was written then: the whole tree's checksum for both packages.
    let whole = checkout(
        &e.loc,
        &url,
        (&commit, None),
        (None, crate::tree_stamp::TreeCheck::Files),
    )
    .unwrap();
    let sum = tree_checksum(&whole).unwrap();
    let lock_file = app.join("velt.lock.json");
    let text = std::fs::read_to_string(&lock_file).unwrap();
    let mut old = text.replace("&sparse=1", "");
    let lock = Lockfile::read(&app).unwrap().unwrap();
    for name in ["lib", "util"] {
        let sparse_sum = lock.get(name).unwrap().checksum.clone().unwrap();
        assert_ne!(sparse_sum, sum);
        old = old.replace(&sparse_sum, &sum);
    }
    assert_ne!(old, text);
    std::fs::write(&lock_file, &old).unwrap();
    let installed = e.install(&app, false).unwrap();
    assert!(!installed.lock_changed);
    assert_eq!(std::fs::read_to_string(&lock_file).unwrap(), old);
    let lib = &installed.graph.packages()[0].dependencies["lib"];
    assert!(lib.starts_with(&whole), "{}", lib.display());
    // `velt update` writes the lock anew, sparse.
    let installed = e.install(&app, true).unwrap();
    assert!(installed.lock_changed);
    let lock = Lockfile::read(&app).unwrap().unwrap();
    assert!(lock.get("lib").unwrap().source.contains("&sparse=1#"));
}

/// Tags and commits name their commit as before; the checkout is the path's.
#[test]
fn a_tag_and_a_commit_check_out_the_path_at_their_commit() {
    let e = env();
    let first = monorepo(&e, true);
    e.upstream.git(&["tag", "-a", "v1", "-m", "v1"]);
    std::fs::write(
        e.upstream.dir.join("packages/lib/src/lib.velt"),
        "export const says = \"later\";\n",
    )
    .unwrap();
    e.upstream.git(&["commit", "-q", "-am", "later"]);
    let url = e.upstream.url();
    for (reference, says) in [
        ("tag: \"v1\"".to_string(), "lib"),
        (format!("rev: \"{first}\""), "lib"),
        ("branch: \"main\"".to_string(), "later"),
    ] {
        let app = e.app(&format!(
            "lib: {{ git: \"{url}\", {reference}, path: \"packages/lib\" }}"
        ));
        let _ = std::fs::remove_file(app.join("velt.lock.json"));
        let installed = e.install(&app, false).unwrap();
        assert!(e.says(&installed).contains(says), "{reference}");
        let lock = Lockfile::read(&app).unwrap().unwrap();
        let source = &lock.get("lib").unwrap().source;
        assert!(source.contains("&sparse=1#"), "{source}");
    }
}

/// Stage a symbolic link at `path` holding `text`, as git stores one (on every OS).
fn stage_link(e: &Env, path: &str, text: &str) {
    let file = e.root.join("link-text");
    std::fs::write(&file, text).unwrap();
    let blob = e
        .upstream
        .git(&["hash-object", "-w", &file.to_string_lossy()]);
    let entry = format!("120000,{blob},{path}");
    e.upstream
        .git(&["update-index", "--add", "--cacheinfo", &entry]);
}

/// What a package's links lead to in the repository is checked out with it: the target, and
/// each link on the way.
#[test]
fn what_a_link_leads_to_is_checked_out_too() {
    for partial in [true, false] {
        let e = env();
        monorepo(&e, partial);
        std::fs::create_dir_all(e.upstream.dir.join("shared/data")).unwrap();
        std::fs::write(e.upstream.dir.join("shared/data/table.txt"), "t").unwrap();
        std::fs::write(e.upstream.dir.join("shared/other.txt"), "o").unwrap();
        e.upstream.git(&["add", "-A"]);
        // `packages/lib/data` → `../../hop` → `shared/data`.
        stage_link(&e, "packages/lib/data", "../../hop");
        stage_link(&e, "hop", "shared/data");
        e.upstream.git(&["commit", "-q", "-m", "links"]);
        let commit = e.upstream.git(&["rev-parse", "HEAD"]);
        let url = e.upstream.url();
        let app = e.app(&lib_dep(&e));
        e.install(&app, false).unwrap();
        let lib = checkout_dir(&e.loc, &url, &commit, Some("packages/lib"));
        assert_eq!(
            files_of(&lib),
            [
                "hop",
                "packages/lib/data",
                "packages/lib/package.velt",
                "packages/lib/src/lib.velt",
                "shared/data/table.txt"
            ],
            "partial: {partial}"
        );
        assert_eq!(links(&lib), ["hop", "packages/lib/data"]);
        if cfg!(unix) {
            let through = std::fs::read_to_string(lib.join("packages/lib/data/table.txt"));
            assert_eq!(through.unwrap(), "t");
        }
    }
}

#[test]
fn a_link_out_of_the_repository_is_refused() {
    let e = env();
    monorepo(&e, true);
    stage_link(&e, "packages/lib/up", "../../../elsewhere");
    e.upstream.git(&["commit", "-q", "-m", "out"]);
    let commit = e.upstream.git(&["rev-parse", "HEAD"]);
    let app = e.app(&lib_dep(&e));
    let err = e.install(&app, false).unwrap_err();
    assert!(
        err.contains("has a symbolic link `packages/lib/up` that points out of the repository (`../../../elsewhere`)"),
        "{err}"
    );
    let dir = checkout_dir(&e.loc, &e.upstream.url(), &commit, Some("packages/lib"));
    assert!(!dir.exists());
}

fn listing(files: &[(&str, bool)]) -> Listing {
    let text: Vec<String> = files
        .iter()
        .map(|(path, link)| {
            let mode = if *link { "120000" } else { "100644" };
            format!("{mode} blob 0123\t{path}")
        })
        .collect();
    Listing::parse(&text.join("\0"))
}

#[test]
fn links_are_followed_in_the_listing() {
    let l = listing(&[
        ("a/link", true),
        ("a/loop", true),
        ("a/up", true),
        ("b/file", false),
        ("b/next", true),
        ("c/d/file", false),
    ]);
    let texts = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
        (pairs.iter())
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    };
    let to = |s: &str| Leads::To(s.to_string());
    let t = texts(&[("a/link", "../b/file")]);
    assert_eq!(leads(&l, &t, "a/link"), to("b/file"));
    // A part that doesn't exist is taken as written; `.` and `//` are no steps.
    let t = texts(&[("a/link", ".//../b/./nope/x")]);
    assert_eq!(leads(&l, &t, "a/link"), to("b/nope/x"));
    // Through another link: its text first, then on from where it leads.
    let t = texts(&[("a/link", "../b/next/file")]);
    assert_eq!(
        leads(&l, &t, "a/link"),
        Leads::Through("b/next".to_string())
    );
    let t = texts(&[("a/link", "../b/next/file"), ("b/next", "../c/d")]);
    assert_eq!(leads(&l, &t, "a/link"), to("c/d/file"));
    // The repository's root is inside; above it, an absolute path and a drive are not.
    let t = texts(&[("a/up", "..")]);
    assert_eq!(leads(&l, &t, "a/up"), to(""));
    for out in ["../..", "/etc/passwd", "C:/x", "..\\..\\x"] {
        let t = texts(&[("a/up", out)]);
        assert_eq!(leads(&l, &t, "a/up"), Leads::Out, "{out}");
    }
    let t = texts(&[("a/loop", "loop")]);
    assert_eq!(leads(&l, &t, "a/loop"), Leads::Loop);
}

/// A file of the repository that a sparse checkout leaves out is named, with what the checkout
/// holds; a file inside it, or in a whole checkout, is not.
#[test]
fn a_file_outside_a_sparse_checkout_is_named() {
    let e = env();
    let commit = monorepo(&e, true);
    let url = e.upstream.url();
    let app = e.app(&lib_dep(&e));
    e.install(&app, false).unwrap();
    let lib = checkout_dir(&e.loc, &url, &commit, Some("packages/lib"));
    let note = outside(&lib.join("docs/big.txt")).unwrap();
    assert!(
        note.starts_with(&format!(
            "`docs/big.txt` is outside what this git dependency checks out of {url}"
        )) && note.contains("that directory alone (`packages/lib`)"),
        "{note}"
    );
    assert_eq!(outside(&lib.join("packages/lib/src/typo.velt")), None);
    assert!(outside(&lib.join("packages/library/x.velt")).is_some());
    let whole = checkout(
        &e.loc,
        &url,
        (&commit, None),
        (None, crate::tree_stamp::TreeCheck::Files),
    )
    .unwrap();
    assert_eq!(outside(&whole.join("docs/nope.txt")), None);
    assert_eq!(outside(&app.join("src/nope.velt")), None);
}

#[test]
fn sparse_sources_round_trip_through_the_lockfile() {
    let s = spec("https://h/r", GitRef::Tag("v1".into()), Some("a/b"));
    let source = s.locked_source(COMMIT, true);
    assert_eq!(
        source,
        format!("git+https://h/r?tag=v1&path=a/b&sparse=1#{COMMIT}")
    );
    assert_eq!(
        GitSpec::parse_locked(&source),
        Some((s.clone(), COMMIT.to_string(), true))
    );
    assert_eq!(
        GitSpec::parse_locked(&s.source(COMMIT)),
        Some((s, COMMIT.to_string(), false))
    );
    // Only a `path` is checked out alone, and `sparse` has one value.
    for bad in ["?sparse=1", "?path=a&sparse=2", "?path=a&sparse="] {
        let source = format!("git+https://h/r{bad}#{COMMIT}");
        assert_eq!(GitSpec::parse_locked(&source), None, "{source}");
    }
}

/// A package inside an npm workspace imports its siblings by name (`crate::npm`), so its
/// checkout holds the workspace: the manifests above the package and the workspace's packages,
/// and still nothing else of the commit.
#[test]
fn the_npm_workspace_around_a_package_is_checked_out_with_it() {
    for partial in [true, false] {
        let e = env();
        monorepo(&e, partial);
        let dir = &e.upstream.dir;
        let write = |rel: &str, text: &str| {
            let path = dir.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write(
            "pnpm-workspace.yaml",
            "packages:\n  - \"packages/*\"\n  - \"!packages/private\"\n",
        );
        write(
            "package.json",
            "{ \"name\": \"root\", \"private\": true }\n",
        );
        write(
            "packages/signals/package.json",
            "{ \"name\": \"@demo/signals\" }\n",
        );
        write("packages/signals/src/index.ts", "export const x = 1;\n");
        write(
            "packages/private/package.json",
            "{ \"name\": \"private\" }\n",
        );
        write("packages/notes/readme.txt", "no package\n");
        write("tools/build/package.json", "{ \"name\": \"build\" }\n");
        e.upstream.git(&["add", "-A"]);
        e.upstream.git(&["commit", "-q", "-m", "workspace"]);
        let commit = e.upstream.git(&["rev-parse", "HEAD"]);
        let url = e.upstream.url();
        let app = e.app(&lib_dep(&e));
        e.install(&app, false).unwrap();
        let lib = checkout_dir(&e.loc, &url, &commit, Some("packages/lib"));
        assert_eq!(
            files_of(&lib),
            [
                "package.json",
                "packages/lib/package.velt",
                "packages/lib/src/lib.velt",
                "packages/signals/package.json",
                "packages/signals/src/index.ts",
                "pnpm-workspace.yaml"
            ],
            "partial: {partial}"
        );
        // The sibling is found from the package as in a whole checkout.
        let mut npm = crate::npm::NpmResolver::default();
        let from = lib.join("packages/lib/src/lib.velt");
        assert_eq!(
            npm.package_dir(&from, "@demo/signals"),
            Some(lib.join("packages/signals"))
        );
    }
}
