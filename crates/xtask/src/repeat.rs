//! `cargo xtask repeat`: the test files a change adds or edits, each run many times at once, so a
//! test that orders events by timing fails before it reaches `main` instead of in the merge queue
//! of someone else's pull request.
//!
//! The changed files (crates/xtask/src/changes.rs) select:
//! - `velt test` files (`*.test.vlt`, `.test.ts`, `.test.tsx`) under `packages/`, `std/`,
//!   `tests/std/` and `examples/`; a changed helper next to them (a `.vlt` file in the same
//!   directory of a `tests/` tree) selects the test files of its directory;
//! - goldens: a changed `tests/golden/**/<name>.vlt` with a `<name>.out`, or a changed `.out`,
//!   `.stderr` or `.code` of one (programs that must not compile, `.err`, are deterministic).
//!
//! A file whose change is only whole-line comments is skipped (a `// timing:` marker added),
//! unless the comment is a golden's `// check:` or `// requires` directive.
//!
//! Each file then runs [`DEFAULT_TIMES`] times (`--times`, `VELT_REPEAT_TIMES`), the runs of all
//! files on twice as many threads as the machine has cores (`--jobs`), beside one thread per core
//! that only spins (`VELT_REPEAT_BURNERS`), so each run competes for the CPU as on a loaded CI
//! runner, however few runs there are. A `velt test` file is built once (`velt test <file> --no-run`)
//! and its test program run by hand, as `velt test` runs it; a golden runs through veltc's golden
//! test (`VELT_GOLDEN=<file>`, debug mode). Every run has a temporary directory and a golden
//! build directory of its own. After `--time-limit` seconds (default 600,
//! `VELT_REPEAT_TIME_LIMIT`) no new run starts, and the report says how many ran. A failing run
//! fails the command, naming the file and the repetition, with the end of its output; the full
//! output is kept in `<target>/repeat/logs`.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::check::{cargo, exe, show, target_dir, workspace_args};
use crate::golden_archive;

/// How many times each file runs by default.
pub const DEFAULT_TIMES: usize = 20;
/// Seconds after which no new run starts, by default.
pub const DEFAULT_TIME_LIMIT: u64 = 600;
/// A run still going after this many seconds is stopped by the test runner (`VELT_TEST_TIMEOUT`,
/// `VELT_GOLDEN_TIMEOUT`), unless the environment sets them.
const RUN_TIMEOUT_SECS: u64 = 300;
/// Lines of a failed run's output printed in the report.
const TAIL_LINES: usize = 60;

/// Where `velt test` files live.
const VELT_TEST_DIRS: &[&str] = &["packages/", "std/", "tests/std/", "examples/"];
const TEST_SUFFIXES: &[&str] = &[".test.vlt", ".test.ts", ".test.tsx"];

/// The files to repeat, relative to the repository root, `/`-separated.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub velt_tests: BTreeSet<String>,
    pub goldens: BTreeSet<String>,
}

impl Selection {
    pub fn is_empty(&self) -> bool {
        self.velt_tests.is_empty() && self.goldens.is_empty()
    }

    pub fn len(&self) -> usize {
        self.velt_tests.len() + self.goldens.len()
    }
}

fn is_velt_test(path: &str) -> bool {
    VELT_TEST_DIRS.iter().any(|d| path.starts_with(d))
        && TEST_SUFFIXES.iter().any(|s| path.ends_with(s))
}

