//! `cargo xtask precheck`: the mechanical checks that fail most pull requests, on the changed
//! files only, fast enough to run before every push (the `pre-push` hook from
//! `cargo xtask install-hooks` runs it).
//!
//! The changed files are the ones `cargo xtask check` sees (crates/xtask/src/changes.rs). They
//! select:
//! - `cargo fmt --check -p <crates with changed Rust files>` (`--all` when the build
//!   configuration changed);
//! - the gate's own `velt fmt --check std examples` when anything under `std/` or `examples/`
//!   changed (it takes about a second, and it picks the `.vlt` files and the `.ts`/`.tsx` files
//!   of packages exactly as the gate does);
//! - the file-size rule (crates/xtask/src/file_size.rs) on the changed Rust files;
//! - `cargo clippy -p <changed crates and their dependents> --all-targets -- -D warnings`, or
//!   `--workspace` when the build configuration changed;
//! - `cargo fmt` and clippy of `tests/difftest` when it changed.
//!
//! Every check runs, and the summary lists each failure with how to fix it.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

use crate::check::{cargo, exe, show, target_dir, workspace_args};
use crate::file_size;
use crate::graph::Graph;
use crate::plan::DIFFTEST;

/// Changes to these lint the whole workspace.
const WORKSPACE_CONFIG: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "rustfmt.toml",
    ".rustfmt.toml",
    "clippy.toml",
    ".clippy.toml",
    ".cargo/",
];

/// The directories `velt fmt --check` checks (as crates/xtask/src/check.rs does): a change under
/// them runs the same command on them.
const FORMATTED: &[&str] = &["std", "examples"];

/// The file-size rule, which veltc's `standards` test includes with `#[path]`.
const FILE_SIZE_RS: &str = "crates/xtask/src/file_size.rs";

/// Which crates a check covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Crates {
    None,
    Packages(BTreeSet<String>),
    Workspace,
}

impl Crates {
    /// `packages`, or the whole workspace when they are all of it.
    fn of(graph: &Graph, packages: BTreeSet<String>) -> Crates {
        if packages.is_empty() {
            Crates::None
        } else if packages.len() == graph.dirs.len() {
            Crates::Workspace
        } else {
            Crates::Packages(packages)
        }
    }

    /// The package arguments for cargo (`None`: nothing to check).
    fn args(&self, root: &Path) -> Option<Vec<String>> {
        match self {
            Crates::None => None,
            Crates::Workspace => Some(workspace_args(root)),
            Crates::Packages(packages) => Some(
                packages
                    .iter()
                    .flat_map(|p| ["-p".into(), p.clone()])
                    .collect(),
            ),
        }
    }
}

/// What a change needs checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// `cargo fmt --check`: the crates with changed Rust files.
    pub rustfmt: Crates,
    /// Clippy: the crates with changed Rust files or manifests, and their dependents.
    pub clippy: Crates,
    /// `velt fmt --check std examples`.
    pub velt_fmt: bool,
    /// Changed Rust files the file-size rule covers.
    pub sized: Vec<String>,
    /// `tests/difftest` changed: its `cargo fmt` and clippy.
    pub difftest: bool,
}

