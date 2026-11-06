//! `allowNative` (docs/internals/design/c-ffi.md "The trust boundary"): native code runs with no
//! safety net, so a package with native code (C declarations, `native.c`, or a Rust crate) is
//! installed only when the root package lists it. A new dependency with native code, or one that
//! gains it in a patch release, is then a change to the root manifest, not a line nobody reads.
//!
//! An entry binds to a source (#2367, `crate::manifest::allow_native`), checked after resolution
//! so the source that won is the one checked: a bare name accepts the registry and local paths, a
//! `git` entry that repository, a `url` entry archives under its prefix. A git repository names
//! its packages whatever it likes, so a bare `"sqlite"` never allows a `sqlite` from git.
//! Every `path` source was written by a manifest the root controls (a git package's paths are
//! its repository, a registry package has none: `crate::resolve`), so a bare name covers them.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};

use crate::manifest::{AllowNative, AllowedFrom, Manifest};
use crate::resolve::{ResolvedPackage, Source};

/// Whether `a` and `b` are one directory.
fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Where `source` is, for messages.
fn describe_source(source: &Source) -> String {
    match source {
        Source::Registry { .. } => "the registry".to_string(),
        Source::Path { dir } => format!("the local path {}", dir.display()),
        Source::Git { spec, .. } => format!("the git repository {}", spec.url.escape_default()),
    }
}

/// Whether `entry` allows a package from `source`; an error when a URL can't be compared safely.
fn allows(entry: &AllowNative, source: &Source) -> Result<bool, String> {
    match (&entry.from, source) {
        (AllowedFrom::Root, Source::Registry { .. } | Source::Path { .. }) => Ok(true),
        (AllowedFrom::Git(url), Source::Git { spec, .. }) => {
            let allowed = crate::source_url::git_identity(url)?;
            Ok(allowed == crate::source_url::git_identity(&spec.url)?)
        }
        _ => Ok(false),
    }
}

/// The entry that would allow `name` from `source`, as package.velt writes it.
fn suggested(name: &str, source: &Source) -> AllowNative {
    match source {
        Source::Git { spec, .. } => AllowNative {
            name: name.to_string(),
            from: AllowedFrom::Git(spec.url.clone()),
        },
        Source::Registry { .. } | Source::Path { .. } => AllowNative::named(name),
    }
}

/// Check each package of `packages` (in `dirs`) with native code against the `allowNative` of
/// `root` (the root package's manifest, its directory and its direct dependencies): an error
/// naming each one it doesn't list, through the chain of dependencies that brings it in, with
/// the line to add; and each one it lists from another source, with both sources. A package that
/// depends back on the root is the root only if it is in the root's directory: anything can have
/// its name. Returns the names of the packages with native code.
pub(crate) fn check<'r>(
    root: (&Manifest, &Path, &[String]),
    packages: impl Iterator<Item = &'r ResolvedPackage> + Clone,
    dirs: &BTreeMap<String, PathBuf>,
) -> Result<Vec<String>, String> {
    let (manifest, root_dir, direct) = root;
    let mut native = vec![];
    // Each refused package and what its C libraries declared `unchecked` add to the message.
    let mut refused: Vec<(&ResolvedPackage, String)> = vec![];
    let mut elsewhere = vec![];
    for p in packages.clone() {
        // A git target with no `package.velt` is npm packages: JavaScript, nothing to read here.
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
        if same_dir(dir, root_dir) {
            continue;
        }
        let m = Manifest::from_dir(dir)
            .map_err(|e| format!("package `{}` can't be checked for native code: {e}", p.name))?;
        if !m.has_native_code() {
            continue;
        }
        native.push(p.name.clone());
        // A repository no entry can name (its URL can't be compared safely) gets no suggestion:
        // the entry it would suggest is refused when added.
        if let Source::Git { spec, .. } = &p.source {
            if let Err(e) = crate::source_url::git_identity(&spec.url) {
                elsewhere.push(format!(
                    "package `{}` has native code and comes from a git repository that allowNative can't name: {e}",
                    p.name
                ));
                continue;
            }
        }
        let entries: Vec<&AllowNative> = (manifest.allow_native.iter())
            .filter(|a| a.name == p.name)
            .collect();
        if entries.is_empty() {
            refused.push((p, unchecked_note(&m)));
            continue;
        }
        let mut allowed = false;
        for entry in &entries {
            allowed |= allows(entry, &p.source).map_err(|e| {
                format!(
                    "package `{}` can't be checked against allowNative: {e}",
                    p.name
                )
            })?;
        }
        if !allowed {
            let from: Vec<String> = entries.iter().map(|e| e.describe_from()).collect();
            elsewhere.push(format!(
                "package `{}` has native code and comes from {}, but allowNative allows it only from {}: if you trust this source, add {}{}",
                p.name,
                describe_source(&p.source),
                from.join(" or "),
                suggested(&p.name, &p.source).to_source(),
                unchecked_note(&m)
            ));
        }
    }
    native.sort();
    if refused.is_empty() && elsewhere.is_empty() {
        return Ok(native);
    }
    if refused.is_empty() {
        return Err(elsewhere.join("\n"));
    }
    let deps: BTreeMap<&str, &[String]> = packages
        .map(|p| (p.name.as_str(), p.dependencies.as_slice()))
        .collect();
    let mut allowed = manifest.allow_native.clone();
    allowed.extend(refused.iter().map(|(p, _)| suggested(&p.name, &p.source)));
    let list: Vec<String> = allowed.iter().map(AllowNative::to_source).collect();
    let mut lines: Vec<String> = refused
        .iter()
        .map(|(p, unchecked)| {
            let via = match chain(direct, &deps, &p.name) {
                Some(path) if path.len() > 1 => format!(
                    " (through {})",
                    path.iter()
                        .map(|n| format!("`{n}`"))
                        .collect::<Vec<_>>()
                        .join(" → ")
                ),
                _ => String::new(),
            };
            format!(
                "package `{}`{via} has native code, which runs with no safety net{unchecked}",
                p.name
            )
        })
        .collect();
    lines.extend(elsewhere);
    Err(format!(
        "{}\nallow it in package.velt, if you trust it: allowNative: [{}]",
        lines.join("\n"),
        list.join(", ")
    ))
}