/// The files to repeat for a change touching `paths`: `comment_only` are skipped, `exists` says
/// whether a file is there, `list_dir` lists the files of a directory (relative paths).
pub fn select(
    paths: &[String],
    comment_only: &BTreeSet<String>,
    exists: impl Fn(&str) -> bool,
    list_dir: impl Fn(&str) -> Vec<String>,
) -> Selection {
    let mut sel = Selection::default();
    for path in paths.iter().filter(|p| !comment_only.contains(*p)) {
        if is_velt_test(path) {
            if exists(path) {
                sel.velt_tests.insert(path.clone());
            }
        } else if let Some(golden) = golden_program(path) {
            let out = format!("{}.out", golden.strip_suffix(".vlt").unwrap_or(&golden));
            if exists(&golden) && exists(&out) {
                sel.goldens.insert(golden);
            }
        } else if path.ends_with(".vlt")
            && VELT_TEST_DIRS.iter().any(|d| path.starts_with(d))
            && (path.starts_with("tests/std/") || path.contains("/tests/"))
        {
            // A helper of the tests in its directory.
            let dir = path.rsplit_once('/').map_or("", |(d, _)| d);
            sel.velt_tests
                .extend(list_dir(dir).into_iter().filter(|f| is_velt_test(f)));
        }
    }
    sel
}

/// The golden program a changed file belongs to: `tests/golden/<dir>/<name>.vlt` for its
/// `.vlt`, `.out`, `.stderr` or `.code`, if it is a program (not a `_helper`, not in a `_dir`,
/// not a manifest).
fn golden_program(path: &str) -> Option<String> {
    let rest = path.strip_prefix("tests/golden/")?;
    if rest.split('/').any(|part| part.starts_with('_')) {
        return None;
    }
    let (stem, ext) = path.rsplit_once('.')?;
    if !["vlt", "out", "stderr", "code"].contains(&ext) || stem.ends_with("/package") {
        return None;
    }
    Some(format!("{stem}.vlt"))
}

/// Whether two versions of a Velt file differ only in whole-line `//` comments (and blank
/// lines). A golden's `// check:` and `// requires` lines are directives, not comments.
pub fn same_vlt_code(old: &str, new: &str) -> bool {
    let code = |text: &str| -> Vec<String> {
        text.lines()
            .map(str::trim)
            .filter(|l| {
                let directive = l.starts_with("// check:") || l.starts_with("// requires");
                !l.is_empty() && (!l.starts_with("//") || directive)
            })
            .map(String::from)
            .collect()
    };
    code(old) == code(new)
}

/// The command line options of `repeat`.
pub struct Options {
    pub times: usize,
    pub jobs: usize,
    pub time_limit: Duration,
    /// Threads that only spin while the runs go on, one per core by default.
    pub burners: usize,
    pub dry_run: bool,
}

impl Options {
    /// The defaults, or the environment's `VELT_REPEAT_TIMES`, `VELT_REPEAT_JOBS` and
    /// `VELT_REPEAT_TIME_LIMIT` (seconds).
    pub fn from_env() -> Result<Options, String> {
        let var = |name: &str, default: u64| -> Result<u64, String> {
            match std::env::var(name) {
                Ok(v) if !v.trim().is_empty() => v
                    .trim()
                    .parse()
                    .map_err(|_| format!("{name}={v}: expected a whole number")),
                _ => Ok(default),
            }
        };
        let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
        Ok(Options {
            times: var("VELT_REPEAT_TIMES", DEFAULT_TIMES as u64)? as usize,
            jobs: var("VELT_REPEAT_JOBS", 2 * cores as u64)? as usize,
            time_limit: Duration::from_secs(var("VELT_REPEAT_TIME_LIMIT", DEFAULT_TIME_LIMIT)?),
            burners: var("VELT_REPEAT_BURNERS", cores as u64)? as usize,
            dry_run: false,
        })
    }
}

/// One file to run: how to start one run of it.
struct Target {
    file: String,
    kind: Kind,
}

enum Kind {
    /// A `velt test` file's test program, run in `dir` with `VELT_SCRIPT` set to the file.
    Harness { exe: PathBuf, dir: PathBuf },
    /// A golden, through veltc's golden test binary.
    Golden { test: PathBuf },
}

struct Outcome {
    file: usize,
    rep: usize,
    ok: bool,
    secs: f64,
    log: String,
}

