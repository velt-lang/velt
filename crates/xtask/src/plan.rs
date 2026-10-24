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
            // `velt test --locked` resolves each example app as a package.
            "example_apps",
        ],
    ),
    ("velt_registry", &["registry_cli", "cli_package"]),
    ("velt_http", &["registry_cli", "cli_package"]),
    // `velt target add`, and the `velt` field `velt new` writes and every command checks.
    (
        "velt_toolchain",
        &["cli_package", "templates", "install_layout"],
    ),
    // A separate binary; its tests run the launcher against fake releases.
    ("velt_launcher", &[]),
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

/// Files that build WebAssembly besides those with `wasm` in their path (crates/velt_rt_wasm,
/// crates/velt_link/src/wasm.rs, the `wasm_goldens` test): a change to them runs the
/// WebAssembly goldens ([`Plan::wasm`]).
const WASM_TOO: &[&str] = &[
    "crates/velt_codegen_llvm/src/target.rs",
    "crates/velt_codegen_llvm/src/llc.rs",
];

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

/// The VS Code extension, a Node package outside the Rust workspace.
pub(crate) const VSCODE: &str = "editors/vscode/";

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
    // The LLDB script the toolchain ships (`install_layout` installs it and asks for its path;
    // `debugger` runs its formatters).
    ("editors/lldb/", &[], &["install_layout", "debugger"]),
    // `example_apps` runs `velt test` for examples/apps/*/ (the compiler, runtime and std run
    // it too: they select every `veltc` test).
    (
        "examples/",
        &["velt_syntax", "velt_codegen_llvm"],
        &["templates", "example_apps"],
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

/// How velt_syntax's robustness test (`prefixes_never_panic_*`, which parses what is before and
/// after cuts through every golden) cuts the goldens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sweep {
    /// At every character of the kitchen sink and of these goldens (paths relative to the
    /// workspace root), and at a sample of positions in the others.
    Sampled(BTreeSet<String>),
    /// At every character of every golden (`VELT_ROBUSTNESS=full`): the parser changed.
    Full,
}

/// Files outside the parser's crates that change what it compiles to: the toolchain, and the
/// root manifest's profiles (`overflow-checks`, `debug-assertions` decide what panics).
const PARSER_BUILD: &[&str] = &["rust-toolchain.toml", "Cargo.toml"];

/// More changed goldens than this sweep every golden in full, which keeps the list of files out
/// of a long environment variable.
const MAX_SWEPT_GOLDENS: usize = 200;

impl Sweep {
    /// The sweep for a change touching `paths` (only comments changed in `comment_only`): full
    /// when code of velt_syntax or of a crate it depends on changed, or how it is compiled
    /// ([`PARSER_BUILD`]), every character of the
    /// changed goldens otherwise.
    fn for_changes(graph: &Graph, paths: &[String], comment_only: &BTreeSet<String>) -> Sweep {
        let parser = BTreeSet::from(["velt_syntax".to_string()]);
        let mut goldens = BTreeSet::new();
        for path in paths.iter().filter(|p| !comment_only.contains(*p)) {
            if PARSER_BUILD.contains(&path.as_str()) {
                return Sweep::Full;
            }
            if let Some(rest) = path.strip_prefix("crates/") {
                let dir = rest.split('/').next().unwrap_or(rest);
                let affects_parser = graph.dirs.get(dir).is_some_and(|name| {
                    graph
                        .with_dependents(&BTreeSet::from([name.clone()]))
                        .is_superset(&parser)
                });
                if affects_parser {
                    return Sweep::Full;
                }
            } else if path.starts_with("tests/golden/") && path.ends_with(".vlt") {
                goldens.insert(path.clone());
            }
        }
        if goldens.len() > MAX_SWEPT_GOLDENS {
            return Sweep::Full;
        }
        Sweep::Sampled(goldens)
    }
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
    /// The VS Code extension (`editors/vscode`): `npm ci`, then `npm test` (tsc and its unit
    /// tests).
    pub vscode: bool,
    /// Why the merge queue checks Windows and macOS too; `None`: Linux is enough
    /// (crates/xtask/src/os.rs).
    pub other_os: Option<String>,
    /// Why the whole gate runs on Windows and macOS after the change lands on `main`; `None`:
    /// the queue already ran it there (crates/xtask/src/os.rs, [`os::after_merge_reason`]).
    pub after_merge: Option<String>,
    /// How velt_syntax's robustness test cuts the goldens, if it runs.
    pub sweep: Sweep,
    /// The WebAssembly goldens (`veltc::wasm_goldens`, every golden built twice more and run
    /// under wasmtime: about half of the test time) run with all of `veltc`'s tests. A change
    /// runs them only when it touches WebAssembly code ([`WASM_TOO`]); a changed golden alone
    /// still runs on WebAssembly (its `veltc` tests are selected by name). The nightly run checks
    /// them in full on every OS, as do the checks after merging when they run. `--full` runs them
    /// unless `VELT_WASM_GOLDENS=skip`, which the merge queue sets from the change's plan.
    pub wasm: bool,
}

impl Plan {
    pub fn everything(reason: impl Into<String>) -> Plan {
        let reason = reason.into();
        Plan {
            full: true,
            other_os: Some(reason.clone()),
            // Without the changes (`--full`, or git can't tell them) nobody knows which OSes the
            // queue checked: check them all again.
            after_merge: Some(reason.clone()),
            reasons: vec![reason],
            rust: true,
            packages: BTreeSet::new(),
            all_tests: true,
            veltc: Veltc::All,
            goldens: Goldens::All,
            vlt_fmt: true,
            difftest: true,
            vscode: true,
            // `--full` (the merge queue, nightly) knows no changes: the environment chooses
            // (`VELT_ROBUSTNESS`, as `cargo xtask affected` sets it for the queue).
            sweep: Sweep::Sampled(BTreeSet::new()),
            wasm: true,
        }
    }

