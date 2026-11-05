//! `c-headers.json` against what a build compiles (docs/internals/design/c-ffi.md "Checking
//! declarations against C headers"): the record of a library that passed the check, and the
//! comparison of a fetched package with its author's.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use velt_common::{Diagnostic, Diagnostics, Span};
use vpm::c_headers::{CHeaders, LibraryRecord, FILE as RECORD};
use vpm::graph::GraphPackage;

use super::{compare, file_digest, Decl, LibraryHeaders};

/// Compare what a fetched package's `library` declares (`decls`, those this program compiles)
/// and the headers it has with what its author checked on `zig_target`, as its `c-headers.json`
/// records. Errors go to `diags`; the record, when everything matched.
pub(super) fn compare_record(
    package: &GraphPackage,
    library: &str,
    decls: &[Decl],
    read: LibraryHeaders,
    zig_target: &str,
    diags: &mut Diagnostics,
) -> Option<LibraryRecord> {
    let name = &package.name;
    let at = decls[0].span;
    let record = match CHeaders::read(&package.root) {
        Ok(Some(record)) => record,
        Ok(None) => {
            diags.push(
                Diagnostic::error(
                    format!(
                        "package `{name}` declares C functions, but was published without a record that they were checked against their headers ({RECORD})"
                    ),
                    at,
                )
                .with_note("its author makes the record with `velt native headers`, which `velt publish` runs; ask for a version that has it"),
            );
            return None;
        }
        Err(e) => {
            diags.push(
                Diagnostic::error(format!("package `{name}` has an invalid {RECORD}"), at)
                    .with_note(e),
            );
            return None;
        }
    };
    let Some(recorded) = record.library(zig_target, library) else {
        diags.push(
            Diagnostic::error(
                format!(
                    "package `{name}`'s C library `{library}` was not checked against its headers for {zig_target}"
                ),
                at,
            )
            .with_note(format!(
                "{RECORD} has no entry for {zig_target}: its author runs `velt native headers` to check every target"
            )),
        );
        return None;
    };
    let before = diags.len();
    for d in decls {
        if !recorded.declarations.contains(&declaration_digest(d)) {
            diags.push(
                Diagnostic::error(
                    format!(
                        "C function `{}` in package `{name}` is not declared as its author checked against C library `{library}`'s headers for {zig_target}",
                        d.name
                    ),
                    d.span,
                )
                .with_note(format!(
                    "{RECORD} has no such declaration: the package changed after the check"
                )),
            );
        }
    }
    if recorded.defines != vpm::c_headers::defines_digest(read.defines) {
        diags.push(
            Diagnostic::error(
                format!(
                    "C library `{library}` in package `{name}` has other `defines` than its author checked its headers with for {zig_target}"
                ),
                at,
            )
            .with_note(format!(
                "{RECORD} records other `defines`: the package changed after the check"
            )),
        );
    }
    let of = format!("of C library `{library}` in package `{name}`");
    let headers = &recorded.headers;
    compare_headers(package, &of, headers, read.headers, zig_target, at, diags);
    (diags.len() == before).then(|| recorded.clone())
}