pub fn run(root: &Path, sel: &Selection, opts: &Options) -> Result<(), String> {
    if sel.is_empty() {
        println!("repeat: no test file changed, nothing to repeat");
        return Ok(());
    }
    if opts.times == 0 || opts.jobs == 0 {
        return Err("--times and --jobs must be at least 1".into());
    }
    println!(
        "repeat: {} file(s), {} times each, {} at once beside {} spinning threads, no new run \
         after {} s",
        sel.len(),
        opts.times,
        opts.jobs,
        opts.burners,
        opts.time_limit.as_secs()
    );
    for f in sel.velt_tests.iter().chain(&sel.goldens) {
        println!("  {f}");
    }
    let start = Instant::now();
    let targets = prepare(root, sel, opts.dry_run)?;
    let build_secs = start.elapsed().as_secs_f64();
    if opts.dry_run {
        return Ok(());
    }
    let work = target_dir(root).join("repeat");
    let logs = work.join("logs");
    std::fs::create_dir_all(&logs).map_err(|e| format!("{}: {e}", logs.display()))?;
    let runs_start = Instant::now();
    let outcomes = run_all(root, &targets, &work, opts);
    let run_secs = runs_start.elapsed().as_secs_f64();
    report(&targets, &outcomes, &logs, opts, build_secs, run_secs)
}

/// Builds what the runs need: `velt` and the runtimes (a no-op after the gate's build, which
/// resolves the same features), the golden test binary, and each `velt test` file's program.
fn prepare(root: &Path, sel: &Selection, dry_run: bool) -> Result<Vec<Target>, String> {
    let ws = workspace_args(root);
    let mut build = cargo(&["build"]);
    build
        .args(&ws)
        .args(["--exclude", "xtask", "--lib", "--bins"]);
    // A test target resolves the dev-dependencies' features as the gate's `--all-targets` does.
    build.args(["--test", "golden"]);
    step(root, "build", &mut build, dry_run)?;
    let mut targets = vec![];
    if !sel.goldens.is_empty() {
        let mut find = cargo(&["test"]);
        find.args(&ws)
            .args(["--test", "golden", "--no-run", "--message-format=json"]);
        println!(
            "\x1b[36m==> the golden test binary\x1b[0m\n    {}",
            show(&find)
        );
        let test = if dry_run {
            PathBuf::from("target/debug/deps/golden-<hash>")
        } else {
            let out = find
                .current_dir(root)
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
            PathBuf::from(
                golden_archive::test_binary(&messages, "golden")
                    .ok_or("cargo built no `golden` test binary for veltc")?,
            )
        };
        for file in &sel.goldens {
            targets.push(Target {
                file: file.clone(),
                kind: Kind::Golden { test: test.clone() },
            });
        }
    }
    let velt = target_dir(root).join("debug").join(exe("velt"));
    for file in &sel.velt_tests {
        let dir = package_dir(root, file);
        let mut build = Command::new(&velt);
        build
            .arg("test")
            .arg(root.join(file))
            .arg("--no-run")
            .env("NO_COLOR", "1")
            .env_remove("CLICOLOR_FORCE");
        println!(
            "\x1b[36m==> build {file}\x1b[0m\n    (in {}) {}",
            dir.display(),
            show(&build)
        );
        if dry_run {
            continue;
        }
        let out = build
            .current_dir(&dir)
            .output()
            .map_err(|e| format!("cannot run {}: {e}", velt.display()))?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let exe = stdout
            .lines()
            .map(str::trim)
            .map(PathBuf::from)
            .find(|p| p.is_file());
        match (out.status.success(), exe) {
            (true, Some(exe)) => targets.push(Target {
                file: file.clone(),
                kind: Kind::Harness { exe, dir },
            }),
            _ => {
                return Err(format!(
                    "`velt test {file} --no-run` built no test program:\n{stdout}{}",
                    String::from_utf8_lossy(&out.stderr)
                ))
            }
        }
    }
    Ok(targets)
}

