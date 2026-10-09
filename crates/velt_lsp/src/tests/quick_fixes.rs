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
pub(super) fn actions(
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
pub(super) fn find<'a>(actions: &'a [Value], title: &str) -> &'a Value {
    actions
        .iter()
        .find(|a| a["title"] == json!(title))
        .unwrap_or_else(|| panic!("no `{title}` among {actions:#?}"))
}

/// `text` with the action's edits for `doc` applied.
pub(super) fn apply(text: &str, action: &Value, doc: &Url) -> String {
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
    let text = "class Jammed {}

class Machine {
  async run(): Promise<i64> {
    throw new Jammed();
  }
}

class Idle extends Machine {
  override run(): Promise<i64> {
    throw new Jammed();
  }
}

async function main() {
  const m: Machine = new Idle();
  try {
    console.log(await m.run());
  } catch (e) {
    console.log(\"jammed\");
  }
}
";
    let (mut client, doc, diags) = open("fix_async.vlt", text);
    let offered = actions(
        &mut client,
        &doc,
        text,
        "run(): Promise<i64> {
    throw new Jammed();
  }
}

async",
        &diags,
    );
    let fixed = apply(text, find(&offered, "Add `async`"), &doc);
    assert!(
        fixed.contains("override async run(): Promise<i64>"),
        "{fixed}"
    );
    assert_eq!(errors_after(&mut client, &doc, &fixed), [] as [Value; 0]);
    client.shutdown();
}

#[test]
fn no_async_fix_for_a_getter() {
    let text = "class Jammed {}

interface Reader {
  get ready(): Promise<bool>;
}

class R implements Reader {
  get ready(): Promise<bool> {
    throw new Jammed();
  }
}

async function main() {
  const r: Reader = new R();
  console.log(await r.ready);
}
";
    let (mut client, doc, diags) = open("fix_async_getter.vlt", text);
    assert_eq!(diags.len(), 1, "{diags:?}");
    let offered = actions(&mut client, &doc, text, "ready(): Promise<bool> {", &diags);
    assert!(
        !offered.iter().any(|a| a["title"] == json!("Add `async`")),
        "{offered:#?}"
    );
    client.shutdown();
}

#[test]
fn converts_string_concatenation_to_a_template_literal() {
    // A number next to a string concatenates as in JS (#740); an array is still an error.
    let text = "function main() {\n  const n = 3;\n  const xs = [1];\n  const s = 1 + n + \"a`\" + xs + \"!\";\n  console.log(s);\n}\n";
    let (mut client, doc, diags) = open("fix_concat.vlt", text);
    let offered = actions(&mut client, &doc, text, "xs +", &diags);
    let fixed = apply(text, find(&offered, "Convert to a template literal"), &doc);
    assert!(fixed.contains("const s = `${1 + n}a\\`${xs}!`;"), "{fixed}");
    assert_eq!(errors_after(&mut client, &doc, &fixed), [] as [Value; 0]);
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
fn no_spawn_fix_for_a_scope_spawn() {
    // A stand-in for `velt:task`'s `TaskScope` (the test loader has no standard library).
    let text = "class TaskScope {
  spawn(p: Promise<i64>): Promise<i64> {
    return p;
  }
}

async function work(): Promise<i64> {
  return 1;
}

async function main() {
  const scope = new TaskScope();
  scope.spawn(work());
}
";
    let (mut client, doc, diags) = open("fix_scope_spawn.vlt", text);
    let offered = actions(&mut client, &doc, text, "scope.spawn(work())", &diags);
    find(&offered, "Add `await`");
    assert!(
        !offered
            .iter()
            .any(|a| a["title"] == json!("Run it in the background with `spawn(...)`")),
        "{offered:#?}"
    );
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
