//! `velt build --host <package>` (docs/internals/contracts/workers.md) with the host package
//! packages/host-cloudflare, under node with a fake Workers environment: tests/workers/drive.mjs
//! imports the build's `worker.mjs` (the host package's entry over the glue's core) as Workers do
//! and sends it requests. `serve/` is an accept loop in a package that uses `probe/`, a package
//! shipping its own host module through `hostModules`; `single/hello.velt` never accepts. Debug
//! and release builds; the release builds must stay within their size budgets. Also the build's
//! errors: host modules nothing provides, `velt:` modules outside std, a host that isn't one, a
//! directory of the user's.
//!
//! Needs what the browser goldens need (tests/wasm_goldens.rs): LLVM's `opt`/`llc`, the
//! wasm32-unknown-unknown Rust target and node; skipped with a note when one is missing.

mod no_window;
mod wasm_tools;
mod work_dir;
mod worker_size;

use std::path::{Path, PathBuf};

use wasm_tools::{build_runtime, copy_dir, for_node, missing_tools, root};
use worker_size::within_budget;

const TRIPLE: &str = "wasm32-unknown-unknown";

/// The host package the fixtures depend on (copied beside them).
const HOST: &str = "host-cloudflare";

/// The most a release Worker of `hello.velt` may weigh, its files gzipped one by one (what
/// Cloudflare's limit measures, 3 MB on the Free plan): 1.5 times its 59,872 bytes.
const HELLO_BUDGET: usize = 90_000;
/// The same for `serve/`, an accept loop with a package host module: 1.5 times 117,799 bytes.
const SERVE_BUDGET: usize = 177_000;
/// The same for `served/`, packages/http-server's `serve` with JSON and streamed bodies: 1.5
/// times 309,826 bytes.
const SERVED_BUDGET: usize = 465_000;

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
    copy_dir(&root.join("packages").join(HOST), &work.join(HOST));
    // packages/http-server's sources (not its build output or tests).
    let server = root.join("packages/http-server");
    std::fs::create_dir_all(work.join("http-server")).expect("http-server dir");
    std::fs::copy(
        server.join("package.velt"),
        work.join("http-server/package.velt"),
    )
    .expect("http-server package.velt");
    copy_dir(&server.join("src"), &work.join("http-server/src"));
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
        let args: Vec<&str> = ["build", "--host", HOST].into_iter().chain(mode).collect();
        drive(&work, &dir, &args, "target/velt/serve-workers", "serve");
    }
    let worker = dir.join("target/velt/serve-workers");
    assert!(worker.join("hosts/probe/probe.mjs").is_file());
    within_budget(&worker, SERVE_BUDGET);
    let _ = std::fs::remove_dir_all(&work);
}

/// packages/http-server's `serve` on a `--host` build: the program it runs natively, `main`
/// awaiting before it serves and returning after; request bodies streamed from the host as the
/// handler reads them; text, JSON, streamed and failed responses; the client's address.
#[test]
fn http_server_serves_on_a_js_host() {
    let Some((_, work)) = fixtures("served") else {
        return;
    };
    let dir = work.join("served");
    for mode in [None, Some("--release")] {
        let args: Vec<&str> = ["build", "--host", HOST].into_iter().chain(mode).collect();
        drive(&work, &dir, &args, "target/velt/served-workers", "served");
    }
    within_budget(&dir.join("target/velt/served-workers"), SERVED_BUDGET);
    let _ = std::fs::remove_dir_all(&work);
}

/// An accept loop over std's velt/host/worker alone builds (std's module loads the hooks that
/// await host promises) and serves.
#[test]
fn std_alone_serves() {
    let Some((_, work)) = fixtures("bare") else {
        return;
    };
    let dir = work.join("bare");
    drive(
        &work,
        &dir,
        &["build", "--host", HOST],
        "target/velt/bare-workers",
        "bare",
    );
    let _ = std::fs::remove_dir_all(&work);
}

