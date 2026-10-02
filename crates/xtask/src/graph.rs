//! The workspace's crates and the dependencies between them, read from `crates/*/Cargo.toml`
//! (normal, dev and build dependencies alike: a crate's tests are affected by a change to
//! anything they build against).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Debug, Default)]
pub struct Graph {
    /// Directory name under `crates/` → package name.
    pub dirs: BTreeMap<String, String>,
    /// Package → the workspace packages it depends on.
    pub deps: BTreeMap<String, BTreeSet<String>>,
}

impl Graph {
    pub fn load(root: &Path) -> Result<Graph, String> {
        let mut manifests = vec![];
        let entries =
            std::fs::read_dir(root.join("crates")).map_err(|e| format!("crates/: {e}"))?;
        for entry in entries.flatten() {
            let manifest = entry.path().join("Cargo.toml");
            if let Ok(text) = std::fs::read_to_string(&manifest) {
                let dir = entry.file_name().to_string_lossy().into_owned();
                manifests.push((dir, text));
            }
        }
        Ok(Graph::from_manifests(&manifests))
    }

    /// `(directory under crates/, Cargo.toml text)` of every crate.
    pub fn from_manifests(manifests: &[(String, String)]) -> Graph {
        let mut graph = Graph::default();
        for (dir, text) in manifests {
            if let Some(name) = package_name(text) {
                graph.dirs.insert(dir.clone(), name);
            }
        }
        let names: BTreeSet<String> = graph.dirs.values().cloned().collect();
        for (dir, text) in manifests {
            let Some(name) = graph.dirs.get(dir).cloned() else {
                continue;
            };
            let deps = dependency_names(text)
                .into_iter()
                .filter(|d| names.contains(d) && *d != name)
                .collect();
            graph.deps.insert(name, deps);
        }
        graph
    }

    /// `changed` and every package that depends on one of them, directly or not.
    pub fn with_dependents(&self, changed: &BTreeSet<String>) -> BTreeSet<String> {
        let mut out = changed.clone();
        loop {
            let more: Vec<String> = self
                .deps
                .iter()
                .filter(|(pkg, deps)| !out.contains(*pkg) && deps.iter().any(|d| out.contains(d)))
                .map(|(pkg, _)| pkg.clone())
                .collect();
            if more.is_empty() {
                return out;
            }
            out.extend(more);
        }
    }
}

/// `name = "..."` in the `[package]` section.
fn package_name(manifest: &str) -> Option<String> {
    let mut in_package = false;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_package = line == "[package]";
        } else if in_package {
            if let Some(value) = line.strip_prefix("name") {
                let value = value.trim_start().strip_prefix('=')?.trim();
                return Some(value.trim_matches('"').to_string());
            }
        }
    }
    None
}

/// The keys of every `[*dependencies]` section, and the `x` of `[*dependencies.x]` headers.
fn dependency_names(manifest: &str) -> Vec<String> {
    let mut out = vec![];
    let mut in_deps = false;
    for line in manifest.lines().map(str::trim) {
        if let Some(header) = line.strip_prefix('[') {
            let header = header.trim_end_matches(']');
            in_deps = header.ends_with("dependencies");
            if let Some((section, name)) = header.rsplit_once('.') {
                if section.ends_with("dependencies") {
                    out.push(name.trim_matches('"').to_string());
                }
            }
        } else if in_deps && !line.starts_with('#') {
            // `name = ...`, `name.workspace = true`, `"name" = ...`
            let key = line.split(['=', '.']).next().unwrap_or("").trim();
            if !key.is_empty() {
                out.push(key.trim_matches('"').to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph() -> Graph {
        let m = |dir: &str, text: &str| (dir.to_string(), text.to_string());
        Graph::from_manifests(&[
            m("common", "[package]\nname = \"velt_common\"\n"),
            m(
                "syntax",
                "[package]\nname = \"velt_syntax\"\n\n[dependencies]\nvelt_common.workspace = true\nmemchr = \"2\"\n",
            ),
            m(
                "sema",
                "[package]\nname = \"velt_sema\"\n[dependencies]\nvelt_syntax = { workspace = true }\n",
            ),
            m(
                "fmt",
                "[package]\nname = \"velt_fmt\"\n[dev-dependencies]\n# velt_sema = \"x\"\n[dependencies.velt_syntax]\npath = \"../syntax\"\n",
            ),
            m(
                "rt",
                "[package]\nname = \"velt_rt\"\n[target.'cfg(unix)'.dependencies]\nlibc = \"0.2\"\n",
            ),
        ])
    }

    #[test]
    fn reads_names_and_workspace_dependencies() {
        let g = graph();
        assert_eq!(g.dirs["sema"], "velt_sema");
        assert_eq!(
            g.deps["velt_syntax"],
            BTreeSet::from(["velt_common".to_string()])
        );
        assert_eq!(
            g.deps["velt_fmt"],
            BTreeSet::from(["velt_syntax".to_string()])
        );
        assert!(g.deps["velt_rt"].is_empty());
    }

    #[test]
    fn dependents_are_transitive() {
        let g = graph();
        let all = g.with_dependents(&BTreeSet::from(["velt_common".to_string()]));
        let want = ["velt_common", "velt_fmt", "velt_sema", "velt_syntax"];
        assert_eq!(all, want.iter().map(|s| s.to_string()).collect());
        let rt = g.with_dependents(&BTreeSet::from(["velt_rt".to_string()]));
        assert_eq!(rt.len(), 1);
    }
}
