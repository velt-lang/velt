//! `cargo xtask`: the repository's tooling.
//!
//! ```text
//! cargo xtask check                  the checks the changes since origin/main need
//! cargo xtask check --full           the whole gate (what the merge queue runs)
//! cargo xtask affected               print what `check` would run, and why
//! ```
//!
//! Run `cargo xtask help` for every option.

mod changes;
mod check;
mod doctests;
mod graph;
mod plan;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use check::{Options, Part};
use graph::Graph;
use plan::Plan;

const HELP: &str = "\
cargo xtask <command> [options]

Commands:
  check       Run the quality gate: by default only the checks the changed files need
              (changed since the merge base with --base, plus uncommitted and untracked files)
  affected    Print the plan `check` would run, and why

Options:
  --full               Check everything (the merge queue and nightly run this)
  --base <rev>         Compare with this revision (default: origin/main, else main)
  --paths <p>...       Plan for these paths instead of the git changes (with `affected`)
  --part <part>        all (default), lint (fmt + clippy), test (build, tests, velt fmt,
                       smoke) or golden (end-to-end goldens)
  --fast               Goldens in debug mode only, no smoke test
  --golden-modes <m>   debug or release (default: both, or debug with --fast)
  --no-smoke           Skip the smoke test (it runs only with --full, on Linux and macOS)
  --dry-run            Print the commands without running them
  --github             (affected) Write the plan to $GITHUB_OUTPUT and $GITHUB_STEP_SUMMARY
";

#[derive(Default)]
struct Args {
    command: String,
    full: bool,
    base: Option<String>,
    paths: Option<Vec<String>>,
    part: Option<String>,
    fast: bool,
    golden_modes: Option<String>,
    no_smoke: bool,
    dry_run: bool,
    github: bool,
}

fn parse(argv: Vec<String>) -> Result<Args, String> {
    let mut args = Args::default();
    let mut it = argv.into_iter().peekable();
    args.command = it.next().unwrap_or_else(|| "help".into());
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--full" => args.full = true,
            "--base" => args.base = Some(value("--base")?),
            "--part" => args.part = Some(value("--part")?),
            "--golden-modes" => args.golden_modes = Some(value("--golden-modes")?),
            "--fast" => args.fast = true,
            "--no-smoke" => args.no_smoke = true,
            "--dry-run" => args.dry_run = true,
            "--github" => args.github = true,
            "--paths" => {
                let mut paths = vec![];
                while let Some(p) = it.next_if(|p| !p.starts_with("--")) {
                    paths.push(p);
                }
                args.paths = Some(paths);
            }
            other => return Err(format!("unknown option `{other}` (see `cargo xtask help`)")),
        }
    }
    Ok(args)
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("\x1b[31m{e}\x1b[0m");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args = parse(std::env::args().skip(1).collect())?;
    let root = repo_root();
    match args.command.as_str() {
        "check" => {
            let plan = plan(&root, &args)?;
            print!("{}", plan.describe());
            let part = args.part.as_deref().unwrap_or("all");
            let opts = Options {
                part: Part::parse(part).ok_or(format!("unknown part `{part}`"))?,
                golden_modes: args
                    .golden_modes
                    .clone()
                    .or(args.fast.then(|| "debug".to_string())),
                smoke: !args.no_smoke && !args.fast,
                dry_run: args.dry_run,
            };
            check::run(&root, &Graph::load(&root)?, &plan, &opts)?;
            println!("\x1b[32mall selected gates passed\x1b[0m");
            Ok(())
        }
        "affected" => {
            let plan = plan(&root, &args)?;
            print!("{}", plan.describe());
            if args.github {
                github_outputs(&plan)?;
            }
            Ok(())
        }
        "help" | "--help" | "-h" => {
            print!("{HELP}");
            Ok(())
        }
        other => Err(format!("unknown command `{other}`\n\n{HELP}")),
    }
}

fn plan(root: &Path, args: &Args) -> Result<Plan, String> {
    if args.full {
        return Ok(Plan::everything("--full"));
    }
    let graph = Graph::load(root)?;
    let paths = match &args.paths {
        Some(paths) => paths.clone(),
        None => {
            let Some(base) = args.base.clone().or_else(|| changes::default_base(root)) else {
                return Ok(Plan::everything(
                    "no base revision (origin/main or main) to compare with",
                ));
            };
            match changes::changed_paths(root, &base) {
                Ok(paths) => paths,
                Err(e) => return Ok(Plan::everything(e)),
            }
        }
    };
    Ok(Plan::for_paths(&graph, &paths))
}

/// For CI: which parts have anything to do, and the plan as the job summary.
fn github_outputs(plan: &Plan) -> Result<(), String> {
    let append = |var: &str, text: &str| -> Result<(), String> {
        let Some(path) = std::env::var_os(var) else {
            return Ok(());
        };
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&path)
            .map_err(|e| format!("{var}: {e}"))?;
        f.write_all(text.as_bytes())
            .map_err(|e| format!("{var}: {e}"))
    };
    let flag = |b: bool| if b { "true" } else { "false" };
    let outputs = format!(
        "full={}\nlint={}\ntest={}\ngolden={}\n",
        flag(plan.full),
        "true",
        flag(plan.needs_build()),
        flag(plan.goldens != plan::Goldens::None),
    );
    append("GITHUB_OUTPUT", &outputs)?;
    append(
        "GITHUB_STEP_SUMMARY",
        &format!("```\n{}```\n", plan.describe()),
    )
}

/// The repository root: two levels above this crate's manifest.
fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or(manifest)
}
