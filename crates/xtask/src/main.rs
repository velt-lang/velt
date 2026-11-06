//! `cargo xtask`: the repository's tooling.
//!
//! ```text
//! cargo xtask check                  the checks the changes since origin/main need
//! cargo xtask check --full           the whole gate (what the merge queue runs)
//! cargo xtask affected               print what `check` would run, and why
//! cargo xtask precheck               fmt, file sizes and clippy on the changed files (fast)
//! cargo xtask repeat                 run the changed test files many times at once (flakiness)
//! cargo xtask install-hooks          a pre-push hook that runs `precheck`
//! cargo xtask pr-lint                check a pull request's text and commits
//! ```
//!
//! Run `cargo xtask help` for every option.

mod c_headers;
#[cfg(all(test, unix))]
mod cache_key_tests;
mod changes;
mod check;
mod comments;
mod doctests;
mod file_size;
mod golden_archive;
mod graph;
mod hook_tests;
mod hooks;
mod junit;
mod os;
mod package_users;
mod plan;
mod pr_lint;
mod pr_size;
mod precheck;
mod read_by_tests;
mod repeat;
#[cfg(test)]
mod requeue_tests;
mod sweep;
mod timing_lint;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use check::{Options, Parts};
use graph::Graph;
use plan::Plan;

const HELP: &str = "\
cargo xtask <command> [options]

Commands:
  check       Run the quality gate: by default only the checks the changed files need
              (changed since the merge base with --base, plus uncommitted and untracked files)
  affected    Print the plan `check` would run, and why
  precheck    The fast checks most failed pull requests miss, on the changed files only:
              cargo fmt, velt fmt (std/, examples/), the file-size rule, clippy of the
              changed crates and their dependents, and the std hook table's tests
  repeat      Run each test file the changes add or edit (velt test files, goldens) many times at
              once, on twice as many threads as there are cores, and fail naming the file and
              the repetition that failed (crates/xtask/src/repeat.rs)
  install-hooks
              Install a git pre-push hook that runs `precheck` (shared by every worktree;
              `git push --no-verify` skips it)
  pr-lint     Check a pull request's title, description and commits for tool footers and
              trailers, session links, machine paths, review wording and the commit author
              (`cargo xtask pr-lint --help` for its options)