/// The directory `velt test` runs a file's tests in: its package (the nearest directory above it
/// with a `package.vlt`, within the repository), else its own directory.
fn package_dir(root: &Path, file: &str) -> PathBuf {
    let path = root.join(file);
    let mut dir = path.parent();
    while let Some(d) = dir {
        if !d.starts_with(root) || d == root {
            break;
        }
        if d.join("package.vlt").is_file() {
            return d.to_path_buf();
        }
        dir = d.parent();
    }
    path.parent().unwrap_or(root).to_path_buf()
}

/// Runs every repetition of every target on `opts.jobs` threads, interleaved (repetition 1 of
/// every file, then 2, …), until all ran or the time limit passed.
fn run_all(root: &Path, targets: &[Target], work: &Path, opts: &Options) -> Vec<Outcome> {
    let mut queue = VecDeque::new();
    for rep in 1..=opts.times {
        for file in 0..targets.len() {
            queue.push_back((file, rep));
        }
    }
    let queue = Arc::new(Mutex::new(queue));
    let outcomes = Arc::new(Mutex::new(vec![]));
    let start = Instant::now();
    let done = AtomicBool::new(false);
    std::thread::scope(|s| {
        // Threads that only spin keep every core busy, so the runs compete for the CPU even when
        // there are fewer of them than cores, as on a loaded CI runner.
        for _ in 0..opts.burners {
            s.spawn(|| {
                let mut x = 1u64;
                while !done.load(Ordering::Relaxed) {
                    for _ in 0..10_000 {
                        x = std::hint::black_box(
                            x.wrapping_mul(6364136223846793005).wrapping_add(1),
                        );
                    }
                }
            });
        }
        let mut workers = vec![];
        for _ in 0..opts.jobs.min(targets.len() * opts.times) {
            let (queue, outcomes) = (queue.clone(), outcomes.clone());
            workers.push(s.spawn(move || loop {
                if start.elapsed() >= opts.time_limit {
                    return;
                }
                let Some((file, rep)) = queue.lock().ok().and_then(|mut q| q.pop_front()) else {
                    return;
                };
                let outcome = run_one(root, &targets[file], file, rep, work);
                if let Ok(mut o) = outcomes.lock() {
                    o.push(outcome);
                }
            }));
        }
        for w in workers {
            let _ = w.join();
        }
        done.store(true, Ordering::Relaxed);
    });
    let mut outcomes = std::mem::take(&mut *outcomes.lock().unwrap_or_else(|e| e.into_inner()));
    outcomes.sort_by_key(|o| (o.file, o.rep));
    outcomes
}

