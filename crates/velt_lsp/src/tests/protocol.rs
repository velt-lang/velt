//! Lifecycle, diagnostics, formatting and robustness.

use lsp_server::ErrorCode;
use serde_json::{json, Value};

use super::client::{at, uri, Client};

#[test]
fn initialize_advertises_capabilities() {
    let client = Client::start();
    let caps = &client.init["capabilities"];
    assert_eq!(caps["textDocumentSync"]["change"], json!(1), "full sync");
    for cap in [
        "documentFormattingProvider",
        "documentSymbolProvider",
        "definitionProvider",
        "hoverProvider",
        "referencesProvider",
        "renameProvider",
        "inlayHintProvider",
        "documentHighlightProvider",
        "workspaceSymbolProvider",
    ] {
        assert_eq!(caps[cap], json!(true), "{cap}");
    }
    assert_eq!(
        caps["codeActionProvider"]["codeActionKinds"],
        json!(["quickfix"])
    );
    assert_eq!(
        caps["signatureHelpProvider"]["triggerCharacters"],
        json!(["(", ","])
    );
    assert_eq!(caps["semanticTokensProvider"]["full"], json!(true));
    assert_eq!(
        caps["completionProvider"]["triggerCharacters"],
        json!(["."])
    );
    assert_eq!(client.init["serverInfo"]["name"], json!("velt-lsp"));
    client.shutdown();
}

#[test]
fn diagnostics_on_open_use_utf16_columns() {
    let mut client = Client::start();
    let doc = uri("diag.vlt");
    // `é` is two bytes but one UTF-16 unit; `😀` is four bytes and two units.
    client.open(
        &doc,
        "function main() {\n  const s = \"é😀\"; let x: i64 = \"no\";\n}\n",
    );
    let params = client.diagnostics(&doc);
    assert_eq!(params["version"], json!(1));
    let diags = params["diagnostics"].as_array().unwrap();
    assert_eq!(diags.len(), 1, "{diags:?}");
    let d = &diags[0];
    assert!(
        d["message"].as_str().unwrap().contains("mismatched types"),
        "{d}"
    );
    assert_eq!(d["severity"], json!(1));
    assert_eq!(d["source"], json!("velt"));
    // `"no"` starts at byte 35 of line 1 but at UTF-16 column 32.
    assert_eq!(d["range"]["start"], json!({ "line": 1, "character": 32 }));
    client.shutdown();
}

#[test]
fn parse_errors_are_reported_and_fixed_after_debounced_edits() {
    let mut client = Client::start();
    let doc = uri("edit.vlt");
    client.open(&doc, "function main() {\n  let x = ;\n}\n");
    let first = client.diagnostics(&doc);
    let msg = first["diagnostics"][0]["message"].as_str().unwrap();
    assert!(msg.contains("expected expression"), "{msg}");
    assert_eq!(
        first["diagnostics"][0]["range"]["start"],
        json!({ "line": 1, "character": 10 })
    );

    // A burst of edits is analyzed once, for the final text.
    client.change(&doc, 2, "function main() {\n  let x = 1\n}\n");
    client.change(&doc, 3, "function main() {\n  let x = 1;\n");
    client.change(&doc, 4, "function main() {\n  let x = 1;\n}\n");
    let fixed = client.diagnostics(&doc);
    assert_eq!(fixed["version"], json!(4));
    assert_eq!(fixed["diagnostics"], json!([]));
    client.shutdown();
}

#[test]
fn errors_in_imported_files_stay_in_their_file() {
    let mut client = Client::start();
    let main = uri("imp_main.vlt");
    let util = uri("imp_util.vlt");
    client.open(&util, "export function f(): i64 {\n  return \"s\";\n}\n");
    client.open(
        &main,
        "import { f } from \"./imp_util\";\nfunction main() {\n  f();\n}\n",
    );
    let util_diags = client.diagnostics(&util);
    assert_eq!(util_diags["diagnostics"].as_array().unwrap().len(), 1);
    let main_diags = client.diagnostics(&main);
    assert_eq!(main_diags["diagnostics"], json!([]), "{main_diags}");
    client.shutdown();
}

#[test]
fn library_files_do_not_need_main() {
    let mut client = Client::start();
    let doc = uri("lib.vlt");
    client.open(
        &doc,
        "export function twice(x: i64): i64 {\n  return x * 2;\n}\n",
    );
    assert_eq!(client.diagnostics(&doc)["diagnostics"], json!([]));
    client.shutdown();
}

#[test]
fn close_clears_diagnostics() {
    let mut client = Client::start();
    let doc = uri("close.vlt");
    client.open(&doc, "function main( {");
    assert!(!client.diagnostics(&doc)["diagnostics"]
        .as_array()
        .unwrap()
        .is_empty());
    client.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": doc } }),
    );
    assert_eq!(client.diagnostics(&doc)["diagnostics"], json!([]));
    client.shutdown();
}

#[test]
fn formatting_replaces_the_document() {
    let mut client = Client::start();
    let doc = uri("fmt.vlt");
    client.open(&doc, "function main(){let x=1;\nconsole.log(x)  ;}\n");
    let params = json!({ "textDocument": { "uri": doc }, "options": { "tabSize": 2, "insertSpaces": true } });
    let edits = client.request("textDocument/formatting", params.clone());
    let edits = edits.as_array().unwrap();
    assert_eq!(edits.len(), 1);
    assert_eq!(
        edits[0]["newText"],
        json!("function main() {\n  let x = 1;\n  console.log(x);\n}\n")
    );
    assert_eq!(
        edits[0]["range"]["start"],
        json!({ "line": 0, "character": 0 })
    );
    assert_eq!(
        edits[0]["range"]["end"],
        json!({ "line": 2, "character": 0 })
    );

    // Unparsable documents are left alone.
    client.change(&doc, 2, "function main( {");
    assert_eq!(
        client.request("textDocument/formatting", params),
        Value::Null
    );
    client.shutdown();
}

#[test]
fn bad_requests_get_errors_not_crashes() {
    let mut client = Client::start();
    let unknown = client.request_raw("textDocument/codeLens", json!({}));
    assert_eq!(
        unknown.response_result.unwrap_err().code,
        ErrorCode::MethodNotFound as i32
    );
    let invalid = client.request_raw("textDocument/hover", json!({ "nonsense": 1 }));
    assert_eq!(
        invalid.response_result.unwrap_err().code,
        ErrorCode::InvalidParams as i32
    );
    // A document that was never opened: no answer, but no error either.
    let closed = uri("never_opened.vlt");
    assert_eq!(
        client.request("textDocument/hover", at(&closed, 0, 0)),
        Value::Null
    );
    client.shutdown();
}

#[test]
fn garbage_input_never_breaks_the_server() {
    let mut client = Client::start();
    let doc = uri("garbage.vlt");
    let text = "class { x: .. } function (( => ${ `unterminated ${ 😀\n enum E { A( }\n match (\n this.\n import { from \"\n";
    client.open(&doc, text);
    client.diagnostics(&doc);
    for (line, l) in text.lines().enumerate() {
        for character in 0..=l.encode_utf16().count() as u32 + 2 {
            let p = at(&doc, line as u32, character);
            for method in [
                "textDocument/hover",
                "textDocument/definition",
                "textDocument/completion",
            ] {
                let resp = client.request_raw(method, p.clone());
                assert!(
                    resp.response_result.is_ok(),
                    "{method} at {line}:{character}"
                );
            }
        }
    }
    let symbols = client.request(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": doc } }),
    );
    assert!(symbols.is_array());
    client.shutdown();
}
