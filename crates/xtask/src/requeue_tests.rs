//! The decision of scripts/requeue/requeue.py, which .github/workflows/requeue.yml uses to put a
//! pull request back into the merge queue when its run failed for GitHub's infrastructure only
//! (#2101), checked on real runs saved in scripts/requeue/fixtures: the runs that lost a runner
//! or hit an internal error go back, the runs with a failed test, a time limit or a fail-fast
//! cancellation do not.

use std::path::{Path, PathBuf};
use std::process::Command;

fn script_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/requeue")
}

/// A Python 3 to run the script with: `python3`, or `python` on Windows runners.
fn python() -> Option<&'static str> {
    ["python3", "python"].into_iter().find(|p| {
        Command::new(p)
            .args(["-I", "-c", "import sys; assert sys.version_info >= (3, 8)"])
            .output()
            .is_ok_and(|o| o.status.success())
    })
}

/// What `decide` writes to its GITHUB_OUTPUT file for `fixture` (a JSON text).
fn decide(python: &str, name: &str, fixture: &str) -> String {
    let dir = std::env::temp_dir().join(format!("xtask-requeue-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join(format!("{name}.json"));
    let output = dir.join(format!("{name}.out"));
    std::fs::write(&input, fixture).unwrap();
    let _ = std::fs::remove_file(&output);
    let out = Command::new(python)
        .arg("-I")
        .arg(script_dir().join("requeue.py"))
        .arg("decide")
        .arg(&input)
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{name}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let written = std::fs::read_to_string(&output).unwrap();
    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&output);
    written
}

fn fixture(run: &str) -> String {
    std::fs::read_to_string(script_dir().join("fixtures").join(format!("{run}.json")))
        .unwrap()
        .replace("\r\n", "\n")
}

const NO_RUNNER: &str =
    "\"The job was not acquired by Runner of type hosted even after multiple attempts\"";

#[test]
fn only_runs_that_failed_for_the_infrastructure_go_back_into_the_queue() {
    let Some(python) = python() else {
        assert!(
            std::env::var_os("CI").is_none(),
            "no Python 3 on this runner for scripts/requeue/requeue.py"
        );
        eprintln!("skipping: no Python 3");
        return;
    };
    let cases = [
        // A macOS job no runner picked up (2026-11-02 and 2026-11-03).
        (
            "41473009735",
            "requeue=true\npr=2059\nwhy=a runner was never acquired\n",
        ),
        (
            "41485022940",
            "requeue=true\npr=2068\nwhy=a runner was never acquired\n",
        ),
        (
            "41664446634",
            "requeue=true\npr=2220\nwhy=a runner was never acquired\n",
        ),
        (
            "41673137170",
            "requeue=true\npr=2219\nwhy=a runner was never acquired\n",
        ),
        // Every job stopped by an internal error of GitHub Actions.
        (
            "41332184046",
            "requeue=true\npr=1988\nwhy=GitHub Actions had an internal error\n",
        ),
        // A failed test on Windows (the http-server idle timeout), the rest cancelled by the
        // fail-fast action.
        ("41631119904", "requeue=false\npr=2191\n"),
        // Failed golden shards on macOS (unwinding), the rest cancelled.
        ("41605907174", "requeue=false\npr=2137\n"),
        // A macOS job without a runner, but the Windows check hit its time limit: a hang is not
        // the infrastructure's.
        ("41530648387", "requeue=false\npr=2073\n"),
        // A pull request's run (not the queue) that lost its macOS runner: nothing to re-queue.
        ("41649340605-1", "requeue=false\npr=\n"),
    ];
    for (run, want) in cases {
        assert_eq!(decide(python, run, &fixture(run)), want, "run {run}");
    }

    // The same run with the no-runner annotation replaced by a step's error, or with a step
    // that failed: a real failure is never retried.
    let run = fixture("41664446634");
    assert_eq!(run.matches(NO_RUNNER).count(), 2);
    let step_error = run.replacen(NO_RUNNER, "\"Process completed with exit code 1.\"", 1);
    assert_eq!(
        decide(python, "step-error", &step_error),
        "requeue=false\npr=2220\n"
    );
    let extra = run.replacen(
        NO_RUNNER,
        &format!("{NO_RUNNER}}}, {{\"level\": \"failure\", \"message\": \"error: test failed\""),
        1,
    );
    assert_eq!(
        decide(python, "extra-error", &extra),
        "requeue=false\npr=2220\n"
    );
    let macos = "\"name\": \"check (macos-latest)\", \"conclusion\": \"cancelled\", \
                 \"runner_name\": \"\", \"failed_steps\": [";
    let failed_step = run.replacen(macos, &format!("{macos}\"cargo xtask check\""), 1);
    assert_ne!(failed_step, run);
    assert_eq!(
        decide(python, "failed-step", &failed_step),
        "requeue=false\npr=2220\n"
    );
    // Neither job lost its runner: only the summaries are left, which never count.
    let summaries_only = run.replace(NO_RUNNER, "\"Process completed with exit code 1.\"");
    assert_eq!(
        decide(python, "summaries", &summaries_only),
        "requeue=false\npr=2220\n"
    );
}
