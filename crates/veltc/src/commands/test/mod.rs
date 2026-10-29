//! `velt test [file|dir]`: find `*.test.vlt` / `*.test.ts` / `*.test.tsx` files ([`discover`]),
//! compile a generated harness ([`harness`]) and run it, printing `ok <name>` / `FAILED <name>`
//! and a summary. Exit code 1 if any test failed or did not build.
//!
//! The test files that only declare (`discover::runs_code`) share one harness, built once; a file
//! that runs code when evaluated gets one of its own, as does each file of a shared harness that
//! fails to build (so its errors are its own). Each file runs in a process of its own, up to
//! `--jobs` of them at once ([`exec`]), and their output is printed in file order.
//!
//! `--filter <text>` keeps the tests whose name contains it (`--exact`: is it). `--no-run` builds
//! the harnesses without running them, with variables for debuggers, into
//! `target/velt/test/debug/` (a harness being debugged must not block the next run's link), and
//! lists them (`--json`: as JSON, which the VS Code extension's "Debug test" reads).

mod cache;
pub(crate) mod discover;
mod harness;
pub(crate) mod member_stamp;

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::{json, Value};
use velt_common::FileId;

use super::build::{failure_code, report};
use super::project::Project;
use crate::driver::{self, BuildOptions, Session};
use crate::style::{paint, Stream, Style};

/// `FAILED`, red on a terminal.
fn failed() -> String {
    paint(Stream::Stdout, Style::Error, "FAILED")
}

/// What `velt test` does besides finding the files.
#[derive(Clone, Debug, Default)]
pub struct TestOptions {
    pub release: bool,
    pub locked: bool,
    /// Only the tests whose name contains this.
    pub filter: Option<String>,
    /// With `filter`: only the test with exactly that name.
    pub exact: bool,
    /// Build the harnesses without running them.
    pub no_run: bool,
    /// With `no_run`: list them as JSON on stdout.
    pub json: bool,
    /// `--cpu`.
    pub cpu: Option<String>,
    /// `--jobs`: how many test files run at once (default: one per core).
    pub jobs: Option<usize>,
    /// `--no-cache`: run every test, keeping and using no results (`cache`).
    pub no_cache: bool,
}

/// Settings shared by every test file of one `velt test` run.
struct Run<'o> {
    opts: &'o TestOptions,
    /// Where test executables go (`<package or cwd>/target/velt/test`).
    out_dir: PathBuf,
    packages: Option<vpm::PackageGraph>,
    /// The CPU the harnesses are built for (`super::cpu`).
    cpu: Option<String>,
    /// Tests that failed before running (files that do not parse) or, with `--no-run`, did not
    /// build.
    failed: usize,
    /// Every file the builds read (for `--watch`).
    watched: Vec<PathBuf>,
    /// Harness builds so far: each gets its own executable, so a `--no-run` harness being
    /// debugged (Windows locks a running executable) never blocks linking another.
    builds: usize,
    /// Tests that matched the filter.
    selected: usize,
    /// A planned file registers tests at run time, which the filter is applied to there.
    registers: bool,
    /// `--no-run`: the harnesses built (`{"file", "executable", "tests", "env"}`), and the
    /// diagnostics of their builds.
    harnesses: Vec<Value>,
    diagnostics: Vec<Value>,
    /// The modules the test files import, as read so far.
    scanned: discover::Scanned,
    /// The package's root, where a run whose every result is kept leaves its member stamp
    /// (`member_stamp`); `None` outside a package.
    root: Option<PathBuf>,
    /// The directories test discovery listed, with the hash of their names.
    listed: Vec<(PathBuf, String)>,
}

/// What one `velt test` run found.
pub struct Outcome {
    /// Every test built and passed.
    pub passed: bool,
    /// Files to watch for `--watch`: sources the builds read, test files, and the package
    /// manifest and lockfile. (Each run discovers test files anew, so a new test file is picked
    /// up with the next change.)
    pub watched: Vec<PathBuf>,
}

