//! The quality gate: the steps a [`Plan`] needs, in order, timed. One implementation for every
//! OS, for local runs and for CI (which runs the parts as parallel jobs).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use crate::doctests;
use crate::golden_archive::{self, Manifest, MANIFEST};
use crate::graph::Graph;
use crate::plan::{Goldens, Plan, DIFFTEST};

/// The parts of the gate to run: `all`, or a comma-separated list of `lint` (`cargo fmt`,
/// clippy), `test` (build, unit and integration tests, doctests, `velt fmt --check`, the VS Code
/// extension's `npm test`, smoke test) and `golden` (the end-to-end goldens).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parts {
    lint: bool,
    test: bool,
    golden: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    Lint,
    Test,
    Golden,
}

impl Parts {
    pub fn parse(s: &str) -> Option<Parts> {
        if s == "all" {
            return Some(Parts {
                lint: true,
                test: true,
                golden: true,
            });
        }
        let mut parts = Parts {
            lint: false,
            test: false,
            golden: false,
        };
        for name in s.split(',').map(str::trim) {
            match name {
                "lint" => parts.lint = true,
                "test" => parts.test = true,
                "golden" => parts.golden = true,
                _ => return None,
            }
        }
        Some(parts)
    }

    fn has(self, part: Part) -> bool {
        match part {
            Part::Lint => self.lint,
            Part::Test => self.test,
            Part::Golden => self.golden,
        }
    }
}

pub struct Options {
    pub parts: Parts,
    /// `VELT_GOLDEN_MODES`: `debug`, `release`, or both (`None`).
    pub golden_modes: Option<String>,
    pub smoke: bool,
    /// `--pack <file>`: build what the goldens run and pack it into this tar (relative to the
    /// repository root) instead of running them (see [`golden_archive`]).
    pub pack: Option<String>,
    /// `--archive <file>`: run the goldens from this tar (made by `--pack`) without building.
    pub archive: Option<String>,
    /// Print the commands instead of running them.
    pub dry_run: bool,
}

struct Gate<'a> {
    root: &'a Path,
    dry_run: bool,
    timings: Vec<(String, u64)>,
}

