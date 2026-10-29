//! Workspaces (docs/internals/design/workspaces.md): a `velt.workspace.vlt` at a repository's root
//! lists member packages by glob. The members share one resolution and one `velt.lock.json` at the
//! workspace root, one `velt` pin and one `registry`, and refer to each other, and to the
//! requirements the workspace writes for them, with `"workspace"`:
//!
//! ```ts ignore
//! import type { Workspace } from "velt:package";
//!
//! export const workspace: Workspace = {
//!   members: ["packages/*", "apps/*"],
//!   velt: "0.2",
//!   dependencies: { json: "1.2" },
//! };
//! ```
//!
//! Everything here reads through [`crate::probe`], so the build stamp sees a workspace file, a
//! member's manifest or a directory a glob listed change.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use velt_common::{Diagnostics, FileId, SourceMap};

use crate::manifest::{Dependency, DetailedDependency, Manifest, MANIFEST_FILE};
use crate::npm::workspace::{dirs_with, MAX_DIRS};
use crate::relpath;

#[cfg(test)]
mod tests;

/// File name of the workspace file at a workspace root.
pub const WORKSPACE_FILE: &str = "velt.workspace.vlt";

/// The decoded `velt.workspace.vlt` ([`crate::manifest::read::read_workspace`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceFile {
    /// `members`: globs relative to the workspace root, `!` in front to exclude.
    pub members: Vec<String>,
    /// `velt`: the toolchain requirement of every member.
    pub toolchain: Option<String>,
    /// `registry`: the registry of every member.
    pub registry: Option<String>,
    /// `dependencies`: the requirements a member's `"workspace"` refers to.
    pub dependencies: BTreeMap<String, Dependency>,
}

/// One member package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    /// Its root directory (absolute, normalized).
    pub dir: PathBuf,
    /// Its `package.vlt`, as written.
    pub manifest: Manifest,
}

impl Member {
    /// The member's name.
    pub fn name(&self) -> &str {
        &self.manifest.package.name
    }
}

/// A loaded and checked workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workspace {
    /// The directory holding `velt.workspace.vlt` (absolute, normalized), and the lockfile.
    pub root: PathBuf,
    pub file: WorkspaceFile,
    /// The members, sorted by directory.
    pub members: Vec<Member>,
}

/// The nearest directory at or above `start` (absolute) holding a `velt.workspace.vlt`.
pub fn find_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|d| crate::probe::is_file(&d.join(WORKSPACE_FILE)))
        .map(Path::to_path_buf)
}

impl WorkspaceFile {
    /// Read and validate the workspace file at `path`, never past
    /// [`crate::manifest::read::MAX_BYTES`].
    pub fn from_path(path: &Path) -> Result<WorkspaceFile, String> {
        let src = crate::probe::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        if src.len() > crate::manifest::read::MAX_BYTES {
            return Err(format!(
                "{}: the workspace file is larger than {} KiB",
                path.display(),
                crate::manifest::read::MAX_BYTES / 1024
            ));
        }
        crate::manifest::read::read_workspace(FileId(0), &src).map_err(|d| render(path, &src, &d))
    }
}

fn render(path: &Path, src: &str, diags: &Diagnostics) -> String {
    let mut sm = SourceMap::new();
    sm.add(path, src);
    let parts: Vec<String> = diags.iter().map(|d| d.render(&sm)).collect();
    parts.join("\n\n")
}

impl Workspace {
    /// The workspace enclosing `start` (absolute), if there is one.
    pub fn find(start: &Path) -> Result<Option<Workspace>, String> {
        find_root(start)
            .map(|root| Workspace::load(&root))
            .transpose()
    }

    /// Load the workspace rooted at `root`: its file, its members' manifests, and the checks that
    /// need all of them (every glob matches, names are distinct, members leave `velt` and
    /// `registry` to the workspace and depend on each other with `"workspace"`).
    pub fn load(root: &Path) -> Result<Workspace, String> {
        let root = relpath::normalize(&relpath::absolute(root));
        let file = WorkspaceFile::from_path(&root.join(WORKSPACE_FILE))?;
        let mut errors = vec![];
        for pattern in file.members.iter().filter(|p| !p.starts_with('!')) {
            let listing = dirs_with(
                &root,
                std::slice::from_ref(pattern),
                MANIFEST_FILE,
                MAX_DIRS,
            );
            if let Some(cut) = listing.cut {
                errors.push(format!(
                    "{WORKSPACE_FILE}: `{cut}` lists more than {MAX_DIRS} directories; narrow it"
                ));
            } else if listing.dirs.is_empty() {
                errors.push(format!(
                    "{WORKSPACE_FILE}: `{pattern}` matches no package (no directory it names has a {MANIFEST_FILE})"
                ));
            }
        }
        let mut members = vec![];
        for dir in dirs_with(&root, &file.members, MANIFEST_FILE, MAX_DIRS).dirs {
            let dir = relpath::normalize(&dir);
            match Manifest::from_dir(&dir) {
                Ok(manifest) => members.push(Member { dir, manifest }),
                Err(e) => errors.push(e),
            }
        }
        members.sort_by(|a, b| a.dir.cmp(&b.dir));
        let ws = Workspace {
            root,
            file,
            members,
        };
        errors.extend(ws.check());
        if errors.is_empty() {
            Ok(ws)
        } else {
            Err(errors.join("\n"))
        }
    }

