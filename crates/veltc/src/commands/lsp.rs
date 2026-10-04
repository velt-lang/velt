//! `velt lsp`: runs the language server (`velt_lsp`) on stdio with the CLI's module loader, so the
//! editor resolves imports (relative, `std/`, packages) exactly like `velt build`, and lists the
//! modules it can import (the std root's public modules, the package's dependencies) for import
//! completion.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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
    /// Package root → what was installed for it.
    packages: Mutex<HashMap<PathBuf, Installed>>,
    /// The std root's public modules, listed once.
    std_modules: Mutex<Option<(PathBuf, Vec<ModuleEntry>)>>,
}

/// A package as installed from one state of its manifest.
struct Installed {
    /// The manifest it was installed from.
    stamp: ManifestStamp,
    /// Its graph (`None`: not installable, package imports fail).
    graph: Option<Arc<PackageGraph>>,
    /// The modules of its dependencies, listed on first use.
    dependency_modules: Option<Arc<Vec<ModuleEntry>>>,
}

/// When `package.vlt` was last written, and its size: what tells a saved change apart.
type ManifestStamp = Option<(std::time::SystemTime, u64)>;

fn manifest_stamp(root: &Path) -> ManifestStamp {
    let meta = std::fs::metadata(root.join(vpm::manifest::MANIFEST_FILE)).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

impl CliLoader {
    /// `f` of the installed package containing `file` (installed again when its manifest
    /// changed); `None` outside a package.
    fn with_package<T>(&self, file: &Path, f: impl FnOnce(&mut Installed) -> T) -> Option<T> {
        let root = vpm::manifest::find_package_root(file.parent()?)?;
        let stamp = manifest_stamp(&root);
        let mut packages = self.packages.lock().unwrap_or_else(|e| e.into_inner());
        let fresh = packages.get(&root).is_some_and(|p| p.stamp == stamp);
        if !fresh {
            let opts = InstallOptions {
                locked: false,
                update: false,
                target: Some(velt_codegen_cl::host_triple()),
            };
            let graph = super::project::Project::open(&root, opts)
                .map_err(|e| eprintln!("velt-lsp: cannot install `{}`: {e}", root.display()))
                .ok()
                .map(|p| Arc::new(p.graph));
            let installed = Installed {
                stamp,
                graph,
                dependency_modules: None,
            };
            packages.insert(root.clone(), installed);
        }
        packages.get_mut(&root).map(f)
    }

    /// An already installed graph with a package containing `file` (none is installed).
    fn installed_graph(&self, file: &Path) -> Option<Arc<PackageGraph>> {
        let packages = self.packages.lock().unwrap_or_else(|e| e.into_inner());
        packages
            .values()
            .filter_map(|p| p.graph.clone())
            .find(|g| g.package_of(file).is_some())
    }

    fn graph(&self, file: &Path) -> Option<Arc<PackageGraph>> {
        self.with_package(file, |p| p.graph.clone()).flatten()
    }

    /// The modules of the dependencies of the package containing `file`, as package specifiers
    /// resolve them (listed once per installed manifest).
    fn dependency_modules(&self, file: &Path) -> Arc<Vec<ModuleEntry>> {
        let listed = self.with_package(file, |p| {
            let graph = p.graph.clone();
            p.dependency_modules
                .get_or_insert_with(|| Arc::new(list_dependencies(graph.as_deref(), file)))
                .clone()
        });
        listed.unwrap_or_default()
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
            packages: graph.as_deref().map(|g| g as &dyn loader::PackageResolver),
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
        out.extend(self.dependency_modules(from).iter().cloned());
        out
    }

    fn resolve_module(&self, spec: &str, from: &Path) -> Option<PathBuf> {
        // Only bare specifiers (dependencies, `paths` aliases) need a package graph: the one
        // already installed for a document that contains `from` (its own package or one of its
        // dependencies), as `load` resolves a dependency's imports. Nothing is installed here: a
        // dependency is never installed as a project of its own.
        let bare =
            !spec.starts_with("./") && !spec.starts_with("../") && !spec.starts_with("velt:");
        let graph = if bare {
            self.installed_graph(from)
        } else {
            None
        };
        let packages = graph.as_deref().map(|g| g as &dyn loader::PackageResolver);
        loader::resolve_module(spec, from, loader::std_root().as_deref(), packages)
    }

    fn resolves_modules(&self) -> bool {
        true
    }
}

/// The modules of the dependencies of the package containing `file`, in `graph`.
fn list_dependencies(graph: Option<&PackageGraph>, file: &Path) -> Vec<ModuleEntry> {
    let Some(package) = graph.and_then(|g| g.package_of(file)) else {
        return vec![];
    };
    let dir = file.parent().unwrap_or(Path::new(""));
    let mut out = vec![];
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

    /// Auto-import follows a dependency's re-export of another package (`export * from "dep2"`)
    /// through the document's installed graph: dep2's names are offered, and the dependency is
    /// never installed as a project of its own (no lock file appears in it, and the loader
    /// installed the document's package only).
    #[test]
    fn re_exports_of_dependencies_resolve_without_installing_them() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (app, dep, dep2) = (root.join("app"), root.join("dep"), root.join("dep2"));
        for (pkg, name) in [(&app, "app"), (&dep, "dep"), (&dep2, "dep2")] {
            std::fs::create_dir_all(pkg.join("src")).unwrap();
            let manifest =
                format!("export const pkg: Package = {{ name: \"{name}\", version: \"0.1.0\" }};");
            std::fs::write(pkg.join(vpm::manifest::MANIFEST_FILE), manifest).unwrap();
        }
        std::fs::write(dep.join("src/lib.vlt"), "export * from \"dep2\";\n").unwrap();
        let lib2 = "export function fromDepTwo(): i64 {\n  return 2;\n}\n";
        std::fs::write(dep2.join("src/lib.vlt"), lib2).unwrap();
        let path_dep = |path: &str| vpm::edit::DependencySpec {
            version: None,
            path: Some(path.into()),
        };
        vpm::edit::add_dependency(&app, "dep", &path_dep("../dep")).unwrap();
        vpm::edit::add_dependency(&dep, "dep2", &path_dep("../dep2")).unwrap();
        let main = app.join("src/main.vlt");
        let text = "function main() {\n  fromDep\n}\n";
        std::fs::write(&main, text).unwrap();
        let main_uri = lsp_types::Url::from_file_path(&main).unwrap();

        let (server_conn, conn) = Connection::memory();
        let loader = Arc::new(CliLoader::default());
        let serving = loader.clone();
        let server = std::thread::spawn(move || velt_lsp::serve(server_conn, &*serving));
        request(&conn, 1, "initialize", json!({ "capabilities": {} }));
        notify(&conn, "initialized", json!({}));
        let doc = json!({ "uri": main_uri, "languageId": "velt", "version": 1, "text": text });
        notify(
            &conn,
            "textDocument/didOpen",
            json!({ "textDocument": doc }),
        );
        let at = json!({ "textDocument": { "uri": main_uri }, "position": { "line": 1, "character": 9 } });
        let items = request(&conn, 2, "textDocument/completion", at);
        let items = items.get("items").cloned().unwrap_or(items);
        let offered = items.as_array().unwrap().iter().any(|i| {
            i["label"] == json!("fromDepTwo") && i["labelDetails"]["description"] == json!("dep")
        });
        assert!(offered, "{items:#}");
        for pkg in [&dep, &dep2] {
            assert!(
                !pkg.join(vpm::lockfile::LOCK_FILE).exists(),
                "{}",
                pkg.display()
            );
        }
        request(&conn, 3, "shutdown", Value::Null);
        notify(&conn, "exit", Value::Null);
        server.join().unwrap().unwrap();
        // Only the document's package was installed (compared canonically: on Windows the
        // loader's keys have no `\\?\` prefix, `canonicalize` adds one).
        let installed: Vec<PathBuf> = loader.packages.lock().unwrap().keys().cloned().collect();
        let installed: Vec<PathBuf> = installed
            .iter()
            .map(|p| p.canonicalize().unwrap())
            .collect();
        assert_eq!(installed, [app]);
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