/// The checks for a change touching `paths` (relative, `/`-separated); `exists` says whether a
/// path is still there (deleted files select their crate, but have nothing to format or count).
pub fn select(graph: &Graph, paths: &[String], exists: impl Fn(&str) -> bool) -> Selection {
    let mut sel = Selection {
        rustfmt: Crates::None,
        clippy: Crates::None,
        velt_fmt: false,
        sized: vec![],
        difftest: false,
    };
    let mut changed = BTreeSet::new();
    let mut formatted = BTreeSet::new();
    let mut workspace = false;
    for path in paths {
        let rust = path.ends_with(".rs") || path.ends_with("Cargo.toml");
        if WORKSPACE_CONFIG
            .iter()
            .any(|c| path == c || (c.ends_with('/') && path.starts_with(c)))
        {
            workspace = true;
        } else if path.starts_with(DIFFTEST) {
            sel.difftest |= rust;
        } else if let Some(rest) = path.strip_prefix("crates/") {
            let dir = rest.split('/').next().unwrap_or("");
            if let (true, Some(name)) = (rust, graph.dirs.get(dir)) {
                changed.insert(name.clone());
                if path.ends_with(".rs") {
                    formatted.insert(name.clone());
                }
            }
            if path == FILE_SIZE_RS {
                changed.insert("veltc".to_string());
            }
        }
        sel.velt_fmt |= FORMATTED.iter().any(|d| {
            path.strip_prefix(d)
                .is_some_and(|rest| rest.starts_with('/'))
        });
        if !exists(path) {
            continue;
        }
        if file_size::applies(path) {
            sel.sized.push(path.clone());
        }
    }
    if workspace {
        sel.rustfmt = Crates::Workspace;
        sel.clippy = Crates::Workspace;
    } else {
        sel.rustfmt = Crates::of(graph, formatted);
        sel.clippy = Crates::of(graph, graph.with_dependents(&changed));
    }
    sel
}

/// One failed check: what failed and how to fix it.
struct Failure {
    check: String,
    fix: String,
}

struct Runner<'a> {
    root: &'a Path,
    failures: Vec<Failure>,
    timings: Vec<(String, f64)>,
}

impl Runner<'_> {
    /// Runs `cmd` in the repository root; a failure is recorded with `fix`.
    fn step(&mut self, name: &str, cmd: &mut Command, fix: impl Into<String>) -> bool {
        println!("\x1b[36m==> {name}\x1b[0m");
        println!("    {}", show(cmd));
        let start = Instant::now();
        let ok = match cmd.current_dir(self.root).status() {
            Ok(status) => status.success(),
            Err(e) => {
                println!("    cannot run it: {e}");
                false
            }
        };
        self.timings
            .push((name.to_string(), start.elapsed().as_secs_f64()));
        if !ok {
            self.fail(name, fix);
        }
        ok
    }

    fn fail(&mut self, check: &str, fix: impl Into<String>) {
        self.failures.push(Failure {
            check: check.to_string(),
            fix: fix.into(),
        });
    }
}

/// Runs the checks `sel` selects. `Err` lists the failures and their fixes.
pub fn run(root: &Path, sel: &Selection, reason: &str) -> Result<(), String> {
    println!("precheck: {reason}");
    let start = Instant::now();
    let mut r = Runner {
        root,
        failures: vec![],
        timings: vec![],
    };
    // `cargo fmt` names the whole workspace `--all`.
    let fmt_args = match &sel.rustfmt {
        Crates::Workspace => Some(vec!["--all".to_string()]),
        other => other.args(root),
    };
    if let Some(packages) = fmt_args {
        let mut fmt = cargo(&["fmt", "--check"]);
        fmt.args(&packages);
        let fix = format!("run `cargo fmt {}`", packages.join(" "));
        r.step("cargo fmt --check", &mut fmt, fix);
    }
    if sel.velt_fmt {
        velt_fmt(&mut r);
    }
    file_sizes(&mut r, &sel.sized);
    clippy(&mut r, &sel.clippy);
    if sel.difftest {
        let manifest = root.join(DIFFTEST).join("Cargo.toml");
        let mut fmt = cargo(&["fmt", "--check", "--manifest-path"]);
        fmt.arg(&manifest);
        r.step(
            "cargo fmt --check (tests/difftest)",
            &mut fmt,
            "run `cargo fmt --manifest-path tests/difftest/Cargo.toml`",
        );
        let mut lint = cargo(&["clippy", "--manifest-path"]);
        lint.arg(&manifest)
            .args(["--all-targets", "--", "-D", "warnings"]);
        r.step(
            "clippy (tests/difftest)",
            &mut lint,
            "fix the warnings above",
        );
    }
    summary(&r, start.elapsed().as_secs_f64())
}

