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
/// library may declare it" and `Date.now()` was user code's `number` (`std/http/server.vlt`
/// subtracts it from an `i64`). A prelude file is the prelude module already loaded.
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
    for rel in ["http/server.vlt", "http/stream.vlt", "prelude/date.vlt"] {
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
