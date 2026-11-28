//! A program whose last `fetch` is done ends at once, as Node's (#3964): the connection it keeps
//! for the next request (an HTTP/2 session for `https:`, a pooled HTTP/1.1 connection for
//! `http:`) does not keep the process alive. The server runs in a process of its own, so its
//! connection stays open after the client's last response (a server in the client's process
//! would keep that process alive itself, and closing it would close the connection). The
//! client's two requests share one connection (the server names the client's address), and the
//! server, which only listens, is still running after the client ended.
//!
//! With the debug runtime's counters (`VELT_RC_STATS=1`) the end gives the idle session back
//! first, so its blocks are freed as the rest (`blocks=A/F` with A == F).
//!
//! The HTTP/2 client session's wait for its server kept the process alive until the session's
//! idle close, 4 s after the last response: the bound below (2.5 s for the whole client, which
//! takes a few milliseconds) fails that and leaves room for a slow machine.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use no_window::command;

mod no_window;
mod runtime_support;
mod test_dir;

fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn exe_name(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}{}", if cfg!(windows) { ".exe" } else { "" }))
}

/// `velt build` in `dir` (`file`: that file; None: the package there) to `exe`.
fn build(dir: &Path, file: Option<&str>, exe: &Path) {
    let mut cmd = command(env!("CARGO_BIN_EXE_velt"));
    cmd.arg("build");
    if let Some(f) = file {
        cmd.arg(dir.join(f));
    }
    let o = cmd.arg("-o").arg(exe).current_dir(dir).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

// timing: how long the client runs is what this test checks (the bug kept it alive 4 s).
const LIMIT: Duration = Duration::from_millis(2500);

const SERVER: &str = r#"import { HttpRequest, HttpResponse, serveHttp } from "http-server";
import { TEST_CERT, TEST_KEY } from "./_tls";

// Every response names the client's address: one connection, one client port.
async function handler(req: HttpRequest, res: HttpResponse) {
  res.send(req.remoteAddr());
}

async function main() {
  const tls = await serveHttp({ port: 0, tls: { cert: TEST_CERT, key: TEST_KEY } }, handler);
  const plain = await serveHttp({ port: 0 }, handler);
  console.error(`${tls.port} ${plain.port}`);
}
"#;

const CLIENT: &str = r#"import { TEST_CA } from "./_tls";

const url = process.argv[2];
const a = await (await fetch(url, { ca: TEST_CA })).text();
const b = await (await fetch(url, { ca: TEST_CA })).text();
console.log(a == b ? "one connection" : `two connections: ${a} ${b}`);
"#;

/// The server, built and started: the process and its two ports (https, http).
fn start_server(dir: &Path, tls: &str) -> (Child, String, String) {
    let server = manifest().join("../../packages/http-server");
    let at = server.to_string_lossy().replace('\\', "/");
    std::fs::write(
        dir.join("package.velt"),
        format!(
            "import type {{ Package }} from \"velt/package\";\n\nexport const pkg: Package = {{\n  name: \"idle-server\",\n  version: \"0.0.0\",\n  dependencies: {{ \"http-server\": {{ path: \"{at}\" }} }},\n}};\n"
        ),
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/_tls.velt"), tls).unwrap();
    std::fs::write(dir.join("src/main.velt"), SERVER).unwrap();
    let exe = exe_name(dir, "idle-server");
    build(dir, None, &exe);
    let mut child = command(&exe)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stderr.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let mut ports = line.split_whitespace().map(str::to_string);
    match (ports.next(), ports.next()) {
        (Some(a), Some(b)) => (child, a, b),
        _ => {
            let _ = child.kill();
            panic!("the server printed no ports: {line:?}");
        }
    }
}

/// `exe url` with `threads` workers (`stats`: with the counters): its stdout, its stderr and how
/// long it ran (killed after a minute).
fn run_client(exe: &Path, url: &str, threads: u32, stats: bool) -> (String, String, Duration) {
    let start = Instant::now();
    let mut cmd = command(exe);
    if stats {
        cmd.env("VELT_RC_STATS", "1");
    }
    let mut child = cmd
        .arg(url)
        .env("VELT_THREADS", threads.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A watchdog, not a synchronization: a client that never ends would hang the test run.
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > Duration::from_secs(60) {
            child.kill().unwrap();
            panic!("{url} (threads {threads}): still running after a minute");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let took = start.elapsed();
    let o = child.wait_with_output().unwrap();
    assert!(
        o.status.success(),
        "{url} (threads {threads}): {}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    (
        String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n"),
        String::from_utf8_lossy(&o.stderr).into_owned(),
        took,
    )
}

/// The blocks a `rc stats: ... blocks=A/F` report on `stderr` left unfreed (None: no report, a
/// runtime without the counters).
fn unfreed(stderr: &str) -> Option<i64> {
    let line = stderr.lines().find(|l| l.starts_with("rc stats:"))?;
    let counts = line.split("blocks=").nth(1)?.split_whitespace().next()?;
    let (a, f) = counts.split_once('/')?;
    Some(a.parse::<i64>().ok()? - f.parse::<i64>().ok()?)
}

#[test]
fn a_program_ends_once_its_last_fetch_is_done() {
    runtime_support::build_native_runtime(manifest());
    let tls = std::fs::read_to_string(manifest().join("../../tests/golden/std/_tls.velt")).unwrap();
    let server_dir = test_dir::TestDir::new();
    let client_dir = test_dir::TestDir::new();
    let (mut server, https_port, http_port) = start_server(server_dir.path(), &tls);
    std::fs::write(client_dir.path().join("_tls.velt"), &tls).unwrap();
    std::fs::write(client_dir.path().join("client.velt"), CLIENT).unwrap();
    let client = exe_name(client_dir.path(), "client");
    build(client_dir.path(), Some("client.velt"), &client);
    let mut failures = Vec::new();
    for url in [
        format!("https://localhost:{https_port}/"),
        format!("http://127.0.0.1:{http_port}/"),
    ] {
        for threads in [1, 4, 8] {
            let (out, _, took) = run_client(&client, &url, threads, false);
            if out != "one connection\n" || took > LIMIT {
                failures.push(format!("{url} (threads {threads}): {out:?} in {took:?}"));
            }
        }
        for threads in [1, 4] {
            let (out, err, took) = run_client(&client, &url, threads, true);
            let left = unfreed(&err).unwrap_or(0);
            if out != "one connection\n" || took > LIMIT || left != 0 {
                failures.push(format!(
                    "{url} (threads {threads}, counted): {out:?} in {took:?}, {left} blocks left: {err}"
                ));
            }
        }
    }
    // The server only listens now: that keeps it running.
    let alive = server.try_wait().unwrap().is_none();
    let _ = server.kill();
    let _ = server.wait();
    assert!(alive, "the server ended while listening");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
