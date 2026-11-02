//! Cloudflare Workers (`velt build --host workers`, docs/internals/contracts/workers.md) under
//! node with a fake Workers environment: tests/workers/drive.mjs imports the build's
//! `worker.mjs` as Workers do and sends it requests. `serve/` is an accept loop in a package
//! that uses `probe/`, a package shipping its own host module through `hostModules`;
//! `hello.vlt` never accepts. Debug and release builds; the release builds' Workers must stay
//! within their size budgets. Also the build's errors for host modules nothing provides.
//!
//! Needs what the browser goldens need (tests/wasm_goldens.rs): LLVM's `opt`/`llc`, the
//! wasm32-unknown-unknown Rust target and node; skipped with a note when one is missing.

mod no_window;
mod wasm_tools;
mod work_dir;

use std::path::{Path, PathBuf};

use wasm_tools::{build_runtime, copy_dir, for_node, missing_tools, root};

const TRIPLE: &str = "wasm32-unknown-unknown";

/// The most a release Worker of `hello.vlt` may weigh, its files gzipped one by one (what
/// Cloudflare's limit measures, 3 MB on the Free plan): 1.5 times its 58,550 bytes.
const HELLO_BUDGET: usize = 88_000;
/// The same for `serve/`, an accept loop with a package host module: 1.5 times 115,811 bytes.
const SERVE_BUDGET: usize = 174_000;

/// The fixtures, copied into a work directory of their own (a build writes `target/` beside
/// its program).
fn fixtures(name: &str) -> Option<(PathBuf, PathBuf)> {
    if let Some(missing) = missing_tools(TRIPLE, "node") {
        eprintln!("note: {missing} not available; skipping the Workers test");
        return None;
    }
    let root = root();
    build_runtime(&root, TRIPLE);
    let work =
        work_dir::work_dir(&root, "workers-work").join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    copy_dir(&root.join("crates/veltc/tests/workers"), &work);
    Some((root, work))
}

fn velt(dir: &Path, args: &[&str]) -> std::process::Output {
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run velt")
}

/// Build in `dir` with `args`, drive the Worker in `worker` with `scenario`, and compare what
/// it prints with `<scenario>.out`.
fn drive(work: &Path, dir: &Path, args: &[&str], worker: &str, scenario: &str) {
    let o = velt(dir, args);
    assert!(
        o.status.success(),
        "velt {args:?}:\n{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let warned = String::from_utf8_lossy(&o.stderr).contains("without `--release`");
    assert_eq!(warned, !args.contains(&"--release"), "{args:?}");
    let o = no_window::command("node")
        .arg(for_node(&work.join("drive.mjs")))
        .arg(for_node(&dir.join(worker)))
        .arg(scenario)
        .current_dir(dir)
        .output()
        .expect("run node");
    let want = std::fs::read_to_string(work.join(format!("{scenario}.out")))
        .expect("expected output")
        .replace("\r\n", "\n");
    let got = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
    assert!(
        o.status.success() && got == want,
        "{scenario} {args:?}: exit {:?}\n--- want ---\n{want}--- got ---\n{got}--- stderr ---\n{}",
        o.status.code(),
        String::from_utf8_lossy(&o.stderr)
    );
}

/// The Worker's files in `dir`, gzipped one by one.
fn gzipped(dir: &Path) -> usize {
    use std::io::Write;
    let mut total = 0;
    for entry in std::fs::read_dir(dir).expect("worker dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            total += gzipped(&path);
            continue;
        }
        let ext = path.extension().and_then(|e| e.to_str());
        if matches!(ext, Some("wasm" | "mjs")) {
            let bytes = std::fs::read(&path).expect("read");
            let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
            gz.write_all(&bytes).expect("gzip");
            total += gz.finish().expect("gzip").len();
        }
    }
    total
}

fn within_budget(worker: &Path, budget: usize) {
    let size = gzipped(worker);
    eprintln!("{}: {size} bytes gzipped", worker.display());
    assert!(
        size <= budget,
        "{}: {size} bytes gzipped, over the budget of {budget}",
        worker.display()
    );
}

