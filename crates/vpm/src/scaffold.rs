//! `velt new` / `velt init`: write a package skeleton. The files come from the caller (the CLI's
//! templates); this module owns the manifest text, name validation and the overwrite rules.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::manifest::{
    is_valid_package_name, Manifest, Package, DEFAULT_ENTRY, LIB_ENTRY, MANIFEST_FILE,
};

const MAIN_TEMPLATE: &str = "function main() {\n  console.log(\"Hello, world!\");\n}\n";
const LIB_TEMPLATE: &str =
    "export function greet(name: string): string {\n  return `Hello, ${name}!`;\n}\n";

/// Lines every generated `.gitignore` has.
pub const GITIGNORE: &str = "target/\n";

/// What to do when a file to be written already exists (and overwriting was not forced).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IfExists {
    /// A conflict: nothing is written.
    Refuse,
    /// Leave the existing file alone (e.g. a project's own README).
    Keep,
    /// Append the lines the existing file lacks (`.gitignore`).
    AppendLines,
}

/// One file of a package skeleton.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScaffoldFile {
    /// Path relative to the package root, `/`-separated.
    pub path: String,
    /// File contents.
    pub contents: String,
    /// Behavior when the file already exists.
    pub if_exists: IfExists,
}

impl ScaffoldFile {
    /// A file that must not overwrite an existing one.
    pub fn new(path: &str, contents: String) -> ScaffoldFile {
        ScaffoldFile {
            path: path.to_string(),
            contents,
            if_exists: IfExists::Refuse,
        }
    }
}

/// What [`write_files`] did with each file (paths relative to the root).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Written {
    /// Files created or overwritten.
    pub created: Vec<String>,
    /// Existing files left as they were ([`IfExists::Keep`]).
    pub kept: Vec<String>,
    /// Existing files that got lines appended ([`IfExists::AppendLines`]).
    pub updated: Vec<String>,
}

/// `package.vlt` of a new package.
pub fn manifest_text(name: &str) -> String {
    manifest_text_with(name, None)
}

/// `package.vlt` of a new package with a `description` (a library's placeholder, say).
pub fn manifest_text_with(name: &str, description: Option<&str>) -> String {
    Manifest {
        registry: None,
        package: Package {
            name: name.to_string(),
            version: "0.1.0".into(),
            description: description.map(str::to_string),
            keywords: vec![],
            entry: DEFAULT_ENTRY.into(),
        },
        dependencies: BTreeMap::new(),
        paths: BTreeMap::new(),
        native: None,
        jsx: None,
    }
    .to_vlt()
}

/// `Err` with an actionable message unless `name` is a valid package name.
pub fn check_name(name: &str) -> Result<(), String> {
    if is_valid_package_name(name) {
        return Ok(());
    }
    Err(format!(
        "invalid package name `{name}` (use lowercase letters, digits, `-` and `_`, starting with a letter)"
    ))
}

/// Create package `<parent>/<name>`; `lib` selects `src/lib.vlt` instead of `src/main.vlt`.
/// Returns the new package root. Fails if the directory already exists.
pub fn new_package(parent: &Path, name: &str, lib: bool) -> Result<PathBuf, String> {
    let (entry, template) = if lib {
        (LIB_ENTRY, LIB_TEMPLATE)
    } else {
        (DEFAULT_ENTRY, MAIN_TEMPLATE)
    };
    let files = [
        ScaffoldFile::new(MANIFEST_FILE, manifest_text(name)),
        ScaffoldFile::new(entry, template.to_string()),
        ScaffoldFile::new(".gitignore", GITIGNORE.to_string()),
    ];
    create_package(parent, name, &files)
}

/// Create package `<parent>/<name>` from `files`. Fails if the directory already exists.
pub fn create_package(
    parent: &Path,
    name: &str,
    files: &[ScaffoldFile],
) -> Result<PathBuf, String> {
    check_name(name)?;
    let root = parent.join(name);
    if root.exists() {
        return Err(format!(
            "`{}` already exists (to turn an existing directory into a package, run `velt init` inside it)",
            root.display()
        ));
    }
    write_files(&root, files, false)?;
    Ok(root)
}

