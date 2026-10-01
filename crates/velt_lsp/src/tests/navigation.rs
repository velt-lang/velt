//! Document symbols, go to definition, hover and completion.

use lsp_types::Url;
use serde_json::{json, Value};

use super::client::{at, pos_of, uri, Client};

const APP: &str = r#"import { helper as twice } from "./nav_util";

class User {
  name: string;
  age: i64;
  constructor(name: string) {
    this.name = name;
    this.age = 0;
  }
  greet(): string {
    return this.name;
  }
}

enum Color { Red, Green = 5 }

type Shape = { kind: "circle"; r: f64 } | { kind: "empty" };

function area(s: Shape, c: Color): f64 {
  switch (s.kind) {
    case "circle":
      return s.r * s.r;
    case "empty":
      return c == Color.Green ? 1.0 : 0.0;
  }
}

function main() {
  const total = twice(2);
  let u: User = new User("a");
  u.greet();
  for (let i = 0; i < 3; i++) {
    console.log(total + i);
  }
}
"#;

const UTIL: &str = "export function helper(x: i64): i64 {\n  return x * 2;\n}\n";

/// A client with `nav_app.vlt` (returned URI) and `nav_util.vlt` open.
fn open_app() -> (Client, Url) {
    let mut client = Client::start();
    let app = uri("nav_app.vlt");
    client.open(&uri("nav_util.vlt"), UTIL);
    client.open(&app, APP);
    let diags = client.diagnostics(&app);
    for d in diags["diagnostics"].as_array().unwrap() {
        let msg = d["message"].as_str().unwrap();
        assert!(
            !msg.starts_with("expected"),
            "test source must parse: {msg}"
        );
    }
    (client, app)
}

/// Go to definition at the first `needle` (+`delta`) of `APP`; returns (uri, line, character).
fn definition(client: &mut Client, app: &Url, needle: &str, delta: usize) -> (String, u32, u32) {
    let (line, col) = pos_of(APP, needle, delta);
    let loc = client.request("textDocument/definition", at(app, line, col));
    assert!(!loc.is_null(), "no definition for `{needle}`");
    let start = &loc["range"]["start"];
    (
        loc["uri"].as_str().unwrap().to_string(),
        start["line"].as_u64().unwrap() as u32,
        start["character"].as_u64().unwrap() as u32,
    )
}

#[test]
fn document_symbols_nest_members() {
    let (mut client, app) = open_app();
    let symbols = client.request(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": app } }),
    );
    let names: Vec<&str> = symbols
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["User", "Color", "Shape", "area", "main"]);
    let user = &symbols[0];
    assert_eq!(user["kind"], json!(5), "class");
    let members: Vec<(&str, u64)> = user["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["name"].as_str().unwrap(), c["kind"].as_u64().unwrap()))
        .collect();
    assert_eq!(
        members,
        [("name", 8), ("age", 8), ("constructor", 9), ("greet", 6)]
    );
    assert_eq!(symbols[1]["children"][0]["name"], json!("Red"));
    assert_eq!(
        symbols[3]["detail"],
        json!("function area(s: Shape, c: Color): f64")
    );
    client.shutdown();
}

#[test]
fn definition_of_locals_items_members_and_imports() {
    let (mut client, app) = open_app();
    let here = app.as_str().to_string();
    // Local: `total` in `console.log(total + i)` → `const total`.
    let (l, c) = pos_of(APP, "total =", 0);
    assert_eq!(
        definition(&mut client, &app, "total + i", 0),
        (here.clone(), l, c)
    );
    // Loop variable.
    let (l, c) = pos_of(APP, "i = 0", 0);
    assert_eq!(
        definition(&mut client, &app, "i++", 0),
        (here.clone(), l, c)
    );
    // Type name → class.
    let (l, c) = pos_of(APP, "User {", 0);
    assert_eq!(
        definition(&mut client, &app, "User(\"a\")", 0),
        (here.clone(), l, c)
    );
    // `this.name` → field.
    let (l, c) = pos_of(APP, "name: string;", 0);
    assert_eq!(
        definition(&mut client, &app, "this.name;", 5),
        (here.clone(), l, c)
    );
    // `u.greet()` → method, via the local's declared type.
    let (l, c) = pos_of(APP, "greet(): string", 0);
    assert_eq!(
        definition(&mut client, &app, "u.greet", 2),
        (here.clone(), l, c)
    );
    // Enum member.
    let (l, c) = pos_of(APP, "Green = 5", 0);
    assert_eq!(
        definition(&mut client, &app, "Color.Green ?", 6),
        (here.clone(), l, c)
    );
    // A field of a narrowed union member → the field in the object type.
    let (l, c) = pos_of(APP, "r: f64", 0);
    assert_eq!(
        definition(&mut client, &app, "s.r * s.r", 2),
        (here.clone(), l, c)
    );
    // Aliased import → the function in the other file.
    let util = uri("nav_util.vlt").as_str().to_string();
    assert_eq!(
        definition(&mut client, &app, "twice(2)", 0),
        (util.clone(), 0, 16)
    );
    assert_eq!(definition(&mut client, &app, "helper as", 0), (util, 0, 16));
    // Nothing to find on a keyword.
    let (line, col) = pos_of(APP, "switch (s", 0);
    let none = client.request("textDocument/definition", at(&app, line, col));
    assert_eq!(none, Value::Null);
    client.shutdown();
}