/// `velt test` (`watch`: rerun on every change, see `crate::dev::test_watch`).
pub fn test_command(path: Option<&Path>, opts: &TestOptions, watch: bool) -> ExitCode {
    if watch {
        crate::dev::test_watch(|| run_all(path, opts));
    }
    match run_all(path, opts) {
        Ok(Outcome { passed: true, .. }) => ExitCode::SUCCESS,
        Ok(_) => ExitCode::from(1),
        Err(msg) if opts.json => {
            let failure = json!({"severity": "error", "message": msg, "location": null,
                                 "labels": [], "notes": [], "code": null, "fix": null});
            print_json(&[], vec![failure]);
            ExitCode::from(1)
        }
        Err(msg) => {
            crate::style::error(&msg);
            ExitCode::from(1)
        }
    }
}

/// `--no-run --json`: `{"harnesses": [{"file", "executable", "tests", "env", "debugInfo"}],
/// "lldbScript", "diagnostics", "errors", "warnings"}`.
fn print_json(harnesses: &[Value], diagnostics: Vec<Value>) {
    let count = |sev: &str| diagnostics.iter().filter(|d| d["severity"] == sev).count();
    let (errors, warnings) = (count("error"), count("warning"));
    let script = crate::debugger::lldb_script().map(|p| p.to_string_lossy().into_owned());
    let out = json!({"harnesses": harnesses, "lldbScript": script, "diagnostics": diagnostics,
                     "errors": errors, "warnings": warnings});
    println!("{out}");
}

/// Run every test file under `path` (or the package / current directory).
pub fn run_all(path: Option<&Path>, opts: &TestOptions) -> Result<Outcome, String> {
    let locked = opts.locked;
    let cwd =
        std::env::current_dir().map_err(|e| format!("cannot read the current directory: {e}"))?;
    let project = Project::find(
        path.unwrap_or(&cwd),
        locked,
        &velt_codegen_cl::host_triple(),
    )?;
    let base = project.as_ref().map_or(cwd.clone(), |p| p.root.clone());
    let search = path.map_or(base.clone(), Path::to_path_buf);
    // What discovery listed, for the member stamp: a new test file is a change.
    let (files, seen) = vpm::probe::record(|| discover::find_test_files(&search));
    let files = files?;
    let listed: Vec<(PathBuf, String)> = seen.listed.into_iter().collect();
    let mut watched = files.clone();
    if project.is_some() {
        watched.push(base.join(vpm::manifest::MANIFEST_FILE));
        watched.push(base.join(vpm::lockfile::LOCK_FILE));
    }
    if files.is_empty() && opts.json {
        print_json(&[], vec![]);
        return Ok(Outcome {
            passed: true,
            watched,
        });
    }
    if files.is_empty() {
        eprintln!(
            "no test files ({}) found under `{}`",
            discover::TEST_FILES,
            search.display()
        );
        return Ok(Outcome {
            passed: true,
            watched,
        });
    }
    let out_dir = base.join("target").join("velt").join("test");
    let host = velt_codegen_cl::host_triple();
    let env = super::cpu::env_cpu();
    let config = project
        .as_ref()
        .and_then(super::project::Project::build_cpu);
    let cpu = super::cpu::resolve_cpu(
        opts.cpu.as_deref(),
        env.as_deref(),
        config.as_deref(),
        &host,
    )?;
    let mut run = Run {
        opts,
        out_dir,
        cpu: super::cpu::for_running_here(cpu, &host, opts.release),
        root: project.as_ref().map(|p| p.root.clone()),
        packages: project.map(|p| p.graph),
        failed: 0,
        watched,
        builds: 0,
        selected: 0,
        registers: false,
        harnesses: vec![],
        diagnostics: vec![],
        scanned: discover::Scanned::default(),
        listed,
    };
    let mut planned = vec![];
    for file in &files {
        planned.extend(run.plan(file)?);
    }
    if let (Some(filter), 0, false) = (&opts.filter, run.selected, run.registers) {
        if !opts.json {
            eprintln!("no tests match `{filter}`");
        }
        // Nothing to build or run, and no summary of nothing; a file that doesn't parse has
        // been reported and fails the run.
        if !opts.no_run {
            return Ok(Outcome {
                passed: run.failed == 0,
                watched: run.watched,
            });
        }
    }
    if !opts.no_run {
        let passed = run.run(&planned)?;
        return Ok(Outcome {
            passed,
            watched: run.watched,
        });
    }
    for p in &planned {
        run.build_only(p);
    }
    if opts.json {
        print_json(&run.harnesses, run.diagnostics);
    } else {
        for h in &run.harnesses {
            println!("{}", h["executable"].as_str().unwrap_or_default());
        }
    }
    Ok(Outcome {
        passed: run.failed == 0,
        watched: run.watched,
    })
}

