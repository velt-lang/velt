//! Which files the tests that run a tool over the repository's programs go through: velt_syntax's
//! robustness test (`prefixes_never_panic_*`, which parses what is before and after cuts through
//! every golden) and velt_fmt's corpus tests (`corpus.rs`, `fuzz.rs`).
//!
//! Each answers a question about the tool and a file, and the answer depends on nothing else. With
//! the tool unchanged, a file that didn't change has the answer it had on `main`, so only the
//! changed files are asked. The nightly run asks about every file, whatever changed.

use std::collections::BTreeSet;

use crate::graph::Graph;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sweep {
    /// The tool is unchanged: only these files (paths relative to the workspace root).
    Changed(BTreeSet<String>),
    /// The test's default, or what the caller's environment says (`--full` knows no changes: the
    /// merge queue and the nightly run set `VELT_ROBUSTNESS`).
    Default,
    /// The tool changed: as much as the test can do (`VELT_ROBUSTNESS=full`, every character of
    /// every golden; the formatter's whole corpus).
    Full,
}

/// Files outside the tools' crates that change what they compile to: the toolchain, and the root
/// manifest's profiles (`overflow-checks`, `debug-assertions` decide what panics).
const TOOL_BUILD: &[&str] = &["rust-toolchain.toml", "Cargo.toml"];

/// Files that can change the tools' dependencies, but rarely do: the test's default.
const TOOL_DEPENDENCIES: &[&str] = &["Cargo.lock", ".cargo/config.toml"];

/// More changed files than this sweep in full, which keeps the list of files out of a long
/// environment variable.
pub const MAX_SWEPT_FILES: usize = 200;

/// The files velt_syntax's robustness test reads.
pub const PARSER_FILES: &[&str] = &["tests/golden/"];

/// The files velt_fmt's corpus tests read (`crates/velt_fmt/tests/common/mod.rs`).
pub const FORMATTER_FILES: &[&str] = &["tests/golden/", "std/", "fuzz/seeds/"];

impl Sweep {
    /// The sweep of `tool` over the `.vlt` files under `files`, for a change touching `paths`
    /// (only comments changed in `comment_only`): [`Sweep::Full`] when code of `tool` or of a crate
    /// it depends on changed, or how it is compiled ([`TOOL_BUILD`]), else the changed files.
    pub fn for_changes(
        graph: &Graph,
        paths: &[String],
        comment_only: &BTreeSet<String>,
        tool: &str,
        files: &[&str],
    ) -> Sweep {
        let tool = BTreeSet::from([tool.to_string()]);
        let mut changed = BTreeSet::new();
        let mut dependencies = false;
        for path in paths.iter().filter(|p| !comment_only.contains(*p)) {
            if TOOL_BUILD.contains(&path.as_str()) {
                return Sweep::Full;
            }
            dependencies |= TOOL_DEPENDENCIES.contains(&path.as_str());
            if let Some(rest) = path.strip_prefix("crates/") {
                let dir = rest.split('/').next().unwrap_or(rest);
                let affects_tool = graph.dirs.get(dir).is_some_and(|name| {
                    graph
                        .with_dependents(&BTreeSet::from([name.clone()]))
                        .is_superset(&tool)
                });
                if affects_tool {
                    return Sweep::Full;
                }
            } else if files.iter().any(|d| path.starts_with(d)) && path.ends_with(".vlt") {
                changed.insert(path.clone());
            }
        }
        if changed.len() > MAX_SWEPT_FILES {
            return Sweep::Full;
        }
        match dependencies {
            true => Sweep::Default,
            false => Sweep::Changed(changed),
        }
    }

    /// `name=changed` and `files` with the file list, for [`Sweep::Changed`]; `full` for
    /// [`Sweep::Full`] when the test has such a mode.
    pub fn env(
        &self,
        name: &'static str,
        files: &'static str,
        full: bool,
    ) -> Vec<(&'static str, String)> {
        match self {
            Sweep::Full if full => vec![(name, "full".into())],
            Sweep::Full | Sweep::Default => vec![],
            Sweep::Changed(changed) => {
                let list: Vec<&str> = changed.iter().map(String::as_str).collect();
                vec![(name, "changed".into()), (files, list.join(","))]
            }
        }
    }

    /// For `cargo xtask affected`: `whole` for the test's default, `full` for [`Sweep::Full`].
    pub fn describe(&self, whole: &str, full: &str) -> String {
        match self {
            Sweep::Full => full.to_string(),
            Sweep::Default => whole.to_string(),
            Sweep::Changed(files) if files.is_empty() => "no files".to_string(),
            Sweep::Changed(files) => format!(
                "the changed files: {}",
                files.iter().cloned().collect::<Vec<_>>().join(", ")
            ),
        }
    }
}
