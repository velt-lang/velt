//! Doc comments in the editor: hover, completion (resolved documentation, the deprecated tag),
//! signature help and the deprecated hint on uses.

use lsp_types::Url;
use serde_json::{json, Value};

use super::client::{at, pos_of, uri, Client};
use super::navigation::hover_text;

const LIB: &str = r#"/**
 * Doubles `x`.
 * @param x - the number to double
 * @returns twice `x`
 */
export function double(x: i64): i64 {
  return x * 2;
}

/** @deprecated use `double` */
export function twice(x: i64): i64 {
  return x * 2;
}
"#;

const APP: &str = r#"import { double, twice } from "./doc_lib";
import * as lib from "./doc_lib";

/** A user of the system. */
class User {
  /** The display name. */
  name: string;
  /** Makes a user called `name`. */
  constructor(name: string) {
    this.name = name;
  }
  /**
   * Greets someone.
   * @param other - who to greet
   */
  greet(other: string): string {
    return `${this.name} greets ${other}`;
  }
}

/// Adds two numbers.
/// @param a - the first number
/// @param b - the second number
function add(a: i64, b: i64): i64 {
  return a + b;
}

function main() {
  const u = new User("a");
  u.greet("b");
  console.log(u.name);
  console.log(add(double(1), twice(2)));
  console.log(lib.double(3));
}
"#;

fn open() -> (Client, Url) {
    let mut client = Client::start();
    client.open(&uri("doc_lib.vlt"), LIB);
    let app = uri("doc_app.vlt");
    client.open(&app, APP);
    (client, app)
}

fn with_doc(signature: &str, doc: &str) -> String {
    format!("{signature}\n```\n\n---\n\n{doc}")
}

#[test]
fn hover_shows_the_doc_below_the_signature() {
    let (mut client, app) = open();
    let mut h = |needle: &str, delta: usize| hover_text(&mut client, &app, APP, needle, delta);
    assert_eq!(
        h("add(double", 0),
        with_doc(
            "function add(a: i64, b: i64): i64",
            "Adds two numbers.\n\n**Parameters**\n\n- `a`: the first number\n- `b`: the second number"
        )
    );
    assert_eq!(
        h("User(\"a\")", 0),
        with_doc("class User", "A user of the system.")
    );
    assert_eq!(
        h("u.name", 2),
        with_doc("(field) User.name: string", "The display name.")
    );
    assert_eq!(
        h("u.greet", 2),
        with_doc(
            "(method) User.greet(other: string): string",
            "Greets someone.\n\n**Parameters**\n\n- `other`: who to greet"
        )
    );
    // A parameter: its function's `@param`.
    assert_eq!(
        h("other}", 0),
        with_doc("(parameter) other: string", "who to greet")
    );
    // Imported, by name and through a namespace.
    let double = with_doc(
        "function double(x: i64): i64",
        "Doubles `x`.\n\n**Parameters**\n\n- `x`: the number to double\n\n**Returns:** twice `x`",
    );
    assert_eq!(h("double(1)", 0), double);
    assert_eq!(h("double(3)", 0), double);
    assert_eq!(
        h("twice(2)", 0),
        with_doc(
            "function twice(x: i64): i64",
            "**Deprecated:** use `double`"
        )
    );
    // Undocumented: the signature alone.
    assert_eq!(h("main()", 0), "function main(): void");
    client.shutdown();
}

fn item<'a>(items: &'a Value, label: &str) -> &'a Value {
    items
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["label"] == json!(label))
        .unwrap_or_else(|| panic!("no completion `{label}` in {items:#}"))
}

