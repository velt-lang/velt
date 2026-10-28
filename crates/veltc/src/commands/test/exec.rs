//! Running the built harnesses: each test file in a process of its own, several files at once,
//! with their output printed in file order.
//!
//! A file runs from its first test; when the process ends before its last test (a failed
//! `assert`, a panic, an uncaught error), the same harness starts again from the test after the
//! one that failed ([`harness::FROM_VAR`]).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;

use super::harness;
use crate::style::{paint, Stream, Style};

/// One test file to run.
pub enum Job {
    /// Its tests in `exe`, where it is file `index`.
    Run {
        file: PathBuf,
        exe: PathBuf,
        index: usize,
        tests: Vec<String>,
    },
    /// Nothing to run: its report is known (it did not build).
    Done(Report),
}

/// What a test file printed and how its tests went.
#[derive(Clone, Default)]
pub struct Report {
    /// Its lines for stdout, in order: test output, `ok <name>`, `FAILED <name>` and why.
    pub lines: Vec<String>,
    pub passed: usize,
    pub failed: usize,
}

/// How many files run at once without `--jobs`: one per core.
pub fn default_workers() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

/// Run `jobs` on up to `workers` threads and hand their reports to `each` in the order of
/// `jobs`, each as soon as it and those before it are done.
pub fn run(jobs: &[Job], workers: usize, mut each: impl FnMut(usize, Report)) {
    let next = AtomicUsize::new(0);
    let (tx, rx) = mpsc::channel();
    std::thread::scope(|s| {
        for _ in 0..workers.clamp(1, jobs.len().max(1)) {
            let (tx, next) = (tx.clone(), &next);
            s.spawn(move || loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(job) = jobs.get(i) else { break };
                let report = match job {
                    Job::Run {
                        file,
                        exe,
                        index,
                        tests,
                    } => run_file(file, exe, *index, tests),
                    Job::Done(report) => report.clone(),
                };
                if tx.send((i, report)).is_err() {
                    break;
                }
            });
        }
        drop(tx);
        let mut ready: Vec<Option<Report>> = (0..jobs.len()).map(|_| None).collect();
        let mut printed = 0;
        for (i, report) in rx {
            ready[i] = Some(report);
            while let Some(report) = ready.get_mut(printed).and_then(Option::take) {
                each(printed, report);
                printed += 1;
            }
        }
    });
}

/// Run the tests of `file` (file `index` of the harness `exe`), starting again after each one
/// that ends the process.
fn run_file(file: &Path, exe: &Path, index: usize, tests: &[String]) -> Report {
    let mut report = Report::default();
    let mut from = 0;
    while from < tests.len() {
        from = run_from(file, exe, index, tests, from, &mut report);
    }
    report
}

/// One process running `tests[from..]`; returns where the next one starts (`tests.len()` when
/// none is needed).
fn run_from(
    file: &Path,
    exe: &Path,
    index: usize,
    tests: &[String],
    from: usize,
    report: &mut Report,
) -> usize {
    let rest = &tests[from..];
    // `process.argv[1]` is the test file, as with `node --test`.
    let output = std::process::Command::new(exe)
        .env(
            super::super::build::SCRIPT_VAR,
            vpm::relpath::absolute(file),
        )
        .env(harness::FILE_VAR, index.to_string())
        .env(harness::FROM_VAR, from.to_string())
        .output();
    let output = match output {
        Ok(o) => o,
        Err(e) => {
            let error = paint(Stream::Stdout, Style::Error, "error:");
            report
                .lines
                .push(format!("{error} cannot run `{}`: {e}", exe.display()));
            report.failed += rest.len();
            return tests.len();
        }
    };
    let run = harness::read_output(&String::from_utf8_lossy(&output.stdout), rest);
    for line in run.display {
        report.lines.push(match line.strip_prefix("ok ") {
            Some(name) => format!("{} {name}", paint(Stream::Stdout, Style::Good, "ok")),
            None => line,
        });
    }
    report.passed += run.passed;
    let code = super::super::build::exit_code(output.status);
    if run.passed == rest.len() {
        if code != 0 {
            report.lines.push(format!(
                "{} (test harness exited with code {code} after all tests passed)",
                super::failed()
            ));
            report.failed += 1;
        }
        return tests.len();
    }
    report.lines.push(format!(
        "{} {} (exit code {code})",
        super::failed(),
        rest[run.passed]
    ));
    for line in String::from_utf8_lossy(&output.stderr).lines() {
        report.lines.push(format!("    {line}"));
    }
    report.failed += 1;
    from + run.passed + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn done(n: usize) -> Job {
        Job::Done(Report {
            lines: vec![format!("job {n}")],
            ..Default::default()
        })
    }

    #[test]
    fn reports_come_in_job_order() {
        let jobs: Vec<Job> = (0..20).map(done).collect();
        let mut seen = vec![];
        run(&jobs, 4, |i, r| seen.push((i, r.lines)));
        let want: Vec<_> = (0..20).map(|n| (n, vec![format!("job {n}")])).collect();
        assert_eq!(seen, want);
        // No jobs, and more workers than jobs.
        run(&[], 8, |_, _| panic!("no job"));
        let mut seen = vec![];
        run(&jobs[..2], 8, |i, _| seen.push(i));
        assert_eq!(seen, [0, 1]);
    }
}
