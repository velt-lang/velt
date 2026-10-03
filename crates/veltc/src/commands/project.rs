//! The package a build happens in: found by searching upward for `package.vlt`, with its
//! dependencies installed (lockfile honored/updated) so the loader can resolve package imports.

use std::path::{Path, PathBuf};

use vpm::{InstallOptions, Locations, Manifest, PackageGraph};

/// An installed package.
pub struct Project {
    /// Package root (directory of package.vlt), absolute.
    pub root: PathBuf,
    /// Its manifest.
    pub manifest: Manifest,
    /// Installed dependency graph (root package first).
    pub graph: PackageGraph,
}

impl Project {
    /// The package enclosing `start`, installed with the native libraries for `target`;
    /// `Ok(None)` if `start` is not inside a package.
    pub fn find(start: &Path, locked: bool, target: &str) -> Result<Option<Project>, String> {
        match vpm::manifest::find_package_root(start) {
            Some(root) => Project::open(
                &root,
                InstallOptions {
                    locked,
                    update: false,
                    target: Some(target.to_string()),
                },
            )
            .map(Some),
            None => Ok(None),
        }
    }

    /// The package enclosing the current directory, or an actionable error.
    pub fn current(opts: InstallOptions) -> Result<Project, String> {
        Project::open(&Project::current_root()?, opts)
    }

    /// Root of the package enclosing the current directory (not installed).
    pub fn current_root() -> Result<PathBuf, String> {
        let cwd = std::env::current_dir()
            .map_err(|e| format!("cannot read the current directory: {e}"))?;
        vpm::manifest::find_package_root(&cwd).ok_or_else(|| no_package_message(&cwd))
    }

    /// Install the package rooted at `root`.
    pub fn open(root: &Path, opts: InstallOptions) -> Result<Project, String> {
        let root = vpm::relpath::absolute(root);
        let manifest = Manifest::from_dir(&root)?;
        let installed = vpm::install(&root, &Locations::from_env()?, opts)?;
        super::package::warn_yanked(&installed);
        Ok(Project {
            root,
            manifest,
            graph: installed.graph,
        })
    }

    /// The runnable entry file (`package.entry`, default `src/main.vlt`).
    pub fn entry(&self) -> Result<PathBuf, String> {
        let entry = self.root.join(&self.manifest.package.entry);
        if entry.is_file() {
            return Ok(entry);
        }
        Err(missing_entry_message(&self.root, &self.manifest))
    }

    /// `<root>/target/velt`, where package builds put their outputs.
    pub fn target_dir(&self) -> PathBuf {
        self.root.join("target").join("velt")
    }
}

/// Why the package at `root` has nothing to build: it names the missing entry, and says the
/// package is a library only when the entry is the default (`src/main.vlt`) and `src/lib.vlt`
/// exists (a custom entry that is missing is a mistake in package.vlt, not a library).
fn missing_entry_message(root: &Path, manifest: &Manifest) -> String {
    let entry = &manifest.package.entry;
    let mut msg = format!(
        "package `{}` has no `{entry}` to build",
        manifest.package.name
    );
    let library = root.join(vpm::manifest::LIB_ENTRY).is_file();
    if entry == vpm::manifest::DEFAULT_ENTRY && library {
        msg.push_str(" (it is a library: import it from another package instead)");
    }
    msg
}

/// The error for a package command run outside any package: what is missing, and the two ways
/// forward (a single file, if there are source files here, or making a package).
fn no_package_message(cwd: &Path) -> String {
    let mut msg = format!(
        "no `package.vlt` in `{}` or any parent directory, so there is no package here",
        cwd.display()
    );
    let files = velt_files(cwd);
    if let Some(first) = files.first() {
        msg.push_str(&format!(
            "\n  hint: to build or run a single file, name it: `velt run {first}`"
        ));
        if files.len() > 1 {
            msg.push_str(&format!(" (files here: {})", files.join(", ")));
        }
    }
    msg.push_str(
        "\n  hint: to make this directory a package, run `velt init` (or `velt new <name>` for a new directory)",
    );
    msg
}