/// A test file with tests to run.
struct Planned {
    file: PathBuf,
    /// Its file name without the extension (`math.test`).
    module_name: String,
    found: discover::TestFunctions,
}

impl Run<'_> {
    /// Find the tests of `file` that the filter selects; `None` when there are none, or when it
    /// does not parse (reported).
    fn plan(&mut self, file: &Path) -> Result<Option<Planned>, String> {
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Some(module_name) = vpm::sources::strip_source_extension(&name) else {
            return Err(format!(
                "`{}` is not a source file (`.vlt`, `.ts` or `.tsx`)",
                file.display()
            ));
        };
        let Some(mut found) = self.discover(file)? else {
            // `--json`: the parse errors are in the document.
            if !self.opts.json {
                println!("{} {} (does not parse)", failed(), file.display());
            }
            self.failed += 1;
            return Ok(None);
        };
        if self.opts.filter.is_some() {
            found.tests.retain(|t| self.selects(t));
            found.async_tests.retain(|t| self.selects(t));
            found.skipped.retain(|(t, _)| self.selects(t));
            // Tests registered at run time are filtered there.
            if found.tests.is_empty() && !found.registers {
                return Ok(None);
            }
        }
        self.selected += found.tests.len();
        self.registers = self.registers || found.registers;
        Ok(Some(Planned {
            file: file.to_path_buf(),
            module_name: module_name.to_string(),
            found,
        }))
    }

    /// Build the harnesses for `planned` and run them: `velt:test` in a harness without test
    /// files runs every file, as the plan it is handed says, prints the report and exits with
    /// the run's verdict. Returns whether every test passed.
    fn run(&mut self, planned: &[Planned]) -> Result<bool, String> {
        // Without test files, so no test file's module code (or what it imports) runs in it.
        let Some(runner) = self.build_harness(&[]) else {
            return Ok(false);
        };
        // For each file: the harness it is in, and its index there.
        let mut built: Vec<Option<(PathBuf, usize)>> = vec![None; planned.len()];
        for group in groups(planned) {
            self.build_group(planned, &group, &mut built);
        }
        // What each harness program reaches, and the results kept for it (`cache`).
        let mut harnesses: BTreeMap<PathBuf, cache::Harness> = BTreeMap::new();
        for (exe, _) in built.iter().flatten() {
            harnesses.entry(exe.clone()).or_insert_with(|| {
                let filter = self.opts.filter.as_deref();
                cache::Harness::of(exe, filter, self.opts.exact, !self.opts.no_cache)
            });
        }
        let files: Vec<Value> = planned
            .iter()
            .zip(&built)
            .map(|(p, b)| {
                let skipped: Vec<Value> = p
                    .found
                    .skipped
                    .iter()
                    .map(|(name, reason)| json!({"name": name, "reason": reason}))
                    .collect();
                json!({
                    "path": p.file.display().to_string(),
                    "script": vpm::relpath::absolute(&p.file).to_string_lossy(),
                    "exe": b.as_ref().map(|(exe, _)| exe.to_string_lossy().into_owned()),
                    "index": b.as_ref().map_or(0, |(_, i)| *i),
                    "tests": p.found.tests,
                    "registers": p.found.registers,
                    "skipped": skipped,
                    "cached": b.as_ref().and_then(|(exe, i)| harnesses[exe].cached(*i).cloned()),
                    "uncacheable": b.as_ref().and_then(|(exe, i)| harnesses[exe].uncacheable(*i)),
                })
            })
            .collect();
        let workers = self.opts.jobs.unwrap_or_else(|| {
            std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
        });
        // Where the runner says how the files it ran went, when any of them may be kept.
        let results = harnesses.values().any(cache::Harness::keeps_any).then(|| {
            self.out_dir
                .join(format!("results-{}.json", std::process::id()))
        });
        let plan = json!({
            "workers": workers,
            "colorOut": crate::style::enabled(Stream::Stdout),
            "colorErr": crate::style::enabled(Stream::Stderr),
            "failed": self.failed,
            "filter": self.opts.filter,
            "exact": self.opts.exact,
            "files": files,
            "results": results,
        });
        // One per run: two runs in a package at once must not read each other's.
        let plan_file = self
            .out_dir
            .join(format!("plan-{}.json", std::process::id()));
        std::fs::create_dir_all(&self.out_dir)
            .and_then(|()| std::fs::write(&plan_file, plan.to_string()))
            .map_err(|e| format!("cannot write `{}`: {e}", plan_file.display()))?;
        let mut command = std::process::Command::new(&runner);
        command.env(harness::PLAN_VAR, &plan_file);
        for var in harness::CHILD_VARS {
            command.env_remove(var);
        }
        let status = command.status();
        let _ = std::fs::remove_file(&plan_file);
        if let Some(results) = &results {
            keep_passed(results, &built, &mut harnesses);
        }
        let passed = status.as_ref().is_ok_and(|s| s.success());
        self.leave_member_stamp(planned, &built, &harnesses, passed);
        let status = status.map_err(|e| format!("cannot run `{}`: {e}", runner.display()))?;
        Ok(status.success())
    }

    /// After a run in a package: a member stamp when it `passed` with every file's result kept
    /// (built, and reaching nothing outside: its tests pass again while nothing changes), so the
    /// workspace command can skip the package next time (`member_stamp`); else none.
    fn leave_member_stamp(
        &self,
        planned: &[Planned],
        built: &[Option<(PathBuf, usize)>],
        harnesses: &BTreeMap<PathBuf, cache::Harness>,
        passed: bool,
    ) {
        let Some(root) = &self.root else {
            return;
        };
        let all_kept = !planned.is_empty()
            && built.iter().all(|b| {
                b.as_ref()
                    .is_some_and(|(exe, i)| harnesses.get(exe).is_some_and(|h| h.keeps(*i)))
            });
        if !(passed && all_kept && self.failed == 0) {
            member_stamp::remove(root);
            return;
        }
        // Every file's report is kept by now (it passed), so they count every test.
        let tests: usize = built
            .iter()
            .flatten()
            .filter_map(|(exe, i)| harnesses[exe].cached(*i))
            .map(|r| r["passed"].as_u64().unwrap_or(0) as usize)
            .sum();
        let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
        let exes: Vec<PathBuf> = harnesses.keys().cloned().collect();
        member_stamp::write(root, &args, tests, &exes, &self.listed);
    }

    /// Build one harness for the files `group` of `planned` and record it in `built`. When a
    /// shared harness fails, the files its errors are in are built alone, which reports them, and
    /// the others share a harness again. Errors in none of them (in a module they import) leave
    /// each file to be built alone, to find out whose they are.
    fn build_group(
        &mut self,
        planned: &[Planned],
        group: &[usize],
        built: &mut [Option<(PathBuf, usize)>],
    ) {
        let files: Vec<&Planned> = group.iter().map(|&i| &planned[i]).collect();
        let quiet = group.len() > 1;
        match self.build_files(&files, quiet) {
            Ok(exe) => {
                for (index, &i) in group.iter().enumerate() {
                    built[i] = Some((exe.clone(), index));
                }
            }
            Err(broken) if quiet => {
                let (bad, rest): (Vec<_>, Vec<_>) = group
                    .iter()
                    .enumerate()
                    .partition(|(at, _)| broken.is_empty() || broken.contains(at));
                for (_, &i) in bad {
                    self.build_group(planned, &[i], built);
                }
                let rest: Vec<usize> = rest.into_iter().map(|(_, &i)| i).collect();
                if !rest.is_empty() {
                    self.build_group(planned, &rest, built);
                }
            }
            Err(_) => {}
        }
    }

    /// The harness of `files` alone; without files, the runner.
    fn build_harness(&mut self, files: &[&Planned]) -> Option<PathBuf> {
        self.build_files(files, false).ok()
    }

    /// Build the harness of `files`; reports the build's diagnostics unless it fails and is
    /// `quiet` (a shared harness, whose files are then built again). A failed build gives the
    /// indexes in `files` of the files its errors are in.
    fn build_files(&mut self, files: &[&Planned], quiet: bool) -> Result<PathBuf, Vec<usize>> {
        let opts = self.harness_options(files);
        let mut sess = Session::new();
        let result = driver::build(&mut sess, &opts);
        // The harness itself is generated (never on disk); everything else it read is watched.
        self.watched.extend(
            sess.sm
                .files()
                .map(|(_, f)| f.path.clone())
                .filter(|p| *p != opts.input),
        );
        match result {
            Ok(artifact) => {
                report(&sess, false);
                Ok(artifact_path(artifact))
            }
            Err(_) if quiet => Err(files_with_errors(&sess, files)),
            Err(e) => {
                report(&sess, false);
                let _ = failure_code(&e);
                Err(vec![])
            }
        }
    }

    /// Whether `--filter` (and `--exact`) select the test `name`.
    fn selects(&self, name: &str) -> bool {
        match &self.opts.filter {
            None => true,
            Some(filter) if self.opts.exact => name == filter,
            Some(filter) => name.contains(filter.as_str()),
        }
    }

    /// Parse `file` and list its tests; `None` if it does not parse, after printing the errors
    /// (`--json`: adding them to the document).
    fn discover(&mut self, file: &Path) -> Result<Option<discover::TestFunctions>, String> {
        let src = std::fs::read_to_string(file)
            .map_err(|e| format!("cannot read `{}`: {e}", file.display()))?;
        let (module, diags) = velt_syntax::parse_file(FileId(0), &src);
        if diags.iter().any(|d| d.is_error()) {
            let mut sess = Session::new();
            sess.sm.add(file, src);
            sess.diagnostics = diags;
            if self.opts.json {
                let report = super::check::report_json(&sess, None, &[]);
                if let Value::Array(ds) = &report["diagnostics"] {
                    self.diagnostics.extend(ds.iter().cloned());
                }
            } else {
                let rendered: Vec<String> = sess
                    .diagnostics
                    .iter()
                    .map(|d| d.render(&sess.sm))
                    .collect();
                eprintln!("{}", rendered.join("\n\n"));
            }
            return Ok(None);
        }
        let mut found = discover::test_functions(&module);
        found.runs_code =
            found.runs_code || discover::imports_run_code(file, &module, &mut self.scanned);
        Ok(Some(found))
    }

    /// `--no-run`: build the harness for `p` alone and record it.
    fn build_only(&mut self, p: &Planned) {
        let (file, found) = (&p.file, &p.found);
        let opts = self.harness_options(&[p]);
        let mut sess = Session::new();
        let built = driver::build(&mut sess, &opts);
        let failure = match &built {
            Err(driver::BuildError::Failed(msg)) => Some(msg.clone()),
            Err(driver::BuildError::Ice(msg)) => Some(format!("internal compiler error: {msg}")),
            Err(driver::BuildError::Diagnostics) | Ok(_) => None,
        };
        if self.opts.json {
            let report = super::check::report_json(&sess, failure.as_deref(), &[]);
            if let Value::Array(ds) = &report["diagnostics"] {
                self.diagnostics.extend(ds.iter().cloned());
            }
        } else {
            report(&sess, false);
            if let Some(msg) = &failure {
                crate::style::error(msg);
            }
        }
        match built {
            Ok(artifact) => {
                let script = vpm::relpath::absolute(file);
                self.harnesses.push(json!({
                    "file": script.to_string_lossy(),
                    "executable": artifact_path(artifact).to_string_lossy(),
                    "tests": found.tests,
                    "env": { super::build::SCRIPT_VAR: script.to_string_lossy() },
                    "debugInfo": opts.has_line_info(),
                }));
            }
            Err(_) => self.failed += found.tests.len(),
        }
    }

    /// The build of a harness running the tests of `files`: next to the file when it is one, else
    /// in the directory that holds them all.
    fn harness_options(&mut self, files: &[&Planned]) -> BuildOptions {
        let dir = common_dir(files.iter().map(|p| p.file.as_path()));
        let name = match files {
            [] => "runner".to_string(),
            [one] => one.module_name.clone(),
            _ => "harness".to_string(),
        };
        let harness_files: Vec<harness::HarnessFile> = files
            .iter()
            .map(|p| harness::HarnessFile {
                // With its extension: `a.test.vlt` and `a.test.ts` side by side are not ambiguous
                // here.
                import_spec: import_spec(&dir, &p.file),
                tests: &p.found.tests,
                async_tests: &p.found.async_tests,
                // Alone, it takes every registration queued, whatever ran them.
                queued: (files.len() > 1).then_some(p.found.queued),
            })
            .collect();
        // `--no-run` harnesses get a directory of their own: one being debugged must not keep
        // the next run from linking its harness (Windows locks a running executable).
        let out_dir = match self.opts.no_run {
            true => self.out_dir.join("debug"),
            false => self.out_dir.clone(),
        };
        let opts = BuildOptions {
            input: dir.join(format!("__velt_test_{name}.vlt")),
            root_source: Some(harness::harness_source(&harness_files)),
            packages: self.packages.clone(),
            output: Some(out_dir.join(format!("{}_{}", name.replace('.', "_"), self.builds))),
            release: self.opts.release,
            cpu: self.cpu.clone(),
            // What `--no-run` builds is for a debugger.
            debug_vars: self.opts.no_run,
            ..Default::default()
        };
        self.builds += 1;
        opts
    }
}

