//! Code actions: each quick fix is requested at its diagnostic, applied to the text, and the result
//! is checked (and re-analyzed where it must compile cleanly).

use lsp_types::{Position, Url};
use serde_json::{json, Value};

use super::client::{pos_of, uri, Client};
use crate::line_index::LineIndex;

/// Open `text` as `name`; returns the client, the URI and the published diagnostics.
fn open(name: &str, text: &str) -> (Client, Url, Vec<Value>) {
    let mut client = Client::start();
    let doc = uri(name);
    client.open(&doc, text);
    let diags = client.diagnostics(&doc)["diagnostics"]
        .as_array()
        .unwrap()
        .clone();
    (client, doc, diags)
}

/// Code actions for the empty range at the first `needle` of `text`.
fn actions(
    client: &mut Client,
    doc: &Url,
    text: &str,
    needle: &str,
    diags: &[Value],
) -> Vec<Value> {
    let (line, character) = pos_of(text, needle, 0);
    let pos = json!({ "line": line, "character": character });
    let params = json!({
        "textDocument": { "uri": doc },
        "range": { "start": pos, "end": pos },
        "context": { "diagnostics": diags },
    });
    let result = client.request("textDocument/codeAction", params);
    result.as_array().cloned().unwrap_or_default()
}

/// The action titled `title` (panics listing the offered ones otherwise).
fn find<'a>(actions: &'a [Value], title: &str) -> &'a Value {
    actions
        .iter()
        .find(|a| a["title"] == json!(title))
        .unwrap_or_else(|| panic!("no `{title}` among {actions:#?}"))
}

/// `text` with the action's edits for `doc` applied.
fn apply(text: &str, action: &Value, doc: &Url) -> String {
    let edits = action["edit"]["changes"][doc.as_str()].as_array().unwrap();
    let index = LineIndex::new(text);
    let offset = |p: &Value| {
        let pos: Position = serde_json::from_value(p.clone()).unwrap();
        index.offset(pos) as usize
    };
    let mut spans: Vec<(usize, usize, &str)> = edits
        .iter()
        .map(|e| {
            let r = &e["range"];
            (
                offset(&r["start"]),
                offset(&r["end"]),
                e["newText"].as_str().unwrap(),
            )
        })
        .collect();
    spans.sort_by_key(|(lo, _, _)| std::cmp::Reverse(*lo));
    let mut out = text.to_string();
    for (lo, hi, new) in spans {
        out.replace_range(lo..hi, new);
    }
    out
}