Options:
  --full               Check everything (the merge queue and nightly run this)
  --base <rev>         Compare with this revision (default: origin/main, else main)
  --paths <p>...       Plan for these paths instead of the git changes (with `affected`)
  --part <parts>       all (default), or a comma-separated list of: lint (fmt + clippy),
                       test (build, tests, velt fmt, smoke), golden (end-to-end goldens),
                       wasm (only the WebAssembly goldens, which `test` runs too; not in
                       `all`, not with `test`; VELT_WASM_SHARD=<i>/<n> runs one of n shards)
  --fast               Goldens in debug mode only, no smoke test
  --golden-modes <m>   debug or release (default: both, or debug with --fast)
  --no-smoke           Skip the smoke test (it runs only with --full, on Linux and macOS)
  --pack <file>        (--part golden, wasm or both) Build what those goldens run and pack it
                       into a tar instead of running them; the path is relative to the
                       repository root
  --archive <file>     (--part golden, wasm or both) Run those goldens from a `--pack` tar,
                       unpacked into a checkout at the same path, without building
                       (VELT_GOLDEN_SHARD=<i>/<n>, VELT_WASM_SHARD=<i>/<n>: one of n shards)
  --dry-run            Print the commands without running them
  --github             (affected) Write the plan to $GITHUB_OUTPUT and $GITHUB_STEP_SUMMARY
  --times <n>          (repeat) Runs per file (default 20, or VELT_REPEAT_TIMES)
  --jobs <n>           (repeat) Runs at once (default: twice the cores, or VELT_REPEAT_JOBS)
  --time-limit <s>     (repeat) No new run after this many seconds (default 600, or
                       VELT_REPEAT_TIME_LIMIT)
  --force              (install-hooks) Replace a pre-push hook xtask didn't install (kept as
                       pre-push.orig)
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
    pack: Option<String>,
    archive: Option<String>,
    dry_run: bool,
    github: bool,
    force: bool,
    times: Option<usize>,
    jobs: Option<usize>,
    time_limit: Option<u64>,
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
            "--pack" => args.pack = Some(value("--pack")?),
            "--archive" => args.archive = Some(value("--archive")?),
            "--dry-run" => args.dry_run = true,
            "--github" => args.github = true,
            "--force" => args.force = true,
            "--times" | "--jobs" | "--time-limit" => {
                let v = value(&arg)?;
                let n: u64 = v
                    .parse()
                    .map_err(|_| format!("{arg} {v}: expected a whole number"))?;
                match arg.as_str() {
                    "--times" => args.times = Some(n as usize),
                    "--jobs" => args.jobs = Some(n as usize),
                    _ => args.time_limit = Some(n),
                }
            }
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
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().is_some_and(|c| c == "pr-lint") {
        return pr_lint::main(&repo_root()?, &argv[1..]);
    }
    let args = parse(argv)?;
    let root = repo_root()?;
    match args.command.as_str() {
        "check" => {
            let plan = plan(&root, &args)?;
            println!("root: {}", root.display());
            print!("{}", plan.describe());
            let part = args.part.as_deref().unwrap_or("all");
            let parts = Parts::parse(part)?;
            if (args.pack.is_some() || args.archive.is_some()) && !parts.only_goldens() {
                return Err("--pack and --archive need `--part golden`, `wasm` or both".into());
            }
            if args.pack.is_some() && args.archive.is_some() {
                return Err("--pack builds, --archive runs without building: use one".into());
            }
            let opts = Options {
                parts,
                golden_modes: args
                    .golden_modes
                    .clone()
                    .or(args.fast.then(|| "debug".to_string())),
                smoke: !args.no_smoke && !args.fast,
                pack: args.pack.clone(),
                archive: args.archive.clone(),
                dry_run: args.dry_run,
                // `--base` even with `--full` (the merge queue passes `HEAD^1`): the revision the
                // change is checked against, which std-lint's allow lists may only shrink from.
                base: args.base.clone().or_else(|| changes::default_base(&root)),
            };
            check::run(&root, &Graph::load(&root)?, &plan, &opts)?;
            println!("\x1b[32mall selected gates passed\x1b[0m");
            Ok(())
        }
        "affected" => {
            let plan = plan(&root, &args)?;
            println!("root: {}", root.display());
            print!("{}", plan.describe());
            let flake = flake_guards(&root, &args);
            print!("{}", flake.describe());
            if args.github {
                github_outputs(&plan, &flake)?;
            }
            Ok(())
        }
        "repeat" => {
            let (sel, reason) = repeat_selection(&root, &args)?;
            println!("repeat: {reason}");
            let mut opts = repeat::Options::from_env()?;
            opts.times = args.times.unwrap_or(opts.times);
            opts.jobs = args.jobs.unwrap_or(opts.jobs);
            if let Some(secs) = args.time_limit {
                opts.time_limit = std::time::Duration::from_secs(secs);
            }
            opts.dry_run = args.dry_run;
            repeat::run(&root, &sel, &opts)
        }
        "precheck" => {
            let (paths, reason) = precheck_paths(&root, &args)?;
            let graph = Graph::load(&root)?;
            let sel = precheck::select(&graph, &paths, |p| root.join(p).exists());
            precheck::run(&root, &sel, &reason)
        }
        "install-hooks" => {
            println!("{}", hooks::describe(&hooks::install(&root, args.force)?));
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
        let mut plan = Plan::everything("--full");
        if std::env::var("VELT_WASM_GOLDENS").as_deref() == Ok("skip") {
            plan.wasm = false;
            plan.reasons
                .push("VELT_WASM_GOLDENS=skip: no WebAssembly goldens".into());
        }
        return Ok(plan);
    }
    let graph = Graph::load(root)?;
    if let Some(paths) = &args.paths {
        return Ok(Plan::for_paths(&graph, paths));
    }
    let Some(base) = args.base.clone().or_else(|| changes::default_base(root)) else {
        return Ok(Plan::unknown_changes(
            "no base revision (origin/main or main) to compare with",
        ));
    };
    match changes::changes(root, &base) {
        Ok(c) => Ok(Plan::for_changes(&graph, &c.paths, &c.comment_only)),
        Err(e) => Ok(Plan::unknown_changes(e)),
    }
}

