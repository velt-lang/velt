//! Semantic token ranges and deltas, the workspace symbol index (with and without a file
//! watcher), and "fix all" code actions.

use std::path::Path;

use lsp_types::Url;
use serde_json::{json, Value};

use super::client::{pos_of, uri, Client};

const APP: &str = "function area(r: f64): f64 {\n  return r * r;\n}\n\nfunction main() {\n  const a = area(2.0);\n  console.log(a);\n}\n";

fn tokens(result: &Value) -> Vec<u32> {
    serde_json::from_value(result["data"].clone()).unwrap()
}

#[test]
fn semantic_tokens_come_as_ranges_and_deltas() {
    let mut client = Client::start();
    let doc = uri("tokens_delta.vlt");
    client.open(&doc, APP);
    client.diagnostics(&doc);
    let full = client.request(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": doc } }),
    );
    let id = full["resultId"].as_str().expect("result id").to_string();
    let old = tokens(&full);

    // The range of `main` holds its tokens only, relative to the first of them.
    let range = client.request(
        "textDocument/semanticTokens/range",
        json!({ "textDocument": { "uri": doc }, "range": {
            "start": { "line": 4, "character": 0 }, "end": { "line": 8, "character": 0 } } }),
    );
    let in_range = tokens(&range);
    assert_eq!(
        in_range.len(),
        4 * 5,
        "main, a, area, a (console is built in): {in_range:?}"
    );
    assert_eq!(in_range[..2], [4, 9], "`main` on line 4, column 9");

    // An edit: the delta against the previous result turns the old tokens into the new ones.
    let edited = APP.replace("console.log(a);", "console.log(a, area(a));");
    client.change(&doc, 2, &edited);
    let delta = client.request(
        "textDocument/semanticTokens/full/delta",
        json!({ "textDocument": { "uri": doc }, "previousResultId": id }),
    );
    let edits = delta["edits"].as_array().expect("edits");
    assert_eq!(edits.len(), 1);
    let start = edits[0]["start"].as_u64().unwrap() as usize;
    let delete = edits[0]["deleteCount"].as_u64().unwrap() as usize;
    let data: Vec<u32> = serde_json::from_value(edits[0]["data"].clone()).unwrap();
    let mut patched = old.clone();
    patched.splice(start..start + delete, data);
    let fresh = client.request(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": doc } }),
    );
    assert_eq!(patched, tokens(&fresh));
    // An unknown previous result: everything.
    let unknown = client.request(
        "textDocument/semanticTokens/full/delta",
        json!({ "textDocument": { "uri": doc }, "previousResultId": "nope" }),
    );
    assert_eq!(tokens(&unknown), tokens(&fresh));
    client.shutdown();
}

fn symbol_names(client: &mut Client, query: &str) -> Vec<String> {
    let result = client.request("workspace/symbol", json!({ "query": query }));
    let mut names: Vec<String> = result
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap().to_string())
        .collect();
    names.sort();
    names
}

/// A fresh workspace folder with one source file.
fn workspace(name: &str) -> (std::path::PathBuf, Url) {
    let dir = std::env::temp_dir().join("velt_lsp_tests").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("shapes.vlt"),
        "export function squareArea(): i64 {\n  return 1;\n}\n",
    )
    .unwrap();
    let root = Url::from_file_path(&dir).unwrap();
    (dir, root)
}

/// Rewrite a file so that its modification time surely changes.
fn rewrite(path: &Path, text: &str) {
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(path, text).unwrap();
}

