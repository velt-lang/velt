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

/// The rules on types run on the document's analysis too: an error with its fix, then a warning
/// on the next change.
#[test]
fn typed_findings_come_live() {
    let tmp = package();
    let mut client = Client::start();
    let text = "export function missing(x?: string): boolean {\n  return x === null;\n}\n";
    let doc = uri_in(&tmp, "shared/lookup.ts");
    client.open(&doc, text);
    let diags = published(&mut client, &doc);
    assert_eq!(sources(&diags), [ts_compat("strict-null-eq")]);
    let offered = actions(&mut client, &doc, text, "===", &diags);
    let fixed = apply(
        text,
        find(&offered, "compare with `==`, which matches `undefined` too"),
        &doc,
    );
    assert!(fixed.contains("return x == null;"), "{fixed}");
    client.change(&doc, 2, &fixed);
    assert_eq!(published(&mut client, &doc), [] as [Value; 0]);

    client.change(
        &doc,
        3,
        "export type U = { nick?: string };\nexport function label(u: U): string {\n  return `${u.nick}`;\n}\n",
    );
    let diags = published(&mut client, &doc);
    assert_eq!(sources(&diags), [ts_compat("nullable-in-template")]);
    assert_eq!(diags[0]["severity"], json!(2));
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
    // The CLI's walk skips these directories under the folders, so they are outside too.
    for rel in [
        "shared/node_modules/m.vlt",
        "shared/target/m.vlt",
        "shared/.cache/m.vlt",
    ] {
        let skipped = uri_in(&tmp, rel);
        client.open(&skipped, "export const x: f64 = 1.5;\n");
        assert_eq!(published(&mut client, &skipped), [] as [Value; 0], "{rel}");
    }
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
    // Under the folder, but in a directory `velt check --ts-compat` doesn't walk.
    std::fs::create_dir(tmp.path().join("shared/node_modules")).unwrap();
    std::fs::write(
        tmp.path().join("shared/node_modules/dep.vlt"),
        "export const three: number = 3;\n",
    )
    .unwrap();
    let mut client = Client::start();
    let doc = uri_in(&tmp, "shared/model.vlt");
    let text = "import { twice } from \"./../util\";\nimport { one } from \"./inside\";\n\
                import { three } from \"./node_modules/dep\";\n\
                export function two(): number {\n  return twice(one) + three;\n}\n";
    client.open(&doc, text);
    let diags = published(&mut client, &doc);
    assert_eq!(
        sources(&diags),
        [ts_compat("outside-import"), ts_compat("outside-import")],
        "{diags:?}"
    );
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

/// Wait for `doc`'s diagnostics until their `(source, code)`s are `want` (earlier publishes may
/// still show the previous state); fails on the client's timeout.
fn published_until(client: &mut Client, doc: &Url, want: &[(String, String)]) -> Vec<Value> {
    loop {
        let diags = published(client, doc);
        if sources(&diags) == want {
            return diags;
        }
    }
}

#[test]
fn closing_an_edited_manifest_restores_the_saved_folders() {
    let tmp = package();
    let mut client = Client::start();
    let doc = uri_in(&tmp, "shared/model.vlt");
    client.open(&doc, "export const x: f64 = 1.5;\n");
    let finding = [ts_compat("velt-number-type")];
    published_until(&mut client, &doc, &finding);
    let pkg = uri_in(&tmp, "package.vlt");
    client.open(&pkg, &manifest("[\"shared\"]"));
    // An unsaved edit moves the folders away from the document...
    client.change(&pkg, 2, &manifest("[\"models\"]"));
    published_until(&mut client, &doc, &[]);
    // ...and closing the manifest without saving brings the file on disk back.
    client.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": pkg } }),
    );
    published_until(&mut client, &doc, &finding);
    client.shutdown();
}

