//! JSX in the editor: completion of tags and attributes, go to definition and hover on
//! component tags and attributes, through a small JSX runtime (`jsx_ui/jsx-runtime.vlt`).

use lsp_types::Url;
use serde_json::Value;

use super::client::{at, pos_of, uri, Client};

pub(super) const RUNTIME: &str = r#"export class Element {
  html: string;
  constructor(html: string) {
    this.html = html;
  }
}
export type Child = Element | string | null;
export type AttrValue = string | bool | null;
export type IntrinsicElements = {
  a: { href?: string; title?: string };
  div: { class?: string; id?: string };
  h1: { class?: string };
  dialog: { open?: bool };
};
export function jsx(
  tag: string,
  names: string[],
  values: AttrValue[],
  children: Child[],
  key: string | null,
): Element {
  return new Element(tag);
}
export function Fragment(children: Child[], key: string | null): Element {
  return new Element("");
}
export function jsxComponent<P>(
  component: (props: P) => Element,
  props: P,
  key: string | null,
  name: string,
): Element {
  return component(props);
}
"#;

const PAGE: &str = r#"// @jsxImportSource ./jsx_ui
type CardProps = { title: string; subtitle?: string };

function Card(props: CardProps): JSX.Element {
  return <div class="card">{props.title}</div>;
}

function page(): JSX.Element {
  return <div id="main"><Card title="Hello" /><a href="/x">x</a></div>;
}
"#;

/// Components in another module, imported by name and as a namespace.
pub(super) const CARDS: &str = r#"import { Element } from "./jsx_ui/jsx-runtime";

export type CardProps = { title: string; tone?: string };

export function Card(props: CardProps): Element {
  return new Element(props.title);
}
"#;

/// A client with the runtime, `jsx_cards.vlt` and `page.vlt` (returned URI) open.
pub(super) fn open(page: &str) -> (Client, Url) {
    let mut client = Client::start();
    client.open(&uri("jsx_ui/jsx-runtime.vlt"), RUNTIME);
    client.open(&uri("jsx_cards.vlt"), CARDS);
    let doc = uri("jsx_page.vlt");
    client.open(&doc, page);
    client.diagnostics(&doc);
    (client, doc)
}

pub(super) fn labels(result: &Value) -> Vec<String> {
    result
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["label"].as_str().unwrap().to_string())
        .collect()
}

/// Completion labels in `text` (opened as the page) right after `needle`.
pub(super) fn complete(text: &str, needle: &str) -> Vec<String> {
    let (mut client, doc) = open(text);
    let (line, col) = pos_of(text, needle, needle.len());
    let items = labels(&client.request("textDocument/completion", at(&doc, line, col)));
    client.shutdown();
    items
}

#[test]
fn tags_complete_from_the_runtime_and_the_scope() {
    let text = PAGE.replace("<a href=\"/x\">x</a>", "<di");
    let items = complete(&text, "<di");
    assert_eq!(items[..3], ["a", "dialog", "div"], "{items:?}");
    assert!(items.contains(&"Card".to_string()), "{items:?}");
    assert!(!items.contains(&"page".to_string()), "{items:?}");
}

#[test]
fn attributes_complete_from_the_tag_or_the_props() {
    let text = PAGE.replace("<a href=\"/x\">x</a>", "<a ");
    assert_eq!(complete(&text, "<a "), ["href", "title"]);
    let text = PAGE.replace("<a href=\"/x\">x</a>", "<a title=\"t\" ");
    assert_eq!(complete(&text, "<a title=\"t\" "), ["href"]);
    let text = PAGE.replace("<a href=\"/x\">x</a>", "<Card ");
    assert_eq!(complete(&text, "<Card "), ["title", "subtitle"]);
}

#[test]
fn definition_and_hover_on_tags_and_attributes() {
    let (mut client, doc) = open(PAGE);
    let (line, col) = pos_of(PAGE, "<Card title", 2);
    let loc = client.request("textDocument/definition", at(&doc, line, col));
    let (def_line, def_col) = pos_of(PAGE, "Card(props", 0);
    assert_eq!(loc["uri"].as_str(), Some(doc.as_str()));
    assert_eq!(loc["range"]["start"]["line"], def_line);
    assert_eq!(loc["range"]["start"]["character"], def_col);
    let hover = |client: &mut Client, needle: &str, delta: usize| {
        let (line, col) = pos_of(PAGE, needle, delta);
        let h = client.request("textDocument/hover", at(&doc, line, col));
        h["contents"]["value"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    };
    let attr = hover(&mut client, "href=", 1);
    assert!(attr.contains("href: string | null"), "{attr}");
    let prop = hover(&mut client, "title=\"Hello", 1);
    assert!(prop.contains("title: string"), "{prop}");
    let component = hover(&mut client, "<Card title", 2);
    assert!(
        component.contains("function Card(props: CardProps)"),
        "{component}"
    );
    client.shutdown();
}

#[test]
fn the_scope_walker_descends_into_elements() {
    let text = "// @jsxImportSource ./jsx_ui\nfunction page(items: string[]): Element {\n  const label = \"x\";\n  return <div id={label}>{items.map((item) => <a title={item}>{label}</a>)}</div>;\n}\n";
    let (mut client, doc) = open(text);
    let refs = client.request(
        "textDocument/references",
        serde_json::json!({
            "textDocument": { "uri": doc },
            "position": { "line": 2, "character": 9 },
            "context": { "includeDeclaration": true },
        }),
    );
    assert_eq!(refs.as_array().map(Vec::len), Some(3), "{refs}");
    let (line, col) = pos_of(text, "{item}", 1);
    let loc = client.request("textDocument/definition", at(&doc, line, col));
    let (def_line, def_col) = pos_of(text, "(item)", 1);
    assert_eq!(loc["range"]["start"]["line"], def_line);
    assert_eq!(loc["range"]["start"]["character"], def_col);
    client.shutdown();
}

#[test]
fn the_ast_fallback_sees_names_inside_elements() {
    use crate::index::scope::{self, Reference};
    let dir = std::env::temp_dir().join("velt_lsp_tests");
    let path = dir.join("jsx_walk.vlt");
    let text = "// @jsxImportSource ./jsx_ui\nfunction Card(props: { t: string }): Element {\n  return <div>{props.t}</div>;\n}\nfunction page(items: string[]): Element {\n  return <div>{items.map((item) => <Card t={item} />)}</div>;\n}\n";
    let overlay = std::collections::HashMap::from([
        (path.clone(), text.to_string()),
        (dir.join("jsx_ui/jsx-runtime.vlt"), RUNTIME.to_string()),
    ]);
    let analysis = crate::analysis::analyze(
        &super::loader::TestLoader,
        &path,
        &overlay,
        &mut Default::default(),
    );
    let card = text.find("<Card").unwrap() as u32 + 2;
    let info = scope::at_offset(&analysis, card);
    assert!(
        matches!(&info.reference, Some(Reference::Name { ident, local: None }) if ident.name == "Card")
    );
    let item = text.find("{item}").unwrap() as u32 + 2;
    let info = scope::at_offset(&analysis, item);
    assert!(
        matches!(&info.reference, Some(Reference::Name { local: Some(l), .. }) if l.name == "item")
    );
}
