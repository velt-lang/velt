//! Doc comments on the newer surfaces: tag rendering (`@throws`, `@example`, `@see`, bracketed
//! `@param` names), plain comments between a doc comment and its declaration, class members
//! (accessors, statics, ES private names in hover), intersection-type fields and generics, in
//! hover, completion and signature help.

use lsp_types::Url;
use serde_json::{json, Value};

use super::client::{at, pos_of, uri, Client};
use super::navigation::hover_text;

const SRC: &str = r#"/**
 * Parses a port.
 * @param text - the digits
 * @param [base=10] - the radix
 * @param [opts=[]] - the options
 * @returns the port
 * @throws RangeError when it is out of range
 * @example
 * parsePort("80", 10, []);
 * @see https://url.spec.whatwg.org
 */
function parsePort(text: string, base: i64, opts: string[]): i64 {
  return text.length + base + opts.length;
}

/** Kept despite the lint line. */
// eslint-disable-next-line no-unused-vars
function linted(): i64 {
  return 1;
}

/** Not attached: a blank line follows. */

function detached(): i64 {
  return 2;
}

/**
 * The first of `items`.
 * @param items - the candidates
 */
function first<T>(items: T[]): T {
  return items[0];
}

/** A box of one value. */
class Box<T> {
  /** The secret, hidden from `console.log`. */
  #secret: string;
  /** The value. */
  value: T;
  /** How many boxes were made. */
  static made: i64 = 0;
  constructor(value: T) {
    this.value = value;
    this.#secret = "s";
  }
  /** The secret's length. */
  get size(): i64 {
    return this.#reveal().length;
  }
  /** Reveals the secret. */
  #reveal(): string {
    return this.#secret;
  }
  /**
   * Replaces the value.
   * @param next - the new value
   */
  put(next: T): void {
    this.value = next;
  }
}

type Named = {
  /** The display name. */
  name: string;
};
type Person = Named & {
  /** Age in years. */
  age: i64;
};

function main() {
  const b = new Box<i64>(1);
  b.put(2);
  console.log(b.size, Box.made, b.value);
  const p: Person = { name: "a", age: 3 };
  console.log(p.name, p.age);
  console.log(parsePort("80", 10, []), linted(), detached(), first([1]));
}
"#;

fn open(text: &str) -> (Client, Url) {
    let mut client = Client::start();
    let doc = uri("doc_surfaces.vlt");
    client.open(&doc, text);
    (client, doc)
}

/// The hover's doc (the part below the signature), or `None` when it shows the signature only.
fn hover_doc(client: &mut Client, doc: &Url, needle: &str, delta: usize) -> Option<String> {
    let text = hover_text(client, doc, SRC, needle, delta);
    text.split_once("\n```\n\n---\n\n")
        .map(|(_, doc)| doc.to_string())
}

#[test]
fn hover_renders_every_tag() {
    let (mut client, doc) = open(SRC);
    assert_eq!(
        hover_doc(&mut client, &doc, "parsePort(\"80\", 10, []), linted", 0).unwrap(),
        "Parses a port.\n\n\
         **Parameters**\n\n\
         - `text`: the digits\n\
         - `base`: the radix\n\
         - `opts`: the options\n\n\
         **Returns:** the port\n\n\
         **Throws:** RangeError when it is out of range\n\n\
         **Example**\n\n```ts\nparsePort(\"80\", 10, []);\n```\n\n\
         **See also:** https://url.spec.whatwg.org"
    );
    // `[opts=[]]`: the name ends at its matching bracket.
    assert_eq!(
        hover_doc(&mut client, &doc, "opts: string[]", 0).as_deref(),
        Some("the options")
    );
    client.shutdown();
}

#[test]
fn hover_follows_the_attachment_rules() {
    let (mut client, doc) = open(SRC);
    // A plain comment line between the doc comment and the declaration is skipped.
    assert_eq!(
        hover_doc(&mut client, &doc, "linted(), detached", 0).as_deref(),
        Some("Kept despite the lint line.")
    );
    // A blank line ends the association.
    assert_eq!(hover_doc(&mut client, &doc, "detached(), first", 0), None);
    client.shutdown();
}