#[test]
fn workspace_symbols_rescan_changed_files_without_a_watcher() {
    let (dir, root) = workspace("ws_index_plain");
    let mut client = Client::start_with(json!({ "capabilities": {}, "rootUri": root }));
    assert_eq!(symbol_names(&mut client, "area"), ["squareArea"]);
    rewrite(
        &dir.join("shapes.vlt"),
        "export function circleArea(): i64 {\n  return 2;\n}\n",
    );
    std::fs::write(dir.join("more.vlt"), "function cubeArea() {}\n").unwrap();
    assert_eq!(
        symbol_names(&mut client, "area"),
        ["circleArea", "cubeArea"]
    );
    assert!(client.server_requests.is_empty());
    client.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn workspace_symbols_follow_watched_file_events() {
    let (dir, root) = workspace("ws_index_watched");
    let caps = json!({ "workspace": { "didChangeWatchedFiles": { "dynamicRegistration": true } } });
    let mut client = Client::start_with(json!({ "capabilities": caps, "rootUri": root }));
    assert_eq!(symbol_names(&mut client, "area"), ["squareArea"]);
    assert_eq!(client.server_requests, ["client/registerCapability"]);
    // Without an event the index is not reread.
    let file = dir.join("shapes.vlt");
    rewrite(
        &file,
        "export function circleArea(): i64 {\n  return 2;\n}\n",
    );
    assert_eq!(symbol_names(&mut client, "area"), ["squareArea"]);
    let event =
        |path: &Path, typ: u32| json!({ "uri": Url::from_file_path(path).unwrap(), "type": typ });
    let skipped = dir.join("target").join("built.vlt");
    std::fs::create_dir_all(skipped.parent().unwrap()).unwrap();
    std::fs::write(&skipped, "function builtArea() {}\n").unwrap();
    client.notify(
        "workspace/didChangeWatchedFiles",
        json!({ "changes": [event(&file, 2), event(&skipped, 1)] }),
    );
    assert_eq!(symbol_names(&mut client, "area"), ["circleArea"]);
    std::fs::remove_file(&file).unwrap();
    client.notify(
        "workspace/didChangeWatchedFiles",
        json!({ "changes": [event(&file, 3)] }),
    );
    assert!(symbol_names(&mut client, "area").is_empty());

    // Directories: watchers report the directory itself when it is created, renamed (deleted +
    // created) or deleted.
    let sub = dir.join("sub");
    std::fs::create_dir_all(sub.join("deep")).unwrap();
    std::fs::write(
        sub.join("deep/cube.vlt"),
        "function cubeArea() {}
",
    )
    .unwrap();
    let changes = json!({ "changes": [event(&sub, 1)] });
    client.notify("workspace/didChangeWatchedFiles", changes);
    assert_eq!(symbol_names(&mut client, "area"), ["cubeArea"]);
    let moved = dir.join("moved");
    std::fs::rename(&sub, &moved).unwrap();
    let changes = json!({ "changes": [event(&sub, 3), event(&moved, 1)] });
    client.notify("workspace/didChangeWatchedFiles", changes);
    let result = client.request("workspace/symbol", json!({ "query": "cube" }));
    let found = result[0]["location"]["uri"].as_str().unwrap_or_default();
    assert!(found.ends_with("moved/deep/cube.vlt"), "{result}");
    assert_eq!(result.as_array().map(Vec::len), Some(1), "{result}");
    std::fs::remove_dir_all(&moved).unwrap();
    let changes = json!({ "changes": [event(&moved, 3)] });
    client.notify("workspace/didChangeWatchedFiles", changes);
    assert!(symbol_names(&mut client, "area").is_empty());
    client.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_refused_watcher_falls_back_to_rescans() {
    let (dir, root) = workspace("ws_index_refused");
    let caps = json!({ "workspace": { "didChangeWatchedFiles": { "dynamicRegistration": true } } });
    let init = json!({ "capabilities": caps, "rootUri": root });
    let mut client = Client::start_with(init);
    client.refuse_server_requests = true;
    assert_eq!(symbol_names(&mut client, "area"), ["squareArea"]);
    assert_eq!(client.server_requests, ["client/registerCapability"]);
    rewrite(
        &dir.join("shapes.vlt"),
        "export function circleArea(): i64 {
  return 2;
}
",
    );
    assert_eq!(symbol_names(&mut client, "area"), ["circleArea"]);
    client.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_token_range_ending_inside_a_name_keeps_the_whole_name() {
    let mut client = Client::start();
    let doc = uri("tokens_range_cut.vlt");
    client.open(&doc, APP);
    client.diagnostics(&doc);
    // The range ends after `ar` of `area(2.0)`.
    let (line, col) = pos_of(APP, "area(2.0)", 2);
    let range = client.request(
        "textDocument/semanticTokens/range",
        json!({ "textDocument": { "uri": doc }, "range": {
            "start": { "line": line, "character": 0 }, "end": { "line": line, "character": col } } }),
    );
    let data = tokens(&range);
    let last = &data[data.len() - 5..];
    // `a` at column 8, then `area` four columns on, all four characters long.
    assert_eq!((last[1], last[2]), (4, 4), "`area` whole: {data:?}");
    client.shutdown();
}

#[test]
fn fixes_apply_to_the_whole_file() {
    let text = "function main() {\n  let a: string | null = undefined;\n  let b: string | null = undefined;\n  console.log(a, b);\n}\n";
    let mut client = Client::start();
    let doc = uri("fix_all.vlt");
    client.open(&doc, text);
    let diags = client.diagnostics(&doc)["diagnostics"].clone();
    let (line, character) = pos_of(text, "undefined", 0);
    let pos = json!({ "line": line, "character": character });
    let request = |client: &mut Client, only: Value| {
        let mut context = json!({ "diagnostics": diags });
        if !only.is_null() {
            context["only"] = only;
        }
        let params = json!({ "textDocument": { "uri": doc }, "range": { "start": pos, "end": pos }, "context": context });
        client
            .request("textDocument/codeAction", params)
            .as_array()
            .cloned()
            .unwrap()
    };
    let edits_of = |action: &Value| {
        action["edit"]["changes"][doc.as_str()]
            .as_array()
            .unwrap()
            .len()
    };
    let offered = request(&mut client, Value::Null);
    let titles: Vec<&str> = offered
        .iter()
        .map(|a| a["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        titles,
        [
            "Replace `undefined` with `null`",
            "Fix all in file: Replace `undefined` with `null`",
        ]
    );
    assert_eq!((edits_of(&offered[0]), edits_of(&offered[1])), (1, 2));
    // `source.fixAll` only when asked for.
    let source_only = request(&mut client, json!(["source.fixAll"]));
    assert_eq!(source_only.len(), 1);
    assert_eq!(source_only[0]["kind"], json!("source.fixAll"));
    assert_eq!(edits_of(&source_only[0]), 2);
    // Applying it leaves nothing to fix.
    let index = crate::line_index::LineIndex::new(text);
    let mut edits: Vec<(usize, usize, String)> = source_only[0]["edit"]["changes"][doc.as_str()]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            let pos = |p: &Value| index.offset(serde_json::from_value(p.clone()).unwrap()) as usize;
            let text = e["newText"].as_str().unwrap().to_string();
            (pos(&e["range"]["start"]), pos(&e["range"]["end"]), text)
        })
        .collect();
    edits.sort();
    let mut fixed = text.to_string();
    for (lo, hi, new) in edits.into_iter().rev() {
        fixed.replace_range(lo..hi, &new);
    }
    client.change(&doc, 2, &fixed);
    assert_eq!(client.diagnostics(&doc)["diagnostics"], json!([]));
    assert!(request(&mut client, json!(["source.fixAll"])).is_empty());
    client.shutdown();
}
