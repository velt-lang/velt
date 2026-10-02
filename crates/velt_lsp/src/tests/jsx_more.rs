//! More JSX in the editor: components from other modules (by name and through a namespace
//! import), intrinsic tags, completion in JSX text, the AST fallback inside spreads and
//! fragments, and a server that keeps answering on incomplete JSX.

use serde_json::{json, Value};

use super::client::{at, pos_of, uri, Client};
use super::jsx::{complete, open, RUNTIME};

const PAGE: &str = r#"// @jsxImportSource ./jsx_ui
import * as ui from "./jsx_cards";
import { Card } from "./jsx_cards";

function title(): JSX.Element {
  return <h1>Title</h1>;
}

function page(): JSX.Element {
  return <div id="main"><ui.Card title="a" /><Card title="b" /></div>;
}
"#;

#[test]
fn the_page_compiles() {
    let mut client = Client::start();
    client.open(&uri("jsx_ui/jsx-runtime.vlt"), RUNTIME);
    client.open(&uri("jsx_cards.vlt"), super::jsx::CARDS);
    let doc = uri("jsx_page.vlt");
    client.open(&doc, PAGE);
    assert_eq!(client.diagnostics(&doc)["diagnostics"], json!([]));
    client.shutdown();
}

#[test]
fn props_of_components_from_other_modules() {
    let text = PAGE.replace("<ui.Card title=\"a\" />", "<ui.Card ");
    assert_eq!(complete(&text, "<ui.Card "), ["title", "tone"]);
    let text = PAGE.replace("<Card title=\"b\" />", "<Card title=\"x\" ");
    assert_eq!(complete(&text, "<Card title=\"x\" "), ["tone"]);
}

#[test]
fn completion_in_jsx_text() {
    let text = PAGE.replace("<Card title=\"b\" />", "Read the <");
    let items = complete(&text, "Read the <");
    assert!(items.contains(&"div".to_string()), "{items:?}");
    let text = PAGE.replace("<Card title=\"b\" />", "Read the <a ");
    assert_eq!(complete(&text, "Read the <a "), ["href", "title"]);
    let text = PAGE.replace("<Card title=\"b\" />", "hello</");
    let items = complete(&text, "hello</");
    assert!(items.contains(&"div".to_string()), "{items:?}");
}

#[test]
fn definition_and_hover_on_namespaced_components_and_intrinsic_tags() {
    let (mut client, doc) = open(PAGE);
    let definition = |client: &mut Client, needle: &str, delta: usize| {
        let (line, col) = pos_of(PAGE, needle, delta);
        let loc = client.request("textDocument/definition", at(&doc, line, col));
        let file = loc["uri"].as_str().unwrap_or_default();
        let file = file.rsplit('/').next().unwrap_or_default().to_string();
        (file, loc["range"]["start"]["line"].as_u64())
    };
    assert_eq!(
        definition(&mut client, "ui.Card", 4),
        ("jsx_cards.vlt".to_string(), Some(4))
    );
    let (file, _) = definition(&mut client, "<div", 2);
    assert_eq!(file, "jsx-runtime.vlt");
    let (line, col) = pos_of(PAGE, "<div", 2);
    let hover = client.request("textDocument/hover", at(&doc, line, col));
    let text = hover["contents"]["value"].as_str().unwrap_or_default();
    assert!(
        text.contains("div: { class: string | null; id: string | null }"),
        "{text}"
    );
    client.shutdown();
}

#[test]
fn the_ast_fallback_sees_spreads_and_fragments() {
    use crate::index::scope::{self, Reference};
    let dir = std::env::temp_dir().join("velt_lsp_tests");
    let path = dir.join("jsx_walk_spread.vlt");
    let text = "// @jsxImportSource ./jsx_ui\nfunction page(rest: { id: string }, items: string[]): JSX.Element {\n  return <><div {...rest}>{...items}</div></>;\n}\n";
    let overlay = std::collections::HashMap::from([
        (path.clone(), text.to_string()),
        (dir.join("jsx_ui/jsx-runtime.vlt"), RUNTIME.to_string()),
    ]);
    let analysis = crate::analysis::analyze(&super::loader::TestLoader, &path, &overlay);
    for needle in ["rest}", "items}"] {
        let offset = text.find(needle).unwrap() as u32 + 1;
        let info = scope::at_offset(&analysis, offset);
        let name = &needle[..needle.len() - 1];
        assert!(
            matches!(&info.reference, Some(Reference::Name { local: Some(l), .. }) if l.name == name),
            "{needle}"
        );
    }
}

#[test]
fn the_server_keeps_answering_on_incomplete_jsx() {
    let prefix = "// @jsxImportSource ./jsx_ui\nfunction ok(): JSX.Element {\n  return <p>x</p>;\n}\nfunction page(): JSX.Element {\n  return ";
    for tail in [
        "<",
        "<Card title={",
        "<div><a href=\"",
        "<a href={() => \"}\"} ",
    ] {
        let text = format!("{prefix}{tail}");
        let (mut client, doc) = open(&text);
        let end = pos_of(&text, tail, tail.len());
        for method in [
            "textDocument/completion",
            "textDocument/hover",
            "textDocument/definition",
        ] {
            let result: Value = client.request(method, at(&doc, end.0, end.1));
            let _ = result;
        }
        client.shutdown();
    }
}

#[test]
fn the_less_than_trigger_only_completes_tags() {
    // After a closed block, `<` still compares.
    let text = "function main() {\n  const a = 1;\n  if (a > 0) {\n    console.log(a);\n  }\n  if (a <\n}\n";
    let mut client = Client::start();
    let doc = uri("jsx_trigger.vlt");
    client.open(&doc, text);
    client.diagnostics(&doc);
    let (line, col) = pos_of(text, "a <", 3);
    let mut params = at(&doc, line, col);
    params["context"] = json!({ "triggerKind": 2, "triggerCharacter": "<" });
    let items = client.request("textDocument/completion", params);
    assert_eq!(items, json!([]));
    client.shutdown();
}
