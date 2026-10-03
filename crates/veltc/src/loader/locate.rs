//! From a classified specifier to a file on disk and its canonical module path.
//!
//! Every loaded module remembers its [`Origin`] (the program's root directory, the std root, or a
//! package's `src/`), which decides the canonical path of files it reaches through relative imports:
//! `"util"` / `"lib/math"` under the root, `"std/x"` under std, `"pkg"` / `"pkg/sub"` in a package.

use std::path::{Path, PathBuf};

use super::spec::ModuleRef;

/// Answers "where is dependency `name` of the package containing `importer`?" (implemented by
/// [`vpm::PackageGraph`]; tests may use a map).
pub trait PackageResolver {
    /// Root directory of dependency `name`, or an error message (`not a dependency`).
    fn dependency_root(&self, importer: &Path, name: &str) -> Result<PathBuf, String>;

    /// The module path (no source extension) a `paths` alias of the package containing
    /// `importer` maps `spec` to, if one matches.
    fn path_alias(&self, importer: &Path, spec: &str) -> Option<PathBuf> {
        let _ = (importer, spec);
        None
    }

    /// The `jsx.importSource` of the package containing `importer`, as a specifier
    /// `importer` can import from.
    fn jsx_import_source(&self, importer: &Path) -> Option<String> {
        let _ = importer;
        None
    }
}

impl PackageResolver for vpm::PackageGraph {
    fn dependency_root(&self, importer: &Path, name: &str) -> Result<PathBuf, String> {
        vpm::PackageGraph::dependency_root(self, importer, name)
    }

    fn path_alias(&self, importer: &Path, spec: &str) -> Option<PathBuf> {
        vpm::PackageGraph::path_alias(self, importer, spec)
    }

    fn jsx_import_source(&self, importer: &Path) -> Option<String> {
        vpm::PackageGraph::jsx_import_source(self, importer)
    }
}

/// The tree a module belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// The user's program; canonical paths are relative to the root file's directory.
    Root(PathBuf),
    /// The standard library rooted here.
    Std(PathBuf),
    /// Package `name` whose modules live in `src`.
    Package {
        /// Dependency name (prefix of canonical paths).
        name: String,
        /// The package's `src/` directory.
        src: PathBuf,
    },
}

impl Origin {
    /// Canonical module path of `file` (a source file inside this origin): its path relative to
    /// the origin, without the source extension (`.vlt`, `.ts`, `.tsx`) or a final `/index`.
    pub fn canonical(&self, file: &Path) -> String {
        let (base, prefix) = match self {
            Origin::Root(dir) => (dir, None),
            Origin::Std(root) => (root, Some("std")),
            Origin::Package { name, src } => (src, Some(name.as_str())),
        };
        let rel = vpm::relpath::relative(file, base);
        let rel = module_path(&rel);
        match (prefix, self) {
            (Some(name), Origin::Package { .. }) if rel == "lib" => name.to_string(),
            (Some(p), _) => format!("{p}/{rel}"),
            (None, _) => rel.to_string(),
        }
    }
}

/// `rel` (a `/`-separated file path) as a module path: without its source extension and a final
/// `/index` (`shapes/index.ts` → `shapes`).
pub fn module_path(rel: &str) -> &str {
    let rel = vpm::sources::strip_source_extension(rel).unwrap_or(rel);
    rel.strip_suffix("/index").unwrap_or(rel)
}

/// A module to load: the files to try, in groups (the first group with an existing file wins;
/// two existing files in one group make the import ambiguous), plus its canonical path and
/// origin.
pub struct Target {
    pub candidates: Vec<Vec<PathBuf>>,
    pub canonical: Option<String>,
    pub origin: Origin,
}

/// Why a module could not be located: message plus notes.
pub type LocateError = (String, Vec<String>);

