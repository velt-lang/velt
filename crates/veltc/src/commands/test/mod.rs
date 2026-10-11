//! `velt test [file|dir]`: find `*.test.vlt` / `*.test.ts` / `*.test.tsx` files ([`discover`]),
//! compile a generated harness per file ([`harness`]) and run it, printing `ok <name>` / `FAILED
//! <name>` and a summary. Exit code 1 if any test failed or did not build.
//!
//! `--filter <text>` keeps the tests whose name contains it (`--exact`: is it). `--no-run` builds
//! the harnesses without running them, with variables for debuggers, into
//! `target/velt/test/debug/` (a harness being debugged must not block the next run's link), and
//! lists them (`--json`: as JSON, which the VS Code extension's "Debug test" reads).

pub(crate) mod discover;
mod harness;

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
}

/// Settings shared by every test file of one `velt test` run.
struct Run<'o> {
    opts: &'o TestOptions,
    /// Where test executables go (`<package or cwd>/target/velt/test`).
    out_dir: PathBuf,
    packages: Option<vpm::PackageGraph>,
    passed: usize,
    failed: usize,
    /// Every file the builds read (for `--watch`).
    watched: Vec<PathBuf>,
    /// Harness builds so far: each gets its own executable, since on Windows the one a crashed
    /// test ran from can stay locked for a moment and the rebuild for the remaining tests would
    /// fail to link over it.
    builds: usize,
    /// Tests that matched the filter.
    selected: usize,
    /// `--no-run`: the harnesses built (`{"file", "executable", "tests", "env"}`), and the
    /// diagnostics of their builds.
    harnesses: Vec<Value>,
    diagnostics: Vec<Value>,
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
    let files = discover::find_test_files(&search)?;
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
    let mut run = Run {
        opts,
        out_dir,
        packages: project.map(|p| p.graph),
        passed: 0,
        failed: 0,
        watched,
        builds: 0,
        selected: 0,
        harnesses: vec![],
        diagnostics: vec![],
    };
    for file in &files {
        run.file(file)?;
    }
    if let (Some(filter), 0) = (&opts.filter, run.selected) {
        if !opts.json {
            eprintln!("no tests match `{filter}`");
        }
    }
    if opts.no_run {
        if opts.json {
            print_json(&run.harnesses, run.diagnostics);
        } else {
            for h in &run.harnesses {
                println!("{}", h["executable"].as_str().unwrap_or_default());
            }
        }
        return Ok(Outcome {
            passed: run.failed == 0,
            watched: run.watched,
        });
    }
    let verdict = if run.failed == 0 {
        paint(Stream::Stdout, Style::Good, "ok")
    } else {
        failed()
    };
    println!(
        "\ntest result: {verdict}. {} passed; {} failed",
        run.passed, run.failed
    );
    Ok(Outcome {
        passed: run.failed == 0,
        watched: run.watched,
    })
}

