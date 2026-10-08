//! Which checks a change needs, from the files it touches.
//!
//! The rules are conservative: a path they don't know, the build configuration, CI and the
//! tooling itself select everything. The merge queue always checks everything (on Linux, and on
//! Windows and macOS when crates/xtask/src/os.rs says so), so a rule that selects too little is
//! caught before `main`, never after.

use std::collections::BTreeSet;

use crate::graph::Graph;
use crate::os;

/// Crates whose change can change what any compiled Velt program does: every end-to-end test
/// runs. (A crate not listed here or in [`TOOLING`] is treated like these.)
const PIPELINE: &[&str] = &[
    "velt_common",
    "velt_syntax",
    "velt_sema",
    "velt_vir",
    "velt_codegen_cl",
    "velt_codegen_llvm",
    "velt_opt",
    "velt_link",
    "velt_rt",
    "velt_rt_host",
    "velt_rt_shared",
    "velt_native",
    "velt_native_macros",
    "veltc",
];

/// Crates that don't change compiled programs, with the `veltc` integration test binaries
/// (crates/veltc/tests/<name>.rs) that exercise them. `veltc`'s unit tests always run with them.
const TOOLING: &[(&str, &[&str])] = &[
    ("velt_lsp", &[]),
    ("velt_doc", &[]),
    ("velt_tscompat", &["ts_compat"]),
    ("velt_fmt", &["templates", "cli_package"]),
    (
        "vpm",
        &[
            "cli_package",
            "registry_cli",
            "native_packages",
            "templates",
            "install_layout",
            // `velt check --ts-compat` in a package reads `tsCompat` from the manifest.
            "ts_compat",
        ],
    ),
    ("velt_registry", &["registry_cli", "cli_package"]),
    ("velt_http", &["registry_cli", "cli_package"]),
    ("velt_rt_wasm", &["wasm_goldens", "playground"]),
];

/// Packages that compile or read a crate's sources by path, which the dependency graph doesn't
/// show (`[lib] path`, `build = …`, `#[path]`, tests reading the sources as text): a change to
/// the crate, comments included, runs their tests too.
const SHARED_SOURCES: &[(&str, &[&str])] = &[
    (
        "velt_rt",
        &["velt_rt_host", "velt_rt_shared", "velt_rt_wasm"],
    ),
    // velt_rt's std_externs test reads velt_rt_wasm's `extern "C"` definitions.
    ("velt_rt_wasm", &["velt_rt"]),
    // velt_codegen_llvm's tests build their programs with velt_opt's test builders.
    ("velt_opt", &["velt_codegen_llvm"]),
];

/// `vpm` resolves imports and packages for every program, so it runs the goldens too.
const GOLDENS_TOO: &[&str] = &["vpm"];

/// Changes to these select everything.
const EVERYTHING: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    ".cargo/",
    ".config/",
    ".github/workflows/",
    ".github/actions/",
    "scripts/",
    "crates/xtask/",
];

/// The differential tester (`tests/difftest`): a crate of its own outside the workspace, which
/// drives the `velt` binary as a black box and depends on no workspace crate. Its changes need
/// only its own build, lints and unit tests.
pub(crate) const DIFFTEST: &str = "tests/difftest/";

/// Changes to these need no check.
const NOTHING: &[&str] = &[
    "LICENSE-APACHE",
    "LICENSE-MIT",
    "NOTICE",
    "CODE_OF_CONDUCT.md",
    "CONTRIBUTING.md",
    "CLAUDE.md",
    "ROADMAP.md",
    "SECURITY.md",
    ".gitignore",
    ".gitattributes",
    ".github/CODEOWNERS",
    ".github/pull_request_template.md",
    ".github/ISSUE_TEMPLATE/",
    ".claude/",
];

/// Paths outside `crates/` that tests read: (prefix, packages whose tests read it, `veltc` test
/// binaries that read it).
const READ_BY_TESTS: &[(&str, &[&str], &[&str])] = &[
    ("docs/", &["velt_doc"], &["docs"]),
    ("README.md", &[], &["docs"]),
    // velt_sema's scaling test reads bench/compile; `bench_programs` compiles every program.
    ("bench/", &["velt_sema"], &["bench_programs"]),
    ("fuzz/", &["velt_fmt"], &[]),
    // The `tsc` oracle's samples and configuration: velt_tscompat's tests read them (and skip
    // the `tsc` run without its Node packages), `veltc`'s check that the samples are valid Velt.
    ("tests/tscompat-oracle/", &["velt_tscompat"], &["ts_compat"]),
    ("packages/", &[], &["native_packages"]),
    ("playground/", &[], &["playground"]),
    ("editors/vscode/templates/", &[], &["debugger"]),
    (
        "examples/",
        &["velt_syntax", "velt_codegen_llvm"],
        &["templates"],
    ),
    (
        "tests/golden/",
        &["velt_fmt", "velt_sema", "velt_syntax", "velt_vir"],
        &[
            "wasm_goldens",
            "llvm_goldens",
            "debug_info",
            "panic_locations",
        ],
    ),
];

