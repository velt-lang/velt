//! packages/registry-conformance over HTTP against `velt registry serve`, which pins what the Rust
//! server answers before the Velt server replaces it (#1740 step 2). This file only starts the
//! servers; the cases are in the package, and it goes away with `crates/velt_registry`.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, Stdio};

mod no_window;
mod runtime_support;
// Only `root` and `copy_dir`.
#[allow(dead_code)]
mod wasm_tools;
mod work_dir;

use wasm_tools::{copy_dir, root};

/// Kills the server when the test ends, also on failure.
struct Kill(Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn add_user(dir: &Path, name: &str) -> String {
    let dir_arg = dir.to_str().expect("a UTF-8 path");
    let out = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["registry", "user", "add", name, "--dir", dir_arg])
        .output()
        .expect("run velt");
    assert!(out.status.success(), "{out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Serves `dir`; returns the server, its log (a closed pipe could fail its writes) and its URL.
fn serve(dir: &Path) -> (Kill, BufReader<ChildStderr>, String) {
    let mut child = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["registry", "serve", "--port", "0", "--dir"])
        .arg(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run velt");
    let stderr = child.stderr.take().expect("piped stderr");
    let server = Kill(child);
    let mut log = BufReader::new(stderr);
    let mut line = String::new();
    log.read_line(&mut line)
        .expect("read the server's first line");
    let start = line
        .find("http://")
        .unwrap_or_else(|| panic!("no URL in `{line}`"));
    let url = line[start..]
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string();
    (server, log, url)
}

/// The package and registry-core in a work directory of their own.
fn work(name: &str) -> (PathBuf, PathBuf) {
    let root = root();
    runtime_support::build_native_runtime(&root);
    let base = work_dir::work_dir(&root, "registry-conformance-work")
        .join(format!("{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    for package in ["registry-conformance", "registry-core"] {
        copy_dir(&root.join("packages").join(package), &base.join(package));
    }
    let package = base.join("registry-conformance");
    (base, package)
}

fn suite(package: &Path, args: &[&str]) {
    let out = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["run", "src/main.velt"])
        .args(args)
        .current_dir(package)
        .output()
        .expect("run velt");
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn the_rust_server_with_users() {
    let (base, package) = work("users");
    let dir = base.join("registry");
    let alice = add_user(&dir, "alice");
    let bob = add_user(&dir, "bob");
    let (_server, _log, url) = serve(&dir);
    suite(&package, &[&url, "--alice", &alice, "--bob", &bob]);
}

#[test]
fn the_rust_server_when_open() {
    let (base, package) = work("open");
    let (_server, _log, url) = serve(&base.join("registry"));
    suite(&package, &[&url, "--open"]);
}
