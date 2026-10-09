//! Quoted property names (#810) in the editor: completing `o.` offers a field named `a-b` and
//! writes it as `o["a-b"]` (`o?.["a-b"]` after `?.`), as TypeScript's editors do; identifier
//! names complete as before.

use serde_json::Value;

use super::client::{at, pos_of, uri, Client};

const PAGE: &str = r#"type Headers = { "content-type": string; plain: boolean };

function f(h: Headers, m: Headers | null): string {
  const a = h.;
  const b = m?.;
  return "";
}
"#;

/// The completion items at the end of `needle` in `PAGE`.
fn items_after(needle: &str) -> Vec<Value> {
    let mut client = Client::start();
    let doc = uri("quoted_keys.vlt");
    client.open(&doc, PAGE);
    client.diagnostics(&doc);
    let (line, col) = pos_of(PAGE, needle, needle.len());
    let result = client.request("textDocument/completion", at(&doc, line, col));
    client.shutdown();
    result.as_array().cloned().unwrap_or_default()
}

fn find<'a>(items: &'a [Value], label: &str) -> &'a Value {
    items
        .iter()
        .find(|i| i["label"] == label)
        .unwrap_or_else(|| panic!("no completion {label:?} in {items:?}"))
}

#[test]
fn a_quoted_field_completes_as_an_element_access() {
    let items = items_after("const a = h.");
    let quoted = find(&items, "content-type");
    assert_eq!(quoted["textEdit"]["newText"], "[\"content-type\"]");
    // The edit replaces the `.`: `h.` becomes `h["content-type"]`.
    assert_eq!(quoted["textEdit"]["range"]["start"]["character"], 13);
    let plain = find(&items, "plain");
    assert!(plain.get("textEdit").is_none_or(Value::is_null));
}

#[test]
fn after_optional_chaining_the_question_mark_stays() {
    let items = items_after("const b = m?.");
    let quoted = find(&items, "content-type");
    assert_eq!(quoted["textEdit"]["newText"], ".[\"content-type\"]");
}