pub fn run(root: &Path, graph: &Graph, plan: &Plan, opts: &Options) -> Result<(), String> {
    let mut gate = Gate {
        root,
        dry_run: opts.dry_run,
        timings: vec![],
    };
    let ws = workspace_args(root);
    // The build leaves this running binary alone (rebuilding it fails on Windows); clippy and
    // the tests only build `xtask`'s check data and test harness, not the binary.
    let mut build_ws = ws.clone();
    build_ws.extend(["--exclude".into(), "xtask".into()]);
    let part = opts.parts;
    if part.has(Part::Lint) {
        gate.step(
            "cargo fmt --check",
            &mut cargo(&["fmt", "--all", "--check"]),
        )?;
    }
    let goldens = part.has(Part::Golden) && plan.goldens != Goldens::None;
    let build = opts.archive.is_none() && ((part.has(Part::Test) && plan.needs_build()) || goldens);
    if build {
        // Every later step reuses this build: the same packages and targets resolve the same
        // features, so nothing compiles twice (`VELT_RT_PREBUILT` below). Any test target makes
        // cargo resolve the dev-dependencies' features too, so the goldens alone (the libraries,
        // `velt`, the runtimes and the golden test) resolve like `--all-targets`.
        let mut b = cargo(&["build"]);
        b.args(&build_ws);
        if part.has(Part::Test) {
            b.arg("--all-targets");
        } else {
            b.args(["--lib", "--bins", "--test", "golden"]);
        }
        gate.step("build", &mut b)?;
    }
    if part.has(Part::Lint) && plan.rust {
        let mut clippy = cargo(&["clippy"]);
        clippy
            .args(&ws)
            .args(["--all-targets", "--", "-D", "warnings"]);
        gate.step("clippy", &mut clippy)?;
    }
    if part.has(Part::Lint) && plan.difftest {
        gate.difftest_lint()?;
    }
    if part.has(Part::Test) {
        gate.tests(plan, graph, &ws)?;
    }
    if part.has(Part::Test) && plan.difftest {
        let mut tests = cargo(&["test"]);
        tests.arg("--manifest-path").arg(difftest_manifest(root));
        gate.step("tests/difftest unit tests", &mut tests)?;
    }
    if goldens && opts.pack.is_some() {
        gate.pack(&ws, opts.pack.as_deref().unwrap_or_default())?;
    } else if goldens {
        let test = match &opts.archive {
            Some(file) => Some(gate.unpack(file)?),
            None => None,
        };
        let golden = || golden_test(root, &ws, test.as_deref());
        let mut goldens = golden();
        if let Some(path) = path_with_rust_lld(root) {
            goldens.env("PATH", path);
        }
        if let Some(filter) = plan.golden_env() {
            goldens.env("VELT_GOLDEN", filter);
        }
        if let Some(modes) = &opts.golden_modes {
            goldens.env("VELT_GOLDEN_MODES", modes);
        }
        let modes = opts.golden_modes.as_deref().unwrap_or("debug + release");
        gate.step(&format!("goldens ({modes})"), &mut goldens)?;
        gate.bundled_linker_goldens(golden(), plan.golden_env(), opts.golden_modes.as_deref())?;
    } else if opts.pack.is_some() {
        return Err("--pack: the plan selects no goldens, so there is nothing to pack".into());
    }
    if part.has(Part::Test) && plan.vscode {
        gate.vscode()?;
    }
    if part.has(Part::Test) && plan.vlt_fmt {
        let velt = target_dir(root).join("debug").join(exe("velt"));
        let mut fmt = Command::new(&velt);
        fmt.args(["fmt", "--check", "std", "examples"]);
        gate.step("velt fmt --check (std, examples)", &mut fmt)?;
    }
    if part.has(Part::Test) && plan.full && opts.smoke && !cfg!(windows) {
        let velt = target_dir(root).join("debug/velt");
        let mut smoke = Command::new("sh");
        smoke.arg("scripts/smoke.sh").arg(velt);
        gate.step("smoke (doctor, new/run, HTTP)", &mut smoke)?;
    }
    gate.summary();
    Ok(())
}

/// veltc's golden test: through cargo, or the binary `test` (relative to `root`) from an archive.
/// `VELT_GOLDEN_SHARD` passes through from the environment.
fn golden_test(root: &Path, ws: &[String], test: Option<&str>) -> Command {
    let mut c = match test {
        Some(test) => Command::new(root.join(test)),
        None => {
            // `--workspace` resolves like the build above (`-p veltc` would compile velt_opt and
            // veltc again); `--exact golden` leaves velt_syntax's parser goldens to the tests.
            let mut c = cargo(&["test"]);
            c.args(ws).args(["--test", "golden", "--"]);
            c
        }
    };
    c.args(["--nocapture", "--exact", "golden"]);
    c.env("VELT_RT_PREBUILT", "1");
    c
}

/// The goldens [`Gate::bundled_linker_goldens`] runs when all of them are selected.
const BUNDLED_LINKER_GOLDENS: &str = "m1/,m2/,m3/,m4/,async_dispatch/";