/// Keep the reports in the runner's `results` file (`Ran[]`) of the files whose tests all passed,
/// for the harnesses whose results may be kept (`cache`), then remove the file.
fn keep_passed(
    results: &Path,
    built: &[Option<(PathBuf, usize)>],
    harnesses: &mut BTreeMap<PathBuf, cache::Harness>,
) {
    let ran: Vec<Value> = std::fs::read_to_string(results)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    let _ = std::fs::remove_file(results);
    for report in ran {
        let passed = report["failed"].as_u64() == Some(0);
        let index = report["index"].as_u64().map(|i| i as usize);
        let Some(Some((exe, at))) = index.and_then(|i| built.get(i)) else {
            continue;
        };
        if let Some(harness) = harnesses.get_mut(exe).filter(|_| passed) {
            harness.keep(*at, report);
        }
    }
    for (exe, harness) in harnesses.iter() {
        harness.save(exe);
    }
}

/// Which files of `planned` share a harness: those that only declare, in one; each other one
/// alone. Of two files with the same module path (`a.test.ts` and `a.test.vlt` side by side,
/// which one program cannot import both of), the second is alone too.
fn groups(planned: &[Planned]) -> Vec<Vec<usize>> {
    let mut shared = vec![];
    let mut alone = vec![];
    let mut paths = HashSet::new();
    for (i, p) in planned.iter().enumerate() {
        let path = p.file.with_file_name(&p.module_name);
        if p.found.runs_code || !paths.insert(path) {
            alone.push(vec![i]);
        } else {
            shared.push(i);
        }
    }
    let mut groups = vec![];
    if !shared.is_empty() {
        groups.push(shared);
    }
    groups.extend(alone);
    groups
}

