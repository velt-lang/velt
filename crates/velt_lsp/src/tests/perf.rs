//! The `perf/…` rules on the syntax tree and types in the editor: hints by default, at the
//! package's `lint` levels, with their fixes as quick fixes (preferred only when they keep the
//! behaviour), only fixes that stay TypeScript in `tsCompat` folders, and edits in other files
//! when a fix rewrites the uses of a type.

use lsp_types::Url;
use serde_json::{json, Value};

use super::client::Client;
use super::quick_fixes::{actions, apply, find};

/// A package on disk with `fields` in its manifest object and a `shared` folder.
fn package(fields: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = format!(
        "import type {{ Package }} from \"velt/package\";\n\n\
         export const pkg: Package = {{ name: \"app\", version: \"0.1.0\"{fields} }};\n"
    );
    std::fs::write(tmp.path().join("package.velt"), manifest).unwrap();
    std::fs::create_dir(tmp.path().join("shared")).unwrap();
    tmp
}

fn uri_in(tmp: &tempfile::TempDir, rel: &str) -> Url {
    Url::from_file_path(tmp.path().join(rel)).unwrap()
}

fn published(client: &mut Client, doc: &Url) -> Vec<Value> {
    client.diagnostics(doc)["diagnostics"]
        .as_array()
        .unwrap()
        .clone()
}

/// The `(source, code, severity)` of each diagnostic.
fn kinds(diags: &[Value]) -> Vec<(String, String, u64)> {
    diags
        .iter()
        .map(|d| {
            (
                d["source"].as_str().unwrap_or("-").to_string(),
                d["code"].as_str().unwrap_or("-").to_string(),
                d["severity"].as_u64().unwrap_or(0),
            )
        })
        .collect()
}

fn perf(rule: &str, severity: u64) -> (String, String, u64) {
    ("velt perf".to_string(), rule.to_string(), severity)
}

const PAIR: &str = "class Pair {\n  a: number;\n  b: string = \"x\";\n  constructor(a: number) {\n    this.a = a;\n  }\n}\n\nexport function make(): Pair {\n  return new Pair(1);\n}\n";

#[test]
fn a_hint_whose_fix_may_change_the_behaviour_is_not_preferred() {
    let tmp = package("");
    let mut client = Client::start();
    let doc = uri_in(&tmp, "pair.velt");
    client.open(&doc, PAIR);
    let diags = published(&mut client, &doc);
    assert_eq!(kinds(&diags), [perf("class-as-data", 4)]);
    let offered = actions(&mut client, &doc, PAIR, "Pair", &diags);
    let fix = find(&offered, "make `Pair` an object type");
    // Copies instead of shares: offered, not preferred, so "fix all" leaves it.
    assert_eq!(fix["isPreferred"], Value::Null);
    let fixed = apply(PAIR, fix, &doc);
    assert_eq!(
        fixed,
        "type Pair = { a: number; b: string };\n\nexport function make(): Pair {\n  return { a: 1, b: \"x\" } as Pair;\n}\n"
    );
    client.change(&doc, 2, &fixed);
    assert_eq!(published(&mut client, &doc), [] as [Value; 0]);
    client.shutdown();
}

#[test]
fn levels_and_allow_comments() {
    let tmp = package(", lint: { perf: \"warn\", rules: { \"perf/dynamic-record\": \"off\" } }");
    let mut client = Client::start();
    let doc = uri_in(&tmp, "pair.velt");
    let text = PAIR;
    client.open(&doc, text);
    assert_eq!(
        kinds(&published(&mut client, &doc)),
        [perf("class-as-data", 2)]
    );
    let allowed = format!("// velt-allow(perf/class-as-data): shared on purpose\n{PAIR}");
    client.change(&doc, 2, &allowed);
    assert_eq!(published(&mut client, &doc), [] as [Value; 0]);
    client.shutdown();
}

/// An exported class may be used by files the document's analysis does not load (those that
/// import it): the editor reports it without the fix, which `velt check` of the package makes
/// with every file. A class used only in its file gets the fix, also in a `tsCompat` folder,
/// since it stays TypeScript.
#[test]
fn exported_classes_are_fixed_by_the_package_check() {
    let tmp = package(", tsCompat: [\"shared\"]");
    let mut client = Client::start();
    let doc = uri_in(&tmp, "shared/point.velt");
    let model = "export class Point {\n  x: number;\n  y: number;\n  constructor(x: number, y: number) {\n    this.x = x;\n    this.y = y;\n  }\n}\n";
    client.open(&doc, model);
    let diags = published(&mut client, &doc);
    assert_eq!(kinds(&diags), [perf("class-as-data", 4)]);
    let message = diags[0]["message"].as_str().unwrap();
    assert!(
        message.contains("in the package rewrites it with every file"),
        "{message}"
    );
    let offered = actions(&mut client, &doc, model, "Point", &diags);
    assert!(offered
        .iter()
        .all(|a| a["title"] != json!("make `Point` an object type")));
    let shared = uri_in(&tmp, "shared/pair.velt");
    client.open(&shared, PAIR);
    let diags = published(&mut client, &shared);
    let offered = actions(&mut client, &shared, PAIR, "Pair", &diags);
    let fixed = apply(PAIR, find(&offered, "make `Pair` an object type"), &shared);
    assert!(
        fixed.starts_with("type Pair = { a: number; b: string };"),
        "{fixed}"
    );
    client.shutdown();
}
