//! `package.vlt` with a live registry (a `velt_registry` server on localhost): requirement
//! diagnostics, version and package-name completion, dependency hover and the update fix.

use std::net::TcpListener;
use std::path::Path;
use std::time::{Duration, Instant};

use lsp_types::Url;
use serde_json::{json, Value};
use velt_http::server::Server;
use vpm::Locations;

use super::client::{at, pos_of, uri, Client};

/// Publish library `name` `version` to the registry of `loc`.
fn publish(loc: &Locations, work: &Path, name: &str, version: &str) {
    let dir = work.join(format!("{name}-{version}"));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let manifest =
        format!("export const pkg: Package = {{ name: \"{name}\", version: \"{version}\" }};");
    std::fs::write(dir.join(vpm::manifest::MANIFEST_FILE), manifest).unwrap();
    std::fs::write(dir.join("src/lib.vlt"), "export const V: i64 = 1;\n").unwrap();
    vpm::registry::publish(&dir, loc).unwrap();
}

/// A registry with `sqlite` 0.1.0 and 0.2.0 and `sql-kit` 1.0.0, served on localhost. `None`
/// when `VELT_REGISTRY` names a remote registry, which takes precedence over the manifest's.
fn registry(tmp: &Path) -> Option<(Server, String)> {
    let env = std::env::var("VELT_REGISTRY").unwrap_or_default();
    if vpm::locations::is_url(&env) {
        eprintln!("skipped: VELT_REGISTRY is a remote registry");
        return None;
    }
    let loc = Locations::under(&tmp.join("home"));
    publish(&loc, &tmp.join("work"), "sqlite", "0.1.0");
    publish(&loc, &tmp.join("work"), "sqlite", "0.2.0");
    publish(&loc, &tmp.join("work"), "sql-kit", "1.0.0");
    let handler = velt_registry::Registry {
        root: loc.registry.clone(),
    }
    .handler();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let server = Server::start(listener, handler, 1 << 20).unwrap();
    let url = format!("http://{}", server.addr());
    Some((server, url))
}

/// Repeat `f` until it gives `Some` (registry data arrives in the background).
fn eventually<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn labels(list: &Value) -> Vec<String> {
    list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["label"].as_str().unwrap().to_string())
        .collect()
}

fn manifest(url: &str, deps: &str) -> String {
    format!(
        "export const pkg: Package = {{\n  name: \"app\",\n  version: \"0.1.0\",\n  registry: \"{url}\",\n  dependencies: {{ {deps} }},\n}};\n"
    )
}

#[test]
fn requirements_are_checked_against_the_registry() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((_server, url)) = registry(tmp.path()) else {
        return;
    };
    let mut client = Client::start();
    let doc = uri("live_diags/package.vlt");
    let text = manifest(&url, "sqlite: \"^0.1\", nope: \"1\"");
    client.open(&doc, &text);
    let messages = eventually("registry diagnostics", || {
        let diags = client.diagnostics(&doc);
        let messages: Vec<String> = diags["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["message"].as_str().unwrap().to_string())
            .collect();
        (messages.len() == 2).then_some(messages)
    });
    assert_eq!(
        messages,
        [
            "`sqlite` 0.2.0 is available; `^0.1` does not include it".to_string(),
            format!("package `nope` is not in the registry `{url}`"),
        ]
    );

    // The quick fix moves the requirement to the newest version.
    let (line, character) = pos_of(&text, "^0.1", 1);
    let range = json!({ "start": { "line": line, "character": character }, "end": { "line": line, "character": character } });
    let actions = client.request(
        "textDocument/codeAction",
        json!({ "textDocument": { "uri": doc }, "range": range, "context": { "diagnostics": [] } }),
    );
    assert_eq!(actions[0]["title"], "Use `^0.2.0` for `sqlite`");
    let edit = &actions[0]["edit"]["changes"][doc.as_str()][0];
    assert_eq!(edit["newText"], "^0.2.0");
    client.shutdown();
}

#[test]
fn versions_names_and_hover_come_from_the_registry() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((_server, url)) = registry(tmp.path()) else {
        return;
    };
    let mut client = Client::start();
    let doc: Url = uri("live_complete/package.vlt");
    let text = manifest(&url, "sqlite: \"\", sq");
    client.open(&doc, &text);

    let (line, character) = pos_of(&text, "sqlite: \"", 9);
    let versions = eventually("versions", || {
        let list = client.request("textDocument/completion", at(&doc, line, character));
        (list["isIncomplete"] == false && !labels(&list).is_empty()).then_some(labels(&list))
    });
    assert_eq!(versions, ["^0.2.0", "0.2.0", "0.1.0"]);

    let (line, character) = pos_of(&text, ", sq", 4);
    let names = eventually("package names", || {
        let list = client.request("textDocument/completion", at(&doc, line, character));
        (list["isIncomplete"] == false).then_some(labels(&list))
    });
    assert_eq!(names, ["sql-kit"], "sqlite is already a dependency");

    let (line, character) = pos_of(&text, "sqlite", 2);
    let hover = eventually("hover", || {
        let h = client.request("textDocument/hover", at(&doc, line, character));
        h["contents"]["value"].as_str().map(str::to_string)
    });
    assert_eq!(hover, "**sqlite**  \nnewest: 0.2.0");
    client.shutdown();
}
