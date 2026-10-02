//! The quality gate: the steps a [`Plan`] needs, in order, timed. One implementation for every
//! OS, for local runs and for CI (which runs the parts as parallel jobs).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use crate::doctests;
use crate::graph::Graph;
use crate::plan::{Goldens, Plan};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    All,
    /// `cargo fmt` and clippy.
    Lint,
    /// Build, unit and integration tests, doctests, `velt fmt --check`, smoke test.
    Test,
    /// The end-to-end goldens.
    Golden,
}

impl Part {
    pub fn parse(s: &str) -> Option<Part> {
        Some(match s {
            "all" => Part::All,
            "lint" => Part::Lint,
            "test" => Part::Test,
            "golden" => Part::Golden,
            _ => return None,
        })
    }

    fn has(self, part: Part) -> bool {
        self == Part::All || self == part
    }
}

pub struct Options {
    pub part: Part,
    /// `VELT_GOLDEN_MODES`: `debug`, `release`, or both (`None`).
    pub golden_modes: Option<String>,
    pub smoke: bool,
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
    let part = opts.part;
    if part.has(Part::Lint) {
        gate.step(
            "cargo fmt --check",
            &mut cargo(&["fmt", "--all", "--check"]),
        )?;
    }
    let build = (part.has(Part::Test) && plan.needs_build())
        || (part.has(Part::Golden) && plan.goldens != Goldens::None);
    if build {
        // Every later step reuses this build: the same packages and targets resolve the same
        // features, so nothing compiles twice (`VELT_RT_PREBUILT` below).
        gate.step(
            "build",
            cargo(&["build"]).args(&build_ws).arg("--all-targets"),
        )?;
    }
    if part.has(Part::Lint) && plan.rust {
        let mut clippy = cargo(&["clippy"]);
        clippy
            .args(&ws)
            .args(["--all-targets", "--", "-D", "warnings"]);
        gate.step("clippy", &mut clippy)?;
    }
    if part.has(Part::Test) {
        gate.tests(plan, graph, &ws)?;
    }
    if part.has(Part::Golden) && plan.goldens != Goldens::None {
        let mut goldens = cargo(&["test"]);
        // `--workspace` resolves like the build above (`-p veltc` would compile velt_opt and
        // veltc again); `--exact golden` leaves velt_syntax's parser goldens to the tests.
        goldens
            .args(&ws)
            .args(["--test", "golden", "--", "--nocapture", "--exact", "golden"]);
        goldens.env("VELT_RT_PREBUILT", "1");
        if let Some(filter) = plan.golden_env() {
            goldens.env("VELT_GOLDEN", filter);
        }
        if let Some(modes) = &opts.golden_modes {
            goldens.env("VELT_GOLDEN_MODES", modes);
        }
        let modes = opts.golden_modes.as_deref().unwrap_or("debug + release");
        gate.step(&format!("goldens ({modes})"), &mut goldens)?;
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

impl Gate<'_> {
    fn tests(&mut self, plan: &Plan, graph: &Graph, ws: &[String]) -> Result<(), String> {
        let Some(filterset) = plan.filterset() else {
            return Ok(());
        };
        let mut tests = if has_nextest() {
            let mut c = cargo(&["nextest", "run"]);
            c.args(ws).args(["--no-tests", "pass", "-E", &filterset]);
            if std::env::var_os("CI").is_some() {
                c.args(["--profile", "ci"]);
            }
            c
        } else {
            println!("note: cargo-nextest is not installed, so every test runs, one binary at a");
            println!(
                "      time (`cargo install cargo-nextest --locked` runs only the selected ones,"
            );
            println!("      in parallel).");
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
        self.step("unit + integration tests", &mut tests)?;
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

fn cargo(args: &[&str]) -> Command {
    let mut c = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    c.args(args);
    c
}

/// `--workspace`, minus the shared runtime on musl hosts (musl links statically and cannot build
/// a cdylib; `velt` falls back to the static runtime).
fn workspace_args(root: &Path) -> Vec<String> {
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

fn exe(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

fn show(cmd: &Command) -> String {
    let program = Path::new(cmd.get_program());
    let mut s = match program.file_stem() {
        Some(stem) if stem == "cargo" => "cargo".to_string(),
        _ => program.to_string_lossy().into_owned(),
    };
    for (k, v) in cmd.get_envs() {
        if let Some(v) = v {
            s = format!("{}={} {s}", k.to_string_lossy(), v.to_string_lossy());
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