fn run_one(root: &Path, target: &Target, file: usize, rep: usize, work: &Path) -> Outcome {
    let dir = work.join(format!("run-{file}-{rep}"));
    let tmp = dir.join("tmp");
    let _ = std::fs::create_dir_all(&tmp);
    let timeout = RUN_TIMEOUT_SECS.to_string();
    let mut cmd = match &target.kind {
        Kind::Harness { exe, dir } => {
            let mut c = Command::new(exe);
            c.current_dir(dir)
                .env("VELT_SCRIPT", root.join(&target.file));
            if std::env::var_os("VELT_TEST_TIMEOUT").is_none() {
                c.env("VELT_TEST_TIMEOUT", format!("{}", RUN_TIMEOUT_SECS * 1000));
            }
            c
        }
        Kind::Golden { test } => {
            let mut c = Command::new(test);
            c.current_dir(root)
                .args(["--nocapture", "--exact", "golden"])
                .env("VELT_GOLDEN", &target.file)
                .env("VELT_GOLDEN_MODES", "debug")
                .env("VELT_GOLDEN_JOBS", "1")
                .env("VELT_GOLDEN_WORK", dir.join("golden"))
                .env("VELT_RT_PREBUILT", "1");
            if std::env::var_os("VELT_GOLDEN_TIMEOUT").is_none() {
                c.env("VELT_GOLDEN_TIMEOUT", &timeout);
            }
            c
        }
    };
    // The debug runs of the gate use the checking allocator; each run has its own temporary
    // directory, so two runs of one test never share a file there.
    cmd.env("VELT_RT_DEBUG_ALLOC", "1")
        .env("NO_COLOR", "1")
        .env_remove("CLICOLOR_FORCE")
        .env_remove("VELT_RC_STATS");
    for var in ["TMPDIR", "TMP", "TEMP"] {
        cmd.env(var, &tmp);
    }
    let start = Instant::now();
    let (ok, log) = match cmd.output() {
        Ok(out) => (
            out.status.success(),
            format!(
                "{} ({})\n{}{}",
                show(&cmd),
                out.status,
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        ),
        Err(e) => (false, format!("cannot run `{}`: {e}", show(&cmd))),
    };
    let _ = std::fs::remove_dir_all(&dir);
    Outcome {
        file,
        rep,
        ok,
        secs: start.elapsed().as_secs_f64(),
        log,
    }
}

fn report(
    targets: &[Target],
    outcomes: &[Outcome],
    logs: &Path,
    opts: &Options,
    build_secs: f64,
    run_secs: f64,
) -> Result<(), String> {
    let mut by_file: BTreeMap<usize, Vec<&Outcome>> = BTreeMap::new();
    for o in outcomes {
        by_file.entry(o.file).or_default().push(o);
    }
    let github = std::env::var_os("GITHUB_ACTIONS").is_some();
    let mut summary = format!(
        "### repeat: {} file(s), {} times each, {} at once\n\n| file | runs | failed | slowest run |\n|---|---|---|---|\n",
        targets.len(),
        opts.times,
        opts.jobs
    );
    let mut failed_files = vec![];
    let mut cut_short = false;
    println!("\nrepeat results (build {build_secs:.0} s, runs {run_secs:.0} s):");
    for (i, target) in targets.iter().enumerate() {
        let runs = by_file.get(&i).map_or(&[][..], Vec::as_slice);
        let failed: Vec<&&Outcome> = runs.iter().filter(|o| !o.ok).collect();
        let slowest = runs.iter().map(|o| o.secs).fold(0.0, f64::max);
        cut_short |= runs.len() < opts.times;
        let reps: Vec<String> = failed.iter().map(|o| o.rep.to_string()).collect();
        println!(
            "  {}: {} of {} runs, {} failed{} (slowest {slowest:.1} s)",
            target.file,
            runs.len(),
            opts.times,
            failed.len(),
            if reps.is_empty() {
                String::new()
            } else {
                format!(": repetition {}", reps.join(", "))
            }
        );
        summary.push_str(&format!(
            "| `{}` | {} of {} | {} | {slowest:.1} s |\n",
            target.file,
            runs.len(),
            opts.times,
            if reps.is_empty() {
                "none".to_string()
            } else {
                reps.join(", ")
            }
        ));
        for o in &failed {
            let name = target.file.replace(['/', '\\', '.'], "_");
            let _ = std::fs::write(logs.join(format!("{name}-{}.log", o.rep)), &o.log);
        }
        if let Some(first) = failed.first() {
            failed_files.push(format!(
                "{} failed in repetition {} of {} (and {} more of {} runs)",
                target.file,
                first.rep,
                opts.times,
                failed.len() - 1,
                runs.len()
            ));
            let lines: Vec<&str> = first.log.lines().collect();
            let tail = &lines[lines.len().saturating_sub(TAIL_LINES)..];
            println!(
                "\n\x1b[31m--- {} repetition {} (last {} lines) ---\x1b[0m\n{}\n",
                target.file,
                first.rep,
                tail.len(),
                tail.join("\n")
            );
            if github {
                println!(
                    "::error file={}::{} failed in repetition {} of {} under load: it passes on its \
                     own but not every time, which is how a test that orders events by timing fails",
                    target.file, target.file, first.rep, opts.times
                );
            }
        }
    }
    if cut_short {
        let note = format!(
            "the time limit ({} s) stopped the runs before every repetition ran",
            opts.time_limit.as_secs()
        );
        println!("note: {note}");
        summary.push_str(&format!("\nNote: {note}.\n"));
        if github {
            println!("::warning::repeat: {note}");
        }
    }
    if let Some(path) = std::env::var_os("GITHUB_STEP_SUMMARY") {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)
        {
            let _ = f.write_all(summary.as_bytes());
        }
    }
    if failed_files.is_empty() {
        println!("\x1b[32mevery run passed\x1b[0m");
        return Ok(());
    }
    Err(format!(
        "FLAKY under load (CONTRIBUTING.md \"Testing\": order events by awaits, channels or \
         observed state, not by sleeps):\n  {}\nlogs: {}",
        failed_files.join("\n  "),
        logs.display()
    ))
}