/// Errors after changing the document to `text`.
fn errors_after(client: &mut Client, doc: &Url, text: &str) -> Vec<Value> {
    client.change(doc, 2, text);
    client.diagnostics(doc)["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["severity"] == json!(1))
        .cloned()
        .collect()
}

#[test]
fn removes_mut_and_links_the_diagnostic() {
    let text =
        "function add(mut a: i64): i64 {\n  return a;\n}\n\nfunction main() {\n  add(1);\n}\n";
    let (mut client, doc, diags) = open("fix_mut.vlt", text);
    assert_eq!(diags.len(), 1, "{diags:?}");
    let offered = actions(&mut client, &doc, text, "mut", &diags);
    let fix = find(&offered, "Remove `mut`");
    assert_eq!(fix["kind"], json!("quickfix"));
    assert_eq!(fix["isPreferred"], json!(true));
    assert_eq!(fix["diagnostics"], json!(diags));
    let fixed = apply(text, fix, &doc);
    assert!(fixed.starts_with("function add(a: i64)"), "{fixed}");
    assert_eq!(errors_after(&mut client, &doc, &fixed), [] as [Value; 0]);
    client.shutdown();
}

#[test]
fn replaces_undefined_with_null() {
    let text = "function main() {\n  let u: string | null = undefined;\n  console.log(u);\n}\n";
    let (mut client, doc, diags) = open("fix_undefined.vlt", text);
    let offered = actions(&mut client, &doc, text, "undefined", &diags);
    let fixed = apply(
        text,
        find(&offered, "Replace `undefined` with `null`"),
        &doc,
    );
    assert!(fixed.contains("= null;"), "{fixed}");
    assert_eq!(errors_after(&mut client, &doc, &fixed), [] as [Value; 0]);
    client.shutdown();
}

#[test]
fn replaces_void_zero_and_an_undefined_type_with_null() {
    let text = "function main() {\n  let u: string | undefined = void 0;\n  console.log(u);\n}\n";
    let (mut client, doc, diags) = open("fix_void.vlt", text);
    let offered = actions(&mut client, &doc, text, "void 0", &diags);
    let fixed = apply(text, find(&offered, "Replace `void 0` with `null`"), &doc);
    let offered = actions(&mut client, &doc, text, "undefined", &diags);
    let fixed = apply(
        &fixed,
        find(&offered, "Replace `undefined` with `null`"),
        &doc,
    );
    assert!(fixed.contains("string | null = null;"), "{fixed}");
    assert_eq!(errors_after(&mut client, &doc, &fixed), [] as [Value; 0]);
    client.shutdown();
}

#[test]
fn makes_a_promise_returning_override_async() {
    let text = "class Machine {
  async run(): Promise<i64> {
    throw new Error(\"jammed\");
  }
}

class Idle extends Machine {
  override run(): Promise<i64> {
    return new Promise<i64>((resolve, reject) => resolve(0));
  }
}

async function main() {
  const m: Machine = new Idle();
  try {
    console.log(await m.run());
  } catch (e) {
    console.log(e.message);
  }
}
";
    let (mut client, doc, diags) = open("fix_async.vlt", text);
    let offered = actions(
        &mut client,
        &doc,
        text,
        "run(): Promise<i64> {
    return",
        &diags,
    );
    let fixed = apply(text, find(&offered, "Add `async`"), &doc);
    assert!(
        fixed.contains("override async run(): Promise<i64>"),
        "{fixed}"
    );
    // The body may need adapting to `async`; the rule itself is satisfied.
    let errors = errors_after(&mut client, &doc, &fixed);
    assert!(
        !errors
            .iter()
            .any(|e| e["message"].as_str().unwrap().contains("must be `async`")),
        "{errors:?}"
    );
    client.shutdown();
}

#[test]
fn converts_string_concatenation_to_a_template_literal() {
    let text = "function main() {\n  const n = 3;\n  const s = 1 + n + \"a`\" + n + \"!\";\n  console.log(s);\n}\n";
    let (mut client, doc, diags) = open("fix_concat.vlt", text);
    let offered = actions(&mut client, &doc, text, "\"a", &diags);
    let fixed = apply(text, find(&offered, "Convert to a template literal"), &doc);
    assert!(fixed.contains("const s = `${1 + n}a\\`${n}!`;"), "{fixed}");
    assert_eq!(errors_after(&mut client, &doc, &fixed), [] as [Value; 0]);
    client.shutdown();
}

#[test]
fn makes_non_bool_conditions_explicit() {
    let text = "function a(count: i64) {\n  if (count) {\n    console.log(\"n\");\n  }\n}\n\nfunction b(name: string) {\n  while (name) {\n    break;\n  }\n}\n\nfunction c(user: string | null) {\n  if (user) {\n    console.log(\"u\");\n  }\n}\n\nfunction d(x: f64) {\n  if (x + 1.0) {\n    console.log(\"x\");\n  }\n}\n\nfunction e(n: i64): bool {\n  return !n;\n}\n\nfunction main() {\n  a(1);\n  b(\"\");\n  c(null);\n  d(0.0);\n  console.log(e(0));\n}\n";
    let (mut client, doc, diags) = open("fix_cond.vlt", text);
    let cases = [
        ("(count)", "Compare with `0`", "(count !== 0)"),
        ("(name)", "Compare with `\"\"`", "(name !== \"\")"),
        ("(user)", "Compare with `null`", "(user !== null)"),
        ("(x + 1.0)", "Compare with `0.0`", "(x + 1.0 !== 0.0)"),
        (" !n", "Compare with `0`", " n === 0"),
    ];
    let mut all_fixed = text.to_string();
    for (needle, title, expected) in cases {
        let offered = actions(&mut client, &doc, text, &needle[1..], &diags);
        let one = apply(text, find(&offered, title), &doc);
        assert!(one.contains(expected), "{one}");
        all_fixed = all_fixed.replace(needle, expected);
    }
    assert_eq!(
        errors_after(&mut client, &doc, &all_fixed),
        [] as [Value; 0],
        "{all_fixed}"
    );
    client.shutdown();
}

#[test]
fn floating_promise_can_be_awaited_or_spawned() {
    let text = "async function load(): Promise<i64> {\n  return 1;\n}\n\nasync function main() {\n  load();\n}\n";
    let (mut client, doc, _) = open("fix_promise.vlt", text);
    let offered = actions(&mut client, &doc, text, "load();", &[]);
    let awaited = apply(text, find(&offered, "Add `await`"), &doc);
    assert!(awaited.contains("  await load();"), "{awaited}");
    let spawned = apply(
        text,
        find(&offered, "Run it in the background with `spawn(...)`"),
        &doc,
    );
    assert!(spawned.contains("  spawn(load());"), "{spawned}");
    // Both resolve the compiler's "floating promise" error.
    for fixed in [&awaited, &spawned] {
        assert_eq!(
            errors_after(&mut client, &doc, fixed),
            [] as [Value; 0],
            "{fixed}"
        );
    }
    // Not offered on a statement that is not a promise.
    let none = actions(&mut client, &doc, text, "return 1", &[]);
    assert_eq!(none, [] as [Value; 0]);
    client.shutdown();
}

#[test]
fn export_default_becomes_a_named_export() {
    let text = "export default function main() {}\n";
    let (mut client, doc, diags) = open("fix_default.vlt", text);
    let offered = actions(&mut client, &doc, text, "default", &diags);
    let fixed = apply(
        text,
        find(&offered, "Remove `default` (use a named export)"),
        &doc,
    );
    assert_eq!(fixed, "export function main() {}\n");
    assert_eq!(errors_after(&mut client, &doc, &fixed), [] as [Value; 0]);

    let text = "const LIMIT = 3;\nexport default LIMIT;\n\nfunction main() {}\n";
    client.change(&doc, 3, text);
    let diags = client.diagnostics(&doc)["diagnostics"]
        .as_array()
        .unwrap()
        .clone();
    let offered = actions(&mut client, &doc, text, "default", &diags);
    let fixed = apply(text, find(&offered, "Export `LIMIT` by name"), &doc);
    assert!(fixed.contains("export { LIMIT };"), "{fixed}");
    client.shutdown();
}
