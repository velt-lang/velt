//! A scripted LSP client: runs the server in-process on a memory connection and exchanges JSON-RPC
//! messages with it.

use std::path::PathBuf;
use std::thread::JoinHandle;
use std::time::Duration;

use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use lsp_types::Url;
use serde_json::{json, Value};

use super::loader::TestLoader;

/// How long to wait for any single server message before failing the test.
const TIMEOUT: Duration = Duration::from_secs(30);

pub struct Client {
    conn: Connection,
    server: Option<JoinHandle<Result<(), String>>>,
    next_id: i32,
    /// Notifications received while waiting for something else.
    backlog: Vec<Notification>,
    /// Methods of the requests the server sent (each answered with `null`, or refused).
    pub server_requests: Vec<String>,
    /// Refuse the server's requests with an error instead.
    pub refuse_server_requests: bool,
    /// Result of `initialize`.
    pub init: Value,
}

impl Client {
    /// Start a server and complete the handshake.
    pub fn start() -> Client {
        Client::start_with(json!({ "capabilities": {} }))
    }

    /// Start a server and complete the handshake with `initialize` params `init`.
    pub fn start_with(init: Value) -> Client {
        let (server_conn, conn) = Connection::memory();
        let server = std::thread::spawn(move || crate::serve(server_conn, &TestLoader));
        let mut client = Client {
            conn,
            server: Some(server),
            next_id: 0,
            backlog: vec![],
            server_requests: vec![],
            refuse_server_requests: false,
            init: Value::Null,
        };
        client.init = client.request("initialize", init);
        client.notify("initialized", json!({}));
        client
    }

    /// Send a request and wait for its successful result.
    pub fn request(&mut self, method: &str, params: Value) -> Value {
        let resp = self.request_raw(method, params);
        match resp.response_result {
            Ok(v) => v,
            Err(e) => panic!("`{method}` failed: {} {}", e.code, e.message),
        }
    }

    /// Send a request and wait for its response (success or error).
    pub fn request_raw(&mut self, method: &str, params: Value) -> Response {
        self.next_id += 1;
        let id = RequestId::from(self.next_id);
        let req = Request::new(id.clone(), method.into(), params);
        self.conn.sender.send(req.into()).unwrap();
        loop {
            match self.recv() {
                Message::Response(r) if r.id == id => return r,
                Message::Notification(n) => self.backlog.push(n),
                other => panic!("unexpected message {other:?}"),
            }
        }
    }

    pub fn notify(&mut self, method: &str, params: Value) {
        let n = Notification::new(method.into(), params);
        self.conn.sender.send(n.into()).unwrap();
    }

    pub fn open(&mut self, uri: &Url, text: &str) {
        let doc = json!({ "uri": uri, "languageId": "velt", "version": 1, "text": text });
        self.notify("textDocument/didOpen", json!({ "textDocument": doc }));
    }

    pub fn change(&mut self, uri: &Url, version: i32, text: &str) {
        let params = json!({
            "textDocument": { "uri": uri, "version": version },
            "contentChanges": [{ "text": text }],
        });
        self.notify("textDocument/didChange", params);
    }

    /// Wait for the next `publishDiagnostics` for `uri`; returns its params.
    pub fn diagnostics(&mut self, uri: &Url) -> Value {
        let is_for = |n: &Notification| {
            n.method == "textDocument/publishDiagnostics" && n.params["uri"] == json!(uri)
        };
        if let Some(i) = self.backlog.iter().position(is_for) {
            return self.backlog.remove(i).params;
        }
        loop {
            match self.recv() {
                Message::Notification(n) if is_for(&n) => return n.params,
                Message::Notification(n) => self.backlog.push(n),
                other => panic!("unexpected message {other:?}"),
            }
        }
    }

    /// The next message from the server; requests from the server are answered (with `null`)
    /// and recorded on the way.
    fn recv(&mut self) -> Message {
        loop {
            let msg = self
                .conn
                .receiver
                .recv_timeout(TIMEOUT)
                .expect("the server did not answer in time");
            let Message::Request(req) = msg else {
                return msg;
            };
            self.server_requests.push(req.method.clone());
            let answer = if self.refuse_server_requests {
                Response::new_err(req.id, -32601, "not supported".into())
            } else {
                Response::new_ok(req.id, Value::Null)
            };
            self.conn.sender.send(answer.into()).unwrap();
        }
    }

    /// `shutdown` + `exit`; the server must stop cleanly.
    pub fn shutdown(mut self) {
        self.request("shutdown", Value::Null);
        self.notify("exit", Value::Null);
        let server = self.server.take().unwrap();
        server.join().unwrap().unwrap();
    }
}

/// A `file:` URI for a (virtual) file `name` in a test directory; the file need not exist.
pub fn uri(name: &str) -> Url {
    let path: PathBuf = std::env::temp_dir().join("velt_lsp_tests").join(name);
    Url::from_file_path(path).unwrap()
}

/// `{ textDocument, position }` params.
pub fn at(uri: &Url, line: u32, character: u32) -> Value {
    json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": character } })
}

/// Position (line, UTF-16 column) of the first occurrence of `needle` in `text`, plus `delta`.
pub fn pos_of(text: &str, needle: &str, delta: usize) -> (u32, u32) {
    let offset = text
        .find(needle)
        .unwrap_or_else(|| panic!("`{needle}` not in text"))
        + delta;
    let before = &text[..offset];
    let line = before.matches('\n').count();
    let col: usize = before[before.rfind('\n').map_or(0, |i| i + 1)..]
        .chars()
        .map(char::len_utf16)
        .sum();
    (line as u32, col as u32)
}