/// `velt fmt --check std examples`, with this checkout's `velt` (built first: a no-op
/// when it is up to date; `--workspace` resolves the features like `cargo xtask check`, so the
/// two never rebuild each other's crates).
fn velt_fmt(r: &mut Runner) {
    let mut build = cargo(&["build", "--quiet", "--workspace", "--bin", "velt"]);
    if !r.step("build velt", &mut build, "fix the build errors above") {
        return;
    }
    let velt = target_dir(r.root).join("debug").join(exe("velt"));
    let mut fmt = Command::new(velt);
    fmt.args(["fmt", "--check"]).args(FORMATTED);
    let fix = format!(
        "fix any parse errors it reports first (`velt fmt` formats only files that parse), then \
         run `cargo run -p veltc --bin velt -- fmt {}` to format the files it lists",
        FORMATTED.join(" ")
    );
    r.step(
        &format!("velt fmt --check {}", FORMATTED.join(" ")),
        &mut fmt,
        fix,
    );
}

fn file_sizes(r: &mut Runner, files: &[String]) {
    if files.is_empty() {
        return;
    }
    let name = "file sizes (CONTRIBUTING.md \"Coding standards\")";
    println!("\x1b[36m==> {name}\x1b[0m");
    let mut over = vec![];
    for rel in files {
        match std::fs::read_to_string(r.root.join(rel)) {
            Ok(text) => over.extend(file_size::violation(rel, &text)),
            Err(e) => over.push(format!("{rel}: cannot read it: {e}")),
        }
    }
    for line in &over {
        println!("    {line}");
    }
    if !over.is_empty() {
        r.fail(
            name,
            "split each file by concern into a module directory (`expr/mod.rs`, `expr/call.rs`, …)",
        );
    }
}

fn clippy(r: &mut Runner, which: &Crates) {
    let Some(packages) = which.args(r.root) else {
        return;
    };
    let mut cmd = cargo(&["clippy"]);
    cmd.args(&packages)
        .args(["--all-targets", "--", "-D", "warnings"]);
    let fix = format!(
        "fix the warnings above (`cargo clippy --fix {} --all-targets --allow-dirty` fixes some \
         automatically)",
        packages.join(" ")
    );
    r.step("clippy", &mut cmd, fix);
}

