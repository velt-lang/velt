//! A host package (CONTRACT: docs/internals/contracts/manifest.md, workers.md): a package that
//! makes a `velt build --host <package>` build run on one JavaScript platform. Velt's compiler
//! knows no platform; everything a platform needs comes from its host package's `host` object:
//!
//! ```ts ignore
//! host: {
//!   target: "wasm32-unknown-unknown",          // the target its builds are for
//!   condition: "workers",                      // the condition `#name` variants pick it by
//!   entry: "./src/worker.mjs",                 // the platform's entry, over the glue's core
//!   scaffold: { "app.toml": "./src/app.toml" }, // config files written when absent
//! },
//! ```
//!
//! The build writes its own files (the module, the runtime glue, the host modules' map, the host
//! modules) and copies `entry` beside them under its file name, every build; each `scaffold` file
//! is copied to its key's path only when there is no such file, so the user's edits stay.

use std::collections::BTreeMap;
use std::path::{Component, Path};

/// The files the build itself writes into a host build's directory, its by-products (object
/// files, stamps) included: neither the entry nor a scaffolded file (nor a folder a scaffolded
/// file goes in) may take one of these names.
pub const BUILD_FILES: &[&str] = &[
    "program.wasm",
    "program.o",
    "program.entry.o",
    "program.wasm.build-stamp",
    "program.wasm.link-stamp",
    "velt_web.mjs",
    "velt_serve.mjs",
    "velt_modules.mjs",
    "hosts",
    MARKER,
];

/// The file marking a directory as a host build's own, listing the entries host builds wrote
/// there. A build writes only into a directory that is new or empty, holds nothing but its host's
/// scaffolded files, or has this marker and nothing beyond the build's files, the entries it
/// lists and the host's scaffolded files.
pub const MARKER: &str = ".velt-host";

/// What `host` of a host package's `package.velt` says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostConfig {
    /// The target triple the package's builds are for.
    pub target: String,
    /// The condition its builds have, on top of the target's (`crate::imports::conditions`).
    pub condition: String,
    /// The platform's entry: a JavaScript module below `./src/`, copied beside the build's files
    /// under its file name.
    pub entry: String,
    /// The files written when absent: the path in the build's directory → the package's file.
    pub scaffold: BTreeMap<String, String>,
}

impl HostConfig {
    /// The entry's file name in the build's directory.
    pub fn entry_name(&self) -> &str {
        self.entry.rsplit('/').next().unwrap_or(&self.entry)
    }
}

/// The conditions the root package's (`graph`'s first) `imports` variants name that no build of
/// it has: neither built-in nor the condition of a host package among its packages. Most likely
/// misspelled (`windows` for `win32`); such a variant is never chosen. A library's variants for
/// hosts its users add are fine, so only the root's are listed.
pub fn unreachable_conditions(graph: &crate::PackageGraph) -> Vec<String> {
    let Some(root) = graph.packages().first() else {
        return vec![];
    };
    let hosts: Vec<&str> = graph
        .packages()
        .iter()
        .filter_map(|p| p.host.as_ref().map(|h| h.condition.as_str()))
        .collect();
    let mut out: Vec<String> = vec![];
    for variant in root.imports.values().flatten() {
        let c = variant.condition.as_str();
        if !crate::imports::CONDITIONS.contains(&c)
            && !hosts.contains(&c)
            && !out.iter().any(|o| o == c)
        {
            out.push(c.to_string());
        }
    }
    out
}

/// Whether `name` is one of the build's files: compared without ASCII case, since a
/// case-insensitive file system (macOS's and Windows's defaults) takes `.VELT-HOST` for
/// `.velt-host`.
pub fn is_build_file(name: &str) -> bool {
    BUILD_FILES.iter().any(|f| f.eq_ignore_ascii_case(name))
}

/// Check `target`: a target triple (no spaces).
pub(crate) fn check_target(target: &str) -> Result<(), String> {
    if target.is_empty() || target.contains(char::is_whitespace) || target.split('-').count() < 2 {
        return Err(format!(
            "`{target}` is not a target triple (`wasm32-unknown-unknown`)"
        ));
    }
    Ok(())
}

/// Check `entry`: a JavaScript module below `./src/` whose file name the build doesn't use.
pub(crate) fn check_entry(entry: &str) -> Result<(), String> {
    crate::host_modules::check_file(entry)
        .map_err(|e| e.replace("a host module is", "the entry is"))?;
    let name = entry.rsplit('/').next().unwrap_or(entry);
    if is_build_file(name) {
        return Err(format!(
            "`{entry}`: the build writes its own `{name}`; name the entry otherwise"
        ));
    }
    Ok(())
}

