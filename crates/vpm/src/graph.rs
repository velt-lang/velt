//! The installed package graph handed to the compiler: for every package (root, path deps, cached
//! registry deps) its root directory and the directories of its direct dependencies. The module
//! loader asks it "which directory is dependency `name` of the package containing this file?".

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One package in the graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphPackage {
    /// Package name.
    pub name: String,
    /// Package root directory (contains package.vlt).
    pub root: PathBuf,
    /// Dependency name → that dependency's package root.
    pub dependencies: BTreeMap<String, PathBuf>,
    /// The package's `paths` import aliases (`crate::paths`).
    pub paths: BTreeMap<String, String>,
    /// The package's conditional imports (`crate::imports`).
    pub imports: crate::imports::ImportMap,
    /// The JavaScript of the host modules the package provides (`crate::host_modules`).
    pub host_modules: crate::host_modules::HostModules,
    /// The package's `host` object: it is a host package (`crate::host`).
    pub host: Option<crate::host::HostConfig>,
    /// The package's native library for the build target (packages with native code, when the
    /// install had a target).
    pub native: Option<crate::native::NativeLib>,
    /// Where the package's C libraries built from `sources` come from for the build target
    /// (when the install had one).
    pub c_bundle: Option<CBundle>,
    /// The package's C libraries (`native.c`), by the name its `declare c("<library>")`
    /// functions give.
    pub c_libraries: BTreeMap<String, crate::manifest::CLibraryConfig>,
    /// The package has native code (`crate::manifest::Manifest::has_native_code`): only such a
    /// package may declare C functions.
    pub has_native_code: bool,
    /// Fetched from a registry or a git repository, not the program's own or a path dependency:
    /// its C declarations were checked against its headers where it was published.
    pub fetched: bool,
    /// Fetched from a git repository: the commit's whole tree is here, its C headers too, where
    /// a registry archive doesn't ship them.
    pub checkout: bool,
    /// The package's `jsx.importSource`, as written.
    pub jsx_import_source: Option<String>,
    /// The package's `define` (name → literal source); the root package's apply to the build.
    pub define: BTreeMap<String, String>,
    /// npm packages its git dependencies provide (`crate::npm::provided`): package name → its
    /// directory in the checkout. Its imports of them resolve there, before `node_modules`.
    pub npm: BTreeMap<String, PathBuf>,
    /// `root`, canonicalized, for matching importing files.
    key: PathBuf,
}

/// Where a package's C libraries built from `sources` come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CBundle {
    /// A published bundle, verified against the lockfile.
    Prebuilt(Box<crate::native::NativeLib>),
    /// Built from the package's sources into this directory when a program links them: a check,
    /// an editor or a program that doesn't call them never runs zig.
    FromSource(PathBuf),
}

/// All packages of one build.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PackageGraph {
    packages: Vec<GraphPackage>,
    /// The root package's canonical directory and its `allowNative`: the packages whose native
    /// code the program allows (`None`: a graph without a root package, such as a workspace's).
    /// The directory, as a root's name is no secret: anything can be named like it.
    allow_native: Option<(PathBuf, Vec<String>)>,
}

/// Canonical form of `p`; for a path that does not exist (yet), its deepest existing ancestor is
/// canonicalized so the result still compares with canonical roots (`\\?\` prefixes on Windows).
fn canonical(p: &Path) -> PathBuf {
    let abs = crate::relpath::absolute(p);
    for ancestor in abs.ancestors() {
        if let Ok(base) = std::fs::canonicalize(ancestor) {
            let rest = abs
                .strip_prefix(ancestor)
                .expect("ICE: ancestor is a prefix");
            return if rest.as_os_str().is_empty() {
                base
            } else {
                base.join(rest)
            };
        }
    }
    abs
}