fn summary(r: &Runner, total: f64) -> Result<(), String> {
    if r.timings.is_empty() && r.failures.is_empty() {
        println!("precheck: nothing to check");
        return Ok(());
    }
    println!("\nsteps:");
    for (name, secs) in &r.timings {
        println!("  {secs:>6.1}s  {name}");
    }
    println!("  {total:>6.1}s  total");
    if r.failures.is_empty() {
        println!("\x1b[32mprecheck passed\x1b[0m");
        return Ok(());
    }
    let mut msg = String::from("precheck FAILED:");
    for f in &r.failures {
        msg.push_str(&format!("\n  - {}: {}", f.check, f.fix));
    }
    msg.push_str(
        "\nFix them and push again (`git push --no-verify` skips the hook, but CI runs the same checks).",
    );
    Err(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph() -> Graph {
        let m = |dir: &str, text: &str| (dir.to_string(), text.to_string());
        Graph::from_manifests(&[
            m("velt_common", "[package]\nname = \"velt_common\"\n"),
            m(
                "velt_syntax",
                "[package]\nname = \"velt_syntax\"\n[dependencies]\nvelt_common.workspace = true\n",
            ),
            m(
                "velt_fmt",
                "[package]\nname = \"velt_fmt\"\n[dependencies]\nvelt_syntax.workspace = true\n",
            ),
            m("xtask", "[package]\nname = \"xtask\"\n"),
        ])
    }

    fn sel(paths: &[&str]) -> Selection {
        let paths: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
        select(&graph(), &paths, |p| !p.contains("deleted"))
    }

    fn packages(names: &[&str]) -> Crates {
        Crates::Packages(names.iter().map(|n| n.to_string()).collect())
    }

    #[test]
    fn a_crate_change_lints_it_and_its_dependents() {
        let s = sel(&["crates/velt_syntax/src/lexer.rs"]);
        assert_eq!(s.rustfmt, packages(&["velt_syntax"]));
        assert_eq!(s.clippy, packages(&["velt_fmt", "velt_syntax"]));
        assert_eq!(s.sized, ["crates/velt_syntax/src/lexer.rs"]);
        assert!(!s.velt_fmt && !s.difftest);
    }

    #[test]
    fn every_crate_affected_lints_the_workspace() {
        assert_eq!(
            sel(&["crates/velt_common/src/lib.rs", "crates/xtask/src/main.rs"]).clippy,
            Crates::Workspace
        );
        let s = sel(&["Cargo.lock"]);
        assert_eq!(s.clippy, Crates::Workspace);
        assert_eq!(s.rustfmt, Crates::Workspace);
        assert!(s.sized.is_empty());
        assert_eq!(sel(&[".cargo/config.toml"]).clippy, Crates::Workspace);
    }

    #[test]
    fn a_manifest_change_lints_without_formatting() {
        let s = sel(&["crates/velt_fmt/Cargo.toml"]);
        assert_eq!(s.clippy, packages(&["velt_fmt"]));
        assert_eq!(s.rustfmt, Crates::None);
        assert!(s.sized.is_empty());
    }

    #[test]
    fn deleted_files_select_their_crate_but_nothing_to_read() {
        let s = sel(&["crates/velt_fmt/src/deleted.rs", "std/deleted.vlt"]);
        assert_eq!(s.clippy, packages(&["velt_fmt"]));
        assert!(s.sized.is_empty());
    }

    #[test]
    fn a_change_under_std_or_examples_runs_the_gates_velt_fmt() {
        for path in [
            "std/json.vlt",
            "std/README.md",
            "examples/apps/chat/src/main.vlt",
            // `velt fmt` formats a package's .ts/.tsx sources too.
            "examples/apps/sigx-ssr/app/src/view.tsx",
            "examples/apps/sigx-ssr/app/src/deleted.ts",
        ] {
            let s = sel(&[path]);
            assert!(s.velt_fmt, "{path}");
            assert_eq!((&s.rustfmt, &s.clippy), (&Crates::None, &Crates::None));
            assert!(s.sized.is_empty());
        }
        for path in [
            "tests/golden/m1/hello.vlt",
            "tests/golden/errors/bad.vlt",
            "docs/book/intro.md",
            "stdlib.md",
            "crates/velt_fmt/examples/x.vlt",
        ] {
            assert!(!sel(&[path]).velt_fmt, "{path}");
        }
    }

    #[test]
    fn the_shared_file_size_rule_lints_veltc_too() {
        let mut g = graph();
        g.dirs.insert("veltc".into(), "veltc".into());
        g.deps.insert("veltc".into(), BTreeSet::new());
        let paths = vec![FILE_SIZE_RS.to_string()];
        let s = select(&g, &paths, |_| true);
        assert_eq!(s.clippy, packages(&["veltc", "xtask"]));
        assert_eq!(s.rustfmt, packages(&["xtask"]));
    }

    #[test]
    fn the_differential_tester_is_linted_on_its_own() {
        let s = sel(&["tests/difftest/src/main.rs", "tests/difftest/README.md"]);
        assert!(s.difftest);
        assert_eq!(s.rustfmt, Crates::None);
        assert_eq!(s.clippy, Crates::None);
        assert!(s.sized.is_empty());
        assert!(!sel(&["tests/difftest/README.md"]).difftest);
    }

    #[test]
    fn unknown_crate_directories_and_other_files_select_nothing() {
        let s = sel(&[
            "crates/gone/src/lib.rs",
            "README.md",
            "crates/velt_fmt/notes.txt",
        ]);
        assert_eq!(s.clippy, Crates::None);
        assert_eq!(s.rustfmt, Crates::None);
        // The file-size rule still covers a Rust file under crates/.
        assert_eq!(s.sized, ["crates/gone/src/lib.rs"]);
    }
}