/// What a refusal adds for `m`'s C libraries declared `unchecked: true` (no headers): empty when
/// there are none.
fn unchecked_note(m: &Manifest) -> String {
    let libs: Vec<String> = (m.c_libraries.iter().flatten())
        .filter(|(_, lib)| lib.unchecked)
        .map(|(name, _)| format!("`{name}`"))
        .collect();
    match libs.len() {
        0 => String::new(),
        1 => format!(
            "; its C library {} is not checked against headers (`unchecked: true`)",
            libs[0]
        ),
        _ => format!(
            "; its C libraries {} are not checked against headers (`unchecked: true`)",
            libs.join(", ")
        ),
    }
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
    mod sources;

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
        check(
            (&manifest, root.path(), &["lib".to_string()]),
            [lib].iter(),
            &dirs,
        )
        .map(drop)
    }

    /// A dependency with the root's name and native code, in the root's directory or not.
    #[test]
    fn a_package_named_like_the_root_needs_allowing_unless_it_is_the_root() {
        let root = dir_with(&manifest_text("app", "native: { c: {} }"));
        let elsewhere = dir_with(&manifest_text("app", "native: { c: {} }"));
        let manifest = Manifest::from_dir(root.path()).unwrap();
        let named_app = ResolvedPackage {
            name: "app".to_string(),
            version: semver::Version::new(1, 0, 0),
            source: Source::Path {
                dir: PathBuf::new(),
            },
            dependencies: vec![],
        };
        let run = |dir: &Path| {
            let dirs = BTreeMap::from([("app".to_string(), dir.to_path_buf())]);
            check(
                (&manifest, root.path(), &[]),
                [&named_app].into_iter(),
                &dirs,
            )
            .map(drop)
        };
        run(root.path()).unwrap();
        let e = run(elsewhere.path()).unwrap_err();
        assert!(e.contains("package `app` has native code"), "{e}");
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

    /// An install that moves a package with native code to another commit, version or checksum
    /// names it; one without native code, or an unchanged one, isn't named.
    #[test]
    fn changed_native_code_is_named() {
        use crate::lockfile::{LockedPackage, Lockfile};
        let locked = |name: &str, version: &str, source: &str| LockedPackage {
            name: name.into(),
            version: version.into(),
            source: source.into(),
            checksum: Some(format!("sha256:{version}")),
            dependencies: vec![],
            native: BTreeMap::new(),
            overrides: vec![],
        };
        let old = Lockfile::new(vec![
            locked("llama", "0.1.0", "git+https://h/o/llama#1111"),
            locked("sqlite", "1.0.0", "registry"),
            locked("plain", "1.0.0", "registry"),
            locked("same", "1.0.0", "registry"),
        ]);
        let new = Lockfile::new(vec![
            locked("llama", "0.1.0", "git+https://h/o/llama#2222"),
            locked("sqlite", "1.1.0", "registry"),
            locked("plain", "2.0.0", "registry"),
            locked("same", "1.0.0", "registry"),
        ]);
        let native = ["llama", "same", "sqlite"].map(String::from);
        let notes = crate::install::native_changes(Some(&old), &new, &native);
        assert_eq!(
            notes,
            [
                "native code of `llama` changes: 0.1.0 (git+https://h/o/llama#1111) → 0.1.0 (git+https://h/o/llama#2222)",
                "native code of `sqlite` changes: 1.0.0 (sha256:1.0.0) → 1.1.0 (sha256:1.1.0)",
            ]
        );
        assert!(crate::install::native_changes(None, &new, &native).is_empty());
    }
}
