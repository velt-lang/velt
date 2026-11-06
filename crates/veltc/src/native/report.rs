//! What `velt check` lists about native code (docs/internals/design/c-ffi.md "The trust
//! boundary"): each package with native code that the program allows, with its C libraries (and
//! whether it has a Rust crate) and its unsafe code by file and line. An audit of what the
//! program trusts reads exactly these places.

use std::path::Path;

use serde_json::{json, Value};
use velt_common::SourceMap;
use vpm::PackageGraph;

use super::unsafe_sites::{Site, SiteKind};

/// A package with native code that the program allows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativePackage {
    pub name: String,
    /// Its C libraries (`native.c`): each with the system library it links on the build's
    /// target (`None`: none there, the C library, or its `sources`), how its declarations are
    /// checked, and the C files it is built from.
    pub c_libraries: Vec<(String, Option<String>, Checked, Vec<String>)>,
    /// It has a Rust crate (`native.crate`), built into a native library.
    pub rust_crate: bool,
    /// Its unsafe code, in module and source order.
    pub unsafe_code: Vec<UnsafeCode>,
}

/// How a C library's declarations are checked (docs/internals/design/c-ffi.md "Checking
/// declarations against C headers").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Checked {
    /// Against the headers package.velt lists, on the build's target.
    Headers,
    /// It lists headers, but no check looked here: the target has no C headers (WebAssembly),
    /// or nothing of the library is declared.
    Listed,
    /// `unchecked: true`: the library has no headers.
    Unchecked,
    /// It lists no headers (yet).
    NoHeaders,
}

impl Checked {
    fn of(lib: &vpm::manifest::CLibraryConfig) -> Checked {
        match (lib.unchecked, lib.headers.is_empty()) {
            (true, _) => Checked::Unchecked,
            (false, false) => Checked::Listed,
            (false, true) => Checked::NoHeaders,
        }
    }

    fn word(self) -> &'static str {
        match self {
            Checked::Headers => "headers checked",
            Checked::Listed => "headers listed",
            Checked::Unchecked => "unchecked",
            Checked::NoHeaders => "no headers",
        }
    }
}

/// One piece of unsafe code: where (relative to the package root, `/`-separated) and what.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnsafeCode {
    pub file: String,
    pub line: usize,
    /// `unsafe block`, `unsafe expression`, `unsafe function \`name\``.
    pub what: String,
}

