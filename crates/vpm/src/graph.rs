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
    /// Package root directory (contains velt.toml).
    pub root: PathBuf,
    /// Dependency name → that dependency's package root.
    pub dependencies: BTreeMap<String, PathBuf>,
    /// The package's `[paths]` import aliases (`crate::paths`).
    pub paths: BTreeMap<String, String>,
    /// `root`, canonicalized, for matching importing files.
    key: PathBuf,
}

/// All packages of one build.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PackageGraph {
    packages: Vec<GraphPackage>,
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
            key,
        });
        self.packages.last_mut().expect("ICE: just pushed")
    }

    /// The module path (absolute, no `.vlt` extension) that a `[paths]` alias of the package
    /// containing `importer` maps `spec` to.
    pub fn path_alias(&self, importer: &Path, spec: &str) -> Option<PathBuf> {
        let package = self.package_of(importer)?;
        let module = crate::paths::resolve(&package.paths, spec)?;
        Some(package.root.join(module))
    }

    /// All packages, in insertion order (root package first when built by [`crate::install`]).
    pub fn packages(&self) -> &[GraphPackage] {
        &self.packages
    }

    /// The innermost package whose root contains `file`.
    pub fn package_of(&self, file: &Path) -> Option<&GraphPackage> {
        let file = canonical(file);
        self.packages
            .iter()
            .filter(|p| file.starts_with(&p.key))
            .max_by_key(|p| p.key.components().count())
    }

    /// Root directory of dependency `name` as seen from the package containing `importer`.
    pub fn dependency_root(&self, importer: &Path, name: &str) -> Result<PathBuf, String> {
        self.package_of(importer)
            .and_then(|p| p.dependencies.get(name))
            .cloned()
            .ok_or_else(|| not_a_dependency(name))
    }
}

/// The error for importing a package that is not declared in `[dependencies]`.
pub fn not_a_dependency(name: &str) -> String {
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
