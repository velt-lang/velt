//! Navigation through namespace imports (`import * as lib`) and re-exports: go to definition and
//! completion.

use serde_json::{json, Value};

use super::client::{at, pos_of, uri, Client};

const LIB: &str = "export function helper(x: i64): i64 {\n  return x * 2;\n}\n\nexport class Point {\n  x: i64 = 0;\n}\n";

const BARREL: &str =
    "export { helper as twice } from \"./nsm_lib\";\nexport * from \"./nsm_lib\";\n";

const APP: &str = r#"import * as lib from "./nsm_lib";
import { twice, Point } from "./nsm_barrel";

function main() {
  const p: lib.Point = new lib.Point();
  const q = new Point();
  console.log(lib.helper(1), twice(2), p.x, q.x);
}
"#;

fn open_app() -> (Client, lsp_types::Url) {
    let mut client = Client::start();
    let app = uri("nsm_app.vlt");
    client.open(&uri("nsm_lib.vlt"), LIB);
    client.open(&uri("nsm_barrel.vlt"), BARREL);
    client.open(&app, APP);
    let diags = client.diagnostics(&app);
    assert_eq!(
        diags["diagnostics"],
        json!([]),
        "the test program must check"
    );
    (client, app)
}

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
    let file = loc["uri"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_string();
    (
        file,
        start["line"].as_u64().unwrap() as u32,
        start["character"].as_u64().unwrap() as u32,
    )
}

#[test]
fn definition_through_namespaces_and_re_exports() {
    let (mut client, app) = open_app();
    let helper = ("nsm_lib.vlt".to_string(), 0, 16);
    let point = ("nsm_lib.vlt".to_string(), 4, 13);
    assert_eq!(definition(&mut client, &app, "helper(1)", 0), helper);
    assert_eq!(definition(&mut client, &app, "twice(2)", 0), helper);
    assert_eq!(definition(&mut client, &app, "Point = ", 0), point);
    assert_eq!(
        definition(&mut client, &app, "Point();\n  const q", 0),
        point
    );
    assert_eq!(
        definition(&mut client, &app, "Point();\n  console", 0),
        point
    );
    client.shutdown();
}

fn labels(result: &Value) -> Vec<String> {
    result
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["label"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn completion_lists_namespace_exports() {
    let (mut client, app) = open_app();
    let (line, col) = pos_of(APP, "lib.helper", "lib.".len());
    let items = labels(&client.request("textDocument/completion", at(&app, line, col)));
    assert_eq!(items, ["Point", "helper"]);
    let (line, col) = pos_of(APP, "console", 0);
    let items = labels(&client.request("textDocument/completion", at(&app, line, col)));
    assert!(items.contains(&"lib".to_string()), "{items:?}");
    assert!(!items.iter().any(|i| i.contains('.')), "{items:?}");
    client.shutdown();
}

#[test]
fn completion_lists_namespace_exports_while_typing() {
    let mut client = Client::start();
    let doc = uri("nsm_typing.vlt");
    client.open(&uri("nsm_lib.vlt"), LIB);
    let text = "import * as lib from \"./nsm_lib\";\n\nfunction main() {\n  lib.\n}\n";
    client.open(&doc, text);
    client.diagnostics(&doc);
    let (line, col) = pos_of(text, "lib.\n", "lib.".len());
    let items = labels(&client.request("textDocument/completion", at(&doc, line, col)));
    assert!(
        items.contains(&"helper".to_string()) && items.contains(&"Point".to_string()),
        "{items:?}"
    );
    client.shutdown();
}