    fn check(&self) -> Vec<String> {
        let mut errors = vec![];
        let mut names: BTreeMap<&str, &Member> = BTreeMap::new();
        for m in &self.members {
            if let Some(first) = names.insert(m.name(), m) {
                errors.push(format!(
                    "two members are named `{}`: {} and {}",
                    m.name(),
                    self.shown(&first.dir),
                    self.shown(&m.dir)
                ));
            }
        }
        for name in self.file.dependencies.keys() {
            if names.contains_key(name.as_str()) {
                errors.push(format!(
                    "{WORKSPACE_FILE}: `{name}` in `dependencies` is a member of this workspace; members depend on it with `{name}: \"workspace\"`"
                ));
            }
        }
        for m in &self.members {
            let file = format!("{}/{MANIFEST_FILE}", self.shown(&m.dir));
            let file = file.strip_prefix("./").unwrap_or(&file);
            for (set, field) in [
                (m.manifest.toolchain.is_some(), "velt"),
                (m.manifest.registry.is_some(), "registry"),
            ] {
                if set {
                    errors.push(format!(
                        "{file}: `{field}` is set by the workspace ({WORKSPACE_FILE}); remove it here"
                    ));
                }
            }
            for (name, dep) in &m.manifest.dependencies {
                if dep.is_workspace() {
                    if !names.contains_key(name.as_str())
                        && !self.file.dependencies.contains_key(name)
                    {
                        errors.push(self.unknown(file, name, &names));
                    }
                } else if names.contains_key(name.as_str()) {
                    errors.push(format!(
                        "{file}: `{name}` is a member of this workspace; write `{name}: \"workspace\"`"
                    ));
                }
            }
        }
        errors
    }

    fn unknown(&self, file: &str, name: &str, members: &BTreeMap<&str, &Member>) -> String {
        let known: Vec<&str> = members
            .keys()
            .copied()
            .chain(self.file.dependencies.keys().map(String::as_str))
            .collect();
        let max = name.chars().count() / 3;
        let close = known
            .iter()
            .find(|k| crate::manifest::read::edit_distance(k, name) <= max);
        let mut message = format!(
            "{file}: `{name}: \"workspace\"`, but no member and no workspace dependency is named `{name}`"
        );
        if let Some(close) = close {
            message.push_str(&format!(" (did you mean `{close}`?)"));
        }
        message
    }

    /// `dir` relative to the workspace root, for messages.
    fn shown(&self, dir: &Path) -> String {
        relpath::relative(dir, &self.root)
    }

    /// The member whose root is `dir`.
    pub fn member_at(&self, dir: &Path) -> Option<&Member> {
        let dir = relpath::normalize(&relpath::absolute(dir));
        self.members.iter().find(|m| m.dir == dir)
    }

    /// The member named `name`.
    pub fn member(&self, name: &str) -> Option<&Member> {
        self.members.iter().find(|m| m.name() == name)
    }

    /// `member`'s manifest as resolution and builds use it: `"workspace"` replaced by the member's
    /// directory or the workspace's requirement (paths made absolute), and the workspace's `velt`
    /// and `registry`. [`Workspace::load`] checked that every `"workspace"` has a meaning.
    pub fn effective_manifest(&self, member: &Member) -> Manifest {
        let mut manifest = member.manifest.clone();
        manifest.toolchain = self.file.toolchain.clone();
        manifest.registry = self.file.registry.clone();
        for (name, dep) in manifest.dependencies.iter_mut() {
            if !dep.is_workspace() {
                continue;
            }
            *dep = match self.member(name) {
                Some(target) => Dependency::Detailed(DetailedDependency {
                    version: None,
                    path: Some(target.dir.to_string_lossy().into_owned()),
                }),
                None => match &self.file.dependencies[name] {
                    Dependency::Detailed(d) if d.path.is_some() => {
                        let path = self.root.join(d.path.as_deref().unwrap_or_default());
                        Dependency::Detailed(DetailedDependency {
                            version: d.version.clone(),
                            path: Some(relpath::normalize(&path).to_string_lossy().into_owned()),
                        })
                    }
                    other => other.clone(),
                },
            };
        }
        manifest
    }

    /// Every member's [`Workspace::effective_manifest`], by member directory.
    pub fn effective_manifests(&self) -> BTreeMap<PathBuf, Manifest> {
        self.members
            .iter()
            .map(|m| (m.dir.clone(), self.effective_manifest(m)))
            .collect()
    }
}
