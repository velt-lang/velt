//! Import help: the exports of a module inside `import { … }` (while the statement parses and
//! while it doesn't), module specifiers after `from "`, auto-import completions and the
//! "Import `x` from …" quick fix, against a small std root of the test's own.

use std::path::PathBuf;

use lsp_types::Url;
use serde_json::{json, Value};

use super::client::{at, pos_of, Client};
use super::loader::StdLoader;
use super::quick_fixes::{actions, apply, find};

const FS: &str = "// std/fs: file system access.
//
// More about it.

export function readFile(path: string): string {
  return path;
}

export function writeFile(path: string, data: string): string {
  return path + data;
}

export class IoError {
  message: string = \"\";
}

export type Mode = string;
";

const UTIL: &str = "export function helper(x: i64): i64 {
  return x;
}

export interface Shape {
  area(): f64;
}

function hidden(): i64 {
  return 1;
}
";

/// A std root and a program folder on disk.
struct Fixture {
    _tmp: tempfile::TempDir,
    std: PathBuf,
    app: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let files = [
        ("std/fs.vlt", FS),
        ("std/collections/set.vlt", "// std/collections/set: sets.\n\nexport function setOf(): i64 {\n  return 1;\n}\n"),
        ("std/url.vlt", "// std/url: URLs.\n\nexport function parseUrl(s: string): string {\n  return s;\n}\n"),
        ("std/url/encode.vlt", "// std/url/encode: helpers.\n\nexport function encodeAll(s: string): string {\n  return s;\n}\n"),
        ("std/prelude/array.vlt", "// arrays\n"),
        ("app/util.vlt", UTIL),
        ("app/sub/deep.vlt", "export const DEPTH: i64 = 2;\n"),
        ("app/main.vlt", ""),
    ];
    for (rel, text) in files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    Fixture {
        _tmp: tmp,
        std: root.join("std"),
        app: root.join("app"),
    }
}

/// A client on the fixture's std, with the program folder as its workspace, and `text` open
/// as `app/main.vlt` (its diagnostics received).
fn open(fx: &Fixture, text: &str) -> (Client, Url, Vec<Value>) {
    let init = json!({ "capabilities": {}, "rootUri": Url::from_file_path(&fx.app).unwrap() });
    let mut client = Client::start_on(init, StdLoader(fx.std.clone()));
    let doc = Url::from_file_path(fx.app.join("main.vlt")).unwrap();
    client.open(&doc, text);
    let diags = client.diagnostics(&doc)["diagnostics"]
        .as_array()
        .unwrap()
        .clone();
    (client, doc, diags)
}

/// The completion items at `needle` (+`delta`) of `text`; `trigger`: the triggering character.
fn complete(
    client: &mut Client,
    doc: &Url,
    text: &str,
    needle: &str,
    delta: usize,
    trigger: Option<&str>,
) -> Vec<Value> {
    let (line, col) = pos_of(text, needle, delta);
    let mut params = at(doc, line, col);
    if let Some(t) = trigger {
        params["context"] = json!({ "triggerKind": 2, "triggerCharacter": t });
    }
    let result = client.request("textDocument/completion", params);
    let items = result.get("items").unwrap_or(&result);
    items.as_array().cloned().unwrap_or_default()
}

fn labels(items: &[Value]) -> Vec<&str> {
    items.iter().map(|i| i["label"].as_str().unwrap()).collect()
}

fn item<'a>(items: &'a [Value], label: &str) -> &'a Value {
    items
        .iter()
        .find(|i| i["label"] == json!(label))
        .unwrap_or_else(|| panic!("no `{label}` among {:?}", labels(items)))
}

/// `text` (of `doc`) with the item's additional edits applied.
fn with_import(text: &str, item: &Value, doc: &Url) -> String {
    let mut changes = serde_json::Map::new();
    changes.insert(doc.to_string(), item["additionalTextEdits"].clone());
    apply(text, &json!({ "edit": { "changes": changes } }), doc)
}

#[test]
fn braces_offer_the_std_module_exports_not_yet_listed() {
    let fx = fixture();
    let text = "import { readFile,  } from \"velt:fs\";\n";
    let (mut client, doc, _) = open(&fx, text);
    let items = complete(&mut client, &doc, text, ",  }", 2, None);
    assert_eq!(labels(&items), ["writeFile", "IoError", "Mode"]);
    assert_eq!(
        item(&items, "writeFile")["detail"],
        json!("function writeFile(path: string, data: string): string")
    );
    // Mid-typing: the statement does not parse.
    let text = "import { readFile as, w } from \"velt:fs\";\n";
    client.change(&doc, 2, text);
    client.diagnostics(&doc);
    let items = complete(&mut client, &doc, text, "w }", 1, None);
    assert_eq!(labels(&items), ["writeFile", "IoError", "Mode"]);
    client.shutdown();
}

#[test]
fn braces_offer_a_relative_module_exports() {
    let fx = fixture();
    let text = "import { helper,  } from \"./util\";\n";
    let (mut client, doc, _) = open(&fx, text);
    let items = complete(&mut client, &doc, text, ",  }", 2, None);
    assert_eq!(labels(&items), ["Shape"]);
    let text = "import {\n  S\n  helper as,\n} from \"./util\"\n";
    client.change(&doc, 2, text);
    client.diagnostics(&doc);
    let items = complete(&mut client, &doc, text, "S\n", 1, None);
    assert_eq!(labels(&items), ["Shape"]);
    client.shutdown();
}

