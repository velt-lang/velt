//! The quality gate: the steps a [`Plan`] needs, in order, timed. One implementation for every
//! OS, for local runs and for CI (which runs the parts as parallel jobs).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use crate::doctests;
use crate::golden_archive::{self, Manifest, MANIFEST};
use crate::graph::Graph;
use crate::junit;
use crate::plan::{Goldens, Plan, DIFFTEST};

/// The parts of the gate to run: `all`, or a comma-separated list of `lint` (`cargo fmt`,
/// clippy), `test` (build, unit and integration tests, doctests, `velt fmt --check`, the VS Code
/// extension's `npm test`, smoke test), `golden` (the end-to-end goldens) and `wasm` (the
/// WebAssembly goldens alone, which `test` otherwise runs with veltc's other tests; the merge
/// queue and the nightly run them in parallel shards of their own). `all` is the first three.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parts {
    lint: bool,
    test: bool,
    golden: bool,
    wasm: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    Lint,
    Test,
    Golden,
    Wasm,
}

impl Parts {
    pub fn parse(s: &str) -> Result<Parts, String> {
        if s == "all" {
            return Ok(Parts {
                lint: true,
                test: true,
                golden: true,
                wasm: false,
            });
        }
        let mut parts = Parts {
            lint: false,
            test: false,
            golden: false,
            wasm: false,
        };
        for name in s.split(',').map(str::trim) {
            match name {
                "lint" => parts.lint = true,
                "test" => parts.test = true,
                "golden" => parts.golden = true,
                "wasm" => parts.wasm = true,
                _ => return Err(format!("unknown part `{name}`")),
            }
        }
        if parts.test && parts.wasm {
            return Err("`--part test` runs the WebAssembly goldens itself: drop `wasm`".into());
        }
        Ok(parts)
    }

    /// Only `golden` and `wasm`, at least one of them: the parts `--pack` and `--archive` take.
    pub fn only_goldens(self) -> bool {
        !self.lint && !self.test && (self.golden || self.wasm)
    }