/// The test files `repeat` runs (`--paths`, else the changes since the merge base with the
/// base revision), and where they come from.
fn repeat_selection(root: &Path, args: &Args) -> Result<(repeat::Selection, String), String> {
    let exists = |p: &str| root.join(p).is_file();
    let list_dir = |dir: &str| -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
            return vec![];
        };
        entries
            .flatten()
            .filter(|e| e.path().is_file())
            .map(|e| format!("{dir}/{}", e.file_name().to_string_lossy()))
            .collect()
    };
    if let Some(paths) = &args.paths {
        let sel = repeat::select(paths, &Default::default(), exists, list_dir);
        return Ok((sel, format!("{} given path(s)", paths.len())));
    }
    let base = args
        .base
        .clone()
        .or_else(|| changes::default_base(root))
        .ok_or("no base revision (origin/main or main) to compare with: pass --base or --paths")?;
    let c = changes::changes(root, &base)?;
    let sel = repeat::select(&c.paths, &c.velt_comment_only, exists, list_dir);
    Ok((
        sel,
        format!("{} file(s) changed since {base}", c.paths.len()),
    ))
}

/// What a pull request runs against flaky tests besides the plan: the changed test files
/// repeated under load (`repeat`, on Linux and Windows) and the packages' tests on Windows.
struct FlakeGuards {
    repeat: Vec<String>,
    /// `VELT_PACKAGES` for Windows: `""` none, `"all"`, or names ([`os::windows_packages`]).
    windows_packages: String,
}

impl FlakeGuards {
    fn describe(&self) -> String {
        let repeat = match self.repeat.len() {
            0 => "none (no test file changed)".to_string(),
            _ => self.repeat.join(", "),
        };
        let packages = match self.windows_packages.as_str() {
            "" => "none",
            other => other,
        };
        format!(
            "  repeated under load (cargo xtask repeat): {repeat}\n  \
             package tests on Windows (pull request): {packages}\n"
        )
    }
}

/// [`FlakeGuards`] for the changes: none with `--full` (the merge queue tests every package on
/// every OS it checks) or when the changes can't be told.
fn flake_guards(root: &Path, args: &Args) -> FlakeGuards {
    let none = FlakeGuards {
        repeat: vec![],
        windows_packages: String::new(),
    };
    if args.full {
        return none;
    }
    let (paths, skip) = match &args.paths {
        Some(paths) => (paths.clone(), Default::default()),
        None => {
            let Some(base) = args.base.clone().or_else(|| changes::default_base(root)) else {
                return none;
            };
            match changes::changes(root, &base) {
                Ok(c) => (c.paths, c.velt_comment_only),
                Err(_) => return none,
            }
        }
    };
    let repeat = match repeat_selection(root, args) {
        Ok((sel, _)) => sel.velt_tests.into_iter().chain(sel.goldens).collect(),
        Err(_) => vec![],
    };
    FlakeGuards {
        repeat,
        windows_packages: os::windows_packages(&paths, &skip),
    }
}

/// The files `precheck` looks at, and why: `--paths`, else the changes since the merge base
/// with the base revision, else (no base to compare with) every file in git.
fn precheck_paths(root: &Path, args: &Args) -> Result<(Vec<String>, String), String> {
    if let Some(paths) = &args.paths {
        return Ok((paths.clone(), format!("{} given path(s)", paths.len())));
    }
    let base = args.base.clone().or_else(|| changes::default_base(root));
    let why = match &base {
        Some(base) => match changes::changes(root, base) {
            Ok(c) => {
                let n = c.paths.len();
                return Ok((c.paths, format!("{n} file(s) changed since {base}")));
            }
            Err(e) => e,
        },
        None => "no base revision (origin/main or main) to compare with".into(),
    };
    let all = changes::git(root, &["ls-files"])?
        .lines()
        .map(|l| l.trim().replace('\\', "/"))
        .filter(|l| !l.is_empty())
        .collect();
    Ok((all, format!("every file ({why})")))
}