/// Names of the source files (`.vlt`, `.ts`, `.tsx`; not tests) directly in `dir`, sorted (at
/// most five).
fn velt_files(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| vpm::sources::is_source_name(n) && !super::test::discover::is_test_file(n))
        .collect();
    names.sort();
    names.truncate(5);
    names
}

/// Check that the input file of `velt build`/`run`/`dev` exists; if not, suggest what the user
/// probably meant (the same name with `.vlt`, a similarly named file, or the package inside a
/// directory).
pub fn check_input_file(file: &Path) -> Result<(), String> {
    if file.is_file() {
        return Ok(());
    }
    if file.is_dir() {
        let hint = if file.join(vpm::manifest::MANIFEST_FILE).is_file() {
            format!(
                "it is a package: run `velt build`/`velt run` without a file inside `{}`",
                file.display()
            )
        } else {
            "name a `.vlt`, `.ts` or `.tsx` file".to_string()
        };
        return Err(format!("`{}` is a directory ({hint})", file.display()));
    }
    let mut msg = format!("`{}` does not exist", file.display());
    let with_ext = file.with_extension("vlt");
    if file.extension().is_none() && with_ext.is_file() {
        msg.push_str(&format!("; did you mean `{}`?", with_ext.display()));
        return Err(msg);
    }
    let dir = file
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let files = velt_files(dir);
    let candidates: Vec<&str> = files.iter().map(String::as_str).collect();
    if let Some(similar) = crate::cli::suggest::closest(&name, &candidates) {
        msg.push_str(&format!(
            "; did you mean `{}`?",
            dir.join(similar).display()
        ));
    }
    Err(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_input_hints() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("hello.vlt"), "").unwrap();
        std::fs::create_dir(dir.join("pkg")).unwrap();
        std::fs::write(dir.join("pkg/package.vlt"), "").unwrap();
        assert!(check_input_file(&dir.join("hello.vlt")).is_ok());
        let err = check_input_file(&dir.join("hello")).unwrap_err();
        assert!(
            err.contains("did you mean") && err.contains("hello.vlt"),
            "{err}"
        );
        let err = check_input_file(&dir.join("helo.vlt")).unwrap_err();
        assert!(err.contains("does not exist; did you mean"), "{err}");
        let err = check_input_file(&dir.join("pkg")).unwrap_err();
        assert!(
            err.contains("is a directory") && err.contains("it is a package"),
            "{err}"
        );
        let err = check_input_file(&dir.join("zzz.vlt")).unwrap_err();
        assert!(err.ends_with("does not exist"), "{err}");
    }

    #[test]
    fn missing_entry_says_library_only_for_the_default_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = |entry: &str| {
            let src = format!(
                "export const pkg: Package = {{ name: \"util\", version: \"0.1.0\", entry: \"{entry}\" }};"
            );
            Manifest::parse(&src).unwrap()
        };
        let default = manifest(vpm::manifest::DEFAULT_ENTRY);
        let custom = manifest("src/app.vlt");
        let msg = missing_entry_message(tmp.path(), &default);
        assert_eq!(msg, "package `util` has no `src/main.vlt` to build");
        std::fs::create_dir(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join(vpm::manifest::LIB_ENTRY), "").unwrap();
        let msg = missing_entry_message(tmp.path(), &default);
        assert!(msg.ends_with("(it is a library: import it from another package instead)"));
        let msg = missing_entry_message(tmp.path(), &custom);
        assert_eq!(msg, "package `util` has no `src/app.vlt` to build");
    }

    #[test]
    fn no_package_hints() {
        let tmp = tempfile::tempdir().unwrap();
        let msg = no_package_message(tmp.path());
        assert!(
            msg.contains("no `package.vlt`") && msg.contains("velt init"),
            "{msg}"
        );
        assert!(!msg.contains("single file"));
        std::fs::write(tmp.path().join("app.vlt"), "").unwrap();
        std::fs::write(tmp.path().join("x.test.vlt"), "").unwrap();
        let msg = no_package_message(tmp.path());
        assert!(
            msg.contains("`velt run app.vlt`") && !msg.contains("files here"),
            "{msg}"
        );
    }
}
