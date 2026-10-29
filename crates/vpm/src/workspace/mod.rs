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

mod check;
#[cfg(test)]
mod tests;

/// File name of the workspace file at a workspace root.
pub const WORKSPACE_FILE: &str = "velt.workspace.vlt";

/// How member globs are walked: directories with a `package.vlt`, never entering build output.
const MEMBER_WALK: (&str, &[&str]) = (MANIFEST_FILE, &["target"]);

/// The decoded `velt.workspace.vlt` ([`crate::manifest::read::read_workspace`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceFile {
    /// `members`: globs relative to the workspace root, `!` in front to exclude.
    pub members: Vec<String>,
    /// Where each of `members` is written in the file (byte ranges).
    pub member_spans: Vec<std::ops::Range<u32>>,
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
    /// The text of its `package.vlt`, for messages that point into it.
    pub src: String,
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
    /// The text of `velt.workspace.vlt`, for messages that point into it.
    pub src: String,
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
        WorkspaceFile::read(path).map(|(file, _)| file)
    }

    /// [`WorkspaceFile::from_path`], with the file's text.
    fn read(path: &Path) -> Result<(WorkspaceFile, String), String> {
        let src = crate::probe::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        if src.len() > crate::manifest::read::MAX_BYTES {
            return Err(format!(
                "{}: the workspace file is larger than {} KiB",
                path.display(),
                crate::manifest::read::MAX_BYTES / 1024
            ));
        }
        let file = crate::manifest::read::read_workspace(FileId(0), &src)
            .map_err(|d| render(path, &src, &d))?;
        Ok((file, src))
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
    /// need all of them ([`check`]: every glob matches, names are distinct, members leave `velt`
    /// and `registry` to the workspace, depend on each other with `"workspace"` and not in a
    /// cycle). Each problem is reported where it is written.
    pub fn load(root: &Path) -> Result<Workspace, String> {
        let root = relpath::normalize(&relpath::absolute(root));
        let (file, src) = WorkspaceFile::read(&root.join(WORKSPACE_FILE))?;
        let mut report = check::Report::default();
        check::globs(&root, &file, &src, &mut report);
        let mut members = vec![];
        for dir in dirs_with(&root, &file.members, MEMBER_WALK, MAX_DIRS).dirs {
            let dir = relpath::normalize(&dir);
            let text = crate::probe::read_to_string(&dir.join(MANIFEST_FILE)).unwrap_or_default();
            match Manifest::from_dir(&dir) {
                Ok(manifest) => members.push(Member {
                    dir,
                    manifest,
                    src: text,
                }),
                Err(e) => report.push(e),
            }
        }
        members.sort_by(|a, b| a.dir.cmp(&b.dir));
        let ws = Workspace {
            root,
            file,
            src,
            members,
        };
        check::members(&ws, &mut report);
        report.finish().map(|()| ws)
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
