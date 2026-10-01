//! `velt playground` end to end: the server answers on a free port, and every example the page
//! offers (`playground/playground.js`) compiles to a browser module; with node installed each
//! module also runs through the JS glue and exits 0. Skipped (with a note) without LLVM's
//! opt/llc or the `wasm32-unknown-unknown` Rust target.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;

// Not `canonicalize()`: on Windows that yields a verbatim `\\?\D:\…` path, from which Node
// cannot load a main script (`EISDIR: lstat 'D:'`).
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root")
        .to_path_buf()
}

/// The `"Name": \`source\`` entries of the page's `EXAMPLES` table, unescaped.
fn examples(js: &str) -> Vec<(String, String)> {
    let table = &js[js.find("const EXAMPLES").expect("EXAMPLES")..];
    let table = &table[..table.find("\n};").expect("end of EXAMPLES")];
    let mut out = vec![];
    let mut rest = table;
    while let Some(start) = rest.find("\": `") {
        let name_start = rest[..start].rfind('"').expect("name quote") + 1;
        let name = rest[name_start..start].to_string();
        let body = &rest[start + 4..];
        let end = find_closing_backtick(body);
        let source = body[..end].replace("\\`", "`").replace("\\$", "$");
        out.push((name, source));
        rest = &body[end + 1..];
    }
    out
}

fn find_closing_backtick(s: &str) -> usize {
    let bytes = s.as_bytes();
    (0..bytes.len())
        .find(|&i| bytes[i] == b'`' && (i == 0 || bytes[i - 1] != b'\\'))
        .expect("closing backtick")
}

fn tools_available() -> bool {
    let target = Command::new("rustc")
        .args(["--print", "sysroot"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .is_some_and(|s| {
            Path::new(&s)
                .join("lib/rustlib/wasm32-unknown-unknown")
                .is_dir()
        });
    target && velt_codegen_llvm::find_wasm_tools().is_some()
}

#[test]
fn examples_compile_and_run() {
    let root = root();
    let js = std::fs::read_to_string(root.join("playground/playground.js")).expect("page script");
    let examples = examples(&js);
    assert!(examples.len() >= 5, "found {} examples", examples.len());
    if !tools_available() {
        eprintln!("note: opt/llc or the wasm32-unknown-unknown target missing; skipping");
        return;
    }
    let st = Command::new(env!("CARGO"))
        .args([
            "build",
            "-p",
            "velt_rt_wasm",
            "--target",
            "wasm32-unknown-unknown",
        ])
        .current_dir(&root)
        .status()
        .expect("cargo build");
    assert!(st.success());
    let node = Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    let work = root.join("target/golden-work-playground");
    std::fs::create_dir_all(&work).expect("work dir");
    let glue = work.join("velt_web.mjs");
    std::fs::copy(root.join("crates/velt_rt_wasm/js/velt_web.mjs"), &glue).expect("glue");
    for (name, source) in examples {
        let module = veltc::playground::compile(&source, false)
            .unwrap_or_else(|d| panic!("example `{name}` does not compile:\n{d}"));
        assert_eq!(&module[..4], b"\0asm", "{name}");
        if node {
            let file = work.join("example.wasm");
            std::fs::write(&file, &module).expect("write module");
            let o = Command::new("node")
                .arg(&glue)
                .arg(&file)
                .output()
                .expect("node");
            assert!(o.status.success(), "example `{name}` failed: {o:?}");
        }
    }
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn server_compiles_over_http() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let server = veltc::playground::start(listener).expect("start");
    let base = format!("http://{}", server.addr());
    let page = velt_http::fetch("GET", &format!("{base}/"), &[], b"").expect("GET /");
    assert_eq!(page.status, 200);
    assert!(page.body_text().contains("playground.js"));
    let bad = velt_http::fetch(
        "POST",
        &format!("{base}/api/compile"),
        &[],
        b"function main( {",
    )
    .expect("POST");
    assert_eq!(bad.status, 422);
    assert!(
        bad.body_text().contains("main.vlt:1:"),
        "{}",
        bad.body_text()
    );
    server.stop();
}