/// Whether `file`, under the package root `root` (both canonical), is in another package below
/// it: under a `node_modules`, or under a directory with its own `package.json` or package.vlt.
/// A directory with a package.vlt that is a package of the graph is the innermost root, so it
/// never comes up here.
fn past_package_boundary(root: &Path, file: &Path) -> bool {
    let Ok(below) = file.strip_prefix(root) else {
        return true;
    };
    let mut dir = root.to_path_buf();
    for part in below.parent().into_iter().flatten() {
        dir.push(part);
        if part == "node_modules"
            || dir.join("package.json").is_file()
            || dir.join(crate::manifest::MANIFEST_FILE).is_file()
        {
            return true;
        }
    }
    false
}

impl PackageGraph {
    /// Add a package (roots are matched against importing files by longest prefix); returns it
    /// so the caller can set its [`GraphPackage::paths`].
    pub fn add(
        &mut self,
        name: &str,
        root: &Path,
        dependencies: BTreeMap<String, PathBuf>,
    ) -> &mut GraphPackage {
        let key = canonical(root);
        self.packages.push(GraphPackage {
            name: name.to_string(),
            root: root.to_path_buf(),
            dependencies,
            paths: BTreeMap::new(),
            imports: BTreeMap::new(),
            host_modules: BTreeMap::new(),
            host: None,
            native: None,
            c_bundle: None,
            c_libraries: BTreeMap::new(),
            has_native_code: false,
            fetched: false,
            checkout: false,
            jsx_import_source: None,
            define: BTreeMap::new(),
            npm: BTreeMap::new(),
            key,
        });
        self.packages.last_mut().expect("ICE: just pushed")
    }

    /// Record the root package's directory and its `allowNative` list.
    pub fn set_allow_native(&mut self, root: &Path, allowed: &[String]) {
        self.allow_native = Some((canonical(root), allowed.to_vec()));
    }

    /// Whether the program allows `package`'s native code: the root package's own (the package
    /// in the root's directory, whatever others are called), and those the root lists in
    /// `allowNative`. A graph without a root allows all (its members are checked when each is
    /// built).
    pub fn allows_native(&self, package: &GraphPackage) -> bool {
        match &self.allow_native {
            Some((root, allowed)) => package.key == *root || allowed.contains(&package.name),
            None => true,
        }
    }

    /// The package added last.
    pub(crate) fn last_mut(&mut self) -> &mut GraphPackage {
        self.packages.last_mut().expect("ICE: no package added yet")
    }

    /// The module path (absolute, no `.vlt` extension) that a `paths` alias of the package
    /// containing `importer` maps `spec` to.
    pub fn path_alias(&self, importer: &Path, spec: &str) -> Option<PathBuf> {
        let package = self.enclosing_package(importer)?;
        let module = crate::paths::resolve(&package.paths, spec)?;
        Some(package.root.join(module))
    }

    /// The package containing `importer` (the innermost one): its name, root and conditional
    /// imports (`crate::imports`, empty when it has none).
    pub fn imports_of(&self, importer: &Path) -> Option<(&str, &Path, &crate::imports::ImportMap)> {
        let package = self.enclosing_package(importer)?;
        Some((&package.name, &package.root, &package.imports))
    }

    /// The `jsx.importSource` of the package containing `importer`, as a specifier `importer`
    /// can import from: a relative source (`./ui`) is relative to the package root, so it is
    /// re-expressed relative to `importer`'s directory.
    pub fn jsx_import_source(&self, importer: &Path) -> Option<String> {
        let package = self.enclosing_package(importer)?;
        let source = package.jsx_import_source.as_ref()?;
        if !source.starts_with("./") && !source.starts_with("../") {
            return Some(source.clone());
        }
        let dir = canonical(importer).parent()?.to_path_buf();
        let rel = crate::relpath::relative(&package.key.join(source), &dir);
        Some(if rel.starts_with("..") {
            rel
        } else {
            format!("./{rel}")
        })
    }

    /// The build-time constants of the build: the root package's (the first added) `define`.
    pub fn defines(&self) -> BTreeMap<String, String> {
        self.packages
            .first()
            .map(|p| p.define.clone())
            .unwrap_or_default()
    }

