//! `package.vlt`: the reader's diagnostics (not a program's), schema completion and hover.

use serde_json::{json, Value};

use super::client::{at, pos_of, uri, Client};

const MANIFEST: &str = r#"import type { Package } from "velt:package";

export const pkg: Package = {
  name: "app",
  version: "0.1.0",
  dependencies: { json: "1.2" },
  native: { targets: [], wasm: false },
};
"#;

fn messages(diags: &Value) -> Vec<String> {
    diags["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["message"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn a_manifest_gets_the_readers_diagnostics_not_a_programs() {
    let mut client = Client::start();
    let doc = uri("manifest_diags/package.vlt");
    client.open(&doc, MANIFEST);
    // Records and arrays in a module constant are fine here: the file is never compiled.
    assert_eq!(messages(&client.diagnostics(&doc)), Vec::<String>::new());

    let typo = MANIFEST.replace("dependencies", "dependecies");
    client.change(&doc, 2, &typo);
    let diags = client.diagnostics(&doc);
    assert_eq!(
        messages(&diags),
        ["unknown key `dependecies` in the manifest\nnote: did you mean `dependencies`?"]
    );
    let (line, character) = pos_of(&typo, "dependecies", 0);
    assert_eq!(
        diags["diagnostics"][0]["range"]["start"],
        json!({ "line": line, "character": character })
    );
    client.shutdown();
}

#[test]
fn completion_and_hover_come_from_the_schema() {
    let mut client = Client::start();
    let doc = uri("manifest_complete/package.vlt");
    let text = MANIFEST.replace("  native:", "  \n  native:");
    client.open(&doc, &text);
    let (line, _) = pos_of(&text, "  \n", 0);
    let list = client.request("textDocument/completion", at(&doc, line, 2));
    assert_eq!(list["isIncomplete"], false);
    let labels: Vec<&str> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["label"].as_str().unwrap())
        .collect();
    assert_eq!(
        labels,
        [
            "velt",
            "description",
            "keywords",
            "entry",
            "registry",
            "paths",
            "jsx",
            "tsCompat"
        ]
    );

    let (line, character) = pos_of(&text, "wasm", 1);
    let hover = client.request("textDocument/hover", at(&doc, line, character));
    let value = hover["contents"]["value"].as_str().unwrap();
    assert!(value.starts_with("```velt\nwasm?: boolean\n```"), "{value}");
    // Program features have nothing to say about data.
    let defs = client.request("textDocument/definition", at(&doc, line, character));
    assert_eq!(defs, Value::Null);
    client.shutdown();
}

/// `"` triggers completion for manifest versions; in a program it offers nothing outside module
/// specifiers.
#[test]
fn a_quote_triggers_nothing_in_a_program() {
    let mut client = Client::start();
    let doc = uri("quote_trigger.vlt");
    let text = "function main() {\n  const s = \"\n}\n";
    client.open(&doc, text);
    let (line, character) = pos_of(text, "= \"", 3);
    let mut params = at(&doc, line, character);
    params["context"] = json!({ "triggerKind": 2, "triggerCharacter": "\"" });
    let items = client.request("textDocument/completion", params);
    assert_eq!(items, json!([]));
    client.shutdown();
}