    fn has(self, part: Part) -> bool {
        match part {
            Part::Lint => self.lint,
            Part::Test => self.test,
            Part::Golden => self.golden,
            Part::Wasm => self.wasm,
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
    let wasm = part.has(Part::Wasm) && plan.wasm_goldens();
    let build =
        opts.archive.is_none() && ((part.has(Part::Test) && plan.needs_build()) || goldens || wasm);
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
            b.args(["--lib", "--bins"]);
            for (selected, test) in [(goldens, "golden"), (wasm, "wasm_goldens")] {
                if selected {
                    b.args(["--test", test]);
                }
            }
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
    if part.has(Part::Lint) && plan.native_crates {
        gate.native_crates_lint()?;
    }
    if part.has(Part::Test) {
        gate.tests(plan, graph, &ws)?;
    }
    if part.has(Part::Test) && plan.difftest {
        let mut tests = cargo(&["test"]);
        tests.arg("--manifest-path").arg(difftest_manifest(root));
        gate.step("tests/difftest unit tests", &mut tests)?;
    }
    if let Some(file) = &opts.pack {
        if !goldens && !wasm {
            return Err("--pack: the plan selects no goldens, so there is nothing to pack".into());
        }
        gate.pack(&ws, file, goldens, wasm)?;
    } else if goldens || wasm {
        let manifest = match &opts.archive {
            Some(file) => Some(gate.unpack(file)?),
            None => None,
        };
        if goldens {
            let test = archived(&manifest, |m| m.test.clone(), "golden")?;
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
            let modes = opts.golden_modes.as_deref();
            gate.bundled_linker_goldens(golden(), plan.golden_env(), modes)?;
        }
        if wasm {
            let test = archived(&manifest, |m| m.wasm.clone(), "wasm")?;
            gate.wasm_goldens(&ws, test.as_deref(), plan.golden_env())?;
        }
    }
    if part.has(Part::Test) && plan.vscode {
        gate.vscode()?;
    }
    if part.has(Part::Test) && plan.vlt_fmt {
        let velt = target_dir(root).join("debug").join(exe("velt"));
        let mut fmt = Command::new(&velt);
        fmt.args(["fmt", "--check", "std", "examples", "packages"]);
        gate.step("velt fmt --check (std, examples, packages)", &mut fmt)?;
        // The std layout and quality rules (docs/internals/design/std-layout.md §2, §6).
        let mut lint = Command::new(&velt);
        lint.current_dir(root)
            .args(["run", "scripts/std-lint/main.vlt"]);
        gate.step("std-lint", &mut lint)?;
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

/// The test binary `pick` names in the archive's manifest, if the run is from an archive; an
/// error if the archive was packed without `--part <part>`.
fn archived(
    manifest: &Option<Manifest>,
    pick: impl Fn(&Manifest) -> Option<String>,
    part: &str,
) -> Result<Option<String>, String> {
    match manifest {
        None => Ok(None),
        Some(m) => pick(m).map(Some).ok_or_else(|| {
            format!("the archive has no test binary for `--part {part}` (pack it with that part)")
        }),
    }
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

    /// Packs what the goldens run into `file` (see [`golden_archive`]): for the goldens
    /// (`goldens`), the WebAssembly goldens (`wasm`, after building their runtimes), or both.
    fn pack(&mut self, ws: &[String], file: &str, goldens: bool, wasm: bool) -> Result<(), String> {
        if wasm {
            for (target, _) in golden_archive::WASM_RUNTIMES {
                let mut b = cargo(&["build", "-p", "velt_rt_wasm", "--target", target]);
                self.step(&format!("the WebAssembly runtime ({target})"), &mut b)?;
            }
        }
        let names: Vec<&str> = [(goldens, "golden"), (wasm, "wasm_goldens")]
            .into_iter()
            .filter_map(|(selected, name)| selected.then_some(name))
            .collect();
        let mut find = cargo(&["test"]);
        find.args(ws);
        for name in &names {
            find.args(["--test", name]);
        }
        find.args(["--no-run", "--message-format=json"]);
        println!("\x1b[36m==> the test binaries\x1b[0m\n    {}", show(&find));
        let mut tests = vec![];
        if self.dry_run {
            tests.extend(
                names
                    .iter()
                    .map(|n| format!("target/debug/deps/{n}-<hash>")),
            );
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
            let messages = String::from_utf8_lossy(&out.stdout);
            for name in &names {
                let exe = golden_archive::test_binary(&messages, name)
                    .ok_or(format!("cargo built no `{name}` test binary for veltc"))?;
                tests.push(golden_archive::relative(self.root, &exe)?);
            }
        }
        let mut binaries = tests.iter().cloned();
        let manifest = Manifest {
            root: self.root.to_string_lossy().into_owned(),
            test: goldens.then(|| binaries.next()).flatten(),
            wasm: wasm.then(|| binaries.next()).flatten(),
        };
        if !self.dry_run {
            std::fs::write(self.root.join(MANIFEST), manifest.render())
                .map_err(|e| format!("{MANIFEST}: {e}"))?;
        }
        // Relative paths only: GNU tar (Git's, on Windows) reads `C:` as a remote host.
        let mut tar = Command::new("tar");
        tar.args(["-cf", file, MANIFEST]).args(&tests);
        tar.args(golden_archive::files(|f| {
            self.dry_run || self.root.join(f).is_file()
        }));
        if wasm {
            tar.args(golden_archive::WASM_RUNTIMES.iter().map(|(_, lib)| lib));
        }
        self.step(&format!("pack the goldens' build into {file}"), &mut tar)
    }

    /// Unpacks `file` (made by [`Gate::pack`]) into the checkout; returns its manifest.
    fn unpack(&mut self, file: &str) -> Result<Manifest, String> {
        let mut tar = Command::new("tar");
        tar.args(["-xf", file]);
        self.step(&format!("unpack {file}"), &mut tar)?;
        if self.dry_run {
            return Ok(Manifest {
                root: self.root.to_string_lossy().into_owned(),
                test: Some("target/debug/deps/golden-<hash>".into()),
                wasm: Some("target/debug/deps/wasm_goldens-<hash>".into()),
            });
        }
        let text = std::fs::read_to_string(self.root.join(MANIFEST))
            .map_err(|e| format!("{file} has no {MANIFEST}: {e}"))?;
        let manifest = Manifest::parse(&text)?;
        let here = self.root.to_string_lossy();
        if manifest.root != here {
            return Err(format!(
                "{file} was built in {}, not in this checkout ({here}): its test binaries have \
                 that checkout's paths compiled in",
                manifest.root
            ));
        }
        Ok(manifest)
    }

    /// The WebAssembly goldens (crates/veltc/tests/wasm_goldens.rs): through cargo, or the test
    /// binary `test` (relative to the root) from an archive, whose runtimes are prebuilt.
    /// `VELT_WASM_SHARD=<i>/<n>` runs the tests of shard `i` ([`golden_archive::shard_tests`]).
    fn wasm_goldens(
        &mut self,
        ws: &[String],
        test: Option<&str>,
        selected: Option<String>,
    ) -> Result<(), String> {
        let root = self.root;
        let command = || {
            let mut c = match test {
                Some(test) => Command::new(root.join(test)),
                None => {
                    let mut c = cargo(&["test"]);
                    c.args(ws).args(["--test", "wasm_goldens", "--"]);
                    c
                }
            };
            if test.is_some() {
                c.env("VELT_RT_WASM_PREBUILT", "1");
            }
            if let Some(filter) = &selected {
                c.env("VELT_GOLDEN", filter);
            }
            c
        };
        let mut run = command();
        run.arg("--nocapture");
        let mut name = "WebAssembly goldens".to_string();
        if let Ok(shard) = std::env::var("VELT_WASM_SHARD") {
            let (i, n) = golden_archive::parse_shard(&shard)?;
            name = format!("WebAssembly goldens, shard {i}/{n}");
            let mut list = command();
            list.args(["--list", "--format", "terse"]);
            println!(
                "\x1b[36m==> the WebAssembly goldens' tests\x1b[0m\n    {}",
                show(&list)
            );
            if self.dry_run {
                run.arg(format!("<the tests of shard {i}/{n}>"));
            } else {
                let out = list
                    .current_dir(root)
                    .output()
                    .map_err(|e| format!("cannot run `{}`: {e}", show(&list)))?;
                if !out.status.success() {
                    return Err(format!(
                        "`{}` failed:\n{}",
                        show(&list),
                        String::from_utf8_lossy(&out.stderr)
                    ));
                }
                let all = golden_archive::listed_tests(&String::from_utf8_lossy(&out.stdout));
                let tests = golden_archive::shard_tests(&all, i, n);
                if tests.is_empty() {
                    return Err(format!(
                        "{name}: no tests ({} in all, fewer than {n} shards)",
                        all.len()
                    ));
                }
                run.arg("--exact").args(tests);
            }
        }
        self.step(&name, &mut run)
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
            junit::summarize_failed_tests(report);
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

    /// `cargo fmt --check` and clippy of each package's native crate (`packages/<name>/native`,
    /// outside the workspace), in the cargo directory `velt` builds it in.
    fn native_crates_lint(&mut self) -> Result<(), String> {
        for dir in native_crates(self.root) {
            let name = dir
                .parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let manifest = dir.join("Cargo.toml");
            let mut fmt = cargo(&["fmt", "--check", "--manifest-path"]);
            fmt.arg(&manifest);
            self.step(
                &format!("cargo fmt --check (packages/{name}/native)"),
                &mut fmt,
            )?;
            let mut clippy = cargo(&["clippy", "--manifest-path"]);
            clippy
                .arg(&manifest)
                .arg("--target-dir")
                .arg(dir.join("../target/velt-native/cargo"))
                .args(["--all-targets", "--", "-D", "warnings"]);
            self.step(&format!("clippy (packages/{name}/native)"), &mut clippy)?;
        }
        Ok(())
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

/// The native crates of the repository's packages (`packages/*/native` with a `Cargo.toml`),
/// sorted.
fn native_crates(root: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(root.join("packages"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path().join("native"))
        .filter(|d| d.join("Cargo.toml").is_file())
        .collect();
    out.sort();
    out
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
