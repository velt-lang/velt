//! `velt lsp`: runs the language server (`velt_lsp`) on stdio with the CLI's module loader, so the
//! editor resolves imports (relative, `std/`, packages) exactly like `velt build`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use velt_common::{Diagnostics, SourceMap};
use velt_lsp::{LoadedProgram, ProgramLoader};
use vpm::{InstallOptions, PackageGraph};

use crate::loader::{self, LoadOptions};

/// Serve until the client exits.
pub fn lsp_command() -> Result<(), String> {
    velt_lsp::serve_stdio(&CliLoader::default())
}

/// [`ProgramLoader`] over [`loader::load_program`]. Package dependency graphs are installed once per
/// package per server session (like the first `velt build`; restart the server after changing
/// dependencies).
#[derive(Default)]
pub struct CliLoader {
    /// Package root → its installed graph (`None`: not installable, package imports fail).
    graphs: Mutex<HashMap<PathBuf, Option<PackageGraph>>>,
}

impl CliLoader {
    fn graph(&self, file: &Path) -> Option<PackageGraph> {
        let root = vpm::manifest::find_package_root(file.parent()?)?;
        let mut graphs = self.graphs.lock().unwrap_or_else(|e| e.into_inner());
        graphs
            .entry(root.clone())
            .or_insert_with(|| {
                let opts = InstallOptions {
                    locked: false,
                    update: false,
                    target: Some(velt_codegen_cl::host_triple()),
                };
                let installed = super::project::Project::open(&root, opts);
                installed
                    .map_err(|e| eprintln!("velt-lsp: cannot install `{}`: {e}", root.display()))
                    .ok()
                    .map(|p| p.graph)
            })
            .clone()
    }
}

impl ProgramLoader for CliLoader {
    fn load(
        &self,
        root: &Path,
        overlay: &HashMap<PathBuf, String>,
        sm: &mut SourceMap,
        diags: &mut Diagnostics,
    ) -> Result<LoadedProgram, String> {
        let graph = self.graph(root);
        let opts = LoadOptions {
            std_root: loader::std_root(),
            packages: graph.as_ref().map(|g| g as &dyn loader::PackageResolver),
            root_source: None,
            overlay: Some(overlay),
        };
        let loaded = loader::load_program(sm, root, opts, diags)?;
        Ok(LoadedProgram {
            modules: loaded.modules,
            root: loaded.root,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use lsp_server::{Connection, Message, Notification, Request, RequestId};
    use serde_json::{json, Value};

    use super::*;

    fn recv(conn: &Connection) -> Message {
        conn.receiver
            .recv_timeout(Duration::from_secs(30))
            .expect("the server did not answer in time")
    }

    fn request(conn: &Connection, id: i32, method: &str, params: Value) -> Value {
        let req = Request::new(RequestId::from(id), method.into(), params);
        conn.sender.send(req.into()).unwrap();
        loop {
            if let Message::Response(r) = recv(conn) {
                return r.response_result.unwrap();
            }
        }
    }

    fn notify(conn: &Connection, method: &str, params: Value) {
        let n = Notification::new(method.into(), params);
        conn.sender.send(n.into()).unwrap();
    }

    /// The real loader behind the server: unsaved buffers win over the disk, and imports of
    /// files that exist only on disk resolve.
    #[test]
    fn server_uses_the_cli_loader_with_open_buffers() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main.vlt");
        let util = dir.path().join("util.vlt");
        std::fs::write(&main, "function main() {}\n").unwrap();
        std::fs::write(&util, "export function helper(): i64 {\n  return 1;\n}\n").unwrap();
        let main_uri = lsp_types::Url::from_file_path(&main).unwrap();

        let (server_conn, conn) = Connection::memory();
        let server =
            std::thread::spawn(move || velt_lsp::serve(server_conn, &CliLoader::default()));
        request(&conn, 1, "initialize", json!({ "capabilities": {} }));
        notify(&conn, "initialized", json!({}));
        let unsaved = "import { helper } from \"./util\";\nfunction main() {\n  const x: string = helper();\n}\n";
        let doc = json!({ "uri": main_uri, "languageId": "velt", "version": 1, "text": unsaved });
        notify(
            &conn,
            "textDocument/didOpen",
            json!({ "textDocument": doc }),
        );

        let diags = loop {
            if let Message::Notification(n) = recv(&conn) {
                if n.method == "textDocument/publishDiagnostics" {
                    break n.params;
                }
            }
        };
        let messages: Vec<&str> = diags["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["message"].as_str().unwrap())
            .collect();
        assert!(
            messages.len() == 1 && messages[0].contains("mismatched types"),
            "{messages:?}"
        );

        let at = json!({ "textDocument": { "uri": main_uri }, "position": { "line": 2, "character": 21 } });
        let loc = request(&conn, 2, "textDocument/definition", at);
        let target = lsp_types::Url::parse(loc["uri"].as_str().unwrap()).unwrap();
        assert_eq!(
            std::fs::canonicalize(target.to_file_path().unwrap()).unwrap(),
            std::fs::canonicalize(&util).unwrap()
        );
        assert_eq!(loc["range"]["start"], json!({ "line": 0, "character": 16 }));

        request(&conn, 3, "shutdown", Value::Null);
        notify(&conn, "exit", Value::Null);
        server.join().unwrap().unwrap();
    }

    /// JSX completion against std/jsx (its `IntrinsicElements` is re-exported from another
    /// module and refers to named attribute types).
    #[test]
    fn jsx_completion_uses_the_std_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main.vlt");
        std::fs::write(
            &main,
            "function main() {}
",
        )
        .unwrap();
        let main_uri = lsp_types::Url::from_file_path(&main).unwrap();
        let (server_conn, conn) = Connection::memory();
        let server =
            std::thread::spawn(move || velt_lsp::serve(server_conn, &CliLoader::default()));
        request(&conn, 1, "initialize", json!({ "capabilities": {} }));
        notify(&conn, "initialized", json!({}));
        let text = "function page(): JSX.Element {
  const x = <p>x</p>;
  return <a 
}
";
        let doc = json!({ "uri": main_uri, "languageId": "velt", "version": 1, "text": text });
        notify(
            &conn,
            "textDocument/didOpen",
            json!({ "textDocument": doc }),
        );
        let at = json!({ "textDocument": { "uri": main_uri }, "position": { "line": 2, "character": 12 } });
        let items = request(&conn, 2, "textDocument/completion", at);
        let labels: Vec<&str> = items
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["label"].as_str().unwrap())
            .collect();
        assert!(
            labels.contains(&"href") && labels.contains(&"class"),
            "{labels:?}"
        );
        request(&conn, 3, "shutdown", Value::Null);
        notify(&conn, "exit", Value::Null);
        server.join().unwrap().unwrap();
    }
}