fn step(root: &Path, name: &str, cmd: &mut Command, dry_run: bool) -> Result<(), String> {
    println!("\x1b[36m==> {name}\x1b[0m\n    {}", show(cmd));
    if dry_run {
        return Ok(());
    }
    let status = cmd
        .current_dir(root)
        .status()
        .map_err(|e| format!("{name}: cannot run `{}`: {e}", show(cmd)))?;
    if !status.success() {
        return Err(format!("FAILED: {name}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(p: &[&str]) -> Vec<String> {
        p.iter().map(|s| s.to_string()).collect()
    }

    fn sel(changed: &[&str], files: &[&str]) -> Selection {
        let files: Vec<String> = paths(files);
        select(
            &paths(changed),
            &BTreeSet::new(),
            |p| files.iter().any(|f| f == p),
            |dir| {
                files
                    .iter()
                    .filter(|f| f.rsplit_once('/').is_some_and(|(d, _)| d == dir))
                    .cloned()
                    .collect()
            },
        )
    }

    #[test]
    fn changed_test_files_are_selected() {
        let files = [
            "packages/http-server/tests/h2.test.vlt",
            "packages/http-server/src/h2.vlt",
            "std/text/format.test.vlt",
            "tests/std/fs/read.test.vlt",
            "examples/apps/chat/tests/room.test.ts",
        ];
        let s = sel(&files, &files);
        assert_eq!(
            s.velt_tests.iter().map(String::as_str).collect::<Vec<_>>(),
            [
                "examples/apps/chat/tests/room.test.ts",
                "packages/http-server/tests/h2.test.vlt",
                "std/text/format.test.vlt",
                "tests/std/fs/read.test.vlt",
            ]
        );
        assert!(s.goldens.is_empty());
        // A deleted test file has nothing to run.
        assert!(sel(&["std/text/gone.test.vlt"], &[]).is_empty());
        // Library code is not a test file.
        assert!(sel(&["packages/http-server/src/h2.vlt"], &files).is_empty());
    }

    #[test]
    fn a_changed_helper_selects_the_tests_beside_it() {
        let files = [
            "tests/std/net/_server.vlt",
            "tests/std/net/a.test.vlt",
            "tests/std/net/b.test.vlt",
            "tests/std/fs/c.test.vlt",
            "packages/redis/tests/support.vlt",
            "packages/redis/tests/x.test.vlt",
        ];
        let s = sel(
            &[
                "tests/std/net/_server.vlt",
                "packages/redis/tests/support.vlt",
            ],
            &files,
        );
        assert_eq!(
            s.velt_tests.iter().map(String::as_str).collect::<Vec<_>>(),
            [
                "packages/redis/tests/x.test.vlt",
                "tests/std/net/a.test.vlt",
                "tests/std/net/b.test.vlt",
            ]
        );
    }

    #[test]
    fn goldens_that_run_are_selected_by_any_of_their_files() {
        let files = [
            "tests/golden/lang/a.vlt",
            "tests/golden/lang/a.out",
            "tests/golden/lang/b.vlt",
            "tests/golden/lang/b.out",
            "tests/golden/lang/errors/c.vlt",
            "tests/golden/lang/errors/c.err",
            "tests/golden/lang/_helper.vlt",
            "tests/golden/std/_dir/index.vlt",
            "tests/golden/std/pkg/package.vlt",
        ];
        let s = sel(
            &[
                "tests/golden/lang/a.vlt",
                "tests/golden/lang/b.out",
                "tests/golden/lang/errors/c.vlt",
                "tests/golden/lang/_helper.vlt",
                "tests/golden/std/_dir/index.vlt",
                "tests/golden/std/pkg/package.vlt",
            ],
            &files,
        );
        assert_eq!(
            s.goldens.iter().map(String::as_str).collect::<Vec<_>>(),
            ["tests/golden/lang/a.vlt", "tests/golden/lang/b.vlt"]
        );
        assert!(s.velt_tests.is_empty());
    }

    #[test]
    fn comment_only_changes_are_skipped() {
        let changed = paths(&["std/a.test.vlt", "std/b.test.vlt"]);
        let comment_only = BTreeSet::from(["std/a.test.vlt".to_string()]);
        let s = select(&changed, &comment_only, |_| true, |_| vec![]);
        assert_eq!(
            s.velt_tests.into_iter().collect::<Vec<_>>(),
            ["std/b.test.vlt"]
        );
    }

    #[test]
    fn only_whole_line_comments_count_as_comments() {
        let old = "import { x } from \"y\";\n\nawait sleep(5);\n";
        let marked = "import { x } from \"y\";\n// timing: the idle timeout is the subject\n\
                      await sleep(5);\n";
        assert!(same_vlt_code(old, marked));
        assert!(!same_vlt_code(old, "await sleep(6);\n"));
        assert!(!same_vlt_code(
            old,
            &old.replace("sleep(5);", "sleep(5); // x")
        ));
        // A golden's directives change how it runs.
        assert!(!same_vlt_code(old, &format!("// check: no leaks\n{old}")));
        assert!(!same_vlt_code(old, &format!("// requires: unix\n{old}")));
    }

    #[test]
    fn the_runs_stop_at_the_time_limit() {
        let opts = Options {
            times: 3,
            jobs: 2,
            time_limit: Duration::ZERO,
            burners: 1,
            dry_run: false,
        };
        let targets = [Target {
            file: "x.test.vlt".into(),
            kind: Kind::Harness {
                exe: PathBuf::from("does-not-exist"),
                dir: PathBuf::from("."),
            },
        }];
        let work = std::env::temp_dir().join(format!("xtask-repeat-{}", std::process::id()));
        assert!(run_all(Path::new("."), &targets, &work, &opts).is_empty());
    }

    #[test]
    fn a_failed_run_names_the_file_and_the_repetition() {
        let targets = [Target {
            file: "packages/p/tests/x.test.vlt".into(),
            kind: Kind::Harness {
                exe: PathBuf::from("x"),
                dir: PathBuf::from("."),
            },
        }];
        let outcome = |rep, ok| Outcome {
            file: 0,
            rep,
            ok,
            secs: 0.5,
            log: format!("FAILED test_order in run {rep}"),
        };
        let outcomes = [outcome(1, true), outcome(2, false), outcome(3, true)];
        let logs = std::env::temp_dir().join(format!("xtask-repeat-logs-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&logs);
        let opts = Options {
            times: 3,
            jobs: 2,
            time_limit: Duration::from_secs(60),
            burners: 0,
            dry_run: false,
        };
        let err = report(&targets, &outcomes, &logs, &opts, 0.0, 0.0).unwrap_err();
        assert!(
            err.contains("packages/p/tests/x.test.vlt failed in repetition 2 of 3"),
            "{err}"
        );
        assert!(logs.join("packages_p_tests_x_test_vlt-2.log").is_file());
        let _ = std::fs::remove_dir_all(&logs);
        assert!(report(&targets, &outcomes[..1], &logs, &opts, 0.0, 0.0).is_ok());
    }
}