impl Gate<'_> {
    /// Some goldens again, linked with the bundled linker (`velt_link::bundled`: a link kit for
    /// the host, written into `target/lib/targets/<host>` and removed afterwards, with the Rust
    /// toolchain's `rust-lld`), so every OS the gate runs on checks it.
    fn bundled_linker_goldens(
        &mut self,
        mut goldens: Command,
        selected: Option<String>,
        modes: Option<&str>,
    ) -> Result<(), String> {
        let host = host_triple()?;
        let kit = target_dir(self.root)
            .join("lib")
            .join("targets")
            .join(&host);
        let velt_kit = target_dir(self.root).join("debug").join(exe("velt-kit"));
        let mut build = Command::new(&velt_kit);
        build.args(["build", "--target", &host, "--out"]).arg(&kit);
        let result = self
            .step("link kit for the bundled linker", &mut build)
            .and_then(|()| {
                goldens.env("VELT_LINKER", "bundled");
                goldens.env(
                    "VELT_GOLDEN",
                    selected.unwrap_or_else(|| BUNDLED_LINKER_GOLDENS.into()),
                );
                if let Some(modes) = modes {
                    goldens.env("VELT_GOLDEN_MODES", modes);
                }
                self.step("goldens (bundled linker)", &mut goldens)
            });
        // Without the kit, later builds in this checkout link with the system linker again.
        if !self.dry_run {
            let _ = std::fs::remove_dir_all(&kit);
        }
        result
    }

    /// Packs what the goldens run into `file` (see [`golden_archive`]).
    fn pack(&mut self, ws: &[String], file: &str) -> Result<(), String> {
        let mut find = cargo(&["test"]);
        find.args(ws)
            .args(["--test", "golden", "--no-run", "--message-format=json"]);
        println!(
            "\x1b[36m==> the golden test binary\x1b[0m\n    {}",
            show(&find)
        );
        let test = if self.dry_run {
            "target/debug/deps/golden-<hash>".to_string()
        } else {
            let out = find
                .current_dir(self.root)
                .output()
                .map_err(|e| format!("cannot run cargo: {e}"))?;
            if !out.status.success() {
                return Err(format!(
                    "`{}` failed:\n{}",
                    show(&find),
                    String::from_utf8_lossy(&out.stderr)
                ));
            }
            let exe = golden_archive::test_binary(&String::from_utf8_lossy(&out.stdout))
                .ok_or("cargo built no golden test binary for veltc")?;
            golden_archive::relative(self.root, &exe)?
        };
        let manifest = Manifest {
            root: self.root.to_string_lossy().into_owned(),
            test: test.clone(),
        };
        if !self.dry_run {
            std::fs::write(self.root.join(MANIFEST), manifest.render())
                .map_err(|e| format!("{MANIFEST}: {e}"))?;
        }
        // Relative paths only: GNU tar (Git's, on Windows) reads `C:` as a remote host.
        let mut tar = Command::new("tar");
        tar.args(["-cf", file, MANIFEST, &test]);
        tar.args(golden_archive::files(|f| {
            self.dry_run || self.root.join(f).is_file()
        }));
        self.step(&format!("pack the goldens' build into {file}"), &mut tar)
    }

    /// Unpacks `file` (made by [`Gate::pack`]) into the checkout; returns the golden test
    /// binary's path.
    fn unpack(&mut self, file: &str) -> Result<String, String> {
        let mut tar = Command::new("tar");
        tar.args(["-xf", file]);
        self.step(&format!("unpack {file}"), &mut tar)?;
        if self.dry_run {
            return Ok("target/debug/deps/golden-<hash>".into());
        }
        let text = std::fs::read_to_string(self.root.join(MANIFEST))
            .map_err(|e| format!("{file} has no {MANIFEST}: {e}"))?;
        let manifest = Manifest::parse(&text)?;
        let here = self.root.to_string_lossy();
        if manifest.root != here {
            return Err(format!(
                "{file} was built in {}, not in this checkout ({here}): its golden test has \
                 that checkout's paths compiled in",
                manifest.root
            ));
        }
        Ok(manifest.test)
    }

    fn tests(&mut self, plan: &Plan, graph: &Graph, ws: &[String]) -> Result<(), String> {
        let Some(filterset) = plan.filterset() else {
            return Ok(());
        };
        // In CI, nextest's `ci` profile writes a JUnit report (.config/nextest.toml).
        let mut junit = None;
        let mut tests = if has_nextest() {
            let mut c = cargo(&["nextest", "run"]);
            c.args(ws).args(["--no-tests", "pass", "-E", &filterset]);
            if std::env::var_os("CI").is_some() {
                c.args(["--profile", "ci"]);
                junit = Some(target_dir(self.root).join("nextest/ci/junit.xml"));
            }
            c
        } else {
            warn_no_nextest();
            let mut c = cargo(&["test"]);
            // Not `--doc`: the doctests are a step of their own below.
            c.args(ws)
                .args(["--lib", "--bins", "--tests", "--no-fail-fast"]);
            c.args(["--", "--skip", "golden", "--exact"]);
            c
        };
        tests.env("VELT_RT_PREBUILT", "1");
        if let Some(filter) = plan.golden_env() {
            // The WebAssembly goldens follow the golden selection.
            tests.env("VELT_GOLDEN", filter);
        }
        tests.envs(plan.sweep_env());
        if let Some(report) = &junit {
            let _ = std::fs::remove_file(report);
        }
        let result = self.step("unit + integration tests", &mut tests);
        if let (Err(_), Some(report)) = (&result, &junit) {
            summarize_failed_tests(report);
        }
        result?;
        if plan.rust {
            let crates = doctests::crates_with_doctests(self.root, &graph.dirs);
            let selected: Vec<&String> = crates
                .iter()
                .filter(|c| plan.all_tests || plan.packages.contains(*c))
                .collect();
            if !selected.is_empty() {
                let mut doc = cargo(&["test", "--doc"]);
                for c in selected {
                    doc.args(["-p", c]);
                }
                self.step("doctests", &mut doc)?;
            }
        }
        Ok(())
    }

    /// The VS Code extension: `npm ci`, then `npm test` (tsc and its unit tests), in
    /// `editors/vscode`. Without npm the step is skipped, except in CI.
    fn vscode(&mut self) -> Result<(), String> {
        let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
        let found = Command::new(npm)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        if !found {
            if std::env::var_os("CI").is_some() {
                return Err("editors/vscode: npm is not installed".into());
            }
            println!("note: npm is not installed, so the VS Code extension's tests are skipped.");
            return Ok(());
        }
        // Steps run in the repository root.
        let prefix = ["--prefix", crate::plan::VSCODE];
        let mut install = Command::new(npm);
        install.args(prefix).args(["ci", "--no-audit", "--no-fund"]);
        self.step("editors/vscode: npm ci", &mut install)?;
        let mut test = Command::new(npm);
        test.args(prefix).arg("test");
        self.step("editors/vscode: npm test", &mut test)
    }

    /// `cargo fmt --check` and clippy of the differential tester, a crate outside the workspace
    /// that `--workspace` steps never see.
    fn difftest_lint(&mut self) -> Result<(), String> {
        let manifest = difftest_manifest(self.root);
        let mut fmt = cargo(&["fmt", "--check", "--manifest-path"]);
        fmt.arg(&manifest);
        self.step("cargo fmt --check (tests/difftest)", &mut fmt)?;
        let mut clippy = cargo(&["clippy", "--manifest-path"]);
        clippy
            .arg(&manifest)
            .args(["--all-targets", "--", "-D", "warnings"]);
        self.step("clippy (tests/difftest)", &mut clippy)
    }

    fn step(&mut self, name: &str, cmd: &mut Command) -> Result<(), String> {
        let shown = show(cmd);
        println!("\x1b[36m==> {name}\x1b[0m");
        println!("    {shown}");
        if self.dry_run {
            return Ok(());
        }
        let start = Instant::now();
        let status = cmd
            .current_dir(self.root)
            .status()
            .map_err(|e| format!("{name}: cannot run `{shown}`: {e}"))?;
        let secs = start.elapsed().as_secs();
        self.timings.push((name.to_string(), secs));
        if !status.success() {
            self.summary();
            return Err(format!("FAILED: {name}"));
        }
        println!("\x1b[32m    ok ({secs}s)\x1b[0m");
        Ok(())
    }

    fn summary(&self) {
        if self.timings.is_empty() {
            return;
        }
        let total: u64 = self.timings.iter().map(|(_, s)| s).sum();
        println!("\nsteps:");
        for (name, secs) in &self.timings {
            println!("  {secs:>5}s  {name}");
        }
        println!("  {total:>5}s  total");
    }
}