    /// All packages, in insertion order (root package first when built by [`crate::install`]).
    pub fn packages(&self) -> &[GraphPackage] {
        &self.packages
    }

    /// The packages with a native library, in insertion order.
    pub fn natives(&self) -> impl Iterator<Item = (&GraphPackage, &crate::native::NativeLib)> {
        self.packages
            .iter()
            .filter_map(|p| p.native.as_ref().map(|n| (p, n)))
    }

    /// The package `file` is code of, which decides what the file may do (native code, `unsafe`
    /// code, the raw FFI API): the innermost package whose root contains it, unless the file is
    /// in another package below that root. An npm package under a package's directory isn't in
    /// the graph, and takes nothing from the package around it.
    pub fn package_of(&self, file: &Path) -> Option<&GraphPackage> {
        let file = canonical(file);
        self.innermost(&file)
            .filter(|p| !past_package_boundary(&p.key, &file))
    }

    /// The innermost package whose root contains `file`, npm packages below it included: where
    /// an import in the file resolves from, and which package an editor shows it in. For what
    /// the file may do, use [`Self::package_of`].
    pub fn enclosing_package(&self, file: &Path) -> Option<&GraphPackage> {
        self.innermost(&canonical(file))
    }

    fn innermost(&self, canonical_file: &Path) -> Option<&GraphPackage> {
        self.packages
            .iter()
            .filter(|p| canonical_file.starts_with(&p.key))
            .max_by_key(|p| p.key.components().count())
    }

    /// The directory of npm package `name` that a git dependency of the package containing
    /// `importer` provides, if one does.
    pub fn npm_package(&self, importer: &Path, name: &str) -> Option<PathBuf> {
        self.enclosing_package(importer)?.npm.get(name).cloned()
    }

    /// Root directory of dependency `name` as seen from the package containing `importer`.
    pub fn dependency_root(&self, importer: &Path, name: &str) -> Result<PathBuf, String> {
        self.enclosing_package(importer)
            .and_then(|p| p.dependencies.get(name))
            .cloned()
            .ok_or_else(|| not_a_dependency(name))
    }
}