/// The indexes in `files` of the test files `sess` reported an error in.
fn files_with_errors(sess: &Session, files: &[&Planned]) -> Vec<usize> {
    let same = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let wanted: Vec<PathBuf> = files.iter().map(|p| same(&p.file)).collect();
    let mut broken: Vec<usize> = sess
        .diagnostics
        .iter()
        .filter(|d| d.is_error())
        .filter_map(|d| d.labels.first())
        .filter_map(|l| sess.sm.files().find(|(id, _)| *id == l.span.file))
        .filter_map(|(_, f)| wanted.iter().position(|w| *w == same(&f.path)))
        .collect();
    broken.sort_unstable();
    broken.dedup();
    broken
}

/// The deepest directory holding every one of `files`.
fn common_dir<'p>(mut files: impl Iterator<Item = &'p Path>) -> PathBuf {
    let parent = |f: &Path| f.parent().unwrap_or(Path::new("")).to_path_buf();
    let mut dir = files.next().map(parent).unwrap_or_default();
    for f in files {
        while !f.starts_with(&dir) {
            match dir.parent() {
                Some(up) => dir = up.to_path_buf(),
                None => return PathBuf::new(),
            }
        }
    }
    dir
}

/// `./sub/a.test.vlt`: how a harness in `dir` imports `file`.
fn import_spec(dir: &Path, file: &Path) -> String {
    let rel = file.strip_prefix(dir).unwrap_or(file);
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    format!("./{}", parts.join("/"))
}

