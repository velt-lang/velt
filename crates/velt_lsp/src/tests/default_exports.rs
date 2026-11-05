//! Default exports and imports (#1916) in the editor: go to definition through a default
//! import, and rename, where the local name at each import is its own.

use serde_json::{json, Value};

use super::client::{at, pos_of, uri, Client};

const LIB: &str = "export default function greet(name: string): string {\n  return name;\n}\n";

const ANON: &str = "export default function () {\n  return 1;\n}\n";

const APP: &str = r#"import hello from "./dflt_lib";
import one from "./dflt_anon";

function main() {
  console.log(hello("a"), hello("b"), one());
}
"#;

fn open_app() -> (Client, lsp_types::Url, lsp_types::Url) {
    let mut client = Client::start();
    let app = uri("dflt_app.vlt");
    let lib = uri("dflt_lib.vlt");
    client.open(&lib, LIB);
    client.open(&uri("dflt_anon.vlt"), ANON);
    client.open(&app, APP);
    let diags = client.diagnostics(&app);
    assert_eq!(
        diags["diagnostics"],
        json!([]),
        "the test program must check"
    );
    (client, app, lib)
}

/// (file name, line, character) of `loc`'s start.
fn place(loc: &Value) -> (String, u64, u64) {
    let start = &loc["range"]["start"];
    let file = loc["uri"].as_str().unwrap().rsplit('/').next().unwrap();
    (
        file.to_string(),
        start["line"].as_u64().unwrap(),
        start["character"].as_u64().unwrap(),
    )
}

/// The (line, character) starts of the edits `rename` makes in `doc`.
fn edit_starts(rename: &Value, doc: &lsp_types::Url) -> Vec<(u64, u64)> {
    let mut starts: Vec<(u64, u64)> = (rename["changes"][doc.as_str()].as_array())
        .map_or(&[][..], |a| a.as_slice())
        .iter()
        .map(|e| {
            let s = &e["range"]["start"];
            (
                s["line"].as_u64().unwrap(),
                s["character"].as_u64().unwrap(),
            )
        })
        .collect();
    starts.sort();
    starts
}

#[test]
fn definition_through_a_default_import() {
    let (mut client, app, _) = open_app();
    let (line, col) = pos_of(APP, "hello(\"a\")", 0);
    let loc = client.request("textDocument/definition", at(&app, line, col));
    assert_eq!(place(&loc), ("dflt_lib.vlt".to_string(), 0, 24), "{loc}");
    // An anonymous default: its `default` keyword.
    let (line, col) = pos_of(APP, "one()", 0);
    let loc = client.request("textDocument/definition", at(&app, line, col));
    assert_eq!(place(&loc), ("dflt_anon.vlt".to_string(), 0, 7), "{loc}");
    client.shutdown();
}

#[test]
fn rename_of_a_default_import_stays_in_its_module() {
    let (mut client, app, lib) = open_app();
    let (line, col) = pos_of(APP, "hello(\"b\")", 0);
    let mut params = at(&app, line, col);
    params["newName"] = json!("hi");
    let edit = client.request("textDocument/rename", params);
    assert_eq!(
        edit_starts(&edit, &app),
        [(0, 7), (4, 14), (4, 26)],
        "{edit}"
    );
    assert_eq!(edit_starts(&edit, &lib), [], "{edit}");

    // Renaming the declaration edits it once, not the importers' own names.
    let (line, col) = pos_of(LIB, "greet", 0);
    let mut params = at(&lib, line, col);
    params["newName"] = json!("salute");
    let edit = client.request("textDocument/rename", params);
    assert_eq!(edit_starts(&edit, &lib), [(0, 24)], "{edit}");
    assert_eq!(edit_starts(&edit, &app), [], "{edit}");
    client.shutdown();
}

/// The titles of the code actions offered on the first line of `doc`.
fn titles_on_first_line(client: &mut Client, doc: &lsp_types::Url) -> Vec<String> {
    let actions = client.request(
        "textDocument/codeAction",
        json!({
            "textDocument": { "uri": doc },
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 0, "character": 10 },
            },
            "context": { "diagnostics": [] },
        }),
    );
    (actions.as_array().map_or(&[][..], |a| a.as_slice()).iter())
        .filter_map(|a| a["title"].as_str().map(str::to_string))
        .collect()
}

const CONVERT: &str = "Convert to a named export";

/// The action edits one file, and `import x from` needs a default: it isn't offered while an
/// open document reads the default.
#[test]
fn convert_to_named_is_not_offered_when_an_open_document_imports_the_default() {
    let (mut client, app, lib) = open_app();
    let titles = titles_on_first_line(&mut client, &lib);
    assert!(!titles.iter().any(|t| t == CONVERT), "{titles:?}");

    // With the import gone, nothing stops it.
    client.change(
        &app,
        2,
        "import one from \"./dflt_anon\";\n\nfunction main() {}\n",
    );
    let _ = client.diagnostics(&app);
    let titles = titles_on_first_line(&mut client, &lib);
    assert!(titles.iter().any(|t| t == CONVERT), "{titles:?}");
    client.shutdown();
}

/// A file of the workspace folder that isn't open counts too.
#[test]
fn convert_to_named_is_not_offered_when_a_workspace_file_imports_the_default() {
    let dir = std::env::temp_dir().join(format!("velt_lsp_default_{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let lib_path = dir.join("lib.vlt");
    std::fs::write(&lib_path, LIB).unwrap();
    std::fs::write(dir.join("other.vlt"), LIB).unwrap();
    std::fs::write(
        dir.join("sub/user.vlt"),
        "import hello from \"../lib\";\nconsole.log(hello(\"a\"));\n",
    )
    .unwrap();
    let root = lsp_types::Url::from_file_path(&dir).unwrap();
    let mut client = Client::start_with(json!({ "capabilities": {}, "rootUri": root }));
    let lib = lsp_types::Url::from_file_path(&lib_path).unwrap();
    client.open(&lib, LIB);
    let _ = client.diagnostics(&lib);
    let titles = titles_on_first_line(&mut client, &lib);
    assert!(!titles.iter().any(|t| t == CONVERT), "{titles:?}");

    let other = lsp_types::Url::from_file_path(dir.join("other.vlt")).unwrap();
    client.open(&other, LIB);
    let _ = client.diagnostics(&other);
    let titles = titles_on_first_line(&mut client, &other);
    assert!(titles.iter().any(|t| t == CONVERT), "{titles:?}");
    client.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}
