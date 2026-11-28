//! A toolchain's hook objects on stand-ins: the manifest, where the directory is, and what makes
//! an object one that may be linked.

use super::*;

/// The file `rel` (`/`-separated) below `root`, built a component at a time as `find` builds it
/// (on Windows `root.join("a/b")` would mix separators, and the paths would not compare equal).
fn under(root: &Path, rel: &str) -> PathBuf {
    rel.split('/')
        .fold(root.to_path_buf(), |p, part| p.join(part))
}

/// A std root in a new directory with the files `files` (path below it, contents).
fn std_with(test: &str, files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::Builder::new()
        .prefix(&format!("velt-shipped-{test}-"))
        .tempdir()
        .unwrap();
    let std_root = tmp.path().join("prefix").join("std");
    for (rel, src) in files {
        let path = under(&std_root, rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, src).unwrap();
    }
    (tmp, std_root)
}

/// An object compiled from `files` of `std_root`, as `prebuilt_object::build` hands it over.
fn built(std_root: &Path, files: &[&str]) -> Built {
    let files = (files.iter())
        .map(|rel| {
            let path = under(std_root, rel);
            let src = std::fs::read_to_string(&path).unwrap();
            super::super::prebuilt::file_entry(&path, &src)
        })
        .collect();
    Built {
        object: b"an object".to_vec(),
        entries: vec!["velt_hook_probe [F64] -> F64".into()],
        files,
        tls: vec![],
        statics: vec!["velt_process_str_epoch".into()],
    }
}

const FILES: [(&str, &str); 2] = [
    ("lang/hooks/probe.velt", "export function probe() {}\n"),
    ("lang/number/digits.velt", "export const TEN = 10;\n"),
];
const RELS: [&str; 2] = ["lang/hooks/probe.velt", "lang/number/digits.velt"];

#[test]
fn a_manifest_reads_back_and_names_nothing_of_the_machine() {
    let (tmp, std_root) = std_with("manifest", &FILES);
    let dir = tmp.path().join("prefix/lib/hooks/t");
    write(
        &dir,
        "m",
        &built(&std_root, &RELS),
        &std_root,
        "velt 1 (h t)",
        "t",
    )
    .unwrap();
    let text = std::fs::read_to_string(dir.join("m.manifest")).unwrap();
    let manifest = Manifest::parse(&text).expect("a manifest");
    assert_eq!(manifest.render(), text);
    assert_eq!(
        (manifest.compiler.as_str(), manifest.target.as_str()),
        ("velt 1 (h t)", "t")
    );
    let paths: Vec<&str> = manifest.files.iter().map(|(_, p)| p.as_str()).collect();
    assert_eq!(paths, RELS);
    // No path of this machine, no size, no time: the hash and the path below std alone.
    assert!(!text.contains(&tmp.path().display().to_string()), "{text}");
    for (hash, rel) in &manifest.files {
        assert!(text.contains(&format!("file {hash}\t{rel}\n")), "{text}");
    }
    assert_eq!(std::fs::read(dir.join("m.o")).unwrap(), b"an object");
    assert!(text.ends_with("\nend\n"), "{text}");
    // The process statics it imports; a symbol that names none is no manifest.
    assert_eq!(manifest.statics, ["velt_process_str_epoch"]);
    let unknown = text.replacen(
        "static velt_process_str_epoch",
        "static velt_process_nothing",
        1,
    );
    assert_ne!(unknown, text);
    assert_eq!(Manifest::parse(&unknown), None);
    // Another form is no manifest.
    assert_eq!(
        Manifest::parse(&text.replacen("module 2", "module 1", 1)),
        None
    );
    assert_eq!(Manifest::parse(&format!("{text}what\n")), None);
    assert_eq!(Manifest::parse(""), None);
}

/// A manifest is read whole or not at all: cut short anywhere (a copy or an unpack that
/// stopped), at a line's end too, it lacks the end marker; and one that lists no std file would
/// vouch for its object against nothing.
#[test]
fn a_manifest_cut_short_or_listing_no_file_is_not_read() {
    let (tmp, std_root) = std_with("cut", &FILES);
    let dir = tmp.path().join("prefix/lib/hooks/t");
    write(
        &dir,
        "m",
        &built(&std_root, &RELS),
        &std_root,
        "velt 1",
        "t",
    )
    .unwrap();
    let path = dir.join("m.manifest");
    let whole = std::fs::read_to_string(&path).unwrap();
    let found = || find(&dir, "m", &std_root, "velt 1", "t").is_some();
    assert!(found());
    let lines: Vec<&str> = whole.lines().collect();
    for keep in 1..lines.len() {
        let cut = lines[..keep].join("\n") + "\n";
        std::fs::write(&path, &cut).unwrap();
        assert!(!found(), "{keep} lines: {cut}");
    }
    std::fs::write(&path, &whole[..whole.len() - 2]).unwrap();
    assert!(!found());
    let no_files: Vec<&str> = (lines.iter().copied())
        .filter(|l| !l.starts_with("file "))
        .collect();
    std::fs::write(&path, no_files.join("\n") + "\n").unwrap();
    assert!(!found());
    std::fs::write(&path, &whole).unwrap();
    assert!(found());
}

/// The object is the one the manifest names by its hash: cut short, emptied or replaced, it is
/// neither linked nor stored in the cache.
#[test]
fn a_damaged_object_is_not_taken() {
    let (tmp, std_root) = std_with("damaged", &FILES);
    let dir = tmp.path().join("prefix/lib/hooks/t");
    write(
        &dir,
        "m",
        &built(&std_root, &RELS),
        &std_root,
        "velt 1",
        "t",
    )
    .unwrap();
    let object = dir.join("m.o");
    let found = || find(&dir, "m", &std_root, "velt 1", "t");
    assert_eq!(found().map(|s| s.bytes), Some(b"an object".to_vec()));
    for damaged in [&b"an obj"[..], b"", b"garbage!!"] {
        std::fs::write(&object, damaged).unwrap();
        assert!(found().is_none(), "{damaged:?}");
    }
    std::fs::remove_file(&object).unwrap();
    assert!(found().is_none());
    std::fs::write(&object, b"an object").unwrap();
    assert!(found().is_some());
}