/// Check a `scaffold` entry: `path` (where it goes, a `/`-separated path below the build's
/// directory that the build doesn't write) and `file` (the package's, below `./src/`).
pub(crate) fn check_scaffold(path: &str, file: &str, entry: &str) -> Result<(), String> {
    // Every component a name: no `conf/` (a trailing `/`) or `a//b`, which the build could not
    // write as a file.
    let normal = |p: &str| {
        !p.is_empty()
            && !p.contains('\\')
            && p.split('/').all(|c| !c.is_empty())
            && Path::new(p)
                .components()
                .all(|c| matches!(c, Component::Normal(_)))
    };
    let first = path.split('/').next().unwrap_or(path);
    let entry_name = entry.rsplit('/').next().unwrap_or(entry);
    if !normal(path) {
        return Err(format!(
            "`{path}`: a scaffolded file is a `/`-separated path inside the build's directory"
        ));
    }
    // The first component too: `worker.mjs/x` would let the build's directory check pass a
    // user's `worker.mjs`, which the entry then replaces.
    // Without ASCII case, as a case-insensitive file system compares (`.VELT-HOST/x`, `Worker.mjs`).
    if is_build_file(first) || first.eq_ignore_ascii_case(entry_name) {
        return Err(format!(
            "`{path}` is a file the build writes itself: scaffold another one"
        ));
    }
    let inside = file
        .strip_prefix("./")
        .is_some_and(|rel| normal(rel) && rel.starts_with("src/"));
    if !inside {
        return Err(format!(
            "`{file}`: a scaffolded file comes from the package, a `./`-relative path below \
             `./src/` (a package's archive carries only `src/`)"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_entries_and_scaffolds() {
        assert!(check_target("wasm32-unknown-unknown").is_ok());
        for bad in ["", "wasm", "wasm32 unknown"] {
            assert!(check_target(bad).is_err(), "{bad:?}");
        }
        assert!(check_entry("./src/worker.mjs").is_ok());
        for (bad, why) in [
            ("./worker.mjs", "below `./src/`"),
            ("./src/../w.mjs", "below `./src/`"),
            ("./src/w.toml", "`.mjs` or `.js`"),
            ("./src/velt_serve.mjs", "writes its own `velt_serve.mjs`"),
            ("./src/VELT_SERVE.mjs", "writes its own `VELT_SERVE.mjs`"),
        ] {
            let err = check_entry(bad).unwrap_err();
            assert!(err.contains(why), "{bad}: {err}");
            assert!(
                err.contains("the entry is") || err.contains("writes its own"),
                "{err}"
            );
        }
        assert!(check_scaffold("app.toml", "./src/app.toml", "./src/w.mjs").is_ok());
        assert!(check_scaffold("conf/app.json", "./src/c/app.json", "./src/w.mjs").is_ok());
        for (path, file, why) in [
            (
                "../app.toml",
                "./src/app.toml",
                "inside the build's directory",
            ),
            (
                "/etc/app.toml",
                "./src/app.toml",
                "inside the build's directory",
            ),
            ("a\\b", "./src/app.toml", "inside the build's directory"),
            ("program.wasm", "./src/x", "the build writes itself"),
            ("hosts/x.mjs", "./src/x", "the build writes itself"),
            ("w.mjs", "./src/x", "the build writes itself"),
            ("w.mjs/x", "./src/x", "the build writes itself"),
            ("program.o/x", "./src/x", "the build writes itself"),
            (
                "program.wasm.link-stamp",
                "./src/x",
                "the build writes itself",
            ),
            (".VELT-HOST", "./src/x", "the build writes itself"),
            ("Program.wasm", "./src/x", "the build writes itself"),
            ("W.MJS/x", "./src/x", "the build writes itself"),
            ("conf/", "./src/x", "inside the build's directory"),
            ("a//b", "./src/x", "inside the build's directory"),
            ("app.toml", "./app.toml", "below `./src/`"),
            ("app.toml", "./src/../app.toml", "below `./src/`"),
        ] {
            let err = check_scaffold(path, file, "./src/w.mjs").unwrap_err();
            assert!(err.contains(why), "{path} {file}: {err}");
        }
        let config = HostConfig {
            entry: "./src/js/worker.mjs".into(),
            ..HostConfig::default()
        };
        assert_eq!(config.entry_name(), "worker.mjs");
    }

    #[test]
    fn conditions_no_build_of_the_root_has() {
        use crate::imports::Variant;
        let variant = |c: &str| Variant {
            condition: c.into(),
            target: "./src/a.velt".into(),
        };
        let mut graph = crate::PackageGraph::default();
        graph.add("app", Path::new("/app"), BTreeMap::new()).imports = BTreeMap::from([(
            "#io".to_string(),
            vec![
                variant("windows"),
                variant("edge"),
                variant("js"),
                variant("default"),
            ],
        )]);
        // A library naming a host its users add is fine.
        graph.add("lib", Path::new("/lib"), BTreeMap::new()).imports =
            BTreeMap::from([("#x".to_string(), vec![variant("deno")])]);
        assert_eq!(unreachable_conditions(&graph), ["windows", "edge"]);
        graph
            .add("host-edge", Path::new("/h"), BTreeMap::new())
            .host = Some(HostConfig {
            condition: "edge".into(),
            ..HostConfig::default()
        });
        assert_eq!(unreachable_conditions(&graph), ["windows"]);
    }
}
