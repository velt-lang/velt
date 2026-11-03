//! A `velt build --host host-cloudflare` Worker under workerd, the open-source Workers runtime,
//! through Miniflare (tests/workerd/package.json pins it): tests/workerd/run.mjs uploads the
//! build's modules as `wrangler deploy` does and sends the fixture `app/` (packages/http-server's
//! `serve` with packages/cloudflare's bindings) its requests, against workerd's local R2, D1, KV,
//! cache and Durable Objects. Debug and release builds; the release build must stay within its
//! size budget. Then the release build's start-up time and CPU per request (tests/workerd/cpu.mjs)
//! against budgets well inside Cloudflare's limits.
//!
//! Needs what tests/workers.rs needs, and `npm ci` in tests/workerd. Without the packages the test
//! is skipped with a note, unless VELT_WORKERD=1 (CI's Linux merge queue job) makes that a
//! failure. The timing test runs only when VELT_WORKERD_TIMINGS=1 (CI's nightly `workerd` job, one
//! test at a time), which also requires the packages: timings on a busy machine, such as a gate
//! running tests side by side, are not the Worker's.

mod no_window;
mod wasm_tools;
mod work_dir;
mod worker_size;

use std::path::{Path, PathBuf};

use wasm_tools::{build_runtime, copy_dir, for_node, missing_tools, root};
use worker_size::within_budget;

const TRIPLE: &str = "wasm32-unknown-unknown";

/// The release Worker of `app/`, its files gzipped one by one: 1.5 times its 445,105 bytes.
const APP_BUDGET: usize = 668_000;
/// The most CPU milliseconds under node, all its threads, from importing the release Worker
/// (compiling its module, the glue, `main`) to its first answer: a quarter of Workers' start-up
/// limit (1 second of CPU for the top-level code under workerd), as a guard against regressions.
/// Measured: 54-60 ms on an M-series Mac under load.
const STARTUP_BUDGET: f64 = 250.0;
/// The most CPU milliseconds per request under node, at the median, for `/hello` and `/json`: a
/// 5th of what Workers' Free plan allows (10 ms). Measured: 0.09-0.19 ms on the same machine.
const CPU_BUDGET: f64 = 2.0;

/// Whether the environment variable `name` is 1.
fn set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| v == "1")
}

/// Whether CI requires the test to run (VELT_WORKERD=1, or the timings asked for).
fn required() -> bool {
    set("VELT_WORKERD") || set("VELT_WORKERD_TIMINGS")
}

/// The fixture in a work directory of its own with the packages it depends on beside it, and
/// tests/workerd's node_modules.
fn fixtures(name: &str) -> Option<(PathBuf, PathBuf)> {
    let root = root();
    let modules = root.join("tests/workerd/node_modules");
    let missing = missing_tools(TRIPLE, "node").or_else(|| {
        (!modules.join("miniflare").is_dir())
            .then(|| "tests/workerd's packages (`npm ci` there)".to_string())
    });
    if let Some(missing) = missing {
        assert!(
            !required(),
            "the workerd test is required, but {missing} not available"
        );
        eprintln!("note: {missing} not available; skipping the workerd test");
        return None;
    }
    build_runtime(&root, TRIPLE);
    let work =
        work_dir::work_dir(&root, "workerd-work").join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    copy_dir(&root.join("crates/veltc/tests/workerd"), &work);
    for package in ["cloudflare", "host-cloudflare"] {
        copy_dir(&root.join("packages").join(package), &work.join(package));
    }
    // packages/http-server's sources (not its build output or tests).
    let server = root.join("packages/http-server");
    std::fs::create_dir_all(work.join("http-server")).expect("http-server dir");
    std::fs::copy(
        server.join("package.vlt"),
        work.join("http-server/package.vlt"),
    )
    .expect("http-server package.vlt");
    copy_dir(&server.join("src"), &work.join("http-server/src"));
    Some((work, modules))
}

/// Builds `app/` in `work`, a release build when `release`; gives the Worker's directory.
fn build(work: &Path, release: bool) -> PathBuf {
    let dir = work.join("app");
    let worker = dir.join("target/velt/app-workers");
    // Only this build's files: run.mjs uploads every module there, and the size counts them.
    let _ = std::fs::remove_dir_all(&worker);
    let mut args = vec!["build", "--host", "host-cloudflare"];
    if release {
        args.push("--release");
    }
    let o = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(&args)
        .current_dir(&dir)
        .output()
        .expect("run velt");
    assert!(
        o.status.success(),
        "velt {args:?}:\n{}",
        String::from_utf8_lossy(&o.stderr)
    );
    worker
}

/// Runs `script` under node with `args`; gives its stdout, failing unless it succeeded.
fn node(work: &Path, script: &str, args: &[&Path]) -> String {
    let mut cmd = no_window::command("node");
    // In the work directory, which the test removes: Miniflare may write a `.wrangler/` cache into
    // the directory it runs in.
    cmd.current_dir(work).arg(for_node(&work.join(script)));
    for a in args {
        cmd.arg(for_node(a));
    }
    let o = cmd.output().expect("run node");
    let out = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
    assert!(
        o.status.success(),
        "{script}: exit {:?}\n--- stdout ---\n{out}--- stderr ---\n{}",
        o.status.code(),
        String::from_utf8_lossy(&o.stderr)
    );
    out
}

/// The Worker answers under workerd as tests/workerd/app.out says: text, JSON, a 100 kB request
/// body, a streamed response, a handler that throws, a 404, a `fetch` of another service,
/// requests at once, and each binding of packages/cloudflare.
#[test]
fn a_worker_runs_under_workerd() {
    let Some((work, modules)) = fixtures("app") else {
        return;
    };
    let want = std::fs::read_to_string(work.join("app.out"))
        .expect("expected output")
        .replace("\r\n", "\n");
    for release in [false, true] {
        let worker = build(&work, release);
        let got = node(&work, "run.mjs", &[&modules, &worker]);
        assert!(
            got == want,
            "release {release}:\n--- want ---\n{want}--- got ---\n{got}"
        );
        if release {
            within_budget(&worker, APP_BUDGET);
        }
    }
    let _ = std::fs::remove_dir_all(&work);
}

/// The release Worker starts and answers within budgets well inside Cloudflare's limits.
#[test]
fn startup_and_cpu_within_budget() {
    if !set("VELT_WORKERD_TIMINGS") {
        eprintln!("note: VELT_WORKERD_TIMINGS is not 1; skipping the Worker timings");
        return;
    }
    let Some((work, _)) = fixtures("cpu") else {
        return;
    };
    let worker = build(&work, true);
    let out = node(&work, "cpu.mjs", &[&worker, Path::new("300")]);
    eprintln!("{out}");
    let report: serde_json::Value = serde_json::from_str(out.trim()).expect("cpu.mjs's report");
    let ms = |path: &[&str]| {
        path.iter()
            .fold(&report, |v, k| &v[*k])
            .as_f64()
            .expect("a number in cpu.mjs's report")
    };
    let startup = ms(&["startup"]);
    assert!(
        startup <= STARTUP_BUDGET,
        "start-up {startup} ms, over the budget of {STARTUP_BUDGET} ms"
    );
    for path in ["hello", "json"] {
        let cpu = ms(&[path, "p50"]);
        assert!(
            cpu <= CPU_BUDGET,
            "/{path}: {cpu} ms of CPU per request, over the budget of {CPU_BUDGET} ms"
        );
    }
    let _ = std::fs::remove_dir_all(&work);
}