    /// Everything, for a change that can't be told (no base revision, or git failed): the
    /// robustness test sweeps in full too, since the change may be the parser's.
    pub fn unknown_changes(reason: impl Into<String>) -> Plan {
        Plan {
            sweep: Sweep::Full,
            ..Plan::everything(reason)
        }
    }

    fn nothing() -> Plan {
        Plan {
            full: false,
            other_os: None,
            after_merge: None,
            reasons: vec![],
            rust: false,
            packages: BTreeSet::new(),
            all_tests: false,
            veltc: Veltc::None,
            goldens: Goldens::None,
            vlt_fmt: false,
            difftest: false,
            vscode: false,
            sweep: Sweep::Sampled(BTreeSet::new()),
            wasm: false,
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
        plan.sweep = Sweep::for_changes(graph, paths, comment_only);
        plan.wasm = paths.iter().any(|p| is_wasm(p));
        let mut changed_crates = BTreeSet::new();
        let mut comment_crates = BTreeSet::new();
        for path in paths {
            match classify(graph, path) {
                Effect::Everything => {
                    let everything = Plan::everything(format!("{path} changed"));
                    return Plan {
                        sweep: plan.sweep,
                        after_merge: os::after_merge_reason(paths, &everything.other_os),
                        ..everything
                    };
                }
                Effect::Nothing => {}
                Effect::Difftest => {
                    plan.reasons
                        .push(format!("{path}: the differential tester"));
                    plan.difftest = true;
                }
                Effect::Vscode => {
                    plan.reasons.push(format!("{path}: the VS Code extension"));
                    plan.vscode = true;
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
        plan.after_merge = os::after_merge_reason(paths, &plan.other_os);
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
        let mut excluded = "binary_id(veltc::golden)".to_string();
        if self.veltc == Veltc::All && !self.wasm {
            excluded.push_str(" - binary_id(veltc::wasm_goldens)");
        }
        if self.all_tests {
            return Some(format!("all() - {excluded}"));
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
        Some(format!("({}) - {excluded}", parts.join(" | ")))
    }

    /// Whether the WebAssembly goldens (`veltc::wasm_goldens`) run.
    pub fn wasm_goldens(&self) -> bool {
        match &self.veltc {
            Veltc::All => self.wasm,
            Veltc::Some(binaries) => binaries.contains("wasm_goldens"),
            Veltc::None => false,
        }
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

    /// The environment variables that tell velt_syntax's robustness test how to cut the goldens
    /// (crates/velt_syntax/tests/robustness.rs); none for its default, so a `VELT_ROBUSTNESS`
    /// set by the caller (nightly, the merge queue) stays.
    pub fn sweep_env(&self) -> Vec<(&'static str, String)> {
        match &self.sweep {
            Sweep::Full => vec![("VELT_ROBUSTNESS", "full".into())],
            Sweep::Sampled(files) if files.is_empty() => vec![],
            Sweep::Sampled(files) => {
                let files: Vec<&str> = files.iter().map(String::as_str).collect();
                vec![("VELT_ROBUSTNESS_FILES", files.join(","))]
            }
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
        let wasm = match &self.veltc {
            Veltc::All if !self.wasm => "no (nightly; no WebAssembly code changed)",
            _ if self.wasm_goldens() => "yes",
            _ => "no",
        };
        out.push_str(&format!("  WebAssembly goldens: {wasm}\n"));
        let sweep = match &self.sweep {
            Sweep::Full => "every character of every golden (VELT_ROBUSTNESS=full)".to_string(),
            Sweep::Sampled(files) if files.is_empty() => "sampled".to_string(),
            Sweep::Sampled(files) => format!("sampled, every character of: {}", join(files)),
        };
        out.push_str(&format!("  parser robustness sweep: {sweep}\n"));
        out.push_str(&format!(
            "  velt fmt --check std examples: {}\n",
            yes(self.vlt_fmt)
        ));
        out.push_str(&format!(
            "  tests/difftest (fmt, clippy, unit tests): {}\n",
            yes(self.difftest)
        ));
        out.push_str(&format!(
            "  editors/vscode (npm ci, npm test): {}\n",
            yes(self.vscode)
        ));
        let os = match &self.other_os {
            Some(reason) => format!("Linux, Windows, macOS ({reason})"),
            None => "Linux".into(),
        };
        out.push_str(&format!("  merge queue OSes: {os}\n"));
        let after = match &self.after_merge {
            Some(reason) => format!("Windows, macOS ({reason})"),
            None => "none (the queue checked every OS)".into(),
        };
        out.push_str(&format!("  whole gate after merging, on main: {after}\n"));
        out
    }
}

enum Effect {
    Everything,
    Nothing,
    Difftest,
    /// The VS Code extension's sources and tests (`editors/vscode/`, other than the templates the
    /// `debugger` test reads).
    Vscode,
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
    if under(VSCODE) {
        return Effect::Vscode;
    }
    if path.starts_with("editors/") {
        return Effect::Nothing;
    }
    Effect::Everything
}

/// Whether `path` builds WebAssembly: `wasm` in its path, or [`WASM_TOO`].
fn is_wasm(path: &str) -> bool {
    path.to_ascii_lowercase().contains("wasm") || WASM_TOO.contains(&path)
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