impl Run<'_> {
    fn file(&mut self, file: &Path) -> Result<(), String> {
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
            return Ok(());
        };
        if self.opts.filter.is_some() {
            found.tests.retain(|t| self.selects(t));
            found.async_tests.retain(|t| self.selects(t));
            found.skipped.retain(|(t, _)| self.selects(t));
            if found.tests.is_empty() {
                return Ok(());
            }
        }
        self.selected += found.tests.len();
        if self.opts.no_run {
            self.build_only(file, module_name, &found);
            return Ok(());
        }
        println!("running {}", file.display());
        for (test, reason) in &found.skipped {
            eprintln!(
                "{} skipping `{test}` in {}: {reason}",
                paint(Stream::Stderr, Style::Warning, "warning:"),
                file.display()
            );
        }
        let mut remaining = found.tests;
        while !remaining.is_empty() {
            remaining = self.run_harness(file, module_name, remaining, &found.async_tests);
        }
        Ok(())
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
        Ok(Some(discover::test_functions(&module)))
    }

    /// `--no-run`: build the harness for `found` and record it.
    fn build_only(&mut self, file: &Path, module_name: &str, found: &discover::TestFunctions) {
        let opts = self.harness_options(file, module_name, &found.tests, &found.async_tests);
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

    /// The build of a harness running `tests` of `file`.
    fn harness_options(
        &mut self,
        file: &Path,
        module_name: &str,
        tests: &[String],
        async_tests: &[String],
    ) -> BuildOptions {
        let dir = file.parent().unwrap_or(Path::new(""));
        // `--no-run` harnesses get a directory of their own: one being debugged must not keep
        // the next run from linking its harness (Windows locks a running executable).
        let out_dir = match self.opts.no_run {
            true => self.out_dir.join("debug"),
            false => self.out_dir.clone(),
        };
        let opts = BuildOptions {
            input: dir.join(format!("__velt_test_{module_name}.vlt")),
            // With its extension: `a.test.vlt` and `a.test.ts` side by side are not ambiguous here.
            root_source: Some(harness::harness_source(
                &format!(
                    "./{}",
                    file.file_name().unwrap_or_default().to_string_lossy()
                ),
                tests,
                async_tests,
            )),
            packages: self.packages.clone(),
            output: Some(out_dir.join(format!(
                "{}_{}",
                module_name.replace('.', "_"),
                self.builds
            ))),
            release: self.opts.release,
            // What `--no-run` builds is for a debugger.
            debug_vars: self.opts.no_run,
            ..Default::default()
        };
        self.builds += 1;
        opts
    }

    /// Build and run a harness for `tests`; returns the tests still to run after a crash.
    fn run_harness(
        &mut self,
        file: &Path,
        module_name: &str,
        tests: Vec<String>,
        async_tests: &[String],
    ) -> Vec<String> {
        let opts = self.harness_options(file, module_name, &tests, async_tests);
        let mut sess = Session::new();
        let built = driver::build(&mut sess, &opts);
        // The harness itself is generated (never on disk); everything else it read is watched.
        self.watched.extend(
            sess.sm
                .files()
                .map(|(_, f)| f.path.clone())
                .filter(|p| *p != opts.input),
        );
        let exe = match built {
            Ok(artifact) => artifact_path(artifact),
            Err(e) => {
                report(&sess, false);
                let _ = failure_code(&e);
                for t in &tests {
                    println!("{} {t} (build failed)", failed());
                }
                self.failed += tests.len();
                return vec![];
            }
        };
        report(&sess, false);
        self.execute(&exe, file, tests)
    }

    fn execute(&mut self, exe: &Path, file: &Path, tests: Vec<String>) -> Vec<String> {
        // `process.argv[1]` is the test file, as with `node --test`.
        let output = match std::process::Command::new(exe)
            .env(super::build::SCRIPT_VAR, vpm::relpath::absolute(file))
            .output()
        {
            Ok(o) => o,
            Err(e) => {
                crate::style::error(&format!("cannot run `{}`: {e}", exe.display()));
                self.failed += tests.len();
                return vec![];
            }
        };
        let run = harness::read_output(&String::from_utf8_lossy(&output.stdout), &tests);
        for line in &run.display {
            match line.strip_prefix("ok ") {
                Some(name) => println!("{} {name}", paint(Stream::Stdout, Style::Good, "ok")),
                None => println!("{line}"),
            }
        }
        self.passed += run.passed;
        let code = super::build::exit_code(output.status);
        if run.passed == tests.len() {
            if code != 0 {
                println!(
                    "{} (test harness exited with code {code} after all tests passed)",
                    failed()
                );
                self.failed += 1;
            }
            return vec![];
        }
        println!("{} {} (exit code {code})", failed(), tests[run.passed]);
        for line in String::from_utf8_lossy(&output.stderr).lines() {
            println!("    {line}");
        }
        self.failed += 1;
        tests[run.passed + 1..].to_vec()
    }
}

fn artifact_path(artifact: driver::Artifact) -> PathBuf {
    match artifact {
        driver::Artifact::Executable(p) => vpm::relpath::absolute(&p),
        other => unreachable!("ICE: test harness built {other:?}"),
    }
}