/// The error for importing a package that is not declared in `dependencies`.
pub fn not_a_dependency(name: &str) -> String {
    if crate::manifest::split_scoped(name).is_some() {
        return format!(
            "package `{name}` is not a dependency, and no npm package of that name was found (add it to `dependencies` with a `path`, or install it into `node_modules`)"
        );
    }
    format!("package `{name}` is not a dependency (add it with `velt add {name}`)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_dependencies_of_innermost_package() {
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("app");
        let nested = app.join("libs/inner");
        std::fs::create_dir_all(nested.join("src")).unwrap();
        std::fs::create_dir_all(app.join("src")).unwrap();
        let mut g = PackageGraph::default();
        g.add(
            "app",
            &app,
            BTreeMap::from([("inner".to_string(), nested.clone())]),
        );
        g.add("inner", &nested, BTreeMap::new());

        assert_eq!(
            g.dependency_root(&app.join("src/main.vlt"), "inner")
                .unwrap(),
            nested
        );
        let err = g
            .dependency_root(&nested.join("src/lib.vlt"), "inner")
            .unwrap_err();
        assert_eq!(
            err,
            "package `inner` is not a dependency (add it with `velt add inner`)"
        );
        assert!(g
            .dependency_root(&tmp.path().join("elsewhere.vlt"), "inner")
            .is_err());
        assert_eq!(
            g.package_of(&nested.join("src/lib.vlt")).unwrap().name,
            "inner"
        );
    }

    /// The three shapes of an npm package below a package's root, and what stays the package's.
    #[test]
    fn a_package_s_code_stops_at_an_npm_package() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("good");
        let write = |rel: &str| {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "").unwrap();
            path
        };
        write("package.json");
        let own = write("src/lib.vlt");
        let installed = write("node_modules/x/src/index.ts");
        let bare = write("node_modules/y.ts");
        write("js/package.json");
        let beside = write("js/src/index.ts");
        write("both/package.json");
        write("both/package.vlt");
        let both = write("both/index.ts");
        write("sub/package.vlt");
        let other = write("sub/index.vlt");
        write("listed/package.vlt");
        let listed = write("listed/src/lib.vlt");
        let mut graph = PackageGraph::default();
        graph.add("good", &root, BTreeMap::new());
        graph.add("listed", &root.join("listed"), BTreeMap::new());
        assert_eq!(graph.package_of(&own).unwrap().name, "good");
        assert_eq!(graph.package_of(&listed).unwrap().name, "listed");
        for file in [&installed, &bare, &beside, &both, &other] {
            assert!(graph.package_of(file).is_none(), "{}", file.display());
            // Imports in those files keep resolving from the package around them.
            assert_eq!(graph.enclosing_package(file).unwrap().name, "good");
        }
    }

    /// A root installed under a `node_modules` is no npm package: only what is below it counts.
    #[test]
    fn a_boundary_above_a_package_s_root_doesn_t_count() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("node_modules/good");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        let mut graph = PackageGraph::default();
        graph.add("good", &root, BTreeMap::new());
        assert_eq!(
            graph.package_of(&root.join("src/lib.vlt")).unwrap().name,
            "good"
        );
    }

    /// The root is its directory: a package in another directory with the root's name gets
    /// nothing from it.
    #[test]
    fn a_package_named_like_the_root_isn_t_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let (app, evil, listed) = (
            dir.path().join("app"),
            dir.path().join("evil"),
            dir.path().join("listed"),
        );
        let mut graph = PackageGraph::default();
        graph.add("app", &app, BTreeMap::new());
        graph.set_allow_native(&app, &["listed".to_string()]);
        graph.add("app", &evil, BTreeMap::new());
        graph.add("listed", &listed, BTreeMap::new());
        graph.add("other", &dir.path().join("other"), BTreeMap::new());
        let allowed: Vec<bool> = graph
            .packages()
            .iter()
            .map(|p| graph.allows_native(p))
            .collect();
        assert_eq!(allowed, [true, false, true, false]);
    }

    #[test]
    fn jsx_import_source_relative_to_the_package_root() {
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("app");
        std::fs::create_dir_all(app.join("src/pages")).unwrap();
        let mut g = PackageGraph::default();
        g.add("app", &app, BTreeMap::new()).jsx_import_source = Some("./ui".into());
        assert_eq!(
            g.jsx_import_source(&app.join("main.vlt")).as_deref(),
            Some("./ui")
        );
        assert_eq!(
            g.jsx_import_source(&app.join("src/pages/home.vlt"))
                .as_deref(),
            Some("../../ui")
        );
        g.add("lib", &tmp.path().join("lib"), BTreeMap::new())
            .jsx_import_source = Some("sigx".into());
        assert_eq!(
            g.jsx_import_source(&tmp.path().join("lib/src/lib.vlt"))
                .as_deref(),
            Some("sigx")
        );
        assert_eq!(g.jsx_import_source(&tmp.path().join("x.vlt")), None);
    }

    #[test]
    fn path_aliases_apply_to_their_own_package() {
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("app");
        std::fs::create_dir_all(app.join("src")).unwrap();
        let mut g = PackageGraph::default();
        g.add("app", &app, BTreeMap::new()).paths =
            BTreeMap::from([("@app/*".to_string(), "src/*".to_string())]);
        assert_eq!(
            g.path_alias(&app.join("src/main.vlt"), "@app/util/strings"),
            Some(app.join("src/util/strings"))
        );
        assert_eq!(g.path_alias(&app.join("src/main.vlt"), "@other/x"), None);
        assert_eq!(
            g.path_alias(&tmp.path().join("elsewhere.vlt"), "@app/x"),
            None
        );
    }
}