#[test]
fn import_type_offers_types_only() {
    let fx = fixture();
    let text = "import type {  } from \"velt:fs\";\n";
    let (mut client, doc, _) = open(&fx, text);
    let items = complete(&mut client, &doc, text, "{  }", 2, None);
    assert_eq!(labels(&items), ["IoError", "Mode"]);
    client.shutdown();
}

#[test]
fn specifiers_offer_std_modules_and_files() {
    let fx = fixture();
    let text = "import { readFile } from \"velt:\";\n";
    let (mut client, doc, _) = open(&fx, text);
    let items = complete(&mut client, &doc, text, "velt:\"", 5, None);
    assert_eq!(
        labels(&items),
        ["velt:collections/set", "velt:fs", "velt:url"]
    );
    let fs = item(&items, "velt:fs");
    assert_eq!(fs["detail"], json!("file system access."));
    assert_eq!(
        fs["textEdit"]["range"],
        json!({ "start": { "line": 0, "character": 26 }, "end": { "line": 0, "character": 31 } })
    );

    let text = "import { helper } from \"\n";
    client.change(&doc, 2, text);
    client.diagnostics(&doc);
    let items = complete(&mut client, &doc, text, "\"\n", 1, Some("\""));
    let found = labels(&items);
    for want in ["../", "./sub/", "./util", "velt:fs"] {
        assert!(found.contains(&want), "{want} in {found:?}");
    }
    assert!(!found.contains(&"./main"), "{found:?}");

    let text = "import { DEPTH } from \"./sub/\";\n";
    client.change(&doc, 3, text);
    client.diagnostics(&doc);
    let items = complete(&mut client, &doc, text, "/\"", 1, Some("/"));
    assert_eq!(labels(&items), ["./sub/deep"]);
    client.shutdown();
}

#[test]
fn auto_import_adds_an_import_to_a_file_without_imports() {
    let fx = fixture();
    let text = "// The app.\n\nfunction main() {\n  readF\n}\n";
    let (mut client, doc, _) = open(&fx, text);
    let items = complete(&mut client, &doc, text, "readF\n", 5, None);
    let read = item(&items, "readFile");
    assert_eq!(read["labelDetails"]["description"], json!("velt:fs"));
    assert_eq!(
        with_import(text, read, &doc),
        "// The app.\n\nimport { readFile } from \"velt:fs\";\n\nfunction main() {\n  readF\n}\n"
    );
    // Only names starting with what was typed.
    assert!(
        !labels(&items).contains(&"writeFile"),
        "{:?}",
        labels(&items)
    );
    // The package's own files too.
    let text = "function main() {\n  hel\n}\n";
    client.change(&doc, 2, text);
    client.diagnostics(&doc);
    let items = complete(&mut client, &doc, text, "hel\n", 3, None);
    let helper = item(&items, "helper");
    assert_eq!(helper["labelDetails"]["description"], json!("./util"));
    assert_eq!(
        with_import(text, helper, &doc),
        "import { helper } from \"./util\";\n\nfunction main() {\n  hel\n}\n"
    );
    client.shutdown();
}

#[test]
fn auto_import_extends_an_existing_import() {
    let fx = fixture();
    let text = "import { readFile } from \"velt:fs\";\n\nfunction main() {\n  readFile(\"a\");\n  wri\n}\n";
    let (mut client, doc, _) = open(&fx, text);
    let items = complete(&mut client, &doc, text, "wri\n", 3, None);
    assert_eq!(
        with_import(text, item(&items, "writeFile"), &doc),
        text.replace("{ readFile }", "{ readFile, writeFile }")
    );
    // A name already imported is not offered again.
    let items = complete(&mut client, &doc, text, "readFile(", 4, None);
    let read: Vec<&Value> = items.iter().filter(|i| i["label"] == "readFile").collect();
    assert_eq!(read.len(), 1);
    assert!(read[0]["additionalTextEdits"].is_null(), "{read:?}");
    client.shutdown();
}

#[test]
fn quick_fix_imports_an_unknown_name() {
    let fx = fixture();
    let text = "function main() {\n  console.log(readFile(\"a\"));\n}\n";
    let (mut client, doc, diags) = open(&fx, text);
    assert!(
        diags.iter().any(|d| d["message"]
            .as_str()
            .unwrap()
            .contains("cannot find `readFile`")),
        "{diags:?}"
    );
    let offered = actions(&mut client, &doc, text, "readFile", &diags);
    let fix = find(&offered, "Import `readFile` from `velt:fs`");
    assert_eq!(fix["isPreferred"], json!(true));
    let fixed = apply(text, fix, &doc);
    assert_eq!(
        fixed,
        format!("import {{ readFile }} from \"velt:fs\";\n\n{text}")
    );
    client.change(&doc, 2, &fixed);
    let errors: Vec<Value> = client.diagnostics(&doc)["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["severity"] == json!(1))
        .cloned()
        .collect();
    assert_eq!(errors, [] as [Value; 0]);
    client.shutdown();
}