/// The `$GITHUB_OUTPUT` lines for `plan`: which parts have anything to do, the OSes the queue
/// checks (`os`), the OSes besides Linux a pull request lints on (`other_os`, the same ones: PR CI
/// otherwise runs on Linux only), and how the
/// parser's robustness test cuts the goldens in the queue, which runs `--full` and so knows no
/// changes (`robustness`, `robustness_files`: `VELT_ROBUSTNESS`, `VELT_ROBUSTNESS_FILES`; the
/// formatter's corpus: `fmt_corpus`, `fmt_corpus_files`), nor
/// whether the WebAssembly goldens run (`wasm_goldens`: `skip` when they don't). The goldens and
/// the WebAssembly goldens run in shards per OS (`golden_shards`, `wasm_shards`). After a push to
/// `main`, `main_os` lists the OSes of the `main` job: Linux (it saves the build cache) and,
/// when [`Plan::after_merge`] says so, Windows and macOS for the whole gate. `c_headers` says
/// whether the `c-headers` job runs ([`Plan::c_headers`]).
fn github_output_text(plan: &Plan) -> String {
    let flag = |b: bool| if b { "true" } else { "false" };
    // Linux, always, stays first.
    let oses: Vec<&str> = os::QUEUE_OSES
        .iter()
        .copied()
        .filter(|name| match *name {
            "windows-latest" => plan.other_os.is_some(),
            "macos-latest" => plan.macos.is_some(),
            _ => true,
        })
        .collect();
    let oses = oses.as_slice();
    let main_os = if plan.after_merge.is_some() {
        r#"["ubuntu-latest","windows-latest","macos-latest"]"#
    } else {
        r#"["ubuntu-latest"]"#
    };
    let json = |list: &[&str]| {
        let quoted: Vec<String> = list.iter().map(|os| format!("\"{os}\"")).collect();
        format!("[{}]", quoted.join(","))
    };
    let (os, other_os) = (json(oses), json(&oses[1..]));
    let golden_shards = os::golden_shards_json(oses);
    let wasm_shards = os::wasm_shards_json(oses);
    let mut text = format!(
        "full={}\nlint={}\ntest={}\ngolden={}\nos={os}\nother_os={other_os}\nmain_os={main_os}\n\
         golden_shards={golden_shards}\nwasm_shards={wasm_shards}\n",
        flag(plan.full),
        "true",
        flag(plan.needs_build() || plan.difftest || plan.vscode),
        flag(plan.goldens != plan::Goldens::None),
    );
    let sweep = plan.sweep_env();
    for (output, var) in [
        ("robustness", "VELT_ROBUSTNESS"),
        ("robustness_files", "VELT_ROBUSTNESS_FILES"),
        ("fmt_corpus", "VELT_FMT_CORPUS"),
        ("fmt_corpus_files", "VELT_FMT_CORPUS_FILES"),
        ("debug_vars", "VELT_DEBUG_VARS"),
    ] {
        let value = sweep.iter().find(|(v, _)| *v == var).map_or("", |(_, x)| x);
        text.push_str(&format!("{output}={value}\n"));
    }
    text.push_str(&format!("c_headers={}\n", flag(plan.c_headers)));
    let wasm = if plan.wasm { "" } else { "skip" };
    text.push_str(&format!("wasm_goldens={wasm}\n"));
    text
}

/// For CI: which parts have anything to do, and the plan as the job summary.
fn github_outputs(plan: &Plan, flake: &FlakeGuards) -> Result<(), String> {
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
    append("GITHUB_OUTPUT", &github_output_text(plan))?;
    let repeat = if flake.repeat.is_empty() {
        "false"
    } else {
        "true"
    };
    append(
        "GITHUB_OUTPUT",
        &format!(
            "repeat={repeat}\nwindows_packages={}\n",
            flake.windows_packages
        ),
    )?;
    append(
        "GITHUB_STEP_SUMMARY",
        &format!("```\n{}{}```\n", plan.describe(), flake.describe()),
    )
}

/// The repository root, found at run time: the nearest ancestor of the current directory whose
/// `Cargo.toml` has a `[workspace]` table. Never the root xtask was compiled in: worktrees that
/// share one target directory share one xtask binary, which would then check another worktree.
fn repo_root() -> Result<PathBuf, String> {
    let cwd =
        std::env::current_dir().map_err(|e| format!("cannot read the current directory: {e}"))?;
    workspace_root(&cwd)
}

