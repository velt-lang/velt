//! TypeScript-compatibility findings for documents in a package's `tsCompat` folders: published
//! with the compiler's diagnostics, live on change, only inside the folders, with each finding's
//! fix as a quick fix.

use lsp_types::Url;
use serde_json::{json, Value};

use super::client::Client;
use super::quick_fixes::{actions, apply, find};

/// A package on disk whose `tsCompat` lists `shared`, which exists.
fn package() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("package.vlt"), manifest("[\"shared\"]")).unwrap();
    std::fs::create_dir(tmp.path().join("shared")).unwrap();
    tmp
}

fn manifest(ts_compat: &str) -> String {
    format!(
        "import type {{ Package }} from \"velt:package\";\n\n\
         export const pkg: Package = {{ name: \"app\", version: \"0.1.0\", tsCompat: {ts_compat} }};\n"
    )
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

/// The `(source, code)` of each diagnostic.
fn sources(diags: &[Value]) -> Vec<(String, String)> {
    diags
        .iter()
        .map(|d| {
            (
                d["source"].as_str().unwrap_or("-").to_string(),
                d["code"].as_str().unwrap_or("-").to_string(),
            )
        })
        .collect()
}

fn ts_compat(code: &str) -> (String, String) {
    ("velt ts-compat".to_string(), code.to_string())
}

#[test]
fn findings_in_the_folders_come_live_with_their_fix() {
    let tmp = package();
    let mut client = Client::start();
    let text = "export const x: f64 = 1.5;\n";
    let doc = uri_in(&tmp, "shared/model.vlt");
    client.open(&doc, text);
    let diags = published(&mut client, &doc);
    assert_eq!(sources(&diags), [ts_compat("velt-number-type")]);
    let d = &diags[0];
    assert_eq!(d["severity"], json!(1));
    assert_eq!(
        d["range"],
        json!({ "start": { "line": 0, "character": 16 }, "end": { "line": 0, "character": 19 } })
    );
    let message = d["message"].as_str().unwrap();
    assert!(
        message.starts_with("`f64` is not a TypeScript type\nnote: "),
        "{message}"
    );

    // The fix: a preferred quick fix tied to the finding.
    let offered = actions(&mut client, &doc, text, "f64", &diags);
    let fix = find(&offered, "replace with `number`");
    assert_eq!(fix["kind"], json!("quickfix"));
    assert_eq!(fix["isPreferred"], json!(true));
    assert_eq!(fix["diagnostics"], json!(diags));
    let fixed = apply(text, fix, &doc);
    assert_eq!(fixed, "export const x: number = 1.5;\n");
    client.change(&doc, 2, &fixed);
    assert_eq!(published(&mut client, &doc), [] as [Value; 0]);

    // Live: a new finding on the next change.
    client.change(&doc, 3, "export const ok: bool = true;\n");
    assert_eq!(
        sources(&published(&mut client, &doc)),
        [ts_compat("bool-type")]
    );
    client.shutdown();
}

#[test]
fn files_outside_the_folders_and_files_with_errors_get_no_findings() {
    let tmp = package();
    let mut client = Client::start();
    let outside = uri_in(&tmp, "server.vlt");
    client.open(&outside, "export const x: f64 = 1.5;\n");
    assert_eq!(published(&mut client, &outside), [] as [Value; 0]);
    // A folder whose name starts like a listed one is another folder.
    let sibling = uri_in(&tmp, "shared2/model.vlt");
    client.open(&sibling, "export const x: f64 = 1.5;\n");
    assert_eq!(published(&mut client, &sibling), [] as [Value; 0]);
    // The rules see only valid Velt: the compiler's error, no finding.
    let broken = uri_in(&tmp, "shared/broken.vlt");
    client.open(&broken, "export const x: f64 = \"a\";\n");
    let diags = published(&mut client, &broken);
    assert!(!diags.is_empty());
    assert!(
        sources(&diags).iter().all(|(source, _)| source == "velt"),
        "{diags:?}"
    );
    client.shutdown();
}

#[test]
fn an_import_leaving_the_folders_is_a_finding() {
    let tmp = package();
    std::fs::write(
        tmp.path().join("util.vlt"),
        "export function twice(x: number): number {\n  return x * 2;\n}\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("shared/inside.vlt"),
        "export const one: number = 1;\n",
    )
    .unwrap();
    let mut client = Client::start();
    let doc = uri_in(&tmp, "shared/model.vlt");
    let text = "import { twice } from \"./../util\";\nimport { one } from \"./inside\";\n\
                export function two(): number {\n  return twice(one);\n}\n";
    client.open(&doc, text);
    let diags = published(&mut client, &doc);
    assert_eq!(sources(&diags), [ts_compat("outside-import")], "{diags:?}");
    client.shutdown();
}

#[test]
fn the_open_manifest_decides_the_folders() {
    let tmp = package();
    let mut client = Client::start();
    let doc = uri_in(&tmp, "shared/model.vlt");
    client.open(&doc, "export const x: f64 = 1.5;\n");
    assert_eq!(
        sources(&published(&mut client, &doc)),
        [ts_compat("velt-number-type")]
    );
    // Unsaved edits to `tsCompat` apply; a folder that isn't there is a warning in the manifest.
    let pkg = uri_in(&tmp, "package.vlt");
    let text = manifest("[\"models\"]");
    client.open(&pkg, &text);
    let warnings = published(&mut client, &pkg);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0]["severity"], json!(2));
    assert!(
        warnings[0]["message"]
            .as_str()
            .unwrap()
            .starts_with("`tsCompat` folder `models` does not exist"),
        "{warnings:?}"
    );
    client.change(&doc, 2, "export const x: f64 = 2.5;\n");
    assert_eq!(published(&mut client, &doc), [] as [Value; 0]);
    client.shutdown();
}