/// Lists the failed tests of a nextest JUnit report in the job summary (`$GITHUB_STEP_SUMMARY`),
/// so a failure is attributable from the run's page. Best effort: without the report or the
/// summary file there is nothing to add to nextest's own output.
fn summarize_failed_tests(report: &Path) {
    let (Some(summary), Ok(xml)) = (
        std::env::var_os("GITHUB_STEP_SUMMARY"),
        std::fs::read_to_string(report),
    ) else {
        return;
    };
    let failed = junit_failures(&xml);
    if failed.is_empty() {
        return;
    }
    let mut text = format!(
        "### {} failed tests ({})\n\n",
        failed.len(),
        std::env::consts::OS
    );
    for test in &failed {
        text += &format!("- `{test}`\n");
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(summary)
    {
        let _ = f.write_all(text.as_bytes());
    }
}

/// The test cases a JUnit report records as failed (a `<failure>` or `<error>` child), as
/// `classname name` (nextest: `velt_sema::scaling sema_time_grows_linearly`).
fn junit_failures(xml: &str) -> Vec<String> {
    let mut failed = vec![];
    for case in xml.split("<testcase ").skip(1) {
        if !(case.contains("<failure") || case.contains("<error")) {
            continue;
        }
        let tag = format!(" {}", &case[..case.find('>').unwrap_or(case.len())]);
        let attr = |name: &str| {
            let key = format!(" {name}=\"");
            let Some(start) = tag.find(&key).map(|i| i + key.len()) else {
                return String::new();
            };
            let value = &tag[start..];
            let value = &value[..value.find('"').unwrap_or(value.len())];
            value
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&quot;", "\"")
                .replace("&apos;", "'")
                .replace("&amp;", "&")
        };
        failed.push(format!("{} {}", attr("classname"), attr("name")));
    }
    failed
}

