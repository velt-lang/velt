//! Find references and rename (from sema's reference table), across files.

use serde_json::{json, Value};

use super::client::{at, pos_of, uri, Client};

const LIB: &str = "export function helper(x: i64): i64 {\n  return x * 2;\n}\n";

const MAIN: &str = r#"import { helper } from "./ref_lib";
import { helper as twice } from "./ref_lib";

function main() {
  let count = helper(1);
  count = count + twice(2);
  console.log(count);
}
"#;

fn open() -> (Client, lsp_types::Url, lsp_types::Url) {
    let mut client = Client::start();
    let lib = uri("ref_lib.vlt");
    let main = uri("ref_main.vlt");
    client.open(&lib, LIB);
    client.open(&main, MAIN);
    assert_eq!(client.diagnostics(&main)["diagnostics"], json!([]));
    (client, lib, main)
}

/// (uri, line, character) of every location in a `Location[]` result.
fn locations(v: &Value) -> Vec<(String, u64, u64)> {
    let mut out: Vec<(String, u64, u64)> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            let s = &l["range"]["start"];
            (
                l["uri"].as_str().unwrap().to_string(),
                s["line"].as_u64().unwrap(),
                s["character"].as_u64().unwrap(),
            )
        })
        .collect();
    out.sort();
    out
}

fn references(client: &mut Client, doc: &lsp_types::Url, line: u32, col: u32, decl: bool) -> Value {
    let mut params = at(doc, line, col);
    params["context"] = json!({ "includeDeclaration": decl });
    client.request("textDocument/references", params)
}

#[test]
fn references_of_a_local_and_of_an_imported_function() {
    let (mut client, lib, main) = open();
    let (l, c) = pos_of(MAIN, "count = count", 0);
    let locs = locations(&references(&mut client, &main, l, c, true));
    assert_eq!(locs.len(), 4, "declaration + 3 uses: {locs:?}");
    let without = locations(&references(&mut client, &main, l, c, false));
    assert_eq!(without.len(), 3);
    let (l, c) = pos_of(MAIN, "helper(1)", 0);
    let locs = locations(&references(&mut client, &main, l, c, true));
    let files: Vec<&str> = locs.iter().map(|(u, _, _)| u.as_str()).collect();
    assert!(
        files.contains(&lib.as_str()),
        "the declaration in the library"
    );
    assert_eq!(
        locs.len(),
        5,
        "declaration, 2 import names, 2 calls: {locs:?}"
    );
    client.shutdown();
}

#[test]
fn rename_edits_every_file_and_keeps_aliases() {
    let (mut client, lib, main) = open();
    let (l, c) = pos_of(MAIN, "helper(1)", 0);
    let mut params = at(&main, l, c);
    params["newName"] = json!("double");
    let edit = client.request("textDocument/rename", params);
    let changes = edit["changes"].as_object().unwrap();
    assert_eq!(changes[lib.as_str()].as_array().unwrap().len(), 1);
    let in_main = changes[main.as_str()].as_array().unwrap();
    assert_eq!(
        in_main.len(),
        3,
        "two import names and the direct call, not `twice`"
    );
    assert!(in_main.iter().all(|e| e["newText"] == json!("double")));

    let (l, c) = pos_of(MAIN, "count)", 0);
    let mut params = at(&main, l, c);
    params["newName"] = json!("total");
    let edit = client.request("textDocument/rename", params);
    assert_eq!(edit["changes"][main.as_str()].as_array().unwrap().len(), 4);

    let (l, c) = pos_of(MAIN, "console", 0);
    let mut params = at(&main, l, c);
    params["newName"] = json!("x");
    let err = client.request_raw("textDocument/rename", params);
    assert!(err.response_result.is_err(), "nothing renamable there");
    let (l, c) = pos_of(MAIN, "count)", 0);
    let mut params = at(&main, l, c);
    params["newName"] = json!("while");
    let err = client.request_raw("textDocument/rename", params);
    assert!(err.response_result.is_err(), "keywords are not identifiers");
    client.shutdown();
}
