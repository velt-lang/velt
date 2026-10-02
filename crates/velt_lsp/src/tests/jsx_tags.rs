//! Closing tags in the editor (definition, references, rename on `</Card>`), completion after
//! `</` (the innermost open element first), hyphenated attribute names, components held in
//! constants, and positions after non-ASCII text.

use serde_json::{json, Value};

use super::client::{at, pos_of, uri, Client};
use super::jsx::{labels, open};

const PAGE: &str = r#"// @jsxImportSource ./jsx_ui
import { Card } from "./jsx_cards";
import * as ui from "./jsx_cards";

function heading(): JSX.Element {
  return <h1>Heading</h1>;
}

function page(): JSX.Element {
  const Badge = (props: { text: string }): JSX.Element => <h1>{props.text}</h1>;
  const Title = "t";
  return <div id="main"><Card title="b"></Card><ui.Card title="c"></ui.Card><Badge text={Title} /><a href="/x">Grüße 😀 x</a></div>;
}
"#;

fn completion(text: &str, needle: &str) -> Value {
    let (mut client, doc) = open(text);
    let (line, col) = pos_of(text, needle, needle.len());
    let items = client.request("textDocument/completion", at(&doc, line, col));
    client.shutdown();
    items
}

#[test]
fn closing_tags_name_their_component() {
    let (mut client, doc) = open(PAGE);
    let (line, col) = pos_of(PAGE, "</Card>", 3);
    let loc = client.request("textDocument/definition", at(&doc, line, col));
    assert!(
        loc["uri"]
            .as_str()
            .unwrap_or_default()
            .ends_with("jsx_cards.vlt"),
        "{loc}"
    );
    let hover = client.request("textDocument/hover", at(&doc, line, col));
    let text = hover["contents"]["value"].as_str().unwrap_or_default();
    assert!(text.contains("function Card("), "{text}");
    // Rename edits both tags (and the import, and the declaration in the other module).
    let mut params = at(&doc, line, col);
    params["newName"] = json!("Panel");
    let edit = client.request("textDocument/rename", params);
    let edits = edit["changes"][doc.as_str()].as_array().unwrap();
    let cols: Vec<(u64, u64)> = edits
        .iter()
        .map(|e| {
            let start = &e["range"]["start"];
            (
                start["line"].as_u64().unwrap(),
                start["character"].as_u64().unwrap(),
            )
        })
        .collect();
    let (open_line, open_col) = pos_of(PAGE, "<Card title", 1);
    assert!(
        cols.contains(&(open_line as u64, open_col as u64)),
        "{cols:?}"
    );
    assert!(
        cols.contains(&(line as u64, (col - 3 + 2) as u64)),
        "{cols:?}"
    );
    client.shutdown();
}

#[test]
fn closing_tags_of_namespaced_components() {
    let (mut client, doc) = open(PAGE);
    let (line, col) = pos_of(PAGE, "</ui.Card>", 6);
    let loc = client.request("textDocument/definition", at(&doc, line, col));
    assert!(
        loc["uri"]
            .as_str()
            .unwrap_or_default()
            .ends_with("jsx_cards.vlt"),
        "{loc}"
    );
    let mut params = at(&doc, line, col);
    params["newName"] = json!("Panel");
    let edit = client.request("textDocument/rename", params);
    let starts: Vec<(u64, u64)> = edit["changes"][doc.as_str()]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            let start = &e["range"]["start"];
            (
                start["line"].as_u64().unwrap(),
                start["character"].as_u64().unwrap(),
            )
        })
        .collect();
    let (open_line, open_col) = pos_of(PAGE, "<ui.Card title", 4);
    assert!(
        starts.contains(&(open_line as u64, open_col as u64)),
        "{starts:?}"
    );
    assert!(
        starts.contains(&(line as u64, (col - 1) as u64)),
        "{starts:?}"
    );
    client.shutdown();
}

#[test]
fn after_a_closing_slash_the_open_element_comes_first() {
    let text = PAGE.replace("<a href=\"/x\">Grüße 😀 x</a>", "<p><b>x</b></");
    let items = completion(&text, "<b>x</b></");
    let first = &items.as_array().unwrap()[0];
    assert_eq!(first["label"], json!("p"));
    assert_eq!(first["preselect"], json!(true));
}

#[test]
fn components_in_constants_are_offered_by_type() {
    let text = PAGE.replace("<a href=\"/x\">Grüße 😀 x</a>", "<Ba");
    let items = labels(&completion(&text, "<Ba"));
    assert!(items.contains(&"Badge".to_string()), "{items:?}");
    assert!(!items.contains(&"Title".to_string()), "{items:?}");
}

#[test]
fn hyphenated_attribute_names_are_replaced_whole() {
    let text = PAGE.replace("<a href=\"/x\">Grüße 😀 x</a>", "<a aria-l");
    let items = completion(&text, "<a aria-l");
    let (line, col) = pos_of(&text, "aria-l", 0);
    for item in items.as_array().unwrap() {
        let start = &item["textEdit"]["range"]["start"];
        assert_eq!(
            (start["line"].as_u64(), start["character"].as_u64()),
            (Some(line as u64), Some(col as u64)),
            "{item}"
        );
    }
}

#[test]
fn positions_after_non_ascii_text_count_utf16_units() {
    let text = PAGE.replace("Grüße 😀 x</a>", "Grüße 😀 x</a> Grüße 😀 <a ");
    let items = labels(&completion(&text, "Grüße 😀 <a "));
    assert_eq!(items, ["href", "title"]);
    let (mut client, doc) = open(PAGE);
    // `href` after `😀` on the same line: the hover range starts at its UTF-16 column.
    let (line, col) = pos_of(PAGE, "href=", 1);
    let hover = client.request("textDocument/hover", at(&doc, line, col));
    assert_eq!(hover["range"]["start"]["character"], json!(col - 1));
    let text = hover["contents"]["value"].as_str().unwrap_or_default();
    assert!(text.contains("href"), "{text}");
    client.shutdown();
}

#[test]
fn the_page_compiles() {
    let mut client = Client::start();
    client.open(&uri("jsx_ui/jsx-runtime.vlt"), super::jsx::RUNTIME);
    client.open(&uri("jsx_cards.vlt"), super::jsx::CARDS);
    let doc = uri("jsx_tags_page.vlt");
    client.open(&doc, PAGE);
    assert_eq!(client.diagnostics(&doc)["diagnostics"], json!([]));
    client.shutdown();
}
