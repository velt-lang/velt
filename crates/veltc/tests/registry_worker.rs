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

/// The registry Worker built and served; `None` (with a note) when the tools are missing.
struct Started {
    _server: Server,
    url: String,
    /// `alice`'s token.
    token: String,
    work: PathBuf,
    home: PathBuf,
}

fn start(name: &str) -> Option<Started> {
    if let Some(missing) = missing_tools() {
        println!("registry_worker: skipped (needs {missing})");
        return None;
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
    let work = work_dir::work_dir(&root, name);
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
    Some(Started {
        url: format!("http://127.0.0.1:{port}"),
        token: token.to_string(),
        home: work.join("home"),
        work,
        _server: server,
    })
}

/// A package `name` at `version` in `dir`.
fn package(dir: &Path, name: &str, version: &str) {
    write(
        &dir.join("package.vlt"),
        &format!("import type {{ Package }} from \"velt:package\";\n\n// A package for the registry Worker test.\nexport const pkg: Package = {{\n  name: \"{name}\",\n  version: \"{version}\",\n  description: \"Small text helpers\",\n  keywords: [\"text\", \"strings\"],\n}};\n"),
    );
    write(
        &dir.join("src/lib.vlt"),
        "/** `s`, shouted. */\nexport function shout(s: string): string {\n  return `${s.toUpperCase()}!`;\n}\n",
    );
}

#[test]
fn the_velt_client_publishes_to_and_installs_from_the_registry_worker() {
    let Some(Started {
        url,
        token,
        work,
        home,
        _server,
    }) = start("registry-worker-test")
    else {
        return;
    };
    let token = token.as_str();
    let url = url.as_str();
    let lib = work.join("textkit");
    package(&lib, "textkit", "1.0.0");
    let refused = velt(&lib, &home, url, "not-a-token", &["publish"]);
    assert!(!refused.status.success(), "{}", text(&refused));
    assert!(text(&refused).contains("a valid `Authorization: Bearer <token>` is required"));
    let out = ok(velt(&lib, &home, url, token, &["publish"]), "velt publish");
    assert!(out.contains("Published `textkit` 1.0.0"), "{out}");
    let again = velt(&lib, &home, url, token, &["publish"]);
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
        velt(&app, &home, url, token, &["add", "textkit"]),
        "velt add",
    );
    let run = ok(velt(&app, &home, url, token, &["run"]), "velt run");
    assert!(run.contains("HELLO FROM THE REGISTRY WORKER!"), "{run}");
    let found = ok(
        velt(&app, &home, url, token, &["search", "text"]),
        "velt search",
    );
    assert!(
        found.contains("textkit  1.0.0  Small text helpers"),
        "{found}"
    );
    let owners = ok(
        velt(&app, &home, url, token, &["owner", "list", "textkit"]),
        "velt owner list",
    );
    assert!(owners.contains("alice"), "{owners}");
    let last = velt(
        &app,
        &home,
        url,
        token,
        &["owner", "remove", "textkit", "alice"],
    );
    assert!(text(&last).contains("is the last owner"), "{}", text(&last));
    ok(
        velt(&app, &home, url, token, &["yank", "textkit@1.0.0"]),
        "velt yank",
    );
    let none = ok(
        velt(&app, &home, url, token, &["search", "text"]),
        "velt search",
    );
    assert!(!none.contains("textkit  1.0.0"), "{none}");
}

/// `GET url` without following redirects: (status, `location`, body).
fn get(url: &str) -> (u16, String, String) {
    let r = velt_http::fetch("GET", url, &[], &[]).expect("GET");
    let location = r.header("location").unwrap_or("").to_string();
    (r.status, location, r.body_text().to_string())
}

/// A sign-in through GitHub with `code`: `(status, page)` of the callback.
fn sign_in(url: &str, code: &str) -> (u16, String) {
    let (status, location, _) = get(&format!("{url}/login"));
    assert_eq!(status, 302, "GET /login");
    let callback = format!("{url}/login/callback");
    let want = format!(
        "https://github.test/login/oauth/authorize?client_id=gh-client&redirect_uri={}",
        callback.replace(':', "%3A").replace('/', "%2F")
    );
    assert!(location.starts_with(&want), "{location}");
    let state = location
        .split("state=")
        .nth(1)
        .and_then(|s| s.split('&').next())
        .expect("a state");
    let (status, _, page) = get(&format!("{callback}?code={code}&state={state}"));
    // A state is used once.
    let (again, _, expired) = get(&format!("{callback}?code={code}&state={state}"));
    assert_eq!(again, 400, "{expired}");
    assert!(expired.contains("Sign-in expired"), "{expired}");
    (status, page)
}

/// The token a successful sign-in shows.
fn shown_token(page: &str) -> String {
    let start = page
        .find("<pre id=\"token\">")
        .expect("a token on the page")
        + 16;
    page[start..start + page[start..].find("</pre>").expect("</pre>")].to_string()
}

#[test]
fn users_sign_up_and_in_with_github() {
    let Some(Started {
        url,
        work,
        home,
        _server,
        ..
    }) = start("registry-worker-github")
    else {
        return;
    };
    let url = url.as_str();
    let (status, page) = sign_in(url, "code-alice");
    assert_eq!(status, 200, "{page}");
    assert!(page.contains("Signed in as alice-gh"), "{page}");
    assert!(page.contains(&format!("velt login {url}")), "{page}");
    let first = shown_token(&page);

    // The token publishes, as the GitHub user, who owns the new package.
    let lib = work.join("ghkit");
    package(&lib, "ghkit", "1.0.0");
    ok(velt(&lib, &home, url, &first, &["publish"]), "velt publish");
    let owners = ok(
        velt(&lib, &home, url, &first, &["owner", "list", "ghkit"]),
        "velt owner list",
    );
    assert!(owners.contains("alice-gh"), "{owners}");

    // The same GitHub account, renamed since: the same registry user, a new token, the old
    // one revoked.
    let (status, page) = sign_in(url, "code-alice-renamed");
    assert_eq!(status, 200, "{page}");
    assert!(page.contains("Signed in as alice-gh"), "{page}");
    let second = shown_token(&page);
    assert_ne!(first, second);
    package(&lib, "ghkit", "1.1.0");
    let old = velt(&lib, &home, url, &first, &["publish"]);
    assert!(
        text(&old).contains("a valid `Authorization: Bearer <token>` is required"),
        "{}",
        text(&old)
    );
    ok(
        velt(&lib, &home, url, &second, &["publish"]),
        "velt publish with the new token",
    );

    // A login that isn't a user name here gets `gh-`; one taken by another user is refused.
    let (status, page) = sign_in(url, "code-9lives");
    assert_eq!(status, 200, "{page}");
    assert!(page.contains("Signed in as gh-9lives"), "{page}");
    let (status, page) = sign_in(url, "code-alice-twin");
    assert_eq!(status, 409, "{page}");
    assert!(
        page.contains("already has a user named <code>alice</code>"),
        "{page}"
    );
    // GitHub refuses a bad code.
    let (status, page) = sign_in(url, "code-wrong");
    assert_eq!(status, 502, "{page}");
    assert!(
        page.contains("GitHub refused the sign-in: The code passed is incorrect or expired."),
        "{page}"
    );
}
