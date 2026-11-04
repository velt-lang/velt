//! The language server on the standard library's own files: a std file opened in the editor is
//! checked as the std module it is, not as a user program.

use lsp_server::{Connection, Message, Notification, Request, RequestId};
use serde_json::{json, Value};

use super::*;

fn notify(conn: &Connection, method: &str, params: Value) {
    conn.sender
        .send(Notification::new(method.into(), params).into())
        .unwrap();
}

/// A std file shows no diagnostics: before, `velt_rt_*` declarations were "only the standard
/// library may declare it" and `Date.now()` was user code's `number` (std/net/http/fetch/file.velt
/// stores it in an `i64`). A prelude file is the prelude module already loaded.
#[test]
fn std_files_are_checked_as_std() {
    let std = loader::std_root().expect("the checkout's std");
    let (server_conn, conn) = Connection::memory();
    let server = std::thread::spawn(move || velt_lsp::serve(server_conn, &CliLoader::default()));
    let init = Request::new(
        RequestId::from(1),
        "initialize".into(),
        json!({ "capabilities": {} }),
    );
    conn.sender.send(init.into()).unwrap();
    notify(&conn, "initialized", json!({}));
    for rel in [
        "net/http/fetch/client/conn.velt",
        "net/http/fetch/file.velt",
        "prelude/date.velt",
    ] {
        let path = std.join(rel);
        let uri = lsp_types::Url::from_file_path(path.canonicalize().unwrap()).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let doc = json!({ "uri": uri, "languageId": "velt", "version": 1, "text": text });
        notify(
            &conn,
            "textDocument/didOpen",
            json!({ "textDocument": doc }),
        );
        let diags = loop {
            let msg = conn
                .receiver
                .recv_timeout(std::time::Duration::from_secs(60))
                .expect("the server did not answer in time");
            if let Message::Notification(n) = msg {
                if n.method == "textDocument/publishDiagnostics" && n.params["uri"] == json!(uri) {
                    break n.params;
                }
            }
        };
        assert_eq!(diags["diagnostics"], json!([]), "{rel}");
    }
    let shutdown = Request::new(RequestId::from(2), "shutdown".into(), Value::Null);
    conn.sender.send(shutdown.into()).unwrap();
    notify(&conn, "exit", Value::Null);
    server.join().unwrap().unwrap();
}

/// `CliLoader`, wrapped to see what it serves during a check.
struct Spy {
    cli: CliLoader,
    served: Mutex<Vec<String>>,
}

impl ProgramLoader for Spy {
    fn load(
        &self,
        root: &Path,
        overlay: &HashMap<PathBuf, String>,
        sm: &mut SourceMap,
        diags: &mut Diagnostics,
    ) -> Result<LoadedProgram, String> {
        self.cli.load(root, overlay, sm, diags)
    }

    fn load_more(
        &self,
        root: &Path,
        overlay: &HashMap<PathBuf, String>,
        state: &mut (dyn std::any::Any + Send),
        base: &[SourceModule],
        demanded: &[String],
        sm: &mut SourceMap,
        diags: &mut Diagnostics,
    ) -> Option<velt_sema::StdBatch> {
        let batch = (self.cli).load_more(root, overlay, state, base, demanded, sm, diags)?;
        let paths = batch.modules.iter().map(|m| m.path.clone());
        self.served.lock().unwrap().extend(paths);
        Some(batch)
    }
}

/// The server's check of a document loads std's hook modules through `CliLoader` when the document
/// needs them, and not otherwise (#2161 step 3), and an error next to a demand is still reported.
#[test]
fn the_server_demands_std_modules_through_the_cli_loader() {
    let dir = tempfile::tempdir().unwrap();
    let spy = Spy {
        cli: CliLoader::default(),
        served: Mutex::new(vec![]),
    };
    let (server_conn, conn) = Connection::memory();
    std::thread::scope(|s| {
        let server = s.spawn(|| velt_lsp::serve(server_conn, &spy));
        let init = Request::new(
            RequestId::from(1),
            "initialize".into(),
            json!({ "capabilities": {} }),
        );
        conn.sender.send(init.into()).unwrap();
        notify(&conn, "initialized", json!({}));
        let hook = "std/lang/hooks/number_text";
        let cases = [
            ("none.velt", "function main() {}\n", false, 0),
            (
                "text.velt",
                "function main() {\n  const n: i64 = 7;\n  console.log(`n=${n}`);\n}\n",
                true,
                0,
            ),
            (
                "error.velt",
                "function main() {\n  const n: i64 = \"text\";\n  console.log(`n=${n}`);\n}\n",
                true,
                1,
            ),
        ];
        // Asserted once the server has stopped: the scope would wait for it forever after a panic.
        let mut seen = vec![];
        for (name, text, demands, errors) in cases {
            spy.served.lock().unwrap().clear();
            let path = dir.path().join(name);
            std::fs::write(&path, text).unwrap();
            let uri = lsp_types::Url::from_file_path(path.canonicalize().unwrap()).unwrap();
            let doc = json!({ "uri": uri, "languageId": "velt", "version": 1, "text": text });
            notify(
                &conn,
                "textDocument/didOpen",
                json!({ "textDocument": doc }),
            );
            let wait = std::time::Duration::from_secs(60);
            let found = loop {
                let Ok(msg) = conn.receiver.recv_timeout(wait) else {
                    break None;
                };
                if let Message::Notification(n) = msg {
                    let of_doc = n.params["uri"] == json!(uri);
                    if n.method == "textDocument/publishDiagnostics" && of_doc {
                        break n.params["diagnostics"].as_array().map(Vec::len);
                    }
                }
            };
            let served = spy.served.lock().unwrap().iter().any(|p| p == hook);
            seen.push(((name, found, served), (name, Some(errors), demands)));
        }
        let shutdown = Request::new(RequestId::from(2), "shutdown".into(), Value::Null);
        conn.sender.send(shutdown.into()).unwrap();
        notify(&conn, "exit", Value::Null);
        server.join().unwrap().unwrap();
        for (got, want) in seen {
            assert_eq!(
                got, want,
                "(document, diagnostics, served the hook's module)"
            );
        }
    });
}
