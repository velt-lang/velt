//! The language server shows what source generators report (`JSON.parse`'s "cannot decode"
//! errors are std's generator's): its own check runs no generator, so `velt lsp` runs them after
//! each analysis, in the background, and adds their errors to the document's diagnostics.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// `velt lsp` over its standard streams.
struct Server {
    child: Child,
    stdin: ChildStdin,
    messages: Receiver<Value>,
    /// Messages received but not asked for yet, in order.
    skipped: Vec<Value>,
}

impl Server {
    fn start(dir: &Path) -> Server {
        Server::start_with(dir, &[])
    }

    fn start_with(dir: &Path, env: &[(&str, &str)]) -> Server {
        let mut child = Command::new(env!("CARGO_BIN_EXE_velt"))
            .arg("lsp")
            .current_dir(dir)
            .env("VELT_BUILD_CACHE", dir.join("cache"))
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start velt lsp");
        let stdin = child.stdin.take().expect("stdin");
        let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));
        let (tx, messages) = channel();
        std::thread::spawn(move || loop {
            let mut len = 0;
            loop {
                let mut line = String::new();
                if stdout.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                let line = line.trim_end();
                if line.is_empty() {
                    break;
                }
                if let Some(n) = line.strip_prefix("Content-Length: ") {
                    len = n.parse().expect("a length");
                }
            }
            let mut body = vec![0; len];
            if stdout.read_exact(&mut body).is_err() {
                return;
            }
            let msg: Value = serde_json::from_slice(&body).expect("a JSON message");
            if tx.send(msg).is_err() {
                return;
            }
        });
        Server {
            child,
            stdin,
            messages,
            skipped: vec![],
        }
    }

    fn send(&mut self, msg: Value) {
        let body = msg.to_string();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).expect("write");
        self.stdin.flush().expect("flush");
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    /// The first message not taken yet that `want` accepts; fails after `limit`.
    fn wait(&mut self, limit: Duration, want: impl Fn(&Value) -> bool) -> Value {
        if let Some(i) = self.skipped.iter().position(&want) {
            return self.skipped.remove(i);
        }
        let end = Instant::now() + limit;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            let msg = (self.messages.recv_timeout(left)).expect("the server answers in time");
            if want(&msg) {
                return msg;
            }
            self.skipped.push(msg);
        }
    }

    fn request(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        self.wait(Duration::from_secs(60), |m| m["id"] == json!(id))
    }

    /// The next diagnostics published for version `version` of the document.
    fn diagnostics(&mut self, version: i64) -> Vec<Value> {
        let msg = self.wait(Duration::from_secs(600), |m| {
            m["method"] == "textDocument/publishDiagnostics" && m["params"]["version"] == version
        });
        msg["params"]["diagnostics"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn program(ty: &str) -> String {
    format!(
        "function main() {{\n\
         \x20 const s = \"{{}}\";\n\
         \x20 const v = JSON.parse<{ty}>(s);\n\
         \x20 console.log(v);\n\
         }}\n"
    )
}

/// `JSON.parse<{ a: number | undefined }>` shows the generator's error at the call, after the
/// document's own diagnostics (which do not wait for it); fixed, the error goes away.
#[test]
fn generator_errors_are_shown_and_cleared() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let main = tmp.path().join("main.vlt");
    let bad = program("{ a: number | undefined }");
    std::fs::write(&main, &bad).expect("main");
    let uri = url(&main);
    let mut server = Server::start(tmp.path());
    server.request(1, "initialize", json!({ "capabilities": {} }));
    server.notify("initialized", json!({}));
    let doc = json!({ "uri": uri, "languageId": "velt", "version": 1, "text": bad });
    server.notify("textDocument/didOpen", json!({ "textDocument": doc }));
    // The analysis's, then the generators'.
    assert_eq!(server.diagnostics(1), Vec::<Value>::new());
    let found = server.diagnostics(1);
    let undefined = |d: &Value| {
        d["message"]
            .as_str()
            .is_some_and(|m| m.contains("may hold `undefined`"))
    };
    let error = found.iter().find(|d| undefined(d));
    let error = error.unwrap_or_else(|| panic!("no generator error in {found:#?}"));
    assert_eq!(error["severity"], 1, "{error:#}");
    assert_eq!(error["range"]["start"]["line"], 2, "{error:#}");
    // Hover answers while the generators run again.
    let fixed = program("{ a: number | null }");
    let change = json!({
        "textDocument": { "uri": uri, "version": 2 },
        "contentChanges": [{ "text": fixed }],
    });
    server.notify("textDocument/didChange", change);
    let at = json!({
        "textDocument": { "uri": uri },
        "position": { "line": 2, "character": 9 },
    });
    let hover = server.request(2, "textDocument/hover", at);
    assert!(hover["result"].is_object(), "{hover:#}");
    // The error is kept until the generators ran on the fixed text (no blinking).
    let kept = server.diagnostics(2);
    assert!(kept.iter().any(undefined), "{kept:#?}");
    assert_eq!(server.diagnostics(2), Vec::<Value>::new());
    // Broken again: shown again (from the generators' cache).
    let change = json!({
        "textDocument": { "uri": uri, "version": 3 },
        "contentChanges": [{ "text": bad }],
    });
    server.notify("textDocument/didChange", change);
    assert_eq!(server.diagnostics(3), Vec::<Value>::new());
    let found = server.diagnostics(3);
    assert!(found.iter().any(undefined), "{found:#?}");
    server.request(3, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

fn url(path: &Path) -> String {
    let path = path.canonicalize().expect("canonical path");
    let text = path.to_string_lossy().replace('\\', "/");
    let text = text.strip_prefix("//?/").unwrap_or(&text).to_string();
    match text.starts_with('/') {
        true => format!("file://{text}"),
        false => format!("file:///{text}"),
    }
}

/// A generator that never returns, for `hello<User>()` in `main.vlt`.
const ENDLESS: &str = "import { InstanceRequest, Output } from \"velt/generate\";\n\
     export function hello(req: InstanceRequest, out: Output): void {\n\
     \x20 let n: i64 = 0;\n\
     \x20 while (n >= 0) { n = (n + 1) % 1000; }\n\
     \x20 out.line(`return \"never\";`);\n\
     }\n";

const HELLO: &str = "declare generated(\"./gen\") function hello<T>(): string;\n\
     class User {}\n\
     function main() { console.log(hello<User>()); }\n";

/// How many processes run a generator host built into the build cache `cache`.
fn hosts_running(cache: &Path) -> usize {
    // As the server names it, and with links resolved (without Windows' `\\?\` prefix).
    let dir = cache.join("generate-host");
    let dirs: Vec<String> = [dir.clone(), dir.canonicalize().unwrap_or(dir)]
        .iter()
        .map(|d| d.to_string_lossy().replace("\\\\?\\", "").to_lowercase())
        .collect();
    let listing = if cfg!(windows) {
        let script = "Get-Process | ForEach-Object { $_.Path }";
        Command::new("powershell")
            .args(["-NoProfile", "-Command", script])
            .output()
    } else {
        Command::new("ps").args(["-eo", "args="]).output()
    };
    let listing = listing.expect("list processes");
    let text = String::from_utf8_lossy(&listing.stdout).to_lowercase();
    text.lines()
        // The program, not an argument (the linker building the host names it too).
        .filter(|l| dirs.iter().any(|d| l.trim_start().starts_with(d.as_str())))
        .count()
}

/// Wait (polling, up to `limit`) until `done` holds.
fn eventually(limit: Duration, what: &str, done: impl Fn() -> bool) {
    let end = Instant::now() + limit;
    while !done() {
        assert!(Instant::now() < end, "{what}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// A project whose generator never returns, open in a language server without a time limit,
/// once the server runs its host.
fn endless_host() -> (tempfile::TempDir, Server) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let main = tmp.path().join("main.vlt");
    std::fs::write(&main, HELLO).expect("main");
    std::fs::write(tmp.path().join("gen.vlt"), ENDLESS).expect("gen");
    let mut server = Server::start_with(tmp.path(), &[("VELT_GENERATOR_TIMEOUT", "0")]);
    server.request(1, "initialize", json!({ "capabilities": {} }));
    server.notify("initialized", json!({}));
    let doc = json!({ "uri": url(&main), "languageId": "velt", "version": 1, "text": HELLO });
    server.notify("textDocument/didOpen", json!({ "textDocument": doc }));
    let cache = tmp.path().join("cache");
    eventually(Duration::from_secs(600), "the host never started", || {
        hosts_running(&cache) > 0
    });
    (tmp, server)
}

/// A host the server started stops when the server exits.
#[test]
fn a_host_stops_with_the_language_server() {
    let (tmp, mut server) = endless_host();
    server.request(2, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
    let _ = server.child.wait();
    let cache = tmp.path().join("cache");
    eventually(
        Duration::from_secs(30),
        "the host outlived the server",
        || hosts_running(&cache) == 0,
    );
}

/// A host the server started dies with it when the server is killed (Linux: the parent-death
/// signal; Windows: the kill-on-close job). macOS stops hosts only at a clean exit.
#[cfg(any(target_os = "linux", windows))]
#[test]
fn a_host_dies_with_a_killed_language_server() {
    let (tmp, mut server) = endless_host();
    server.child.kill().expect("kill the server");
    let _ = server.child.wait();
    let cache = tmp.path().join("cache");
    eventually(
        Duration::from_secs(30),
        "the host outlived the killed server",
        || hosts_running(&cache) == 0,
    );
}
