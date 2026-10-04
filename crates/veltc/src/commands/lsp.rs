//! `velt lsp`: runs the language server (`velt_lsp`) on stdio with the CLI's module loader, so the
//! editor resolves imports (relative, `std/`, packages) exactly like `velt build`, and lists the
//! modules it can import (the std root's public modules, the package's dependencies) for import
//! completion.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use velt_common::{Diagnostics, SourceMap};
use velt_lsp::{LoadedProgram, ModuleEntry, ProgramLoader};
use vpm::{InstallOptions, PackageGraph};

use crate::loader::{self, LoadOptions};

/// Serve until the client exits.
pub fn lsp_command() -> Result<(), String> {
    velt_lsp::serve_stdio(&CliLoader::default())
}

/// [`ProgramLoader`] over [`loader::load_program`]. A package's dependency graph is installed like
/// the first `velt build` does, and again whenever its saved `package.vlt` changes (so editing the
/// dependencies needs no server restart).
#[derive(Default)]
pub struct CliLoader {
    /// Package root → the manifest it was installed from and its graph (`None`: not installable,
    /// package imports fail).
    graphs: Mutex<HashMap<PathBuf, (ManifestStamp, Option<PackageGraph>)>>,
    /// The std root's public modules, listed once.
    std_modules: Mutex<Option<(PathBuf, Vec<ModuleEntry>)>>,
}

/// When `package.vlt` was last written, and its size: what tells a saved change apart.
type ManifestStamp = Option<(std::time::SystemTime, u64)>;