#[test]
fn completion_resolves_docs_and_tags_deprecated_items() {
    let (mut client, app) = open();
    assert_eq!(
        client.init["capabilities"]["completionProvider"]["resolveProvider"],
        json!(true)
    );
    let (line, col) = pos_of(APP, "  const u", 2);
    let items = client.request("textDocument/completion", at(&app, line, col));
    let add = item(&items, "add");
    assert!(add["documentation"].is_null(), "resolved lazily: {add}");
    let resolved = client.request("completionItem/resolve", add.clone());
    assert_eq!(resolved["documentation"]["kind"], json!("markdown"));
    let doc = resolved["documentation"]["value"].as_str().unwrap();
    assert!(doc.starts_with("Adds two numbers."), "{doc}");

    let twice = item(&items, "twice");
    assert_eq!(twice["tags"], json!([1]));
    assert_eq!(twice["deprecated"], json!(true));
    assert!(item(&items, "double")["tags"].is_null());
    // Undocumented: nothing to resolve.
    let main = item(&items, "main");
    assert!(main["data"].is_null(), "{main}");
    let resolved = client.request("completionItem/resolve", main.clone());
    assert!(resolved["documentation"].is_null());

    // Members.
    let text = APP.replace("u.greet(\"b\");", "u.");
    let doc_uri = uri("doc_members.vlt");
    client.open(&doc_uri, &text);
    let (line, col) = pos_of(&text, "u.\n", 2);
    let items = client.request("textDocument/completion", at(&doc_uri, line, col));
    let resolved = client.request("completionItem/resolve", item(&items, "name").clone());
    assert_eq!(
        resolved["documentation"]["value"],
        json!("The display name.")
    );
    client.shutdown();
}

#[test]
fn completion_resolves_docs_of_jsx_attributes() {
    let page = r#"// @jsxImportSource ./jsx_ui
type CardProps = {
  /** The card's heading. */
  title: string;
};

/** A card. */
function Card(props: CardProps): JSX.Element {
  return <div>{props.title}</div>;
}

function page(): JSX.Element {
  return <div><Card /></div>;
}
"#;
    let (mut client, doc) = super::jsx::open(page);
    let (line, col) = pos_of(page, "<Card />", 6);
    let items = client.request("textDocument/completion", at(&doc, line, col));
    let resolved = client.request("completionItem/resolve", item(&items, "title").clone());
    assert_eq!(
        resolved["documentation"]["value"],
        json!("The card's heading.")
    );
    let (line, col) = pos_of(page, "<Card />", 3);
    let items = client.request("textDocument/completion", at(&doc, line, col));
    let resolved = client.request("completionItem/resolve", item(&items, "Card").clone());
    assert_eq!(resolved["documentation"]["value"], json!("A card."));
    client.shutdown();
}

#[test]
fn signature_help_documents_the_call_and_its_parameters() {
    let (mut client, app) = open();
    let (line, col) = pos_of(APP, "double(1), twice", 4);
    let help = client.request("textDocument/signatureHelp", at(&app, line, col));
    let sig = &help["signatures"][0];
    assert_eq!(sig["documentation"]["value"], json!("Adds two numbers."));
    assert_eq!(
        sig["parameters"][0]["documentation"]["value"],
        json!("the first number")
    );
    assert_eq!(
        sig["parameters"][1]["documentation"]["value"],
        json!("the second number")
    );
    // `new C(`: the constructor's doc.
    let (line, col) = pos_of(APP, "User(\"a\")", 5);
    let help = client.request("textDocument/signatureHelp", at(&app, line, col));
    assert_eq!(
        help["signatures"][0]["documentation"]["value"],
        json!("Makes a user called `name`.")
    );
    // An imported function: returns included.
    let (line, col) = pos_of(APP, "double(3)", 7);
    let help = client.request("textDocument/signatureHelp", at(&app, line, col));
    assert_eq!(
        help["signatures"][0]["documentation"]["value"],
        json!("Doubles `x`.\n\n**Returns:** twice `x`")
    );
    client.shutdown();
}

#[test]
fn uses_of_deprecated_definitions_get_a_hint() {
    let (mut client, app) = open();
    let diags = client.diagnostics(&app);
    let hints: Vec<&Value> = diags["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["tags"] == json!([2]))
        .collect();
    let starts: Vec<(u32, u32)> = hints
        .iter()
        .map(|d| {
            let s = &d["range"]["start"];
            (
                s["line"].as_u64().unwrap() as u32,
                s["character"].as_u64().unwrap() as u32,
            )
        })
        .collect();
    // The import and the call, not `double`.
    assert_eq!(
        starts,
        [pos_of(APP, "twice }", 0), pos_of(APP, "twice(2)", 0)],
        "{diags:#}"
    );
    assert_eq!(hints[0]["severity"], json!(4));
    assert_eq!(
        hints[0]["message"],
        json!("`twice` is deprecated: use `double`")
    );
    client.shutdown();
}
