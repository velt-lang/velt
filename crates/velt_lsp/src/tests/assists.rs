//! Inlay hints, signature help, semantic tokens, document highlight and workspace symbols.

use lsp_types::Url;
use serde_json::{json, Value};

use super::client::{at, pos_of, uri, Client};

const APP: &str = r#"class Circle {
  radius: f64;
  constructor(radius: f64) {
    this.radius = radius;
  }
  scaled(factor: f64, extra: f64): f64 {
    return this.radius * factor + extra;
  }
}

function area(r: f64, scale: f64): f64 {
  return r * r * scale;
}

function main() {
  const c = new Circle(2.0);
  let total = area(1.5, 2.0);
  const radius = 3.0;
  total = total + area(radius, c.scaled(1.0, 0.5));
  for (const x of [1, 2]) {
    console.log(x);
  }
  console.log(total);
}
"#;

fn open_app(name: &str) -> (Client, Url) {
    let mut client = Client::start();
    let app = uri(name);
    client.open(&app, APP);
    assert_eq!(client.diagnostics(&app)["diagnostics"], json!([]));
    (client, app)
}

/// Hints as `(line, character, label)`.
fn hints(client: &mut Client, app: &Url) -> Vec<(u64, u64, String)> {
    let params = json!({
        "textDocument": { "uri": app },
        "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 100, "character": 0 } },
    });
    let result = client.request("textDocument/inlayHint", params);
    result
        .as_array()
        .unwrap()
        .iter()
        .map(|h| {
            (
                h["position"]["line"].as_u64().unwrap(),
                h["position"]["character"].as_u64().unwrap(),
                h["label"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[test]
fn inlay_hints_show_inferred_types_and_parameter_names() {
    let (mut client, app) = open_app("assist_hints.vlt");
    let hints = hints(&mut client, &app);
    let has = |needle: &str, delta: usize, label: &str| {
        let (line, col) = pos_of(APP, needle, delta);
        let wanted = (line as u64, col as u64, label.to_string());
        assert!(hints.contains(&wanted), "missing {wanted:?} in {hints:#?}");
    };
    has("total = area", 5, ": f64");
    has("radius = 3.0", 6, ": f64");
    has("x of", 1, ": i64");
    has("1.5, 2.0", 0, "r:");
    has("2.0);\n  const radius", 0, "scale:");
    has("1.0, 0.5", 0, "factor:");
    has("0.5))", 0, "extra:");
    // `new Circle(...)` already names its type; `radius` is passed to a parameter named `r`, but
    // `area(radius, ...)` still gets `r:`; the constructor argument gets its parameter name.
    let (cl, cc) = pos_of(APP, "c = new", 1);
    assert!(
        !hints.iter().any(|h| (h.0, h.1) == (cl as u64, cc as u64)),
        "{hints:#?}"
    );
    has("2.0);\n  let", 0, "radius:");
    has("radius, c.scaled", 0, "r:");
    client.shutdown();
}

#[test]
fn signature_help_while_typing_a_call() {
    let mut client = Client::start();
    let app = uri("assist_sig_app.vlt");
    client.open(&app, APP);
    client.diagnostics(&app);
    let doc = uri("assist_sig.vlt");
    let text = APP.replace(
        "  console.log(total);\n",
        "  console.log(total);\n  area(1.0, \n",
    );
    client.open(&doc, &text);
    client.diagnostics(&doc);
    let (line, col) = pos_of(&text, "area(1.0, \n", 10);
    let help = client.request("textDocument/signatureHelp", at(&doc, line, col));
    let sig = &help["signatures"][0];
    assert_eq!(
        sig["label"],
        json!("function area(r: f64, scale: f64): f64")
    );
    assert_eq!(help["activeParameter"], json!(1));
    let label = sig["label"].as_str().unwrap();
    let range = &sig["parameters"][1]["label"];
    let (lo, hi) = (
        range[0].as_u64().unwrap() as usize,
        range[1].as_u64().unwrap() as usize,
    );
    assert_eq!(&label[lo..hi], "scale: f64");

    // A method call inside another call's arguments: the innermost call wins.
    let (line, col) = pos_of(APP, "1.0, 0.5", 0);
    let help = client.request("textDocument/signatureHelp", at(&app, line, col));
    assert_eq!(
        help["signatures"][0]["label"],
        json!("(method) Circle.scaled(factor: f64, extra: f64): f64")
    );
    assert_eq!(help["activeParameter"], json!(0));

    // `new C(`: the constructor.
    let (line, col) = pos_of(APP, "Circle(2.0", 7);
    let help = client.request("textDocument/signatureHelp", at(&app, line, col));
    assert_eq!(
        help["signatures"][0]["label"],
        json!("constructor Circle(radius: f64)")
    );

    // Outside any call: nothing.
    let help = client.request("textDocument/signatureHelp", at(&doc, 0, 3));
    assert_eq!(help, Value::Null);
    client.shutdown();
}

/// Decoded semantic tokens: (line, character, length, type name, modifier names).
fn tokens(client: &mut Client, doc: &Url) -> Vec<(u32, u32, u32, String, Vec<String>)> {
    let legend = &client.init["capabilities"]["semanticTokensProvider"]["legend"];
    let types: Vec<String> = serde_json::from_value(legend["tokenTypes"].clone()).unwrap();
    let mods: Vec<String> = serde_json::from_value(legend["tokenModifiers"].clone()).unwrap();
    let result = client.request(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": doc } }),
    );
    let data: Vec<u32> = serde_json::from_value(result["data"].clone()).unwrap();
    let (mut line, mut col) = (0, 0);
    data.chunks(5)
        .map(|t| {
            line += t[0];
            col = if t[0] == 0 { col + t[1] } else { t[1] };
            let names = (0..mods.len())
                .filter(|i| t[4] & (1 << i) != 0)
                .map(|i| mods[i].clone())
                .collect();
            (line, col, t[2], types[t[3] as usize].clone(), names)
        })
        .collect()
}

#[test]
fn semantic_tokens_classify_names() {
    let (mut client, app) = open_app("assist_tokens.vlt");
    let tokens = tokens(&mut client, &app);
    let token = |needle: &str, delta: usize| {
        let (line, col) = pos_of(APP, needle, delta);
        tokens
            .iter()
            .find(|t| (t.0, t.1) == (line, col))
            .unwrap_or_else(|| panic!("no token at `{needle}`: {tokens:#?}"))
            .clone()
    };
    let (_, _, len, ty, mods) = token("Circle {", 0);
    assert_eq!((len, ty.as_str()), (6, "class"));
    assert!(mods.contains(&"declaration".to_string()));
    assert_eq!(token("area(1.5", 0).3, "function");
    assert_eq!(token("scaled(1.0", 0).3, "method");
    assert_eq!(token("r * r", 0).3, "parameter");
    assert_eq!(token("radius * factor", 0).3, "property");
    let (_, _, _, ty, mods) = token("total = total", 0);
    assert_eq!(ty, "variable");
    assert!(mods.contains(&"mutable".to_string()), "{mods:?}");
    let (_, _, _, _, mods) = token("c.scaled", 0);
    assert!(mods.contains(&"readonly".to_string()), "{mods:?}");
    // Keywords and literals are not semantic tokens.
    let (l, c) = pos_of(APP, "function area", 0);
    assert!(!tokens.iter().any(|t| (t.0, t.1) == (l, c)));
    client.shutdown();
}

#[test]
fn document_highlight_marks_reads_and_writes() {
    let (mut client, app) = open_app("assist_highlight.vlt");
    let (line, col) = pos_of(APP, "total = area", 0);
    let result = client.request("textDocument/documentHighlight", at(&app, line, col));
    let mut kinds: Vec<(u64, u64, u64)> = result
        .as_array()
        .unwrap()
        .iter()
        .map(|h| {
            let s = &h["range"]["start"];
            (
                s["line"].as_u64().unwrap(),
                s["character"].as_u64().unwrap(),
                h["kind"].as_u64().unwrap(),
            )
        })
        .collect();
    kinds.sort();
    let (decl_line, decl_col) = pos_of(APP, "total = area", 0);
    let (asg_line, asg_col) = pos_of(APP, "total = total", 0);
    let (read_line, read_col) = pos_of(APP, "total + area", 0);
    let (log_line, log_col) = pos_of(APP, "total);\n}", 0);
    assert_eq!(
        kinds,
        [
            (decl_line as u64, decl_col as u64, 3),
            (asg_line as u64, asg_col as u64, 3),
            (read_line as u64, read_col as u64, 2),
            (log_line as u64, log_col as u64, 2),
        ]
    );
    client.shutdown();
}

#[test]
fn workspace_symbols_search_open_programs_and_folders() {
    let dir = std::env::temp_dir().join("velt_lsp_ws_symbols");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("target")).unwrap();
    std::fs::write(
        dir.join("src/geometry.vlt"),
        "export function areaOfSquare(s: f64): f64 {\n  return s * s;\n}\n",
    )
    .unwrap();
    std::fs::write(dir.join("target/skip.vlt"), "function areaSkipped() {}\n").unwrap();
    let root = Url::from_file_path(&dir).unwrap();

    let mut client = Client::start_with(json!({ "capabilities": {}, "rootUri": root }));
    let app = uri("assist_ws.vlt");
    client.open(&app, APP);
    client.diagnostics(&app);
    let result = client.request("workspace/symbol", json!({ "query": "area" }));
    let mut names: Vec<(String, String)> = result
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            let file = s["location"]["uri"].as_str().unwrap();
            let file = file.rsplit('/').next().unwrap().to_string();
            (s["name"].as_str().unwrap().to_string(), file)
        })
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            ("area".to_string(), "assist_ws.vlt".to_string()),
            ("areaOfSquare".to_string(), "geometry.vlt".to_string()),
        ]
    );
    let members = client.request("workspace/symbol", json!({ "query": "scaled" }));
    assert_eq!(members[0]["containerName"], json!("Circle"));
    assert_eq!(members[0]["kind"], json!(6), "method");
    client.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}