fn artifact_path(artifact: driver::Artifact) -> PathBuf {
    match artifact {
        driver::Artifact::Executable(p) => vpm::relpath::absolute(&p),
        other => unreachable!("ICE: test harness built {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn planned(file: &str, runs_code: bool) -> Planned {
        let name = Path::new(file).file_name().unwrap().to_string_lossy();
        Planned {
            file: PathBuf::from(file),
            module_name: vpm::sources::strip_source_extension(&name)
                .unwrap()
                .to_string(),
            found: discover::TestFunctions {
                runs_code,
                ..Default::default()
            },
        }
    }

    #[test]
    fn files_that_only_declare_share_a_harness() {
        let files = [
            planned("p/tests/a.test.vlt", false),
            planned("p/tests/b.test.vlt", true),
            planned("p/src/c.test.vlt", false),
            // Same module path as `a.test.vlt`: one program can't import both.
            planned("p/tests/a.test.ts", false),
        ];
        assert_eq!(groups(&files), [vec![0, 2], vec![1], vec![3]]);
        assert_eq!(groups(&files[1..2]), [vec![0]]);
    }

    #[test]
    fn harness_goes_where_it_can_import_every_file() {
        let (a, c) = (
            Path::new("p/tests/a.test.vlt"),
            Path::new("p/src/x/c.test.vlt"),
        );
        assert_eq!(common_dir([a].into_iter()), Path::new("p/tests"));
        let dir = common_dir([a, c].into_iter());
        assert_eq!(dir, Path::new("p"));
        assert_eq!(import_spec(&dir, c), "./src/x/c.test.vlt");
        assert_eq!(import_spec(Path::new("p/tests"), a), "./a.test.vlt");
    }
}