/// A `fetch` response's body is read in the context of the request that fetched it, even when
/// another request's turn reads it (platforms tie I/O to the request that made it); the
/// program's timer fires while a request's body waits for its client; a second answer is logged
/// and ignored, and the instance goes on.
#[test]
fn a_fetch_response_is_read_in_its_request_s_context() {
    let Some((_, work)) = fixtures("relay") else {
        return;
    };
    let dir = work.join("relay");
    drive(
        &work,
        &dir,
        &["build", "--host", HOST],
        "target/velt/relay-workers",
        "relay",
    );
    let _ = std::fs::remove_dir_all(&work);
}

/// A program that never accepts: its requests get a 500 saying so, logged once.
#[test]
fn a_program_that_never_accepts_answers_500() {
    let Some((_, work)) = fixtures("never") else {
        return;
    };
    let dir = work.join("single");
    for mode in [None, Some("--release")] {
        let args: Vec<&str> = ["build", "--host", HOST]
            .into_iter()
            .chain(mode)
            .chain(["hello.velt", "-o", "hello-worker"])
            .collect();
        drive(&work, &dir, &args, "hello-worker", "never");
    }
    // The host package's scaffolded file, with the program's name.
    let config = std::fs::read_to_string(dir.join("hello-worker/wrangler.toml")).expect("toml");
    assert!(
        config.contains("name = \"hello\"\nmain = \"worker.mjs\"\n"),
        "{config}"
    );
    within_budget(&dir.join("hello-worker"), HELLO_BUDGET);
    let _ = std::fs::remove_dir_all(&work);
}

/// A host module nothing provides is an error at its declaration; `velt:` modules are std's.
#[test]
fn a_host_module_nothing_provides_is_an_error() {
    let Some((_, work)) = fixtures("missing") else {
        return;
    };
    let dir = work.join("single");
    let o = velt(&dir, &["build", "--host", HOST, "missing.velt"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success(), "{err}");
    assert!(
        err.contains(
            "the `--host host-cloudflare` build has no JavaScript for host module `nowhere`"
        ) && err.contains("missing.velt:4:")
            && err.contains("list it in `single`'s package.velt"),
        "{err}"
    );
    let o = velt(&dir, &["build", "--host", HOST, "glue.velt"]);
    let err = String::from_utf8_lossy(&o.stderr);
    for want in [
        "glue.velt:4:",
        "glue.velt:5:",
        "the import modules `velt:…` are the runtime glue's",
    ] {
        assert!(!o.status.success() && err.contains(want), "{want}:\n{err}");
    }
    // A plain browser build leaves the host functions to its embedder.
    let o = velt(&dir, &["build", "--target", TRIPLE, "missing.velt"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let _ = std::fs::remove_dir_all(&work);
}

/// `--host` naming no host package, with another target, outside a package, into a directory
/// of the user's, and for `velt run`.
#[test]
fn host_option_errors() {
    let Some((_, work)) = fixtures("errors") else {
        return;
    };
    let dir = work.join("single");
    let mine = dir.join("mine");
    std::fs::create_dir_all(&mine).expect("mkdir");
    std::fs::write(mine.join("worker.mjs"), "// the user's\n").expect("write");
    let outside = tempfile::tempdir().expect("temp dir");
    std::fs::write(outside.path().join("a.velt"), "function main() {}\n").expect("write");
    for (at, args, want) in [
        (
            &dir,
            &["build", "--host", HOST, "--target", "wasm32-wasip1", "hello.velt"][..],
            "`--host host-cloudflare` builds for `wasm32-unknown-unknown`: leave out `--target wasm32-wasip1`",
        ),
        (&dir, &["build", "--host", "nope", "hello.velt"], "no package `nope` among the build's dependencies"),
        (&dir, &["build", "--host", "single", "hello.velt"], "package `single` is not a host package"),
        (
            &dir,
            &["build", "--host", HOST, "hello.velt", "-o", "mine"],
            "holds files a `--host` build did not write (`worker.mjs`…)",
        ),
        (&outside.path().to_path_buf(), &["build", "--host", HOST, "a.velt"], "builds a package"),
        (&dir, &["run", "--host", HOST, "hello.velt"], "`velt run` cannot run a `--host` build"),
    ] {
        let o = velt(at, args);
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(!o.status.success() && err.contains(want), "{args:?}: {err}");
    }
    assert_eq!(
        std::fs::read_to_string(mine.join("worker.mjs")).expect("read"),
        "// the user's\n"
    );
    let _ = std::fs::remove_dir_all(&work);
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