/// Which `veltc` tests run besides the end-to-end goldens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Veltc {
    None,
    /// Its unit tests and these integration test binaries.
    Some(BTreeSet<String>),
    All,
}

/// Which end-to-end goldens run (`tests/golden/**` and `examples/` with an `.out`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Goldens {
    None,
    /// `VELT_GOLDEN` substrings.
    Matching(BTreeSet<String>),
    All,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Everything runs; `reasons` says why.
    pub full: bool,
    pub reasons: Vec<String>,
    /// Rust sources changed: clippy and the doctests run.
    pub rust: bool,
    /// Packages whose tests all run (`veltc` is in [`Plan::veltc`] instead).
    pub packages: BTreeSet<String>,
    /// Every test of the workspace runs (`packages` lists them all).
    pub all_tests: bool,
    pub veltc: Veltc,
    pub goldens: Goldens,
    /// `velt fmt --check std examples`.
    pub vlt_fmt: bool,
    /// `tests/difftest`: `cargo fmt`, clippy and its unit tests.
    pub difftest: bool,
    /// Why the merge queue checks Windows and macOS too; `None`: Linux is enough
    /// (crates/xtask/src/os.rs).
    pub other_os: Option<String>,
}

impl Plan {
    pub fn everything(reason: impl Into<String>) -> Plan {
        let reason = reason.into();
        Plan {
            full: true,
            other_os: Some(reason.clone()),
            reasons: vec![reason],
            rust: true,
            packages: BTreeSet::new(),
            all_tests: true,
            veltc: Veltc::All,
            goldens: Goldens::All,
            vlt_fmt: true,
            difftest: true,
        }
    }

    fn nothing() -> Plan {
        Plan {
            full: false,
            other_os: None,
            reasons: vec![],
            rust: false,
            packages: BTreeSet::new(),
            all_tests: false,
            veltc: Veltc::None,
            goldens: Goldens::None,
            vlt_fmt: false,
            difftest: false,
        }
    }

    /// The plan for a change touching `paths` (relative, `/`-separated).
    pub fn for_paths(graph: &Graph, paths: &[String]) -> Plan {
        Plan::for_changes(graph, paths, &BTreeSet::new())
    }

    /// The plan for a change touching `paths`, where only comments changed in the Rust files
    /// `comment_only`.
    pub fn for_changes(graph: &Graph, paths: &[String], comment_only: &BTreeSet<String>) -> Plan {
        let mut plan = Plan::nothing();
        let mut changed_crates = BTreeSet::new();
        let mut comment_crates = BTreeSet::new();
        for path in paths {
            match classify(graph, path) {
                Effect::Everything => return Plan::everything(format!("{path} changed")),
                Effect::Nothing => {}
                Effect::Difftest => {
                    plan.reasons
                        .push(format!("{path}: the differential tester"));
                    plan.difftest = true;
                }
                Effect::Crate(name) if comment_only.contains(path) => {
                    comment_crates.insert(name);
                }
                Effect::Crate(name) => {
                    changed_crates.insert(name);
                }
                Effect::Std => {
                    plan.reasons.push(format!(
                        "{path}: the standard library is read by every test"
                    ));
                    plan.all_tests(graph);
                    plan.goldens = Goldens::All;
                    plan.vlt_fmt = true;
                }
                Effect::ReadByTests(packages, binaries) => {
                    plan.reasons.push(format!("{path}: read by tests"));
                    plan.packages.extend(packages.iter().map(|p| p.to_string()));
                    plan.add_veltc(binaries);
                    if let Some(filter) = golden_filter(path) {
                        plan.add_golden(filter);
                    }
                    if path.starts_with("examples/") {
                        plan.vlt_fmt = true;
                    }
                }
            }
        }
        plan.add_crates(graph, &changed_crates);
        plan.add_comment_crates(
            &comment_crates
                .difference(&changed_crates)
                .cloned()
                .collect(),
        );
        plan.add_source_readers(&changed_crates.union(&comment_crates).cloned().collect());
        let code: Vec<String> = paths
            .iter()
            .filter(|p| !comment_only.contains(*p))
            .cloned()
            .collect();
        plan.other_os = os::other_os_reason(graph, &code, plan.full);
        plan
    }

    fn all_tests(&mut self, graph: &Graph) {
        let all = graph.deps.keys().filter(|p| *p != "veltc").cloned();
        self.packages.extend(all);
        self.all_tests = true;
        self.veltc = Veltc::All;
    }