/// A file the object's world loaded from outside the std root cannot be shipped.
#[test]
fn a_file_outside_std_is_refused() {
    let (tmp, std_root) = std_with("outside", &FILES);
    let outside = tmp.path().join("elsewhere.velt");
    std::fs::write(&outside, "x").unwrap();
    let mut b = built(&std_root, &RELS);
    b.files
        .push(super::super::prebuilt::file_entry(&outside, "x"));
    let err = write(&tmp.path().join("out"), "m", &b, &std_root, "c", "t").unwrap_err();
    assert!(err.contains("is not below the std root"), "{err}");
}

/// The object is taken when it is this compiler's, for this target, and its std files are as
/// they were; the `file` entries it comes with are those of the files now.
#[test]
fn an_object_of_this_toolchain_and_these_sources_is_found() {
    let (tmp, std_root) = std_with("found", &FILES);
    let dir = tmp.path().join("prefix/lib/hooks/t");
    let b = built(&std_root, &RELS);
    write(&dir, "m", &b, &std_root, "velt 1", "t").unwrap();
    let found = find(&dir, "m", &std_root, "velt 1", "t").expect("found");
    assert_eq!(found.object, dir.join("m.o"));
    assert_eq!(found.entries, b.entries);
    assert_eq!(found.files, b.files);
    // The same sources in another place (an install is a copy): found, with that place's entries.
    let copy = tmp.path().join("copy");
    for (rel, src) in FILES {
        let path = under(&copy, rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, src).unwrap();
    }
    let moved = find(&dir, "m", &copy, "velt 1", "t").expect("found");
    assert!(moved
        .files
        .iter()
        .all(|f| f.contains(&copy.display().to_string())));
    assert_eq!(
        find(&dir, "other", &std_root, "velt 1", "t").map(|f| f.object),
        None
    );
}

/// Not taken: another compiler's, another target's, a source that changed or is gone, an object
/// that is gone.
#[test]
fn anything_else_is_not_found() {
    let (tmp, std_root) = std_with("stale", &FILES);
    let dir = tmp.path().join("prefix/lib/hooks/t");
    write(
        &dir,
        "m",
        &built(&std_root, &RELS),
        &std_root,
        "velt 1",
        "t",
    )
    .unwrap();
    let found =
        |compiler: &str, target: &str| find(&dir, "m", &std_root, compiler, target).is_some();
    assert!(found("velt 1", "t"));
    assert!(!found("velt 2", "t"));
    assert!(!found("velt 1", "u"));
    let source = under(&std_root, RELS[1]);
    std::fs::write(&source, "export const TEN = 11;\n").unwrap();
    assert!(!found("velt 1", "t"));
    std::fs::write(&source, FILES[1].1).unwrap();
    assert!(found("velt 1", "t"));
    std::fs::remove_file(&source).unwrap();
    assert!(!found("velt 1", "t"));
    std::fs::write(&source, FILES[1].1).unwrap();
    std::fs::remove_file(dir.join("m.o")).unwrap();
    assert!(!found("velt 1", "t"));
}

/// A manifest's path is one below the std root, or the object is not taken: a path out of it
/// would have this read, and vouch for, a file that is not std's.
#[test]
fn a_path_out_of_the_std_root_is_not_followed() {
    let std_root = Path::new("/p/std");
    assert_eq!(
        below(std_root, "lang/a.velt"),
        Some(PathBuf::from("/p/std/lang/a.velt"))
    );
    for bad in [
        "../x.velt",
        "lang/../../x",
        "/etc/passwd",
        "",
        "a//b",
        "./a",
        "c:\\x",
        "a\\b",
    ] {
        assert_eq!(below(std_root, bad), None, "{bad:?}");
    }
    let (tmp, std_root) = std_with("escape", &FILES);
    let dir = tmp.path().join("prefix/lib/hooks/t");
    write(
        &dir,
        "m",
        &built(&std_root, &RELS),
        &std_root,
        "velt 1",
        "t",
    )
    .unwrap();
    let manifest = dir.join("m.manifest");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(
        &manifest,
        text.replace(RELS[1], "../std/lang/number/digits.velt"),
    )
    .unwrap();
    assert!(find(&dir, "m", &std_root, "velt 1", "t").is_none());
}

/// The objects are an installed toolchain's: `<prefix>/bin/velt` with `<prefix>/std`, and only
/// where the toolchain has the directory.
#[test]
fn the_directory_is_an_installed_toolchain_s() {
    let (tmp, std_root) = std_with("dir", &FILES);
    let prefix = tmp.path().join("prefix");
    let exe = prefix.join("bin").join("velt");
    assert_eq!(dir_of(&exe, &std_root, "t"), None);
    let dir = prefix.join("lib/hooks/t");
    std::fs::create_dir_all(&dir).unwrap();
    assert_eq!(dir_of(&exe, &std_root, "t"), Some(dir));
    assert_eq!(dir_of(&exe, &std_root, "u"), None);
    // A development build (no `bin`), and a std root from elsewhere (`$VELT_STD`).
    assert_eq!(
        dir_of(&prefix.join("target/debug/velt"), &std_root, "t"),
        None
    );
    assert_eq!(dir_of(&exe, &tmp.path().join("other/std"), "t"), None);
}
