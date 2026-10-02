//! Inferred effects in the editor: `throws` and mutation inlay hints on declarations, and the
//! `mutating` semantic token modifier on calls.

use serde_json::json;

use super::client::{pos_of, uri, Client};

const APP: &str = r#"class NotFound {
  message: string;
  constructor(message: string) {
    this.message = message;
  }
}

class Cart {
  items: string[];
  constructor() {
    this.items = [];
  }
  add(item: string /* ) */) {
    this.items.push(item);
  }
  count(): usize {
    return this.items.length;
  }
}

function fill(cart: Cart, n: i64) {
  cart.add("x");
  console.log(n);
}

function find(id: string): string {
  if (id == "") {
    throw new NotFound(id);
  }
  return id;
}

function declared(id: string): string throws NotFound {
  return find(id);
}

function main() {
  const lookup = (id: string) => find(id);
  const c = new Cart();
  fill(c, 1);
  console.log(c.count());
  try {
    lookup("a");
    declared("b");
  } catch (e) {
    console.log(e.message);
  }
}
"#;

#[test]
fn inlay_hints_show_inferred_throws_and_mutation() {
    let mut client = Client::start();
    let doc = uri("effects_hints.vlt");
    client.open(&doc, APP);
    assert_eq!(client.diagnostics(&doc)["diagnostics"], json!([]));
    let params = json!({
        "textDocument": { "uri": doc },
        "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 100, "character": 0 } },
    });
    let result = client.request("textDocument/inlayHint", params);
    let hints: Vec<(u64, u64, String)> = result
        .as_array()
        .unwrap()
        .iter()
        .map(|h| {
            let p = &h["position"];
            let label = h["label"].as_str().unwrap().to_string();
            (
                p["line"].as_u64().unwrap(),
                p["character"].as_u64().unwrap(),
                label,
            )
        })
        .collect();
    let at = |needle: &str, delta: usize| {
        let (line, col) = pos_of(APP, needle, delta);
        (line as u64, col as u64)
    };
    // Every effect hint, in order: nothing on `count` (reads), `declared` (written `throws`),
    // `main`, or `n` (a number).
    let effect = |h: &&(u64, u64, String)| {
        h.2.starts_with("throws") || h.2.starts_with("modifies") || h.2 == "modified"
    };
    let mut effects: Vec<(u64, u64, String)> = hints.iter().filter(effect).cloned().collect();
    effects.sort();
    let hint = |needle: &str, delta: usize, label: &str| {
        let (line, col) = at(needle, delta);
        (line, col, label.to_string())
    };
    assert_eq!(
        effects,
        [
            hint("/* ) */) {", 8, "modifies this"),
            hint("cart: Cart", 0, "modified"),
            hint(
                "): string {
  if",
                9,
                "throws NotFound"
            ),
            hint("(id: string) => find", 12, "throws NotFound"),
        ]
    );
    client.shutdown();
}

#[test]
fn calls_that_modify_are_marked_mutating() {
    let mut client = Client::start();
    let doc = uri("effects_tokens.vlt");
    client.open(&doc, APP);
    client.diagnostics(&doc);
    let legend = &client.init["capabilities"]["semanticTokensProvider"]["legend"];
    let mods: Vec<String> = serde_json::from_value(legend["tokenModifiers"].clone()).unwrap();
    let bit = 1 << mods.iter().position(|m| m == "mutating").expect("legend");
    let result = client.request(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": doc } }),
    );
    let data: Vec<u32> = serde_json::from_value(result["data"].clone()).unwrap();
    let (mut line, mut col) = (0, 0);
    let mutating: Vec<(u32, u32)> = data
        .chunks(5)
        .filter_map(|t| {
            line += t[0];
            col = if t[0] == 0 { col + t[1] } else { t[1] };
            (t[4] & bit != 0).then_some((line, col))
        })
        .collect();
    let expected = [pos_of(APP, "add(\"x\")", 0), pos_of(APP, "fill(c, 1)", 0)];
    assert_eq!(mutating, expected);
    client.shutdown();
}
