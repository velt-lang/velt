//! The registry Worker (packages/registry, docs/tooling/registry-worker.md) against the real
//! `velt` client: the package is built with `velt build --host workers` and served over HTTP by
//! tests/workers/registry_server.mjs under node, with in-memory R2 and D1 (the D1 tables made by
//! its migrations); then `velt publish`, `velt add`, `velt run`, `velt search`, `velt owner` and
//! `velt yank` run against it as they would against any registry.
//!
//! Needs what tests/workers.rs needs, and node:sqlite (node 22.5 or newer); skipped with a note
//! when something is missing, a failure instead with `VELT_RT_WASM_PREBUILT=1`.

mod no_window;
mod work_dir;

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Output, Stdio};

const TRIPLE: &str = "wasm32-unknown-unknown";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn prebuilt() -> bool {
    std::env::var_os("VELT_RT_WASM_PREBUILT").is_some_and(|v| v == "1")
}

fn succeeds(program: &str, args: &[&str]) -> bool {
    no_window::command(program)
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success())
}

fn has_rust_target() -> bool {
    let Ok(out) = no_window::command("rustc")
        .args(["--print", "sysroot"])
        .output()
    else {
        return false;
    };
    let sysroot = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Path::new(&sysroot)
        .join("lib/rustlib")
        .join(TRIPLE)
        .join("lib")
        .is_dir()
}

/// Why the test can't run here, if it can't.
fn missing_tools() -> Option<&'static str> {
    let missing = if velt_codegen_llvm::find_wasm_tools().is_none() {
        Some("LLVM's opt/llc (rustup component add llvm-tools)")
    } else if !has_rust_target() {
        Some("the wasm32-unknown-unknown Rust target")
    } else if !succeeds("node", &["--version"]) {
        Some("node")
    } else if !succeeds("node", &["--no-warnings", "-e", "require('node:sqlite')"]) {
        Some("node:sqlite (node 22.5 or newer)")
    } else {
        None
    };
    if let Some(missing) = missing {
        assert!(
            !prebuilt(),
            "VELT_RT_WASM_PREBUILT=1, but {missing} is not available"
        );
    }
    missing
}

/// The registry server; stopped when dropped.
struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `velt <args>` in `dir` against the registry at `url` with `token`; its output.
fn velt(dir: &Path, home: &Path, url: &str, token: &str, args: &[&str]) -> Output {
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .current_dir(dir)
        .env("VELT_HOME", home)
        .env("VELT_REGISTRY", url)
        .env("VELT_REGISTRY_TOKEN", token)
        .output()
        .expect("run velt")
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[track_caller]
fn ok(o: Output, what: &str) -> String {
    let t = text(&o);
    assert!(o.status.success(), "{what} failed:\n{t}");
    t
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, text).expect("write");
}

#[test]
fn the_velt_client_publishes_to_and_installs_from_the_registry_worker() {
    if let Some(missing) = missing_tools() {
        println!("registry_worker: skipped (needs {missing})");
        return;
    }
    let root = root();
    if !prebuilt() {
        let st = no_window::command(env!("CARGO"))
            .args(["build", "-p", "velt_rt_wasm", "--target", TRIPLE])
            .current_dir(&root)
            .status()
            .expect("cargo build -p velt_rt_wasm");
        assert!(st.success(), "building velt_rt_wasm for {TRIPLE} failed");
    }
    let work = work_dir::work_dir(&root, "registry-worker-test");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).expect("work dir");
    let build = work.join("build");
    ok(
        no_window::command(env!("CARGO_BIN_EXE_velt"))
            .args(["build", "--host", "workers", "-o"])
            .arg(build.join("registry"))
            .current_dir(root.join("packages/registry"))
            .output()
            .expect("velt build"),
        "building packages/registry",
    );

    let mut child = no_window::command("node")
        .arg("--no-warnings")
        .arg(root.join("crates/veltc/tests/workers/registry_server.mjs"))
        .arg(&build)
        .arg("registry.wasm")
        .arg(root.join("packages/registry/migrations"))
        .stdout(Stdio::piped())
        .spawn()
        .expect("start the registry server");
    let stdout = child.stdout.take().expect("stdout");
    let server = Server(child);
    let mut first = String::new();
    BufReader::new(stdout)
        .read_line(&mut first)
        .expect("the server's first line");
    let words: Vec<&str> = first.split_whitespace().collect();
    let [_, port, _, token] = words[..] else {
        panic!("unexpected first line from the registry server: {first:?}");
    };
    let url = format!("http://127.0.0.1:{port}");
    let home = work.join("home");

    let lib = work.join("textkit");
    write(
        &lib.join("package.vlt"),
        "import type { Package } from \"velt:package\";\n\n// A package for the registry Worker test.\nexport const pkg: Package = {\n  name: \"textkit\",\n  version: \"1.0.0\",\n  description: \"Small text helpers\",\n  keywords: [\"text\", \"strings\"],\n};\n",
    );
    write(
        &lib.join("src/lib.vlt"),
        "/** `s`, shouted. */\nexport function shout(s: string): string {\n  return `${s.toUpperCase()}!`;\n}\n",
    );
    let refused = velt(&lib, &home, &url, "not-a-token", &["publish"]);
    assert!(!refused.status.success(), "{}", text(&refused));
    assert!(text(&refused).contains("a valid `Authorization: Bearer <token>` is required"));
    let out = ok(velt(&lib, &home, &url, token, &["publish"]), "velt publish");
    assert!(out.contains("Published `textkit` 1.0.0"), "{out}");
    let again = velt(&lib, &home, &url, token, &["publish"]);
    assert!(
        text(&again).contains("is already published"),
        "{}",
        text(&again)
    );

    let app = work.join("app");
    write(
        &app.join("package.vlt"),
        "import type { Package } from \"velt:package\";\n\nexport const pkg: Package = {\n  name: \"app\",\n  version: \"0.1.0\",\n};\n",
    );
    write(
        &app.join("src/main.vlt"),
        "import { shout } from \"textkit\";\n\nfunction main() {\n  console.log(shout(\"hello from the registry worker\"));\n}\n",
    );
    ok(
        velt(&app, &home, &url, token, &["add", "textkit"]),
        "velt add",
    );
    let run = ok(velt(&app, &home, &url, token, &["run"]), "velt run");
    assert!(run.contains("HELLO FROM THE REGISTRY WORKER!"), "{run}");
    let found = ok(
        velt(&app, &home, &url, token, &["search", "text"]),
        "velt search",
    );
    assert!(
        found.contains("textkit  1.0.0  Small text helpers"),
        "{found}"
    );
    let owners = ok(
        velt(&app, &home, &url, token, &["owner", "list", "textkit"]),
        "velt owner list",
    );
    assert!(owners.contains("alice"), "{owners}");
    let last = velt(
        &app,
        &home,
        &url,
        token,
        &["owner", "remove", "textkit", "alice"],
    );
    assert!(text(&last).contains("is the last owner"), "{}", text(&last));
    ok(
        velt(&app, &home, &url, token, &["yank", "textkit@1.0.0"]),
        "velt yank",
    );
    let none = ok(
        velt(&app, &home, &url, token, &["search", "text"]),
        "velt search",
    );
    assert!(!none.contains("textkit  1.0.0"), "{none}");
    drop(server);
}