/// Where `module` (imported by `importer`, which has origin `from`) lives.
pub fn target(
    module: ModuleRef,
    importer: &Path,
    from: &Origin,
    std_root: Option<&Path>,
    packages: Option<&dyn PackageResolver>,
) -> Result<Target, LocateError> {
    match module {
        ModuleRef::Relative { file } if file.to_str().is_some_and(|f| f.ends_with(".d.ts")) => {
            let note = "Velt has no declaration files: write the declarations in a `.ts` (or \
                        `.vlt`) module and import that"
                .to_string();
            Err((
                "declaration files (`.d.ts`) are not modules".to_string(),
                vec![note],
            ))
        }
        ModuleRef::Relative { file } => Ok(Target {
            candidates: relative_files(file),
            canonical: None,
            origin: from.clone(),
        }),
        ModuleRef::Std { rel } => {
            let Some(root) = std_root else {
                let note = "the standard library was not found (set VELT_STD to its directory)"
                    .to_string();
                return Err((format!("cannot find module `velt:{rel}`"), vec![note]));
            };
            Ok(Target {
                candidates: module_files(root, &rel),
                canonical: Some(format!("std/{rel}")),
                origin: Origin::Std(root.to_path_buf()),
            })
        }
        ModuleRef::Package { name, sub } => {
            let root = match packages {
                Some(p) => p
                    .dependency_root(importer, &name)
                    .map_err(|e| (e, vec![]))?,
                None => return Err((vpm::graph::not_a_dependency(&name), vec![])),
            };
            let src = root.join(vpm::manifest::SRC_DIR);
            let (candidates, canonical) = match &sub {
                None => (
                    vec![vec![root.join(vpm::manifest::LIB_ENTRY)]],
                    name.clone(),
                ),
                Some(s) => (module_files(&src, s), format!("{name}/{s}")),
            };
            Ok(Target {
                candidates,
                canonical: Some(canonical),
                origin: Origin::Package { name, src },
            })
        }
    }
}

/// `<dir>/<rel>.vlt`, then the folder module `<dir>/<rel>/index.vlt` (standard library and
/// package modules are `.vlt` files).
fn module_files(dir: &Path, rel: &str) -> Vec<Vec<PathBuf>> {
    vec![
        vec![dir.join(format!("{rel}.vlt"))],
        vec![dir.join(rel).join("index.vlt")],
    ]
}

/// The files a relative import of `file` (the specifier's path, extension as written) may name:
/// - with a source extension (`./x.ts`, `./x.tsx`, `./x.vlt`), exactly that file;
/// - with `.js` or `.jsx`, the TypeScript file it is compiled to, as TypeScript resolves it
///   (`./x.js` → `x.ts` or `x.tsx`, `./x.jsx` → `x.tsx`);
/// - otherwise `x.vlt`, `x.ts` or `x.tsx`, then the folder module `x/index.vlt`, `x/index.ts`
///   or `x/index.tsx` (never a declaration file: `./types.d` does not name `types.d.ts`).
fn relative_files(file: PathBuf) -> Vec<Vec<PathBuf>> {
    let Some(name) = file.to_str() else {
        return vec![vec![file]];
    };
    if vpm::sources::is_source_name(name) {
        return vec![vec![file]];
    }
    if let Some(stem) = name.strip_suffix(".js") {
        return vec![vec![
            format!("{stem}.ts").into(),
            format!("{stem}.tsx").into(),
        ]];
    }
    if let Some(stem) = name.strip_suffix(".jsx") {
        return vec![vec![format!("{stem}.tsx").into()]];
    }
    let with_extensions = |base: &str| {
        vpm::sources::SOURCE_EXTENSIONS
            .iter()
            .map(|ext| format!("{base}.{ext}"))
            // `./types.d` must not name `types.d.ts`, a declaration file.
            .filter(|f| vpm::sources::is_source_name(f))
            .map(PathBuf::from)
            .collect()
    };
    let index = file.join("index");
    let index = index.to_str().unwrap_or(name);
    vec![with_extensions(name), with_extensions(index)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_paths_per_origin() {
        let root = Origin::Root(PathBuf::from("/p"));
        assert_eq!(root.canonical(Path::new("/p/util.vlt")), "util");
        assert_eq!(root.canonical(Path::new("/p/lib/math.vlt")), "lib/math");
        assert_eq!(root.canonical(Path::new("/q/x.vlt")), "../q/x");
        let std = Origin::Std(PathBuf::from("/std"));
        assert_eq!(std.canonical(Path::new("/std/fs.vlt")), "std/fs");
        assert_eq!(std.canonical(Path::new("/std/net/index.vlt")), "std/net");
        assert_eq!(root.canonical(Path::new("/p/shapes/index.vlt")), "shapes");
        let pkg = Origin::Package {
            name: "json".into(),
            src: PathBuf::from("/c/json/src"),
        };
        assert_eq!(pkg.canonical(Path::new("/c/json/src/lib.vlt")), "json");
        assert_eq!(
            pkg.canonical(Path::new("/c/json/src/parse.vlt")),
            "json/parse"
        );
    }

    #[test]
    fn package_without_graph_is_not_a_dependency() {
        let module = ModuleRef::Package {
            name: "json".into(),
            sub: None,
        };
        let err = target(
            module,
            Path::new("main.vlt"),
            &Origin::Root(PathBuf::new()),
            None,
            None,
        )
        .err()
        .unwrap();
        assert_eq!(
            err.0,
            "package `json` is not a dependency (add it with `velt add json`)"
        );
    }
}