pub(crate) fn cargo(args: &[&str]) -> Command {
    let mut c = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    c.args(args);
    c
}

/// `--workspace`, minus the shared runtime on musl hosts (musl links statically and cannot build
/// a cdylib; `velt` falls back to the static runtime).
pub(crate) fn workspace_args(root: &Path) -> Vec<String> {
    let mut args = vec!["--workspace".to_string()];
    let host = Command::new("rustc").arg("-vV").current_dir(root).output();
    let musl = host.is_ok_and(|o| {
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .any(|l| l.starts_with("host:") && l.ends_with("-musl"))
    });
    if musl {
        args.extend(["--exclude".into(), "velt_rt_shared".into()]);
    }
    args
}

fn difftest_manifest(root: &Path) -> PathBuf {
    root.join(DIFFTEST).join("Cargo.toml")
}

/// Without cargo-nextest the selection can't be applied to the tests: say so loudly, since the
/// fallback takes many times longer.
fn warn_no_nextest() {
    let bar = "!".repeat(78);
    eprintln!("\x1b[1;33m{bar}");
    eprintln!("WARNING: cargo-nextest is not installed. The test selection is IGNORED: every test");
    eprintln!("of the workspace runs, one test binary after another (over 10 minutes).");
    eprintln!("Install it once to run only the selected tests, in parallel:");
    eprintln!();
    eprintln!("    cargo install cargo-nextest --locked");
    eprintln!("{bar}\x1b[0m");
}

fn has_nextest() -> bool {
    cargo(&["nextest", "--version"])
        .output()
        .is_ok_and(|o| o.status.success())
}

