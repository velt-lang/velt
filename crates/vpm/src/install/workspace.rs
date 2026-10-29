//! Installing in a workspace ([`crate::workspace`]): one resolution of every member, in the
//! lockfile at the workspace root, whichever member asks; the graph is that member's.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::{build_graph, fetch, natives, settle_lock, to_lockfile, InstallOptions, Installed};
use crate::locations::Locations;
use crate::lockfile::{Lockfile, LOCK_FILE};
use crate::relpath;
use crate::resolve::{resolve_members, Resolution, ResolvedPackage};
use crate::workspace::Workspace;

/// Install the workspace `ws`. With `member` (a member's directory) the graph holds that member,
/// first, and the packages it uses; without one, every member and package.
pub fn install(
    ws: &Workspace,
    member: Option<&Path>,
    loc: &Locations,
    opts: InstallOptions,
) -> Result<Installed, String> {
    for m in ws.members.iter().filter(|m| m.dir != ws.root) {
        if crate::probe::is_file(&m.dir.join(LOCK_FILE)) {
            let rel = relpath::relative(&m.dir, &ws.root);
            return Err(format!(
                "{rel}/{LOCK_FILE}: a member of a workspace has no lockfile of its own; the workspace's is the {LOCK_FILE} beside velt.workspace.vlt (delete this one, or move it there when the workspace has none yet to keep its versions)"
            ));
        }
    }
    let mut loc = loc.clone();
    if loc.remote.is_none() {
        loc.remote = ws.file.registry.clone();
    }
    let existing = Lockfile::read(&ws.root)?;
    let prefer = if opts.update { None } else { existing.as_ref() };
    let manifests = ws.effective_manifests();
    let resolution = resolve_members(&ws.root, &manifests, &loc, prefer)?;
    let member_dirs: Vec<PathBuf> = ws.members.iter().map(|m| m.dir.clone()).collect();
    let lockfile = to_lockfile(&resolution, &ws.root, &member_dirs);
    let (lockfile, lock_changed) = settle_lock(
        existing.as_ref(),
        lockfile,
        ws.file.toolchain.as_deref(),
        &opts,
    )?;
    let dirs = fetch(&loc, resolution.packages.values())?;
    let (root, packages) = match member.and_then(|dir| ws.member_at(dir)) {
        Some(m) => {
            let deps = &resolution.packages[m.name()].dependencies;
            let used = used_by(&resolution, deps);
            let packages: Vec<&ResolvedPackage> = resolution
                .packages
                .values()
                .filter(|p| p.name != m.name() && used.contains(p.name.as_str()))
                .collect();
            let root = (&manifests[&m.dir], m.dir.as_path(), deps.as_slice());
            (Some(root), packages)
        }
        None => (None, resolution.packages.values().collect()),
    };
    let natives = natives(
        &loc,
        root.map(|(manifest, dir, _)| (dir, manifest)),
        packages.iter().copied(),
        (&lockfile, &dirs),
        &opts,
    )?;
    if lock_changed {
        lockfile.write(&ws.root)?;
    }
    let graph = build_graph(root, packages.iter().copied(), &dirs, natives);
    Ok(Installed {
        graph,
        lockfile,
        lock_changed,
        yanked: resolution.yanked.clone(),
    })
}

/// The names of the packages `roots` depend on, directly or not, in `resolution`.
fn used_by<'r>(resolution: &'r Resolution, roots: &'r [String]) -> BTreeSet<&'r str> {
    let mut used = BTreeSet::new();
    let mut todo: Vec<&str> = roots.iter().map(String::as_str).collect();
    while let Some(name) = todo.pop() {
        if used.insert(name) {
            if let Some(pkg) = resolution.packages.get(name) {
                todo.extend(pkg.dependencies.iter().map(String::as_str));
            }
        }
    }
    used
}