/// Compare the headers of a fetched package with those its author's check read (`recorded`,
/// the headers `of` a library or the constants, as messages say). A git checkout has them all,
/// so one missing is an error. A registry archive ships `native/` only for a package that
/// builds C `sources`: one missing from there is an error too, and elsewhere only what is
/// present can be compared (docs/internals/design/c-ffi.md).
pub(super) fn compare_headers(
    package: &GraphPackage,
    of: &str,
    recorded: &BTreeMap<String, String>,
    listed: &[String],
    zig_target: &str,
    at: Span,
    diags: &mut Diagnostics,
) {
    let ships_sources = package.c_libraries.values().any(|l| !l.sources.is_empty());
    let shipped = |header: &str| {
        package.checkout
            || (ships_sources
                && header
                    .strip_prefix(vpm::manifest::C_SOURCES_DIR)
                    .is_some_and(|rest| rest.starts_with('/')))
    };
    let mut refuse = |header: &str, what: &str| {
        diags.push(
            Diagnostic::error(
                format!("header `{header}` {of} {what} for {zig_target}"),
                at,
            )
            .with_note(format!(
                "its author runs `velt native headers` again and commits {RECORD}"
            )),
        );
    };
    for (header, digest) in recorded {
        // The record is the package's: nothing it names outside the package is read.
        let inside = Path::new(header)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)));
        let found = inside
            .then(|| file_digest(&package.root.join(header)))
            .flatten();
        match found {
            Some(found) if found == *digest => {}
            Some(_) => refuse(header, "is not the one its author checked"),
            None if shipped(header) || !inside => {
                refuse(header, "is gone, though its author checked it")
            }
            None => {}
        }
    }
    for header in listed.iter().filter(|h| !h.starts_with('<')) {
        let here = shipped(header) || package.root.join(header).is_file();
        if here && !recorded.contains_key(header) {
            refuse(header, "is listed, but its author didn't check it");
        }
    }
}