fn manifest_stamp(root: &Path) -> ManifestStamp {
    let meta = std::fs::metadata(root.join(vpm::manifest::MANIFEST_FILE)).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

impl CliLoader {
    fn graph(&self, file: &Path) -> Option<PackageGraph> {
        let root = vpm::manifest::find_package_root(file.parent()?)?;
        let stamp = manifest_stamp(&root);
        let mut graphs = self.graphs.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((installed_from, graph)) = graphs.get(&root) {
            if *installed_from == stamp {
                return graph.clone();
            }
        }
        let opts = InstallOptions {
            locked: false,
            update: false,
            target: Some(velt_codegen_cl::host_triple()),
        };
        let graph = super::project::Project::open(&root, opts)
            .map_err(|e| eprintln!("velt-lsp: cannot install `{}`: {e}", root.display()))
            .ok()
            .map(|p| p.graph);
        graphs.insert(root, (stamp, graph.clone()));
        graph
    }

    /// The public modules of the std root (listed on first use), as `velt:` specifiers resolve
    /// them.
    fn std_modules(&self) -> Vec<ModuleEntry> {
        let Some(root) = loader::std_root() else {
            return vec![];
        };
        let mut cached = self.std_modules.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((listed, entries)) = cached.as_ref() {
            if *listed == root {
                return entries.clone();
            }
        }
        let entries: Vec<ModuleEntry> = velt_lsp::std_module_entries(&root)
            .into_iter()
            .filter(|e| {
                matches!(
                    loader::resolve_spec(&e.spec, &root),
                    Ok(loader::ModuleRef::Std { .. })
                )
            })
            .collect();
        *cached = Some((root, entries.clone()));
        entries
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

    fn module_index(&self, from: &Path) -> Vec<ModuleEntry> {
        let mut out = self.std_modules();
        let graph = self.graph(from);
        let Some(package) = graph.as_ref().and_then(|g| g.package_of(from)) else {
            return out;
        };
        let dir = from.parent().unwrap_or(Path::new(""));
        for (name, root) in &package.dependencies {
            out.extend(
                velt_lsp::package_module_entries(name, root)
                    .into_iter()
                    .filter(|e| {
                        matches!(
                            loader::resolve_spec(&e.spec, dir),
                            Ok(loader::ModuleRef::Package { .. })
                        )
                    }),
            );
        }
        out
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

    /// Saving a changed `package.vlt` reinstalls the package's graph: a new dependency resolves
    /// without restarting the server.
    #[test]
    fn a_saved_manifest_change_reinstalls_the_graph() {
        let dir = tempfile::tempdir().unwrap();
        let (app, util) = (dir.path().join("app"), dir.path().join("util"));
        for (root, name) in [(&app, "app"), (&util, "util")] {
            std::fs::create_dir_all(root.join("src")).unwrap();
            let manifest =
                format!("export const pkg: Package = {{ name: \"{name}\", version: \"0.1.0\" }};");
            std::fs::write(root.join(vpm::manifest::MANIFEST_FILE), manifest).unwrap();
        }
        std::fs::write(util.join("src/lib.vlt"), "export const X: i64 = 1;\n").unwrap();
        let main = app.join("src/main.vlt");
        std::fs::write(&main, "function main() {}\n").unwrap();

        let loader = CliLoader::default();
        let graph = loader.graph(&main).expect("the app installs");
        assert!(graph.dependency_root(&main, "util").is_err());
        // Unchanged: the cached graph is reused.
        assert!(loader
            .graph(&main)
            .unwrap()
            .dependency_root(&main, "util")
            .is_err());

        vpm::edit::add_dependency(
            &app,
            "util",
            &vpm::edit::DependencySpec {
                version: None,
                path: Some("../util".into()),
            },
        )
        .unwrap();
        let graph = loader.graph(&main).expect("the app installs again");
        assert!(graph.dependency_root(&main, "util").is_ok());
    }

    /// The module index: std's public modules (no prelude, no internal modules) and the
    /// modules of the package's dependencies.
    #[test]
    fn module_index_lists_std_and_dependencies() {
        let dir = tempfile::tempdir().unwrap();
        let (app, util) = (dir.path().join("app"), dir.path().join("util"));
        for (root, name) in [(&app, "app"), (&util, "util")] {
            std::fs::create_dir_all(root.join("src")).unwrap();
            let manifest =
                format!("export const pkg: Package = {{ name: \"{name}\", version: \"0.1.0\" }};");
            std::fs::write(root.join(vpm::manifest::MANIFEST_FILE), manifest).unwrap();
        }
        std::fs::write(util.join("src/lib.vlt"), "export const X: i64 = 1;\n").unwrap();
        std::fs::write(util.join("src/extra.vlt"), "export const Y: i64 = 1;\n").unwrap();
        let spec = vpm::edit::DependencySpec {
            version: None,
            path: Some("../util".into()),
        };
        vpm::edit::add_dependency(&app, "util", &spec).unwrap();
        let main = app.join("src/main.vlt");
        std::fs::write(&main, "function main() {}\n").unwrap();

        let specs: Vec<String> = CliLoader::default()
            .module_index(&main)
            .into_iter()
            .map(|e| e.spec)
            .collect();
        for want in [
            "velt:fs",
            "velt:collections/set",
            "velt:jsx",
            "velt:package",
            "util",
            "util/extra",
        ] {
            assert!(specs.iter().any(|s| s == want), "{want} in {specs:?}");
        }
        for internal in [
            "velt:prelude/array",
            "velt:url/encode",
            "velt:net_bytes",
            "velt:redis/args",
        ] {
            assert!(
                !specs.iter().any(|s| s == internal),
                "{internal} in {specs:?}"
            );
        }
    }

    /// Import help against the real std: specifiers after `from "velt:`, the exports of a std
    /// module inside the braces, and auto-import of a std function.
    #[test]
    fn import_help_uses_the_std_modules() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main.vlt");
        std::fs::write(&main, "function main() {}\n").unwrap();
        let main_uri = lsp_types::Url::from_file_path(&main).unwrap();
        let (server_conn, conn) = Connection::memory();
        let server =
            std::thread::spawn(move || velt_lsp::serve(server_conn, &CliLoader::default()));
        request(&conn, 1, "initialize", json!({ "capabilities": {} }));
        notify(&conn, "initialized", json!({}));
        let text = "import { readFile,  } from \"velt:fs\";\nimport { x } from \"velt:co\";\n\nfunction main() {\n  normali\n}\n";
        let doc = json!({ "uri": main_uri, "languageId": "velt", "version": 1, "text": text });
        notify(
            &conn,
            "textDocument/didOpen",
            json!({ "textDocument": doc }),
        );
        let mut id = 1;
        let mut labels = |line: u32, character: u32| {
            id += 1;
            let at = json!({ "textDocument": { "uri": main_uri }, "position": { "line": line, "character": character } });
            let items = request(&conn, id, "textDocument/completion", at);
            let items = items.get("items").cloned().unwrap_or(items);
            items
                .as_array()
                .unwrap()
                .iter()
                .map(|i| {
                    let from = i["labelDetails"]["description"].as_str().unwrap_or("");
                    format!("{} {from}", i["label"].as_str().unwrap())
                })
                .collect::<Vec<String>>()
        };
        let names = labels(0, 19);
        assert!(names.contains(&"writeFile ".to_string()), "{names:?}");
        assert!(!names.contains(&"readFile ".to_string()), "{names:?}");
        let specs = labels(1, 26);
        assert!(
            specs.contains(&"velt:collections/set ".to_string()),
            "{specs:?}"
        );
        let auto = labels(4, 9);
        assert!(
            auto.contains(&"normalize velt:path".to_string()),
            "{auto:?}"
        );
        request(&conn, 99, "shutdown", Value::Null);
        notify(&conn, "exit", Value::Null);
        server.join().unwrap().unwrap();
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

    const GENERATORS: &str = "function* count(limit: i64): Generator<i64> {
  for (let i = 0; i < limit; i++) {
    yield i * 2;
  }
}

async function* pages(n: i64): AsyncGenerator<string> {
  yield `page ${n}`;
}

async function main() {
  for await (const page of pages(2)) {
    console.log(page);
  }
  for (const c of count(3)) {
    console.log(c);
  }
}
";

    /// (line, character) of `needle` (+`delta`) in `text`.
    fn position(text: &str, needle: &str, delta: usize) -> Value {
        let offset = text.find(needle).expect("needle in text") + delta;
        let before = &text[..offset];
        let line = before.matches('\n').count();
        let character = before.len() - before.rfind('\n').map_or(0, |i| i + 1);
        json!({ "line": line, "character": character })
    }

    /// Hover and go to definition on `yield` operands, generator parameters and `for await`
    /// bindings, with the std prelude's `Generator` / `AsyncGenerator`.
    #[test]
    fn hover_and_definition_in_generators() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main.vlt");
        std::fs::write(&main, GENERATORS).unwrap();
        let main_uri = lsp_types::Url::from_file_path(&main).unwrap();
        let (server_conn, conn) = Connection::memory();
        let server =
            std::thread::spawn(move || velt_lsp::serve(server_conn, &CliLoader::default()));
        request(&conn, 1, "initialize", json!({ "capabilities": {} }));
        notify(&conn, "initialized", json!({}));
        let doc =
            json!({ "uri": main_uri, "languageId": "velt", "version": 1, "text": GENERATORS });
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
        assert_eq!(diags["diagnostics"], json!([]));
        let mut id = 1;
        let mut ask = |method: &str, needle: &str, delta: usize| {
            id += 1;
            let at = json!({
                "textDocument": { "uri": main_uri },
                "position": position(GENERATORS, needle, delta),
            });
            request(&conn, id, method, at)
        };
        let cases = [
            ("limit; i++", "(parameter) limit: i64", Some((0, 16))),
            ("i * 2", "let i: i64", Some((1, 11))),
            ("* 2", "i64", None),
            ("n}`", "(parameter) n: i64", Some((6, 22))),
            ("page);", "const page: string", Some((11, 19))),
            ("c);", "const c: i64", Some((14, 13))),
            (
                "pages(2)",
                "function pages(n: i64): AsyncGenerator<string, never>",
                Some((6, 16)),
            ),
        ];
        for (needle, hover, def) in cases {
            let h = ask("textDocument/hover", needle, 0);
            assert_eq!(
                h["contents"]["value"],
                json!(format!("```velt\n{hover}\n```")),
                "hover at `{needle}`"
            );
            let d = ask("textDocument/definition", needle, 0);
            let want = def.map_or(
                Value::Null,
                |(line, character)| json!({ "line": line, "character": character }),
            );
            let got = if d.is_null() {
                d
            } else {
                d["range"]["start"].clone()
            };
            assert_eq!(got, want, "definition of `{needle}`");
        }
        id += 1;
        request(&conn, id, "shutdown", Value::Null);
        notify(&conn, "exit", Value::Null);
        server.join().unwrap().unwrap();
    }
}
