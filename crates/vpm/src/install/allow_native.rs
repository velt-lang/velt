//! `allowNative` (docs/internals/design/c-ffi.md "The trust boundary"): native code runs with no
//! safety net, so a package with native code (C declarations, `native.c`, or a Rust crate) is
//! installed only when the root package lists it. A new dependency with native code, or one that
//! gains it in a patch release, is then a change to the root manifest, not a line nobody reads.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;

use crate::manifest::Manifest;
use crate::resolve::{ResolvedPackage, Source};

/// An error for each package of `packages` (in `dirs`) with native code that `root` (the root
/// package's manifest and its direct dependencies) doesn't list in `allowNative`, naming the
/// chain of dependencies that brings it in and the line to add.
pub(crate) fn check<'r>(
    root: (&Manifest, &[String]),
    packages: impl Iterator<Item = &'r ResolvedPackage> + Clone,
    dirs: &BTreeMap<String, PathBuf>,
) -> Result<(), String> {
    let (manifest, direct) = root;
    let mut refused = vec![];
    for p in packages.clone() {
        if manifest.allow_native.contains(&p.name) || p.name == manifest.package.name {
            continue;
        }
        // A git target with no `package.vlt` is npm packages: JavaScript, nothing to read here.
        if matches!(&p.source, Source::Git { npm, .. } if !npm.is_empty()) {
            continue;
        }
        // A package this can't read is refused, not passed: the check doesn't rely on another
        // having reported it.
        let Some(dir) = dirs.get(&p.name) else {
            return Err(format!(
                "package `{}` isn't installed, so it can't be checked for native code",
                p.name
            ));
        };
        let m = Manifest::from_dir(dir)
            .map_err(|e| format!("package `{}` can't be checked for native code: {e}", p.name))?;
        if m.has_native_code() {
            refused.push(p.name.clone());
        }
    }
    if refused.is_empty() {
        return Ok(());
    }
    let deps: BTreeMap<&str, &[String]> = packages
        .map(|p| (p.name.as_str(), p.dependencies.as_slice()))
        .collect();
    let mut allowed = manifest.allow_native.clone();
    allowed.extend(refused.iter().cloned());
    let list: Vec<String> = allowed.iter().map(|n| format!("{n:?}")).collect();
    let lines: Vec<String> = refused
        .iter()
        .map(|name| {
            let via = match chain(direct, &deps, name) {
                Some(path) if path.len() > 1 => format!(
                    " (through {})",
                    path.iter()
                        .map(|n| format!("`{n}`"))
                        .collect::<Vec<_>>()
                        .join(" → ")
                ),
                _ => String::new(),
            };
            format!("package `{name}`{via} has native code, which runs with no safety net")
        })
        .collect();
    Err(format!(
        "{}\nallow it in package.vlt, if you trust it: allowNative: [{}]",
        lines.join("\n"),
        list.join(", ")
    ))
}

/// The packages from one of `direct` to `target`, shortest first (breadth-first).
fn chain(direct: &[String], deps: &BTreeMap<&str, &[String]>, target: &str) -> Option<Vec<String>> {
    let mut queue: VecDeque<Vec<String>> = direct.iter().map(|d| vec![d.clone()]).collect();
    let mut seen: Vec<String> = direct.to_vec();
    while let Some(path) = queue.pop_front() {
        let last = path.last()?;
        if last == target {
            return Some(path);
        }
        for next in deps.get(last.as_str()).copied().unwrap_or(&[]) {
            if !seen.contains(next) {
                seen.push(next.clone());
                let mut longer = path.clone();
                longer.push(next.clone());
                queue.push_back(longer);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{GitRef, GitSpec};
    use crate::manifest::MANIFEST_FILE;

    fn manifest_text(name: &str, fields: &str) -> String {
        format!(
            "import type {{ Package }} from \"velt/package\";\nexport const pkg: Package = {{ name: \"{name}\", version: \"1.0.0\", {fields} }};\n"
        )
    }

    fn dir_with(text: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(MANIFEST_FILE), text).unwrap();
        dir
    }

    /// `check` for a root with `fields` and one dependency `lib`, whose directory is `lib_dir`.
    fn run(fields: &str, lib_dir: Option<&std::path::Path>) -> Result<(), String> {
        let source = Source::Path {
            dir: PathBuf::new(),
        };
        run_from(source, fields, lib_dir)
    }

    fn run_from(
        source: Source,
        fields: &str,
        lib_dir: Option<&std::path::Path>,
    ) -> Result<(), String> {
        let root = dir_with(&manifest_text("app", fields));
        let manifest = Manifest::from_dir(root.path()).unwrap();
        let lib = ResolvedPackage {
            name: "lib".to_string(),
            version: semver::Version::new(1, 0, 0),
            source,
            dependencies: vec![],
        };
        let dirs: BTreeMap<String, PathBuf> = lib_dir
            .map(|d| ("lib".to_string(), d.to_path_buf()))
            .into_iter()
            .collect();
        check((&manifest, &["lib".to_string()]), [lib].iter(), &dirs)
    }

    #[test]
    fn native_code_is_refused_until_allowed() {
        let plain = dir_with(&manifest_text("lib", ""));
        run("", Some(plain.path())).unwrap();
        let native = dir_with(&manifest_text("lib", "native: { c: {} }"));
        let refused = run("", Some(native.path())).unwrap_err();
        assert!(
            refused.contains("package `lib` has native code"),
            "{refused}"
        );
        run("allowNative: [\"lib\"]", Some(native.path())).unwrap();
    }

    #[test]
    fn a_package_that_can_t_be_read_is_refused() {
        let e = run("", None).unwrap_err();
        assert!(e.contains("package `lib` isn't installed"), "{e}");
        let unchecked = "package `lib` can't be checked for native code";
        let broken = dir_with("export const pkg = {");
        let e = run("", Some(broken.path())).unwrap_err();
        assert!(e.contains(unchecked), "{e}");
        let empty = tempfile::tempdir().unwrap();
        let e = run("", Some(empty.path())).unwrap_err();
        assert!(e.contains(unchecked), "{e}");
    }
    /// Only a git target that provides npm packages goes without a manifest.
    #[test]
    fn a_git_package_without_a_manifest_is_refused_unless_it_is_npm() {
        let git = |npm: BTreeMap<String, PathBuf>| Source::Git {
            spec: GitSpec {
                url: "https://example.com/lib.git".to_string(),
                reference: GitRef::Rev("abc".to_string()),
                subdir: None,
            },
            commit: "abc".to_string(),
            checksum: String::new(),
            dir: PathBuf::new(),
            npm,
        };
        let empty = tempfile::tempdir().unwrap();
        let e = run_from(git(BTreeMap::new()), "", Some(empty.path())).unwrap_err();
        assert!(e.contains("can't be checked for native code"), "{e}");
        let npm = BTreeMap::from([("lib".to_string(), empty.path().to_path_buf())]);
        run_from(git(npm), "", Some(empty.path())).unwrap();
    }
}