pub fn target_dir(root: &Path) -> PathBuf {
    match std::env::var_os("CARGO_TARGET_DIR") {
        Some(dir) => root.join(dir),
        None => root.join("target"),
    }
}

/// `rustc -vV`'s host triple.
/// Linux: `PATH` with the Rust toolchain's `ld.lld` in front (`<sysroot>/lib/rustlib/<host>/
/// bin/gcc-ld`, rustc's own wrapper around `rust-lld`), so the goldens link static runtimes the
/// way `velt` does wherever lld is installed (`cc -fuse-ld=lld`, `velt_link`'s `fast_ld`)
/// instead of with GNU ld, which takes about five times as long (#1135). Debug builds still link
/// the shared runtime with GNU ld, and the other tests link static runtimes with it. `None`
/// elsewhere or when the toolchain has no wrapper.
fn path_with_rust_lld(root: &Path) -> Option<std::ffi::OsString> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let sysroot = Command::new("rustc")
        .args(["--print", "sysroot"])
        .current_dir(root)
        .output()
        .ok()?;
    let sysroot = PathBuf::from(String::from_utf8_lossy(&sysroot.stdout).trim());
    let dir = sysroot
        .join("lib/rustlib")
        .join(host_triple().ok()?)
        .join("bin/gcc-ld");
    if !dir.join("ld.lld").is_file() {
        return None;
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::join_paths(std::iter::once(dir).chain(std::env::split_paths(&path))).ok()
}

fn host_triple() -> Result<String, String> {
    let out = Command::new("rustc")
        .arg("-vV")
        .output()
        .map_err(|e| format!("cannot run rustc: {e}"))?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .map(|h| h.trim().to_string())
        .ok_or_else(|| "rustc -vV printed no host".into())
}

pub(crate) fn exe(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

pub(crate) fn show(cmd: &Command) -> String {
    let program = Path::new(cmd.get_program());
    let mut s = match program.file_stem() {
        Some(stem) if stem == "cargo" => "cargo".to_string(),
        _ => program.to_string_lossy().into_owned(),
    };
    for (k, v) in cmd.get_envs() {
        if let Some(v) = v {
            // A `PATH` is the caller's with a directory in front: show that one.
            let shown = match std::env::split_paths(v).next() {
                Some(first) if k == "PATH" => format!("{}:$PATH", first.display()),
                _ => v.to_string_lossy().into_owned(),
            };
            s = format!("{}={shown} {s}", k.to_string_lossy());
        }
    }
    for a in cmd.get_args() {
        let a = a.to_string_lossy();
        if a.contains([' ', '|', '&', '(']) {
            s.push_str(&format!(" '{a}'"));
        } else {
            s.push(' ');
            s.push_str(&a);
        }
    }
    s
}

#[cfg(test)]
mod junit_tests {
    use super::junit_failures;

    #[test]
    fn failed_cases_are_named_by_class_and_name() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="3" failures="1" errors="1">
    <testsuite name="velt_sema::scaling" tests="2" failures="1">
        <testcase name="sema_time_grows_linearly" classname="velt_sema::scaling" time="17.5">
            <failure type="test failure">thread panicked: &lt;grew&gt;</failure>
        </testcase>
        <testcase name="instantiation_work_grows_linearly" classname="velt_sema::scaling" time="1.0"/>
    </testsuite>
    <testsuite name="xtask" tests="1" errors="1">
        <testcase name="a &amp; b &lt;T&gt;" classname="xtask" time="0.1">
            <error type="test abort">SIGSEGV</error>
        </testcase>
    </testsuite>
</testsuites>"#;
        assert_eq!(
            junit_failures(xml),
            [
                "velt_sema::scaling sema_time_grows_linearly",
                "xtask a & b <T>"
            ]
        );
        assert!(junit_failures("<testsuites/>").is_empty());
    }
}