    fn add_veltc(&mut self, binaries: &[&str]) {
        match &mut self.veltc {
            Veltc::All => {}
            Veltc::Some(set) => set.extend(binaries.iter().map(|b| b.to_string())),
            Veltc::None => {
                self.veltc = Veltc::Some(binaries.iter().map(|b| b.to_string()).collect());
            }
        }
    }

    fn add_golden(&mut self, filter: String) {
        match &mut self.goldens {
            Goldens::All => {}
            Goldens::Matching(set) => {
                set.insert(filter);
            }
            Goldens::None => self.goldens = Goldens::Matching(BTreeSet::from([filter])),
        }
    }

    /// Crates changed directly: they, their dependents, and what the rules above add.
    fn add_crates(&mut self, graph: &Graph, changed: &BTreeSet<String>) {
        if changed.is_empty() {
            return;
        }
        self.rust = true;
        let affected = graph.with_dependents(changed);
        self.reasons.push(format!(
            "crates changed: {} (with dependents: {})",
            join(changed),
            join(&affected)
        ));
        for name in &affected {
            if name != "veltc" {
                self.packages.insert(name.clone());
            }
        }
        if affected.contains("velt_fmt") {
            self.vlt_fmt = true;
        }
        let tooling = |name: &str| TOOLING.iter().find(|(t, _)| *t == name);
        // A pipeline crate (or one the rules don't know) changes compiled programs.
        let pipeline = changed
            .iter()
            .any(|c| PIPELINE.contains(&c.as_str()) || tooling(c).is_none());
        if pipeline {
            self.veltc = Veltc::All;
            self.goldens = Goldens::All;
            self.vlt_fmt = true;
            return;
        }
        for name in &affected {
            if let Some((_, binaries)) = tooling(name) {
                self.add_veltc(binaries);
            }
            if GOLDENS_TOO.contains(&name.as_str()) {
                self.goldens = Goldens::All;
            }
        }
        if affected.contains("veltc") {
            self.add_veltc(&[]);
        }
    }

    /// Crates where only comments changed: no compiled code changed, so no dependent's tests,
    /// goldens or other OSes. The comments are still checked (rustfmt, clippy's documentation
    /// lints, the doctests), and so are the crate's own tests (some read its sources) and the
    /// file-size rule (`standards`).
    fn add_comment_crates(&mut self, crates: &BTreeSet<String>) {
        if crates.is_empty() {
            return;
        }
        self.rust = true;
        self.reasons
            .push(format!("only comments changed in: {}", join(crates)));
        for name in crates {
            if name != "veltc" {
                self.packages.insert(name.clone());
            }
        }
        self.add_veltc(&[]);
    }

    /// The packages that compile or read the changed crates' sources ([`SHARED_SOURCES`]).
    fn add_source_readers(&mut self, changed: &BTreeSet<String>) {
        for (name, readers) in SHARED_SOURCES {
            if changed.contains(*name) && !self.all_tests {
                self.reasons.push(format!(
                    "{name}'s sources are also used by: {}",
                    readers.join(", ")
                ));
                self.packages.extend(readers.iter().map(|r| r.to_string()));
            }
        }
    }

    /// Anything to build and test at all?
    pub fn has_tests(&self) -> bool {
        !self.packages.is_empty() || self.veltc != Veltc::None
    }

    pub fn needs_build(&self) -> bool {
        self.has_tests() || self.goldens != Goldens::None || self.vlt_fmt
    }

    /// The nextest filterset of the tests to run (never the goldens, which run on their own).
    pub fn filterset(&self) -> Option<String> {
        if self.all_tests {
            return Some("all() - binary_id(veltc::golden)".into());
        }
        let mut parts: Vec<String> = self
            .packages
            .iter()
            .map(|p| format!("package({p})"))
            .collect();
        match &self.veltc {
            Veltc::None => {}
            Veltc::All => parts.push("package(veltc)".into()),
            Veltc::Some(binaries) => {
                let mut kinds = vec!["kind(lib)".to_string(), "kind(bin)".to_string()];
                // The coding standards (file sizes) are cheap and cover every crate.
                kinds.push("binary(standards)".into());
                kinds.extend(binaries.iter().map(|b| format!("binary({b})")));
                parts.push(format!("(package(veltc) & ({}))", kinds.join(" | ")));
            }
        }
        if parts.is_empty() {
            return None;
        }
        Some(format!(
            "({}) - binary_id(veltc::golden)",
            parts.join(" | ")
        ))
    }

    /// `VELT_GOLDEN` for the goldens (and the WebAssembly goldens): `None` for all of them.
    pub fn golden_env(&self) -> Option<String> {
        match &self.goldens {
            Goldens::Matching(filters) => {
                Some(filters.iter().cloned().collect::<Vec<_>>().join(","))
            }
            _ => None,
        }
    }