#[test]
fn manifest_and_folder_changes_on_disk_refresh_open_documents() {
    let tmp = package();
    let mut client = Client::start();
    let event = |rel: &str, typ: u32| json!({ "uri": uri_in(&tmp, rel), "type": typ });
    let doc = uri_in(&tmp, "shared/model.vlt");
    client.open(&doc, "export const x: f64 = 1.5;\n");
    published_until(&mut client, &doc, &[ts_compat("velt-number-type")]);
    // `package.vlt` saved by another program: the document leaves the folders.
    std::fs::write(tmp.path().join("package.vlt"), manifest("[\"models\"]")).unwrap();
    client.notify(
        "workspace/didChangeWatchedFiles",
        json!({ "changes": [event("package.vlt", 2)] }),
    );
    published_until(&mut client, &doc, &[]);

    // The open manifest's warning for the missing folder goes once the folder is created.
    let pkg = uri_in(&tmp, "package.vlt");
    client.open(&pkg, &manifest("[\"models\"]"));
    assert_eq!(published(&mut client, &pkg).len(), 1);
    std::fs::create_dir(tmp.path().join("models")).unwrap();
    client.notify(
        "workspace/didChangeWatchedFiles",
        json!({ "changes": [event("models", 1)] }),
    );
    loop {
        if published(&mut client, &pkg).is_empty() {
            break;
        }
    }
    client.shutdown();
}

#[test]
fn ranges_and_fixes_count_utf16_columns() {
    let tmp = package();
    let mut client = Client::start();
    let doc = uri_in(&tmp, "shared/model.vlt");
    // The emoji is two UTF-16 code units (four UTF-8 bytes) before `f64` on the same line.
    let text = "export const s: string = \"😀\"; export const x: f64 = 1.5;\n";
    client.open(&doc, text);
    let diags = published_until(&mut client, &doc, &[ts_compat("velt-number-type")]);
    let (line, start) = super::client::pos_of(text, "f64", 0);
    let range = json!({
        "start": { "line": line, "character": start },
        "end": { "line": line, "character": start + 3 },
    });
    assert_eq!(diags[0]["range"], range);
    let offered = actions(&mut client, &doc, text, "f64", &diags);
    let fix = find(&offered, "replace with `number`");
    let edits = fix["edit"]["changes"][doc.as_str()].as_array().unwrap();
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0]["range"], range);
    assert_eq!(
        apply(text, fix, &doc),
        "export const s: string = \"😀\"; export const x: number = 1.5;\n"
    );
    client.shutdown();
}

#[test]
fn fix_all_includes_ts_compat_fixes() {
    let tmp = package();
    let mut client = Client::start();
    let doc = uri_in(&tmp, "shared/model.vlt");
    // Two `f64`s with a fix, and an `i64`, whose fix is the author's call (none offered).
    let text = "export const x: f64 = 1.5;\nexport const y: f64 = 2.5;\nexport const z: i64 = 2;\n";
    client.open(&doc, text);
    let finding = ts_compat("velt-number-type");
    let diags = published_until(
        &mut client,
        &doc,
        &[finding.clone(), finding.clone(), finding.clone()],
    );
    let params = json!({
        "textDocument": { "uri": doc },
        "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } },
        "context": { "diagnostics": diags, "only": ["source.fixAll"] },
    });
    let offered = client.request("textDocument/codeAction", params);
    let offered = offered.as_array().unwrap();
    assert_eq!(offered.len(), 1, "{offered:?}");
    assert_eq!(offered[0]["kind"], json!("source.fixAll"));
    let fixed = apply(text, &offered[0], &doc);
    assert_eq!(
        fixed,
        "export const x: number = 1.5;\nexport const y: number = 2.5;\nexport const z: i64 = 2;\n"
    );
    client.change(&doc, 2, &fixed);
    published_until(&mut client, &doc, &[finding]);
    client.shutdown();
}

#[test]
fn only_package_changes_refresh_open_documents() {
    use crate::server::affects_packages;
    let tmp = package();
    let at = |rel: &str| tmp.path().join(rel);
    std::fs::create_dir_all(at(".git/refs")).unwrap();
    std::fs::create_dir_all(at("node_modules/x")).unwrap();
    std::fs::create_dir_all(at("shared/sub")).unwrap();
    assert!(affects_packages(&at("package.vlt"), false, false));
    assert!(affects_packages(&at("shared/sub"), true, false));
    assert!(affects_packages(&at("gone"), false, true));
    // Source files, and what the walk never enters, don't.
    assert!(!affects_packages(&at("shared/a.vlt"), false, true));
    assert!(!affects_packages(&at(".git/refs"), true, false));
    assert!(!affects_packages(&at(".git/index.lock"), false, true));
    assert!(!affects_packages(&at("node_modules/x"), true, false));
    // Outside any package nothing has `tsCompat` folders.
    let outside = tempfile::tempdir().unwrap();
    assert!(!affects_packages(outside.path(), true, false));
}