#[test]
fn hover_documents_class_members_private_names_and_generics() {
    let (mut client, doc) = open(SRC);
    let mut h = |needle: &str, delta: usize| hover_doc(&mut client, &doc, needle, delta);
    assert_eq!(h("Box<i64>(1)", 0).as_deref(), Some("A box of one value."));
    assert_eq!(
        h("put(2)", 0).as_deref(),
        Some("Replaces the value.\n\n**Parameters**\n\n- `next`: the new value")
    );
    assert_eq!(h("size, Box", 0).as_deref(), Some("The secret's length."));
    assert_eq!(
        h("made, b", 0).as_deref(),
        Some("How many boxes were made.")
    );
    assert_eq!(h("value);", 0).as_deref(), Some("The value."));
    // ES private names, inside the class.
    assert_eq!(
        h("#secret = \"s\"", 0).as_deref(),
        Some("The secret, hidden from `console.log`.")
    );
    assert_eq!(
        h("#reveal().length", 0).as_deref(),
        Some("Reveals the secret.")
    );
    // A generic function, called with an inferred type argument.
    assert_eq!(
        h("first([1])", 0).as_deref(),
        Some("The first of `items`.\n\n**Parameters**\n\n- `items`: the candidates")
    );
    // Fields of both parts of an intersection type.
    assert_eq!(h("name, p.age", 0).as_deref(), Some("The display name."));
    assert_eq!(h("age);", 0).as_deref(), Some("Age in years."));
    client.shutdown();
}

fn item<'a>(items: &'a Value, label: &str) -> &'a Value {
    let list = items.get("items").unwrap_or(items);
    list.as_array()
        .unwrap()
        .iter()
        .find(|i| i["label"] == json!(label))
        .unwrap_or_else(|| panic!("no completion `{label}` in {items:#}"))
}

/// The resolved documentation of the completion `label` offered at `needle` + `delta` in `text`.
fn resolved(client: &mut Client, text: &str, needle: &str, delta: usize, label: &str) -> Value {
    let doc = uri("doc_surfaces_completion.vlt");
    client.open(&doc, text);
    let (line, col) = pos_of(text, needle, delta);
    let items = client.request("textDocument/completion", at(&doc, line, col));
    let resolved = client.request("completionItem/resolve", item(&items, label).clone());
    resolved["documentation"].clone()
}

#[test]
fn completion_documents_members_accessors_and_intersection_fields() {
    let (mut client, _) = open(SRC);
    // Members after `.`.
    let text = SRC.replace("b.put(2);", "b.");
    let put = resolved(&mut client, &text, "b.\n", 2, "put");
    assert_eq!(put["kind"], json!("markdown"));
    assert_eq!(
        put["value"],
        json!("Replaces the value.\n\n**Parameters**\n\n- `next`: the new value")
    );
    let size = resolved(&mut client, &text, "b.\n", 2, "size");
    assert_eq!(size["value"], json!("The secret's length."));
    // An intersection type's fields.
    let text = SRC.replace("console.log(p.name, p.age);", "p.");
    let age = resolved(&mut client, &text, "p.\n", 2, "age");
    assert_eq!(age["value"], json!("Age in years."));
    let name = resolved(&mut client, &text, "p.\n", 2, "name");
    assert_eq!(name["value"], json!("The display name."));
    // A scope item: the whole comment.
    let text = SRC.replace("const b = new", "parse\n  const b = new");
    let parse = resolved(&mut client, &text, "parse\n", 5, "parsePort");
    let value = parse["value"].as_str().unwrap();
    assert!(
        value.starts_with("Parses a port.") && value.contains("**Throws:** RangeError"),
        "{value}"
    );
    client.shutdown();
}

#[test]
fn signature_help_documents_parameters_of_generics_and_methods() {
    let (mut client, doc) = open(SRC);
    let (line, col) = pos_of(SRC, "log(parsePort(\"80\", 10", 20);
    let help = client.request("textDocument/signatureHelp", at(&doc, line, col));
    let sig = &help["signatures"][0];
    assert_eq!(
        sig["documentation"]["value"],
        json!(
            "Parses a port.\n\n**Returns:** the port\n\n**Throws:** RangeError when it is out of range"
        )
    );
    assert_eq!(help["activeParameter"], json!(1));
    let params: Vec<&Value> = sig["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| &p["documentation"]["value"])
        .collect();
    assert_eq!(
        params,
        [
            &json!("the digits"),
            &json!("the radix"),
            &json!("the options")
        ]
    );
    // A generic function.
    let (line, col) = pos_of(SRC, "first([1])", 6);
    let help = client.request("textDocument/signatureHelp", at(&doc, line, col));
    assert_eq!(
        help["signatures"][0]["parameters"][0]["documentation"]["value"],
        json!("the candidates")
    );
    // A method of a generic class.
    let (line, col) = pos_of(SRC, "put(2)", 4);
    let help = client.request("textDocument/signatureHelp", at(&doc, line, col));
    let sig = &help["signatures"][0];
    assert_eq!(sig["documentation"]["value"], json!("Replaces the value."));
    assert_eq!(
        sig["parameters"][0]["documentation"]["value"],
        json!("the new value")
    );
    client.shutdown();
}