/// Write `files` under `root`. Unless `force`, any existing [`IfExists::Refuse`] file is a
/// conflict and nothing is written; the error lists every conflicting file. `force` overwrites
/// those; [`IfExists::Keep`] and [`IfExists::AppendLines`] files are never replaced.
pub fn write_files(root: &Path, files: &[ScaffoldFile], force: bool) -> Result<Written, String> {
    let conflicts: Vec<&str> = files
        .iter()
        .filter(|f| !force && f.if_exists == IfExists::Refuse && root.join(&f.path).exists())
        .map(|f| f.path.as_str())
        .collect();
    if !conflicts.is_empty() {
        return Err(format!(
            "refusing to overwrite existing files in `{}`: {} (pass `--force` to overwrite them)",
            root.display(),
            conflicts.join(", ")
        ));
    }
    let mut written = Written::default();
    for f in files {
        let path = root.join(&f.path);
        match (path.exists(), f.if_exists) {
            (true, IfExists::Keep) => written.kept.push(f.path.clone()),
            (true, IfExists::AppendLines) => {
                if append_missing_lines(&path, &f.contents)? {
                    written.updated.push(f.path.clone());
                } else {
                    written.kept.push(f.path.clone());
                }
            }
            _ => {
                write(&path, &f.contents)?;
                written.created.push(f.path.clone());
            }
        }
    }
    Ok(written)
}

/// Append the lines of `wanted` that `path` lacks; `false` if it already had them all.
fn append_missing_lines(path: &Path, wanted: &str) -> Result<bool, String> {
    let existing = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
    let have: Vec<&str> = existing.lines().map(str::trim).collect();
    let missing: Vec<&str> = wanted
        .lines()
        .filter(|l| !have.contains(&l.trim()))
        .collect();
    if missing.is_empty() {
        return Ok(false);
    }
    let mut text = existing.clone();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    for line in missing {
        text.push_str(line);
        text.push('\n');
    }
    write(path, &text)?;
    Ok(true)
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create `{}`: {e}", dir.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("cannot write `{}`: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_binary_and_library_packages() {
        let tmp = tempfile::tempdir().unwrap();
        let app = new_package(tmp.path(), "app", false).unwrap();
        let m = Manifest::from_dir(&app).unwrap();
        assert_eq!(
            (m.package.name.as_str(), m.package.version.as_str()),
            ("app", "0.1.0")
        );
        assert!(std::fs::read_to_string(app.join("src/main.vlt"))
            .unwrap()
            .contains("Hello, world!"));
        assert_eq!(
            std::fs::read_to_string(app.join(".gitignore")).unwrap(),
            "target/\n"
        );

        let lib = new_package(tmp.path(), "mylib", true).unwrap();
        assert!(lib.join("src/lib.vlt").is_file() && !lib.join("src/main.vlt").exists());

        assert!(new_package(tmp.path(), "app", false)
            .unwrap_err()
            .contains("already exists"));
        assert!(new_package(tmp.path(), "Bad", false)
            .unwrap_err()
            .contains("invalid package name"));
    }

    fn skeleton() -> Vec<ScaffoldFile> {
        vec![
            ScaffoldFile::new(MANIFEST_FILE, manifest_text("p")),
            ScaffoldFile::new("src/main.vlt", "function main() {}\n".into()),
            ScaffoldFile {
                if_exists: IfExists::Keep,
                ..ScaffoldFile::new("README.md", "# p\n".into())
            },
            ScaffoldFile {
                if_exists: IfExists::AppendLines,
                ..ScaffoldFile::new(".gitignore", GITIGNORE.into())
            },
        ]
    }

    #[test]
    fn write_files_refuses_conflicts_unless_forced() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.vlt"), "mine").unwrap();
        let err = write_files(root, &skeleton(), false).unwrap_err();
        assert!(
            err.contains("src/main.vlt") && err.contains("--force"),
            "{err}"
        );
        assert!(
            !root.join(MANIFEST_FILE).exists(),
            "nothing written on conflict"
        );

        let w = write_files(root, &skeleton(), true).unwrap();
        assert_eq!(w.created.len(), 4);
        assert_eq!(
            std::fs::read_to_string(root.join("src/main.vlt")).unwrap(),
            "function main() {}\n"
        );
    }

    #[test]
    fn write_files_keeps_readme_and_merges_gitignore() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("README.md"), "mine").unwrap();
        std::fs::write(root.join(".gitignore"), "node_modules/").unwrap();
        let w = write_files(root, &skeleton(), false).unwrap();
        assert_eq!(w.created, [MANIFEST_FILE, "src/main.vlt"]);
        assert_eq!(w.kept, ["README.md"]);
        assert_eq!(w.updated, [".gitignore"]);
        assert_eq!(
            std::fs::read_to_string(root.join("README.md")).unwrap(),
            "mine"
        );
        assert_eq!(
            std::fs::read_to_string(root.join(".gitignore")).unwrap(),
            "node_modules/\ntarget/\n"
        );
        // Running again: the .gitignore already has the line.
        std::fs::remove_file(root.join(MANIFEST_FILE)).unwrap();
        std::fs::remove_file(root.join("src/main.vlt")).unwrap();
        let w = write_files(root, &skeleton(), false).unwrap();
        assert_eq!(w.kept, ["README.md", ".gitignore"]);
    }
}
