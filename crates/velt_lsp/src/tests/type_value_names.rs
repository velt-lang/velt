//! A name that is both a type and a value (`type guid = string` next to `const guid = …`, as
//! TypeScript allows): go to definition and hover pick the meaning the position uses.

use serde_json::json;

use super::client::{at, pos_of, uri, Client};
use super::navigation::hover_text;

const LIB: &str = "export function makeId(): string {\n  return \"id\";\n}\n\nexport type guid = string;\nexport const guid = makeId;\n";

const APP: &str = r#"import { guid } from "./tvn_lib";

interface Point {
  x: i64;
}
function Point(x: i64): Point {
  return { x };
}

function main() {
  const id: guid = guid();
  const p: Point = Point(1);
  console.log(id, p.x);
}
"#;

/// (file name, line, character) of the definition at `needle` (+`delta`) of `APP`.
fn definition(
    client: &mut Client,
    app: &lsp_types::Url,
    needle: &str,
    delta: usize,
) -> (String, u32, u32) {
    let (line, col) = pos_of(APP, needle, delta);
    let loc = client.request("textDocument/definition", at(app, line, col));
    assert!(!loc.is_null(), "no definition for `{needle}`");
    let start = &loc["range"]["start"];
    let file = loc["uri"].as_str().unwrap().rsplit('/').next().unwrap();
    (
        file.to_string(),
        start["line"].as_u64().unwrap() as u32,
        start["character"].as_u64().unwrap() as u32,
    )
}

#[test]
fn type_and_value_of_one_name() {
    let mut client = Client::start();
    let app = uri("tvn_app.vlt");
    client.open(&uri("tvn_lib.vlt"), LIB);
    client.open(&app, APP);
    let diags = client.diagnostics(&app);
    assert_eq!(
        diags["diagnostics"],
        json!([]),
        "the test program must check"
    );
    let lib = "tvn_lib.vlt".to_string();
    // The type position goes to the type alias, the call to the constant.
    assert_eq!(
        definition(&mut client, &app, "guid = guid", 0),
        (lib.clone(), 4, 12)
    );
    assert_eq!(definition(&mut client, &app, "guid();", 0), (lib, 5, 13));
    let here = "tvn_app.vlt".to_string();
    let (l, c) = pos_of(APP, "Point {", 0);
    assert_eq!(
        definition(&mut client, &app, "Point = Point", 0),
        (here.clone(), l, c)
    );
    let (l, c) = pos_of(APP, "Point(x", 0);
    assert_eq!(definition(&mut client, &app, "Point(1)", 0), (here, l, c));
    let h = |c: &mut Client, needle, delta| hover_text(c, &app, APP, needle, delta);
    assert_eq!(h(&mut client, "guid = guid", 0), "type guid");
    assert_eq!(h(&mut client, "guid();", 0), "const guid: () => string");
    assert_eq!(h(&mut client, "Point = Point", 0), "interface Point");
    assert_eq!(
        h(&mut client, "Point(1)", 0),
        "function Point(x: i64): Point"
    );
    client.shutdown();
}

const RE: &str =
    "export type { guid } from \"./tvn_lib\";\nexport { makeId } from \"./tvn_lib\";\n";

const USER: &str = r#"import * as r from "./tvn_re";
import { guid } from "./tvn_re";
import * as lib from "./tvn_lib";

function main() {
  const id: guid = r.makeId();
  const other: lib.guid = lib.guid();
  console.log(id, other);
}
"#;

fn labels(result: &serde_json::Value) -> Vec<String> {
    let items = result.as_array().unwrap();
    items
        .iter()
        .map(|i| i["label"].as_str().unwrap().to_string())
        .collect()
}

/// Names exported only as types, and names that are a type and a value, complete as their name
/// alone: no internal key of either meaning is listed.
#[test]
fn completion_lists_each_name_once() {
    let mut client = Client::start();
    let doc = uri("tvn_user.vlt");
    client.open(&uri("tvn_lib.vlt"), LIB);
    client.open(&uri("tvn_re.vlt"), RE);
    client.open(&doc, USER);
    let diags = client.diagnostics(&doc);
    assert_eq!(
        diags["diagnostics"],
        json!([]),
        "the test program must check"
    );
    let clean = |items: &[String]| {
        items
            .iter()
            .all(|i| !i.contains(['\u{1}', '\u{2}']) && !i.contains('.'))
    };
    for (needle, expected) in [
        ("r.makeId", vec!["guid", "makeId"]),
        ("lib.guid()", vec!["guid", "makeId"]),
    ] {
        let (line, col) = pos_of(USER, needle, needle.find('.').unwrap() + 1);
        let mut items = labels(&client.request("textDocument/completion", at(&doc, line, col)));
        items.sort();
        assert_eq!(items, expected, "after `{needle}`");
    }
    let (line, col) = pos_of(USER, "console", 0);
    let items = labels(&client.request("textDocument/completion", at(&doc, line, col)));
    assert!(clean(&items), "{items:?}");
    let count = |n: &str| items.iter().filter(|i| *i == n).count();
    assert_eq!(
        (count("guid"), count("r"), count("lib")),
        (1, 1, 1),
        "{items:?}"
    );
    client.shutdown();
}
