//! The registry's Worker (packages/registry-cloudflare) under workerd, with workerd's local R2 and
//! D1: packages/registry-conformance over HTTP, the upload limit, and signing in with GitHub
//! (#1740 step 4). This file only builds and starts them; the cases are in the package and in
//! registry_worker/login.mjs.
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
    uploads_are_limited_to_10_mib(&base, &url, alice);
    signing_in_gives_tokens_that_publish(&base, &url);
    signing_in_fails_closed(&base, &modules, &worker);
    commits_from_a_stale_revision_lose(&base, &modules);
    drop(server);
    let _ = std::fs::remove_dir_all(&base);
}

/// The answer to a `PUT` of `path` whose head announces a body of `length` bytes, none of which is
/// sent: the registry refuses a body announced too large before reading it.
fn put_announcing(url: &str, path: &str, token: &str, length: usize) -> String {
    use std::io::{Read, Write};
    let addr = url.trim_start_matches("http://").trim_end_matches('/');
    let mut stream = std::net::TcpStream::connect(addr).expect("connect to the Worker");
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(60)))
        .expect("read timeout");
    write!(
        stream,
        "PUT {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {token}\r\nX-Velt-Checksum: sha256:00\r\nContent-Length: {length}\r\n\r\n"
    )
    .expect("send the head");
    let mut answer = Vec::new();
    let mut buf = [0u8; 4096];
    // Up to the end of the body, which the head's `content-length` gives.
    loop {
        let n = stream.read(&mut buf).unwrap_or(0);
        if n == 0 {
            break;
        }
        answer.extend_from_slice(&buf[..n]);
        let text = String::from_utf8_lossy(&answer).to_string();
        if let Some(end) = text.find("\r\n\r\n") {
            let length = text[..end]
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("content-length")
                        .then(|| v.trim().parse().ok())?
                })
                .unwrap_or(0usize);
            if answer.len() >= end + 4 + length {
                break;
            }
        }
    }
    String::from_utf8_lossy(&answer).to_string()
}

/// The Worker takes archives up to its 10 MiB, past http-server's default body limit (8 MiB),
/// and refuses a larger one. Publishes with `velt publish`, as `token`'s user.
fn uploads_are_limited_to_10_mib(base: &Path, url: &str, token: &str) {
    // `velt publish` of a larger one only fails (the Worker answers before the body is sent, and
    // the client reports the closed connection), so the refusal itself is read here.
    let answer = put_announcing(url, "/api/v1/big-11/1.0.0", token, 11 * 1024 * 1024);
    assert!(
        answer.starts_with("HTTP/1.1 413")
            && answer.ends_with("the upload is larger than 10485760 bytes"),
        "a body announced as 11 MiB: {answer}"
    );
    const MIB: usize = 1024 * 1024;
    for (name, size, ok) in [("big-9", 9 * MIB, true), ("big-11", 11 * MIB, false)] {
        let out = publish(base, url, token, name, "1.0.0", size);
        assert_eq!(
            out.status.success(),
            ok,
            "velt publish of {name}:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// `velt publish` of package `name` `version`, with a file of `size` bytes, as `token`'s user.
fn publish(
    base: &Path,
    url: &str,
    token: &str,
    name: &str,
    version: &str,
    size: usize,
) -> std::process::Output {
    let dir = base.join(format!("{name}-{version}"));
    std::fs::create_dir_all(dir.join("src")).expect("package dir");
    let manifest = format!(
        "import type {{ Package }} from \"velt/package\";\n\nexport const pkg: Package = {{\n  name: \"{name}\",\n  version: \"{version}\",\n  entry: \"src/lib.velt\",\n}};\n"
    );
    std::fs::write(dir.join("package.velt"), manifest).expect("package.velt");
    std::fs::write(dir.join("src/lib.velt"), "export const V: i64 = 1;\n").expect("lib");
    std::fs::write(dir.join("src/data.bin"), vec![b'x'; size]).expect("data");
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .arg("publish")
        .env("VELT_HOME", base.join("velt-home"))
        .env("VELT_REGISTRY", url)
        .env("VELT_REGISTRY_TOKEN", token)
        .current_dir(&dir)
        .output()
        .expect("run velt")
}

/// D1MetadataDb's commit, from a revision that is no longer the package's, changes nothing
/// (registry_worker/d1probe.mjs, on registry_worker/d1-probe built as a Worker of its own).
fn commits_from_a_stale_revision_lose(base: &Path, modules: &Path) {
    let probe = base.join("scripts/d1-probe");
    let args = ["build", "--release", "--host", "registry-cloudflare"];
    let out = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .current_dir(&probe)
        .output()
        .expect("run velt");
    assert!(
        out.status.success(),
        "velt {args:?} in d1-probe:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let worker = probe.join("target/velt/d1-probe-workers");
    let out = no_window::command("node")
        .current_dir(base)
        .arg(script(base, "d1probe.mjs"))
        .arg(for_node(modules))
        .arg(for_node(&worker))
        .output()
        .expect("run node");
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A Worker without its GitHub secret starts no sign-in, and a user who owns packages can't be
/// deleted (registry_worker/accounts.mjs, on a Worker of its own).
fn signing_in_fails_closed(base: &Path, modules: &Path, worker: &Path) {
    let out = no_window::command("node")
        .current_dir(base)
        .arg(script(base, "accounts.mjs"))
        .arg(for_node(modules))
        .arg(for_node(worker))
        .output()
        .expect("run node");
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Signing in with GitHub (registry_worker/login.mjs, against serve.mjs's stub of GitHub) gives
/// tokens that publish: carol publishes with her first token, and a second version, which only
/// an owner may, with the token of her second sign-in.
fn signing_in_gives_tokens_that_publish(base: &Path, url: &str) {
    let out = no_window::command("node")
        .current_dir(base)
        .arg(script(base, "login.mjs"))
        .arg(url)
        .output()
        .expect("run node");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let line = stdout.lines().find_map(|l| l.strip_prefix("tokens "));
    let [first, second] = line
        .expect("no tokens from login.mjs")
        .split(' ')
        .collect::<Vec<_>>()[..]
    else {
        panic!("two tokens from login.mjs: {stdout}");
    };
    for (token, version) in [(first, "1.0.0"), (second, "1.1.0")] {
        let out = publish(base, url, token, "carols-lib", version, 10);
        assert!(
            out.status.success(),
            "carol's publish of {version}:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