/// The packages with native code that `graph` (the program's) allows, with the unsafe code of
/// their modules (`sites`: each module's file and sites, in load order). Empty without packages.
pub(super) fn native_packages(
    graph: Option<&PackageGraph>,
    sites: &[(velt_common::FileId, Vec<Site>)],
    sm: &SourceMap,
    target: &str,
) -> Vec<NativePackage> {
    let Some(graph) = graph else {
        return Vec::new();
    };
    let mut out: Vec<NativePackage> = graph
        .packages()
        .iter()
        .filter(|p| p.has_native_code && graph.allows_native(p))
        .map(|p| NativePackage {
            name: p.name.clone(),
            c_libraries: p
                .c_libraries
                .iter()
                .map(|(name, lib)| {
                    (
                        name.clone(),
                        lib.system_for(target).map(str::to_string),
                        Checked::of(lib),
                        lib.sources.clone(),
                    )
                })
                .collect(),
            rust_crate: p.native.is_some(),
            unsafe_code: Vec::new(),
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    for (file, module_sites) in sites {
        let path = &sm.get(*file).path;
        let Some(package) = graph.package_of(path) else {
            continue;
        };
        let Some(entry) = out.iter_mut().find(|p| p.name == package.name) else {
            continue;
        };
        let rel = relative(path, &package.root);
        for site in module_sites {
            let (line, _) = sm.line_col(*file, site.span.lo);
            let what = match &site.kind {
                SiteKind::Block => "unsafe block".to_string(),
                SiteKind::Expr => "unsafe expression".to_string(),
                SiteKind::Function(name) => format!("unsafe function `{name}`"),
            };
            entry.unsafe_code.push(UnsafeCode {
                file: rel.clone(),
                line,
                what,
            });
        }
    }
    out
}

/// Say which of `packages`' C libraries were checked against their headers (`checked`, by the
/// roots in `graph`): only those may be listed as checked.
pub fn note_checked(
    packages: &mut [NativePackage],
    graph: Option<&PackageGraph>,
    checked: &super::CChecked,
) {
    let Some(graph) = graph else { return };
    for package in graph.packages() {
        let Some(libraries) = checked.get(&package.root) else {
            continue;
        };
        let listed = packages.iter_mut().find(|p| p.name == package.name);
        for (name, _, state, _) in listed.into_iter().flat_map(|p| &mut p.c_libraries) {
            if libraries.contains_key(name) {
                *state = Checked::Headers;
            }
        }
    }
}

fn relative(path: &Path, root: &Path) -> String {
    let canonical = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let (path, root) = (canonical(path), canonical(root));
    let rel = path.strip_prefix(&root).unwrap_or(&path);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// The listing for people (`velt check` on stdout); empty when no package has native code.
pub fn render(packages: &[NativePackage]) -> String {
    if packages.is_empty() {
        return String::new();
    }
    let mut s = String::from("Native code the program allows (allowNative):\n");
    for p in packages {
        let mut parts: Vec<String> = Vec::new();
        if !p.c_libraries.is_empty() {
            let libs: Vec<String> = p
                .c_libraries
                .iter()
                .map(|(name, system, checked, sources)| {
                    let links = match system {
                        Some(system) => system.clone(),
                        None if !sources.is_empty() => format!("built from {}", sources.join(", ")),
                        None if name == "c" => "the platform's".into(),
                        None => "none on this target".into(),
                    };
                    format!("{name} ({links}; {})", checked.word())
                })
                .collect();
            parts.push(format!("C libraries {}", libs.join(", ")));
        }
        if p.rust_crate {
            parts.push("a Rust crate".into());
        }
        let n = p.unsafe_code.len();
        parts.push(match n {
            0 => "no unsafe code".into(),
            1 => "1 place of unsafe code".into(),
            n => format!("{n} places of unsafe code"),
        });
        s.push_str(&format!("  {}: {}\n", p.name, parts.join("; ")));
        let width = p
            .unsafe_code
            .iter()
            .map(|u| u.file.len() + u.line.to_string().len() + 1)
            .max()
            .unwrap_or(0);
        for u in &p.unsafe_code {
            let at = format!("{}:{}", u.file, u.line);
            s.push_str(&format!("    {at:width$}  {}\n", u.what));
        }
    }
    s
}

/// The listing for tools (`velt check --json`'s `native`).
pub fn json(packages: &[NativePackage]) -> Value {
    Value::Array(
        packages
            .iter()
            .map(|p| {
                json!({
                    "package": p.name,
                    "cLibraries": p.c_libraries.iter().map(|(name, system, checked, sources)| json!({
                        "name": name,
                        "system": system,
                        "checked": checked.word(),
                        "sources": sources,
                    })).collect::<Vec<_>>(),
                    "rustCrate": p.rust_crate,
                    "unsafe": p.unsafe_code.iter().map(|u| json!({
                        "file": u.file,
                        "line": u.line,
                        "what": u.what,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only what the check reports is "headers checked"; a list of headers alone isn't.
    #[test]
    fn a_library_is_checked_only_when_the_check_says_so() {
        let lib = |headers: &[&str], unchecked| vpm::manifest::CLibraryConfig {
            headers: headers.iter().map(|h| h.to_string()).collect(),
            unchecked,
            ..Default::default()
        };
        assert_eq!(Checked::of(&lib(&["<a.h>"], false)), Checked::Listed);
        assert_eq!(Checked::of(&lib(&[], true)), Checked::Unchecked);
        assert_eq!(Checked::of(&lib(&[], false)), Checked::NoHeaders);
        let mut graph = PackageGraph::default();
        graph.add("bind", Path::new("/p/bind"), Default::default());
        let package = |name: &str| NativePackage {
            name: name.into(),
            c_libraries: vec![
                ("own".into(), None, Checked::Listed, vec![]),
                ("other".into(), None, Checked::Listed, vec![]),
            ],
            rust_crate: false,
            unsafe_code: vec![],
        };
        let mut packages = vec![package("bind"), package("else")];
        let mut checked = crate::native::headers::Checked::new();
        checked
            .entry("/p/bind".into())
            .or_default()
            .insert("own".to_string(), Default::default());
        note_checked(&mut packages, Some(&graph), &checked);
        assert_eq!(packages[0].c_libraries[0].2, Checked::Headers);
        assert_eq!(packages[0].c_libraries[1].2, Checked::Listed);
        assert_eq!(packages[1].c_libraries[0].2, Checked::Listed);
    }

    #[test]
    fn the_listing_names_libraries_and_places() {
        let packages = vec![
            NativePackage {
                name: "sqlite".into(),
                c_libraries: vec![
                    (
                        "sqlite3".into(),
                        Some("sqlite3".into()),
                        Checked::Headers,
                        vec![],
                    ),
                    ("blas".into(), None, Checked::Unchecked, vec![]),
                    (
                        "tiny".into(),
                        None,
                        Checked::Headers,
                        vec!["native/tiny.c".into()],
                    ),
                ],
                rust_crate: false,
                unsafe_code: vec![
                    UnsafeCode {
                        file: "src/db.velt".into(),
                        line: 12,
                        what: "unsafe block".into(),
                    },
                    UnsafeCode {
                        file: "src/stmt.velt".into(),
                        line: 7,
                        what: "unsafe function `step`".into(),
                    },
                ],
            },
            NativePackage {
                name: "pg".into(),
                c_libraries: vec![],
                rust_crate: true,
                unsafe_code: vec![],
            },
        ];
        assert_eq!(
            render(&packages),
            "Native code the program allows (allowNative):\n  \
             sqlite: C libraries sqlite3 (sqlite3; headers checked), blas (none on this target; unchecked), tiny (built from native/tiny.c; headers checked); 2 places of unsafe code\n    \
             src/db.velt:12   unsafe block\n    \
             src/stmt.velt:7  unsafe function `step`\n  \
             pg: a Rust crate; no unsafe code\n"
        );
        assert_eq!(render(&[]), "");
        assert_eq!(json(&packages)[0]["unsafe"][1]["line"], 7);
        assert_eq!(json(&packages)[1]["rustCrate"], true);
        assert_eq!(
            json(&packages)[0]["cLibraries"][2]["sources"][0],
            "native/tiny.c"
        );
    }
}