/// The digest of a declaration as `c-headers.json` records it: the function's name and what its
/// parameters and result pass.
fn declaration_digest(d: &Decl) -> String {
    let mut h = Sha256::new();
    h.update(&d.name);
    for p in &d.params {
        h.update([0]);
        h.update(compare::describe(p));
    }
    h.update([1]);
    h.update(compare::describe(&d.ret));
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// The record of a library that passed: its declarations, and the files `read` that are in the
/// package at `root`.
pub(super) fn library_record(
    root: &Path,
    decls: &[Decl],
    defines: &[(String, String)],
    read: &[(PathBuf, String)],
) -> LibraryRecord {
    LibraryRecord {
        declarations: decls.iter().map(declaration_digest).collect(),
        headers: package_headers(root, read),
        defines: vpm::c_headers::defines_digest(defines),
    }
}

/// The files of `read` that are in the package at `root`: `/`-separated path in it → digest.
pub(super) fn package_headers(root: &Path, read: &[(PathBuf, String)]) -> BTreeMap<String, String> {
    let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let package = canonical(root);
    let mut headers = BTreeMap::new();
    for (file, digest) in read {
        let file = canonical(file);
        if let Ok(path) = file.strip_prefix(&package) {
            let parts: Vec<_> = path
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect();
            headers.insert(parts.join("/"), digest.clone());
        }
    }
    headers
}

#[cfg(test)]
mod tests {
    use super::*;

    use compare::Shape;
    use vpm::PackageGraph;

    fn decl(name: &str, params: Vec<Shape>) -> Decl {
        let int = compare::Scalar::Int {
            bits: 32,
            signed: true,
            pointer_sized: false,
        };
        Decl {
            name: name.into(),
            params,
            ret: Shape::Scalar(int),
            span: Span::DUMMY,
        }
    }

    #[test]
    fn a_declaration_s_digest_changes_with_it() {
        let base = declaration_digest(&decl("f", vec![Shape::Text]));
        assert_eq!(base, declaration_digest(&decl("f", vec![Shape::Text])));
        assert_ne!(base, declaration_digest(&decl("g", vec![Shape::Text])));
        assert_ne!(base, declaration_digest(&decl("f", vec![Shape::Null])));
        assert_ne!(base, declaration_digest(&decl("f", vec![])));
    }

    #[test]
    fn a_file_s_digest_ignores_how_its_lines_end() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.h"), dir.path().join("b.h"));
        std::fs::write(&a, "int f(void);\nint g(void);\n").unwrap();
        std::fs::write(&b, "int f(void);\r\nint g(void);\r\n").unwrap();
        assert_eq!(file_digest(&a), file_digest(&b));
        std::fs::write(&b, "int f(void);\r\nint h(void);\r\n").unwrap();
        assert_ne!(file_digest(&a), file_digest(&b));
    }

    const TARGET: &str = "x86_64-linux-gnu";

    /// `compare_record` of `decls`: the errors' messages, and the notes of the first.
    fn compared(package: &GraphPackage, decls: &[Decl], listed: &[&str]) -> (Vec<String>, bool) {
        let listed: Vec<String> = listed.iter().map(|h| h.to_string()).collect();
        let mut diags = Diagnostics::default();
        let read = LibraryHeaders {
            headers: &listed,
            defines: &[],
        };
        let record = compare_record(package, "own", decls, read, TARGET, &mut diags);
        let messages = diags.iter().map(|d| d.message.clone()).collect();
        (messages, record.is_some())
    }

    /// Declarations are a set: a program compiles some of what the record has, in any order.
    #[test]
    fn a_fetched_package_s_record_is_compared() {
        let tmp = tempfile::tempdir().unwrap();
        let mut graph = PackageGraph::default();
        graph.add("bind", tmp.path(), BTreeMap::new()).fetched = true;
        let package = &graph.packages()[0];
        let (f, g) = (decl("f", vec![Shape::Text]), decl("g", vec![]));
        let (e, passed) = compared(package, std::slice::from_ref(&f), &[]);
        assert!(
            !passed && e[0].contains("was published without a record"),
            "{e:?}"
        );
        std::fs::write(tmp.path().join(RECORD), "{").unwrap();
        let (e, _) = compared(package, std::slice::from_ref(&f), &[]);
        assert_eq!(e, ["package `bind` has an invalid c-headers.json"]);
        let mut record = CHeaders::default();
        record.checked.entry(TARGET.into()).or_default().insert(
            "own".into(),
            LibraryRecord {
                declarations: [&f, &g].map(declaration_digest).into(),
                headers: BTreeMap::new(),
                defines: String::new(),
            },
        );
        record.write(tmp.path()).unwrap();
        for decls in [
            vec![f, g],
            vec![decl("g", vec![])],
            vec![decl("g", vec![]), decl("f", vec![Shape::Text])],
        ] {
            assert_eq!(compared(package, &decls, &[]), (vec![], true));
        }
        let (e, passed) = compared(package, &[decl("f", vec![])], &[]);
        assert!(!passed);
        assert_eq!(e, ["C function `f` in package `bind` is not declared as its author checked against C library `own`'s headers for x86_64-linux-gnu"]);
        let mut diags = Diagnostics::default();
        let other = compare_record(
            package,
            "own",
            &[decl("g", vec![])],
            LibraryHeaders {
                headers: &[],
                defines: &[],
            },
            "aarch64-macos",
            &mut diags,
        );
        assert!(other.is_none());
        assert!(diags.iter().any(|d| d
            .message
            .contains("was not checked against its headers for aarch64-macos")));
    }

    /// A checkout has the headers its author checked; a registry archive has none, which is
    /// the documented limit, but one it has is compared.
    #[test]
    fn a_fetched_package_s_headers_are_compared_when_it_has_them() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir(root.join("native")).unwrap();
        let header = root.join("native/own.h");
        std::fs::write(&header, "int g(void);\n").unwrap();
        let g = decl("g", vec![]);
        let mut record = CHeaders::default();
        record.checked.entry(TARGET.into()).or_default().insert(
            "own".into(),
            library_record(
                root,
                std::slice::from_ref(&g),
                &[],
                &[(header.clone(), file_digest(&header).unwrap())],
            ),
        );
        assert_eq!(
            record
                .library(TARGET, "own")
                .unwrap()
                .headers
                .keys()
                .collect::<Vec<_>>(),
            ["native/own.h"]
        );
        record.write(root).unwrap();
        let listed = ["native/own.h", "<string.h>"];
        let mut archive = PackageGraph::default();
        archive.add("bind", root, BTreeMap::new()).fetched = true;
        let mut checkout = PackageGraph::default();
        let added = checkout.add("bind", root, BTreeMap::new());
        added.fetched = true;
        added.checkout = true;
        for graph in [&archive, &checkout] {
            assert_eq!(
                compared(&graph.packages()[0], std::slice::from_ref(&g), &listed),
                (vec![], true)
            );
        }
        std::fs::write(&header, "long g(void);\n").unwrap();
        for graph in [&archive, &checkout] {
            let (e, passed) = compared(&graph.packages()[0], std::slice::from_ref(&g), &listed);
            assert!(!passed && e[0].contains("`native/own.h` of C library `own` in package `bind` is not the one its author checked"), "{e:?}");
        }
        std::fs::remove_file(&header).unwrap();
        assert_eq!(
            compared(&archive.packages()[0], std::slice::from_ref(&g), &listed),
            (vec![], true)
        );
        let (e, _) = compared(&checkout.packages()[0], std::slice::from_ref(&g), &listed);
        assert!(e[0].contains("is gone"), "{e:?}");
        // A registry archive of a package that builds C `sources` ships all of `native/`.
        let mut bundled = PackageGraph::default();
        let added = bundled.add("bind", root, BTreeMap::new());
        added.fetched = true;
        let own = vpm::manifest::CLibraryConfig {
            sources: vec!["native/own.c".into()],
            ..Default::default()
        };
        added.c_libraries.insert("own".into(), own);
        let (e, _) = compared(&bundled.packages()[0], std::slice::from_ref(&g), &listed);
        assert!(
            e[0].contains("`native/own.h`") && e[0].contains("is gone"),
            "{e:?}"
        );
        // A header listed since the check, and a path out of the package in the record.
        let (e, _) = compared(
            &checkout.packages()[0],
            std::slice::from_ref(&g),
            &["native/new.h"],
        );
        assert!(
            e.iter()
                .any(|m| m.contains("`native/new.h`") && m.contains("didn't check it")),
            "{e:?}"
        );
        let mut out = record.clone();
        let own = out.checked.get_mut(TARGET).unwrap().get_mut("own").unwrap();
        own.headers = BTreeMap::from([("../x.h".to_string(), "00".to_string())]);
        out.write(root).unwrap();
        let (e, _) = compared(&archive.packages()[0], std::slice::from_ref(&g), &[]);
        assert!(
            e[0].contains("`../x.h`") && e[0].contains("is gone"),
            "{e:?}"
        );
    }

    /// `defines` change what a library's headers declare: a record holds whose they were.
    #[test]
    fn a_fetched_package_s_defines_are_compared() {
        let tmp = tempfile::tempdir().unwrap();
        let mut graph = PackageGraph::default();
        graph.add("bind", tmp.path(), BTreeMap::new()).fetched = true;
        let package = &graph.packages()[0];
        let g = decl("g", vec![]);
        let answer = [("ANSWER".to_string(), "40".to_string())];
        let mut record = CHeaders::default();
        let checked = library_record(tmp.path(), std::slice::from_ref(&g), &answer, &[]);
        record
            .checked
            .entry(TARGET.into())
            .or_default()
            .insert("own".into(), checked);
        record.write(tmp.path()).unwrap();
        let wide = [answer[0].clone(), ("WIDE".to_string(), "1".to_string())];
        for (defines, refused) in [(&answer[..], false), (&wide[..], true), (&[][..], true)] {
            let mut diags = Diagnostics::default();
            let read = LibraryHeaders {
                headers: &[],
                defines,
            };
            let passed = compare_record(
                package,
                "own",
                std::slice::from_ref(&g),
                read,
                TARGET,
                &mut diags,
            );
            assert_eq!(passed.is_none(), refused, "{defines:?}");
            if refused {
                assert!(
                    diags.iter().any(|d| d
                        .message
                        .contains("has other `defines` than its author checked")),
                    "{defines:?}"
                );
            }
        }
    }
}