/// The nearest ancestor of `start` (itself included) whose `Cargo.toml` declares `[workspace]`.
fn workspace_root(start: &Path) -> Result<PathBuf, String> {
    start
        .ancestors()
        .find(|dir| {
            std::fs::read_to_string(dir.join("Cargo.toml"))
                .is_ok_and(|text| text.lines().any(|l| l.trim() == "[workspace]"))
        })
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            format!(
                "`{}` is not inside a Cargo workspace (run `cargo xtask` from the repository)",
                start.display()
            )
        })
}

#[cfg(test)]
mod root_tests {
    use super::*;

    #[test]
    fn the_root_is_the_nearest_workspace_above_the_current_directory() {
        let base = std::env::temp_dir().join(format!("xtask-root-{}", std::process::id()));
        let member = base.join("ws").join("crates").join("a").join("src");
        std::fs::create_dir_all(&member).unwrap();
        std::fs::write(
            base.join("ws").join("Cargo.toml"),
            "[workspace]
members = [\"crates/*\"]
",
        )
        .unwrap();
        std::fs::write(
            base.join("ws").join("crates").join("a").join("Cargo.toml"),
            "[package]
name = \"a\"
",
        )
        .unwrap();
        // From a member's directory, the member's own Cargo.toml (no [workspace]) is skipped.
        assert_eq!(workspace_root(&member).unwrap(), base.join("ws"));
        assert_eq!(workspace_root(&base.join("ws")).unwrap(), base.join("ws"));
        let outside = base.join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        let err = workspace_root(&outside);
        // The temp directory itself may sit in a workspace on a developer machine; only check
        // that an answer is never inside `elsewhere`.
        assert!(err.map_or(true, |r| !r.starts_with(&outside)));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn this_process_runs_in_this_repository() {
        // `cargo test` runs xtask's tests from crates/xtask: the root is two levels up.
        let here = std::env::current_dir().unwrap();
        let root = repo_root().unwrap();
        assert!(
            here.starts_with(&root),
            "{} vs {}",
            here.display(),
            root.display()
        );
        assert!(root.join("crates").join("xtask").is_dir());
    }

    #[test]
    fn other_oses_lint_a_pull_request_only_for_os_specific_changes() {
        let everything = Plan::everything("test");
        assert!(everything.other_os.is_some());
        let text = github_output_text(&everything);
        assert!(
            text.contains("\nother_os=[\"windows-latest\",\"macos-latest\"]\n"),
            "{text}"
        );
        assert!(text.contains("\nos=[\"ubuntu-latest\",\"windows-latest\",\"macos-latest\"]\n"));
        let mut linux_only = Plan::everything("test");
        linux_only.other_os = None;
        linux_only.macos = None;
        let text = github_output_text(&linux_only);
        assert!(
            text.contains("\nother_os=[]\n") && text.contains("\nos=[\"ubuntu-latest\"]\n"),
            "{text}"
        );
        // The queue's golden shards follow the OSes.
        assert!(
            text.contains("\ngolden_shards={\"ubuntu-latest\":[\"1/"),
            "{text}"
        );
        let shards = text.lines().find(|l| l.starts_with("golden_shards="));
        assert!(shards.is_some_and(|l| !l.contains("windows")), "{text}");
    }

    #[test]
    fn windows_and_macos_are_decided_apart() {
        let line = |text: &str, name: &str| {
            let prefix = format!("{name}=");
            text.lines()
                .find_map(|l| l.strip_prefix(prefix.as_str()).map(str::to_string))
                .unwrap_or_default()
        };
        // A CI or loader change: Windows, and no macOS job of any kind.
        let mut windows = Plan::everything("test");
        windows.macos = None;
        let text = github_output_text(&windows);
        assert_eq!(line(&text, "os"), r#"["ubuntu-latest","windows-latest"]"#);
        assert_eq!(line(&text, "other_os"), r#"["windows-latest"]"#);
        for shards in ["golden_shards", "wasm_shards"] {
            let shards = line(&text, shards);
            assert!(
                shards.contains("windows") && !shards.contains("macos"),
                "{text}"
            );
        }
        // A package's file-path code: macOS, and no Windows job.
        let mut macos = Plan::everything("test");
        macos.other_os = None;
        let text = github_output_text(&macos);
        assert_eq!(line(&text, "os"), r#"["ubuntu-latest","macos-latest"]"#);
        assert_eq!(line(&text, "other_os"), r#"["macos-latest"]"#);
        for shards in ["golden_shards", "wasm_shards"] {
            let shards = line(&text, shards);
            assert!(
                shards.contains("macos") && !shards.contains("windows"),
                "{text}"
            );
        }
    }

    #[test]
    fn every_os_the_queue_checks_has_golden_shards() {
        let mut linux_only = Plan::everything("test");
        linux_only.other_os = None;
        linux_only.macos = None;
        let mut no_macos = Plan::everything("test");
        no_macos.macos = None;
        let mut no_windows = Plan::everything("test");
        no_windows.other_os = None;
        for plan in [Plan::everything("test"), linux_only, no_macos, no_windows] {
            let text = github_output_text(&plan);
            let line = |name: &str| {
                let prefix = format!("{name}=");
                text.lines()
                    .find_map(|l| l.strip_prefix(prefix.as_str()))
                    .unwrap_or_default()
                    .to_string()
            };
            let oses = line("os");
            for shards in [line("golden_shards"), line("wasm_shards")] {
                for os in os::QUEUE_OSES.iter().filter(|os| oses.contains(*os)) {
                    assert!(shards.contains(&format!("\"{os}\":[\"1/")), "{text}");
                }
            }
        }
    }

    #[test]
    fn main_checks_windows_and_macos_only_after_a_linux_only_queue_run() {
        let mut plan = Plan::everything("test");
        plan.after_merge = None;
        let text = github_output_text(&plan);
        assert!(text.contains("\nmain_os=[\"ubuntu-latest\"]\n"), "{text}");
        plan.after_merge = Some("Cargo.lock changed: refresh the build cache".into());
        let text = github_output_text(&plan);
        assert!(
            text.contains("\nmain_os=[\"ubuntu-latest\",\"windows-latest\",\"macos-latest\"]\n"),
            "{text}"
        );
    }

    #[test]
    fn the_queue_learns_how_to_cut_the_goldens() {
        let mut plan = Plan::everything("test");
        let text = github_output_text(&plan);
        assert!(
            text.contains("\nrobustness=\nrobustness_files=\n"),
            "{text}"
        );
        plan.sweep = sweep::Sweep::Changed(
            ["tests/golden/a/x.velt", "tests/golden/b.velt"]
                .map(String::from)
                .into(),
        );
        plan.fmt_corpus = sweep::Sweep::Changed(["std/a.velt"].map(String::from).into());
        let text = github_output_text(&plan);
        assert!(
            text.contains(
                "\nrobustness=changed\nrobustness_files=tests/golden/a/x.velt,tests/golden/b.velt\n\
                 fmt_corpus=changed\nfmt_corpus_files=std/a.velt\n"
            ),
            "{text}"
        );
        plan.sweep = sweep::Sweep::Full;
        plan.fmt_corpus = sweep::Sweep::Full;
        let text = github_output_text(&plan);
        assert!(
            text.contains("\nrobustness=full\nrobustness_files=\nfmt_corpus=\nfmt_corpus_files=\ndebug_vars=\n"),
            "{text}"
        );
    }

    #[test]
    fn ci_learns_whether_the_header_check_runs() {
        let mut plan = Plan::everything("test");
        assert!(github_output_text(&plan).contains("\nc_headers=false\n"));
        plan.c_headers = true;
        assert!(github_output_text(&plan).contains("\nc_headers=true\n"));
    }

    #[test]
    fn the_queue_learns_whether_the_webassembly_goldens_run() {
        let mut plan = Plan::everything("test");
        let text = github_output_text(&plan);
        assert!(text.ends_with("\nwasm_goldens=\n"), "{text}");
        plan.wasm = false;
        let text = github_output_text(&plan);
        assert!(text.ends_with("\nwasm_goldens=skip\n"), "{text}");
    }
}
