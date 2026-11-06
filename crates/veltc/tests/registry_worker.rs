//! The registry's Worker (packages/registry-cloudflare) under workerd, with workerd's local R2, D1
//! and Durable Objects: packages/registry-conformance over HTTP, and the publish lock's Durable
//! Object directly (#1740 step 4). This file only builds and starts them; the cases are in the
//! package and in registry_worker/lock.mjs.
//!
//! Needs what tests/workerd.rs needs, and is skipped or required the same way.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};

mod no_window;
mod runtime_support;
mod wasm_tools;
mod work_dir;
mod worker_size;

use wasm_tools::{build_runtime, copy_dir, for_node, missing_tools, root};
use worker_size::within_budget;

const TRIPLE: &str = "wasm32-unknown-unknown";

/// The release Worker, its files gzipped one by one: 1.5 times its 620,692 bytes.
const WORKER_BUDGET: usize = 931_000;

/// Stops the server when the test ends, also on failure: closing its stdin lets it stop workerd,
/// which killing it would leave running.
struct Stop(Child);

impl Drop for Stop {
    fn drop(&mut self) {
        drop(self.0.stdin.take());
        for _ in 0..100 {
            if matches!(self.0.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The packages in a work directory of their own, tests/workerd's node_modules, and the release
/// Worker's directory, built there.
fn build(name: &str) -> Option<(PathBuf, PathBuf, PathBuf)> {
    let root = root();
    let modules = root.join("tests/workerd/node_modules");
    let missing = missing_tools(TRIPLE, "node").or_else(|| {
        (!modules.join("miniflare").is_dir())
            .then(|| "tests/workerd's packages (`npm ci` there)".to_string())
    });
    if let Some(missing) = missing {
        assert!(
            std::env::var_os("VELT_WORKERD").is_none_or(|v| v != "1"),
            "the workerd test is required, but {missing} not available"
        );
        eprintln!("note: {missing} not available; skipping the registry's Worker test");
        return None;
    }
    build_runtime(&root, TRIPLE);
    runtime_support::build_native_runtime(&root);
    let base = work_dir::work_dir(&root, "registry-worker-work")
        .join(format!("{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    for package in [
        "registry-cloudflare",
        "registry-conformance",
        "registry-core",
        "cloudflare",
    ] {
        copy_dir(&root.join("packages").join(package), &base.join(package));
    }
    // packages/http-server's sources (not its build output or tests).
    let server = root.join("packages/http-server");
    std::fs::create_dir_all(base.join("http-server")).expect("http-server dir");
    std::fs::copy(
        server.join("package.velt"),
        base.join("http-server/package.velt"),
    )
    .expect("http-server package.velt");
    copy_dir(&server.join("src"), &base.join("http-server/src"));
    copy_dir(
        &root.join("crates/veltc/tests/registry_worker"),
        &base.join("scripts"),
    );
    let package = base.join("registry-cloudflare");
    let args = ["build", "--release", "--host", "registry-cloudflare"];
    let out = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .current_dir(&package)
        .output()
        .expect("run velt");
    assert!(
        out.status.success(),
        "velt {args:?}:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let worker = package.join("target/velt/registry-cloudflare-workers");
    Some((base, modules, worker))
}

fn script(base: &Path, name: &str) -> PathBuf {
    for_node(&base.join("scripts").join(name))
}

/// Starts the Worker with `users` (`name=token`), logging its purges to `purges`; gives the server
/// and its URL.
fn serve(
    base: &Path,
    modules: &Path,
    worker: &Path,
    purges: &Path,
    users: &[&str],
) -> (Stop, String) {
    let mut child = no_window::command("node")
        .current_dir(base)
        .arg(script(base, "serve.mjs"))
        .arg(for_node(modules))
        .arg(for_node(worker))
        .arg(for_node(purges))
        .args(users)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("run node");
    let stdout = child.stdout.take().expect("piped stdout");
    let server = Stop(child);
    let mut lines = BufReader::new(stdout).lines();
    // Miniflare may log before the URL; a closed pipe would fail what it logs after.
    let url = lines
        .by_ref()
        .map_while(Result::ok)
        .find(|line| line.starts_with("http://"))
        .expect("no URL from serve.mjs");
    std::thread::spawn(move || lines.for_each(drop));
    (server, url)
}

#[test]
fn the_conformance_suite_passes() {
    let Some((base, modules, worker)) = build("suite") else {
        return;
    };
    within_budget(&worker, WORKER_BUDGET);
    let (alice, bob) = ("token-of-alice", "token-of-bob");
    let users = [format!("alice={alice}"), format!("bob={bob}")];
    let users = [users[0].as_str(), users[1].as_str()];
    let purges = base.join("purges.log");
    let (server, url) = serve(&base, &modules, &worker, &purges, &users);
    let out = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["run", "src/main.velt", &url, "--alice", alice, "--bob", bob])
        .current_dir(base.join("registry-conformance"))
        .output()
        .expect("run velt");
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // Publishing purged the package's index, through Cloudflare's API, for the public URL.
    let purges = std::fs::read_to_string(&purges).expect("the Worker purged nothing");
    let line = purges.lines().next().expect("a purge");
    let purge: serde_json::Value = serde_json::from_str(line).expect("a purge as JSON");
    assert_eq!(
        purge["url"],
        "https://api.cloudflare.com/client/v4/zones/the-zone/purge_cache"
    );
    assert_eq!(purge["method"], "POST");
    assert_eq!(purge["authorization"], "Bearer the-purge-token");
    let file = purge["body"]["files"][0].as_str().expect("a file to purge");
    assert!(
        file.starts_with("https://registry.test/api/v1/conf-") && file.ends_with("/index"),
        "purged `{file}`"
    );
    drop(server);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn the_publish_lock_serializes_and_recovers() {
    let Some((base, modules, worker)) = build("lock") else {
        return;
    };
    let out = no_window::command("node")
        .current_dir(&base)
        .arg(script(&base, "lock.mjs"))
        .arg(for_node(&modules))
        .arg(for_node(&worker))
        .output()
        .expect("run node");
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&base);
}