fn hover_text(client: &mut Client, doc: &Url, text: &str, needle: &str, delta: usize) -> String {
    let (line, col) = pos_of(text, needle, delta);
    let hover = client.request("textDocument/hover", at(doc, line, col));
    let value = hover["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("no hover at `{needle}`"));
    value
        .trim_start_matches("```velt\n")
        .trim_end_matches("\n```")
        .to_string()
}

#[test]
fn hover_shows_signatures() {
    let (mut client, app) = open_app();
    let h = |c: &mut Client, needle, delta| hover_text(c, &app, APP, needle, delta);
    assert_eq!(
        h(&mut client, "twice(2)", 0),
        "function helper(x: i64): i64"
    );
    assert_eq!(
        h(&mut client, "area(", 0),
        "function area(s: Shape, c: Color): f64"
    );
    assert_eq!(h(&mut client, "User(\"a\")", 0), "class User");
    assert_eq!(h(&mut client, "this.age", 5), "(field) User.age: i64");
    assert_eq!(
        h(&mut client, "u.greet", 2),
        "(method) User.greet(): string"
    );
    assert_eq!(h(&mut client, "Green ?", 0), "Color.Green = 5");
    assert_eq!(h(&mut client, "s: Shape", 0), "(parameter) s: Shape");
    assert_eq!(
        h(&mut client, "Color {", 0),
        "enum Color { Red, Green = 5 }"
    );
    client.shutdown();
}

#[test]
fn hover_uses_sema_types_when_the_program_checks() {
    let mut client = Client::start();
    let doc = uri("typed.vlt");
    let text = "function add(a: i64, b: i64): i64 {\n  return a + b;\n}\n\nfunction main() {\n  const sum = add(1, 2);\n  const half = 0.5;\n  console.log(sum, half);\n}\n";
    client.open(&doc, text);
    assert_eq!(client.diagnostics(&doc)["diagnostics"], json!([]));
    assert_eq!(
        hover_text(&mut client, &doc, text, "sum, half", 0),
        "const sum: i64"
    );
    assert_eq!(
        hover_text(&mut client, &doc, text, "half)", 0),
        "const half: f64"
    );
    // Not a name: the type of the innermost expression.
    assert_eq!(hover_text(&mut client, &doc, text, "+ b", 0), "i64");
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
fn completion_offers_scope_items_and_keywords() {
    let (mut client, app) = open_app();
    let (line, col) = pos_of(APP, "console.log(total", 0);
    let items = labels(&client.request("textDocument/completion", at(&app, line, col)));
    for expected in [
        "total", "u", "i", "twice", "User", "Shape", "area", "main", "while", "i64", "console",
    ] {
        assert!(
            items.contains(&expected.to_string()),
            "missing `{expected}` in {items:?}"
        );
    }
    assert!(
        !items.contains(&"r".to_string()),
        "match binding out of scope"
    );
    assert!(
        !items.contains(&"helper".to_string()),
        "imported under an alias"
    );
    client.shutdown();
}

#[test]
fn completion_after_a_dot_lists_members() {
    let mut client = Client::start();
    let doc = uri("members.vlt");
    // `this.` and `u.` are incomplete statements, as while typing.
    let text = "class User {\n  name: string;\n  greet(): string {\n    this.\n  }\n}\n\nenum Color { Red, Green }\n\nfunction main() {\n  let u = new User();\n  u.\n  Color.\n}\n";
    client.open(&doc, text);
    client.diagnostics(&doc);
    let complete = |client: &mut Client, needle: &str| {
        let (line, col) = pos_of(text, needle, needle.len());
        labels(&client.request("textDocument/completion", at(&doc, line, col)))
    };
    assert_eq!(complete(&mut client, "this."), ["name", "greet"]);
    assert_eq!(complete(&mut client, "u."), ["name", "greet"]);
    assert_eq!(complete(&mut client, "Color."), ["Red", "Green"]);
    client.shutdown();
}
