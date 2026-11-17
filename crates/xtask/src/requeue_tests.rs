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
        // A failed test (2026-11-16, the ABI symbol table), a golden killed at its time limit, and
        // a golden shard at its job's time limit: each stays out, with a comment (`explain`).
        ("43840651349", "requeue=false\npr=3059\n"),
        ("43869250865", "requeue=false\npr=3058\n"),
        ("43820418135", "requeue=false\npr=3035\n"),
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

/// The comment `explain` writes for the saved run `run.json` (its failed jobs' logs in
/// scripts/requeue/fixtures/logs).
fn explain(python: &str, name: &str, run: &str) -> String {
    let dir = std::env::temp_dir().join(format!("xtask-explain-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (input, output) = (
        dir.join(format!("{name}.json")),
        dir.join(format!("{name}.md")),
    );
    std::fs::write(&input, run).unwrap();
    let out = Command::new(python)
        .arg("-I")
        .arg(script_dir().join("requeue.py"))
        .arg("explain")
        .arg(&input)
        .arg(&output)
        .arg(format!(
            "https://github.com/velt-lang/velt/actions/runs/{name}"
        ))
        .arg(script_dir().join("fixtures/logs"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = std::fs::read_to_string(&output).unwrap();
    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&output);
    text
}

/// The fail-fast action leaves every job of a failed run cancelled, the failed one included: the
/// comment names each job whose step failed or that hit its time limit, the step, and the failure
/// lines of its log. A run with no failed job (the queue cancelled it because an entry ahead
/// dropped) gets none.
#[test]
fn a_run_that_stays_out_says_what_failed() {
    let Some(python) = python() else {
        assert!(
            std::env::var_os("CI").is_none(),
            "no Python 3 on this runner for scripts/requeue/requeue.py"
        );
        eprintln!("skipping: no Python 3");
        return;
    };
    let test = explain(python, "43840651349", &fixture("43840651349"));
    for want in [
        "https://github.com/velt-lang/velt/actions/runs/43840651349 failed",
        "- **check (ubuntu-latest)**: step `cargo xtask check --full --part lint,test` failed\n",
        "  FAIL [   0.024s] (1884/5367) velt_rt_host::abi_symbols table_covers_the_documented_abi\n",
        "  documented but not exported: [\"velt_rt_fs_append_file\", ",
        "  FAILED: unit + integration tests\n",
    ] {
        assert!(test.contains(want), "no {want:?} in:\n{test}");
    }
    // Once each (nextest repeats its failures at the end), and no summary job.
    let named = test.matches("table_covers_the_documented_abi").count();
    assert_eq!(named, 1, "{test}");
    assert!(!test.contains("**ci**"), "{test}");

    let golden = explain(python, "43869250865", &fixture("43869250865"));
    for want in [
        "- **golden (macos-latest) / shard 1/2**: step `cargo xtask check --full --part golden",
        "  tests/golden/std/abort_signal_timeout_dropped.velt [--release]: exit -1 (want 0)\n",
        "  (killed after 120 s)\n",
        "  FAILED: goldens (debug + release)\n",
    ] {
        assert!(golden.contains(want), "no {want:?} in:\n{golden}");
    }
    assert!(!golden.contains("2026-11-16T"), "a timestamp in:\n{golden}");
    assert!(!golden.contains("/ goldens**"), "{golden}");

    let shard = fixture("43820418135");
    let limit = explain(python, "43820418135", &shard);
    let want = "- **golden (ubuntu-latest) / shard 2/2**: The job has exceeded the maximum \
                execution time of 30m0s\n";
    assert!(limit.contains(want), "{limit}");

    // The same run with the shard cancelled instead: nothing failed, no comment.
    let time_limit = "\"The job has exceeded the maximum execution time of 30m0s\"";
    assert_eq!(shard.matches(time_limit).count(), 1);
    let cancelled = shard.replace(time_limit, "\"The operation was canceled.\"");
    assert_eq!(explain(python, "cancelled", &cancelled), "");
}