    pub fn describe(&self) -> String {
        let mut out = String::new();
        let mode = if self.full {
            "everything"
        } else {
            "selected checks"
        };
        out.push_str(&format!("plan: {mode}\n"));
        for r in &self.reasons {
            out.push_str(&format!("  - {r}\n"));
        }
        let yes = |b: bool| if b { "yes" } else { "no" };
        out.push_str(&format!("  clippy + doctests: {}\n", yes(self.rust)));
        let tests = self.filterset().unwrap_or_else(|| "none".into());
        out.push_str(&format!("  tests (nextest filterset): {tests}\n"));
        let goldens = match &self.goldens {
            Goldens::None => "none".to_string(),
            Goldens::All => "all".to_string(),
            Goldens::Matching(_) => {
                format!("VELT_GOLDEN={}", self.golden_env().unwrap_or_default())
            }
        };
        out.push_str(&format!("  goldens: {goldens}\n"));
        out.push_str(&format!(
            "  velt fmt --check std examples: {}\n",
            yes(self.vlt_fmt)
        ));
        out.push_str(&format!(
            "  tests/difftest (fmt, clippy, unit tests): {}\n",
            yes(self.difftest)
        ));
        let os = match &self.other_os {
            Some(reason) => format!("Linux, Windows, macOS ({reason})"),
            None => "Linux (Windows and macOS after merging, on main)".into(),
        };
        out.push_str(&format!("  merge queue OSes: {os}\n"));
        out
    }
}

enum Effect {
    Everything,
    Nothing,
    Difftest,
    Crate(String),
    Std,
    ReadByTests(&'static [&'static str], &'static [&'static str]),
}

fn classify(graph: &Graph, path: &str) -> Effect {
    let under =
        |prefix: &str| path == prefix || (prefix.ends_with('/') && path.starts_with(prefix));
    if EVERYTHING.iter().any(|p| under(p)) {
        return Effect::Everything;
    }
    if NOTHING.iter().any(|p| under(p)) {
        return Effect::Nothing;
    }
    if let Some(rest) = path.strip_prefix("crates/") {
        let (dir, file) = rest.split_once('/').unwrap_or((rest, ""));
        // A manifest can change features, which reach every crate through unification.
        return match graph.dirs.get(dir) {
            Some(name) if file != "Cargo.toml" => Effect::Crate(name.clone()),
            _ => Effect::Everything,
        };
    }
    if path.starts_with("std/") {
        return Effect::Std;
    }
    if under(DIFFTEST) {
        return Effect::Difftest;
    }
    for (prefix, packages, binaries) in READ_BY_TESTS {
        if under(prefix) {
            return Effect::ReadByTests(packages, binaries);
        }
    }
    if path.starts_with("editors/") {
        return Effect::Nothing;
    }
    Effect::Everything
}

/// The `VELT_GOLDEN` substring that selects the goldens a changed file belongs to.
///
/// A program sits directly in a category directory (`tests/golden/lang/`) or in its `errors/`:
/// `tests/golden/lang/foo.out` → `tests/golden/lang/foo.` (that program). Anything else is
/// shared (a `_helper.vlt`, a `_dir/` module, a package directory, a `.pending` marker) and may
/// be imported from anywhere in the category: → `tests/golden/lang/`.
fn golden_filter(path: &str) -> Option<String> {
    if let Some(rest) = path.strip_prefix("examples/") {
        return Some(match rest.split_once('/') {
            Some((dir, _)) => format!("examples/{dir}/"),
            None => format!("examples/{}", program_prefix(rest)),
        });
    }
    let rest = path.strip_prefix("tests/golden/")?;
    let Some((dir, file)) = rest.rsplit_once('/') else {
        return Some("tests/golden/".into());
    };
    let dirs: Vec<&str> = dir.split('/').collect();
    let category = dirs[0];
    let program_dir = dirs.len() == 1 || (dirs.len() == 2 && dirs[1] == "errors");
    if !program_dir || file.starts_with('_') || file.starts_with('.') {
        return Some(format!("tests/golden/{category}/"));
    }
    Some(format!("tests/golden/{dir}/{}", program_prefix(file)))
}

/// `foo.vlt`, `foo.out`, `foo.err`... → `foo.` (matches `foo.vlt`, not `foo_bar.vlt`).
fn program_prefix(file: &str) -> String {
    match file.split_once('.') {
        Some((stem, _)) => format!("{stem}."),
        None => file.to_string(),
    }
}

fn join(set: &BTreeSet<String>) -> String {
    set.iter().cloned().collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
#[path = "plan_tests.rs"]
mod tests;
