//! `allowNative` (docs/internals/design/c-ffi.md "The trust boundary"): native code runs with no
//! safety net, so a package with native code (C declarations, `native.c`, or a Rust crate) is
//! installed only when the root package lists it. A new dependency with native code, or one that
//! gains it in a patch release, is then a change to the root manifest, not a line nobody reads.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;

use crate::manifest::Manifest;
use crate::resolve::ResolvedPackage;

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
        let Some(dir) = dirs.get(&p.name) else {
            continue;
        };
        let Ok(m) = Manifest::from_dir(dir) else {
            continue;
        };
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
