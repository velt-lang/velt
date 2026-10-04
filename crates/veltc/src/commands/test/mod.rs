//! `velt test [file|dir]`: find `*.test.vlt` / `*.test.ts` / `*.test.tsx` files ([`discover`]),
//! compile a generated harness per file ([`harness`]) and run it, printing `ok <name>` / `FAILED
//! <name>` and a summary. Exit code 1 if any test failed or did not build.

pub(crate) mod discover;
mod harness;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use velt_common::{FileId, SourceMap};

use super::build::{failure_code, report};
use super::project::Project;
use crate::driver::{self, BuildOptions, Session};
use crate::style::{paint, Stream, Style};

/// `FAILED`, red on a terminal.
fn failed() -> String {
    paint(Stream::Stdout, Style::Error, "FAILED")
}

/// Settings shared by every test file of one `velt test` run.
struct Run {
    /// Where test executables go (`<package or cwd>/target/velt/test`).
    out_dir: PathBuf,
    packages: Option<vpm::PackageGraph>,
    release: bool,
    passed: usize,
    failed: usize,
    /// Every file the builds read (for `--watch`).
    watched: Vec<PathBuf>,
    /// Harness builds so far: each gets its own executable, since on Windows the one a crashed
    /// test ran from can stay locked for a moment and the rebuild for the remaining tests would
    /// fail to link over it.
    builds: usize,
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
pub fn test_command(path: Option<&Path>, release: bool, locked: bool, watch: bool) -> ExitCode {
    if watch {
        crate::dev::test_watch(|| run_all(path, release, locked));
    }
    match run_all(path, release, locked) {
        Ok(Outcome { passed: true, .. }) => ExitCode::SUCCESS,
        Ok(_) => ExitCode::from(1),
        Err(msg) => {
            crate::style::error(&msg);
            ExitCode::from(1)
        }
    }
}

/// Run every test file under `path` (or the package / current directory).
pub fn run_all(path: Option<&Path>, release: bool, locked: bool) -> Result<Outcome, String> {
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
        out_dir,
        packages: project.map(|p| p.graph),
        release,
        passed: 0,
        failed: 0,
        watched,
        builds: 0,
    };
    for file in &files {
        run.file(file)?;
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

impl Run {
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
        println!("running {}", file.display());
        let Some(found) = self.discover(file)? else {
            println!("{} {} (does not parse)", failed(), file.display());
            self.failed += 1;
            return Ok(());
        };
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

    /// Parse `file` and list its tests; `None` (after printing the errors) if it does not parse.
    fn discover(&self, file: &Path) -> Result<Option<discover::TestFunctions>, String> {
        let src = std::fs::read_to_string(file)
            .map_err(|e| format!("cannot read `{}`: {e}", file.display()))?;
        let (module, diags) = velt_syntax::parse_file(FileId(0), &src);
        if diags.iter().any(|d| d.is_error()) {
            let mut sm = SourceMap::new();
            sm.add(file, src);
            let rendered: Vec<String> = diags.iter().map(|d| d.render(&sm)).collect();
            eprintln!("{}", rendered.join("\n\n"));
            return Ok(None);
        }
        Ok(Some(discover::test_functions(&module)))
    }

    /// Build and run a harness for `tests`; returns the tests still to run after a crash.
    fn run_harness(
        &mut self,
        file: &Path,
        module_name: &str,
        tests: Vec<String>,
        async_tests: &[String],
    ) -> Vec<String> {
        let dir = file.parent().unwrap_or(Path::new(""));
        let opts = BuildOptions {
            input: dir.join(format!("__velt_test_{module_name}.vlt")),
            // With its extension: `a.test.vlt` and `a.test.ts` side by side are not ambiguous here.
            root_source: Some(harness::harness_source(
                &format!(
                    "./{}",
                    file.file_name().unwrap_or_default().to_string_lossy()
                ),
                &tests,
                async_tests,
            )),
            packages: self.packages.clone(),
            output: Some(self.out_dir.join(format!(
                "{}_{}",
                module_name.replace('.', "_"),
                self.builds
            ))),
            release: self.release,
            ..Default::default()
        };
        self.builds += 1;
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