/// An accept loop: one instance for many requests, `main` once, requests waiting while `main`
/// does and served at once, bodies both ways, `waitUntil` while work goes on after an answer,
/// a dropped request and a panic (a 500 for the requests in flight, then a new instance).
#[test]
fn an_accept_loop_serves_requests() {
    let Some((_, work)) = fixtures("serve") else {
        return;
    };
    let dir = work.join("serve");
    for mode in [None, Some("--release")] {
        let args: Vec<&str> = ["build", "--host", "workers"]
            .into_iter()
            .chain(mode)
            .collect();
        drive(&work, &dir, &args, "target/velt/serve-worker", "serve");
    }
    let worker = dir.join("target/velt/serve-worker");
    assert!(worker.join("hosts/probe/probe.mjs").is_file());
    within_budget(&worker, SERVE_BUDGET);
    let _ = std::fs::remove_dir_all(&work);
}

/// A program that never accepts: its requests get a 500 saying so, logged once.
#[test]
fn a_program_that_never_accepts_answers_500() {
    let Some((_, work)) = fixtures("never") else {
        return;
    };
    for mode in [None, Some("--release")] {
        let args: Vec<&str> = ["build", "--host", "workers"]
            .into_iter()
            .chain(mode)
            .chain(["hello.vlt", "-o", "hello-worker"])
            .collect();
        drive(&work, &work, &args, "hello-worker", "never");
    }
    let config = std::fs::read_to_string(work.join("hello-worker/wrangler.toml")).expect("toml");
    assert!(
        config.contains("name = \"hello\"\nmain = \"worker.mjs\"\n"),
        "{config}"
    );
    within_budget(&work.join("hello-worker"), HELLO_BUDGET);
    let _ = std::fs::remove_dir_all(&work);
}

/// A host module nothing provides is an error at its declaration.
#[test]
fn a_host_module_nothing_provides_is_an_error() {
    let Some((_, work)) = fixtures("missing") else {
        return;
    };
    let o = velt(&work, &["build", "--host", "workers", "missing.vlt"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success(), "{err}");
    assert!(
        err.contains("the `workers` build has no JavaScript for host module `nowhere`")
            && err.contains("missing.vlt:4:")
            && err.contains("only a package provides host modules"),
        "{err}"
    );
    // A plain browser build leaves the host functions to its embedder.
    let o = velt(&work, &["build", "--target", TRIPLE, "missing.vlt"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let _ = std::fs::remove_dir_all(&work);
}

/// `--host` with another target, and a host Velt doesn't know.
#[test]
fn host_option_errors() {
    let dir = tempfile::tempdir().expect("temp dir");
    for (args, want) in [
        (
            &["build", "--host", "workers", "--target", "wasm32-wasip1", "a.vlt"][..],
            "`--host workers` builds for `wasm32-unknown-unknown`: leave out `--target wasm32-wasip1`",
        ),
        (&["build", "--host", "deno", "a.vlt"], "unknown host `deno` (expected workers)"),
        (&["run", "--host", "workers", "a.vlt"], "`velt run` cannot run a Worker"),
    ] {
        let o = velt(dir.path(), args);
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(!o.status.success() && err.contains(want), "{args:?}: {err}");
    }
}

/// The glue's `HostRef` table over a long-lived instance: slots are used again, with a generation
/// that makes a released slot miss.
#[test]
fn host_ref_slots_are_used_again() {
    if !wasm_tools::runs("node") {
        eprintln!("note: node not available; skipping the HostRef table test");
        return;
    }
    let root = root();
    let o = no_window::command("node")
        .arg(for_node(&root.join("crates/veltc/tests/workers/refs.mjs")))
        .arg(for_node(&root.join("crates/velt_rt_wasm/js/velt_web.mjs")))
        .output()
        .expect("run node");
    assert_eq!(
        String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n"),
        "indexes up to 3, 2 held, kept kept\n\
         a released slot misses: true; its index holds another value: true\n\
         slots are u32: true\n",
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
}
