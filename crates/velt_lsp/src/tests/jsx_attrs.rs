//! `JSX.IntrinsicAttributes` in the editor: attributes every component accepts besides its props
//! (sigx's `client:load`) complete after the props, also while typing across the `:`, hover
//! shows their declared type, and a misspelled one is a diagnostic with TypeScript's message.

use serde_json::Value;

use super::client::{at, pos_of, uri, Client};
use super::jsx::RUNTIME;

/// The JSX test runtime plus `IntrinsicAttributes` and `jsxComponentAttributes`.
fn runtime() -> String {
    format!(
        "{RUNTIME}
export type IntrinsicAttributes = {{
  /** The element's key. */
  key?: string;
  /** Hydrate as soon as the page loads. */
  \"client:load\"?: bool;
  \"client:media\"?: string;
}};
export function jsxComponentAttributes<P>(
  component: (props: P) => Element,
  props: P,
  key: string | null,
  name: string,
  names: string[],
  values: AttrValue[],
): Element {{
  return component(props);
}}
"
    )
}

const PAGE: &str = r#"// @jsxImportSource ./jsx_attrs_ui
type CardProps = { title: string };

function Card(props: CardProps): JSX.Element {
  return <div>{props.title}</div>;
}

function page(): JSX.Element {
  return <div><Card client:load title="Hello" /></div>;
}
"#;

fn open(page: &str) -> (Client, lsp_types::Url) {
    let mut client = Client::start();
    client.open(&uri("jsx_attrs_ui/jsx-runtime.vlt"), &runtime());
    let doc = uri("jsx_attrs_page.vlt");
    client.open(&doc, page);
    (client, doc)
}

fn completion(page: &str, needle: &str) -> Vec<Value> {
    let (mut client, doc) = open(page);
    client.diagnostics(&doc);
    let (line, col) = pos_of(page, needle, needle.len());
    let result = client.request("textDocument/completion", at(&doc, line, col));
    client.shutdown();
    result.as_array().cloned().unwrap_or_default()
}

fn labels(items: &[Value]) -> Vec<&str> {
    items.iter().filter_map(|i| i["label"].as_str()).collect()
}

#[test]
fn component_attributes_complete_after_the_props() {
    let page = PAGE.replace("<Card client:load title=\"Hello\" />", "<Card ");
    let items = completion(&page, "<Card ");
    assert_eq!(labels(&items), ["title", "client:load", "client:media"]);
    let load = &items[1];
    assert_eq!(load["detail"], "boolean | null");
}

#[test]
fn typing_across_the_colon_replaces_the_whole_name() {
    let page = PAGE.replace("<Card client:load title=\"Hello\" />", "<Card client:lo");
    let items = completion(&page, "<Card client:lo");
    let load = items
        .iter()
        .find(|i| i["label"] == "client:load")
        .expect("client:load");
    let (_, col) = pos_of(&page, "client:lo", 0);
    assert_eq!(load["textEdit"]["range"]["start"]["character"], col);
}

#[test]
fn hover_shows_the_declared_type_and_docs() {
    let (mut client, doc) = open(PAGE);
    client.diagnostics(&doc);
    let (line, col) = pos_of(PAGE, "client:load", 3);
    let hover = client.request("textDocument/hover", at(&doc, line, col));
    let text = hover["contents"]["value"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    client.shutdown();
    assert!(text.contains("client:load"), "{text}");
    assert!(text.contains("boolean | null"), "{text}");
    assert!(
        text.contains("Hydrate as soon as the page loads."),
        "{text}"
    );
}

#[test]
fn go_to_definition_finds_the_field_after_key() {
    // `key` is declared first and is not one of these attributes: the field index still
    // points at `client:load`, not at the field before it.
    let (mut client, doc) = open(PAGE);
    client.diagnostics(&doc);
    let (line, col) = pos_of(PAGE, "client:load", 3);
    let loc = client.request("textDocument/definition", at(&doc, line, col));
    client.shutdown();
    let runtime = runtime();
    let (def_line, _) = pos_of(&runtime, "\"client:load\"?: bool", 0);
    assert_eq!(loc["range"]["start"]["line"], def_line, "{loc}");
}

#[test]
fn a_misspelled_attribute_is_a_diagnostic() {
    let page = PAGE.replace("client:load", "client:lod");
    let (mut client, doc) = open(&page);
    let diags = client.diagnostics(&doc);
    client.shutdown();
    let messages: Vec<&str> = diags["diagnostics"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| d["message"].as_str())
        .collect();
    assert!(
        messages.iter().any(|m| m.contains(
            "Property 'client:lod' does not exist on type 'IntrinsicAttributes & CardProps'. Did you mean 'client:load'?"
        )),
        "{messages:?}"
    );
}
