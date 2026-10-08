//! The behaviour claims of `velt check --ts-compat` under Node (docs/internals/design/tsx.md
//! "The oracle"): each behaviour sample with a `main` (tests/tscompat-oracle/behaviour) prints
//! one thing under `velt run` and another under Node, and with the lint's fixes applied (from
//! `--json`) the same under both. Node runs the sample with its types stripped
//! (`--experimental-transform-types`, Node 22.7 or newer) and a call of `main()` appended.
//!
//! It runs only with `VELT_TSC_ORACLE` set (the nightly oracle job sets it), where a missing
//! Node is a failure; otherwise it is skipped with a message, so the pull request gate never
//! runs Node.

use std::path::Path;
use std::process::Command;

use serde_json::Value;

use super::{json, stderr, test_dir, velt};

/// stdout of `velt run <file>` in `dir`.
fn run_velt(dir: &Path, file: &str) -> String {
    let o = velt(dir, &["run", file]);
    assert!(o.status.success(), "velt run {file}:\n{}", stderr(&o));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// stdout of the sample `file` under Node, with `main()` called.
fn run_node(dir: &Path, file: &str) -> String {
    let src = std::fs::read_to_string(dir.join(file)).unwrap();
    let runner = format!("{}.node.ts", file.trim_end_matches(".ts"));
    std::fs::write(dir.join(&runner), format!("{src}\nmain();\n")).unwrap();
    let o = Command::new("node")
        .args(["--experimental-transform-types", "--no-warnings", &runner])
        .current_dir(dir)
        .output()
        .expect("run node");
    assert!(
        o.status.success(),
        "node {runner}:\n{}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// Byte offset of a 1-based `line` and byte `column` in `src`.
fn offset(src: &str, line: u64, column: u64) -> usize {
    let start: usize = src
        .split_inclusive('\n')
        .take(line as usize - 1)
        .map(str::len)
        .sum();
    start + column as usize - 1
}

/// `src` with the fixes of a `--json` report applied.
fn apply_fixes(src: &str, report: &Value) -> Option<String> {
    let mut fixes: Vec<(usize, usize, String)> = report["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|d| {
            let fix = d.get("fix")?;
            let at = &fix["location"];
            let lo = offset(src, at["line"].as_u64()?, at["column"].as_u64()?);
            let hi = offset(src, at["endLine"].as_u64()?, at["endColumn"].as_u64()?);
            Some((lo, hi, fix["replacement"].as_str()?.to_string()))
        })
        .collect();
    if fixes.is_empty() {
        return None;
    }
    fixes.sort_by_key(|f| std::cmp::Reverse(f.0));
    let mut out = src.to_string();
    for (lo, hi, replacement) in fixes {
        out.replace_range(lo..hi, &replacement);
    }
    Some(out)
}

/// `Err(why)` without a Node that strips types (22.7 or newer).
fn node_ready() -> Result<(), String> {
    let out = Command::new("node")
        .arg("--version")
        .output()
        .map_err(|_| "`node` is not on PATH".to_string())?;
    let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let parts: Vec<u32> = version
        .trim_start_matches('v')
        .split('.')
        .filter_map(|p| p.parse().ok())
        .collect();
    match parts.as_slice() {
        [major, minor, ..] if (*major, *minor) >= (22, 7) => Ok(()),
        _ => Err(format!(
            "Node {version} can't strip types (22.7 or newer can)"
        )),
    }
}

#[test]
fn behaviour_samples_differ_under_node_and_their_fixes_agree() {
    if std::env::var_os("VELT_TSC_ORACLE").is_none() {
        eprintln!("skipped: set VELT_TSC_ORACLE=1 to run the behaviour samples under Node");
        return;
    }
    if let Err(why) = node_ready() {
        panic!("VELT_TSC_ORACLE is set but {why}");
    }
    // `velt run` links the samples against the native runtime.
    super::runtime_support::build_native_runtime(Path::new(env!("CARGO_MANIFEST_DIR")));
    let samples =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/tscompat-oracle/behaviour");
    let mut files: Vec<_> = std::fs::read_dir(&samples)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "ts"))
        .collect();
    files.sort();
    let mut ran = 0;
    for path in files {
        let src = std::fs::read_to_string(&path).unwrap();
        if !src.contains("export function main()") {
            continue;
        }
        ran += 1;
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let tmp = test_dir::TestDir::new();
        let dir = tmp.path();
        std::fs::write(dir.join(&name), &src).unwrap();
        let (in_velt, in_node) = (run_velt(dir, &name), run_node(dir, &name));
        assert_ne!(in_velt, in_node, "{name}: Velt and Node print the same");
        let report = json(&velt(dir, &["check", "--ts-compat", "--json", &name]));
        let Some(fixed) = apply_fixes(&src, &report) else {
            eprintln!("{name}: Velt {in_velt:?}, Node {in_node:?}; no fix");
            continue;
        };
        std::fs::write(dir.join(&name), &fixed).unwrap();
        let (fixed_velt, fixed_node) = (run_velt(dir, &name), run_node(dir, &name));
        assert_eq!(
            fixed_velt, fixed_node,
            "{name}: the fixed sample differs:\n{fixed}"
        );
        eprintln!("{name}: Velt {in_velt:?}, Node {in_node:?}; fixed: {fixed_velt:?} in both");
    }
    assert!(ran >= 5, "behaviour samples with a `main`: {ran}");
}

/// `examples/apps/ssr-blog` shares its components with a TypeScript client: they render the same
/// HTML on the server (std/jsx, `velt run`) and in Node (client/jsx-runtime.ts, compiled by the
/// oracle's `tsc`). `src/shared/samples.tsx` renders them with data escaping must handle.
#[test]
fn ssr_blog_shared_components_render_the_same_under_node() {
    if std::env::var_os("VELT_TSC_ORACLE").is_none() {
        eprintln!("skipped: set VELT_TSC_ORACLE=1 to render the ssr-blog components under Node");
        return;
    }
    if let Err(why) = node_ready() {
        panic!("VELT_TSC_ORACLE is set but {why}");
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let tsc = root.join("tests/tscompat-oracle/node_modules/typescript/bin/tsc");
    assert!(
        tsc.is_file(),
        "VELT_TSC_ORACLE is set but {} is missing",
        tsc.display()
    );
    super::runtime_support::build_native_runtime(Path::new(env!("CARGO_MANIFEST_DIR")));
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    for part in [
        "package.vlt",
        "tsconfig.json",
        "jsx",
        "client",
        "src/shared",
    ] {
        super::copy_tree(
            &root.join("examples/apps/ssr-blog").join(part),
            &dir.join(part),
        );
    }

    // The server side.
    std::fs::write(
        dir.join("samples_main.vlt"),
        r#"import { renderToStringSync } from "velt:jsx";
import { samples } from "./src/shared/samples";

function main() {
  const els = samples();
  els.reverse();
  while (els.length > 0) {
    const el = els.pop();
    if (el != null) {
      console.log(renderToStringSync(el));
    }
  }
}
"#,
    )
    .unwrap();
    let in_velt = run_velt(dir, "samples_main.vlt");

    // The client side: emitted by tsc, with the provider and relative imports made Node paths.
    std::fs::write(
        dir.join("tsconfig.emit.json"),
        r#"{ "extends": "./tsconfig.json", "compilerOptions": { "noEmit": false,
  "allowImportingTsExtensions": false, "outDir": "out", "rootDir": "." } }"#,
    )
    .unwrap();
    let o = Command::new("node")
        .arg(&tsc)
        .args(["-p", "tsconfig.emit.json"])
        .current_dir(dir)
        .output()
        .expect("run tsc");
    assert!(
        o.status.success(),
        "tsc:\n{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    for (file, runtime) in [
        (
            "out/src/shared/components.js",
            "../../client/jsx-runtime.js",
        ),
        ("out/src/shared/samples.js", "../../client/jsx-runtime.js"),
        ("out/src/shared/model.js", "../../client/jsx-runtime.js"),
    ] {
        let path = dir.join(file);
        let js = std::fs::read_to_string(&path).unwrap();
        let js = js
            .replace("\"ssr-blog-jsx/jsx-runtime\"", &format!("\"{runtime}\""))
            .replace("from \"./model\"", "from \"./model.js\"")
            .replace("from \"./components\"", "from \"./components.js\"");
        std::fs::write(&path, js).unwrap();
    }
    std::fs::write(dir.join("out/package.json"), r#"{ "type": "module" }"#).unwrap();
    std::fs::write(
        dir.join("samples_main.mjs"),
        "import { samples } from \"./out/src/shared/samples.js\";\n\
         for (const el of samples()) console.log(el.html);\n",
    )
    .unwrap();
    let o = Command::new("node")
        .arg("samples_main.mjs")
        .current_dir(dir)
        .output()
        .expect("run node");
    assert!(
        o.status.success(),
        "node:\n{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let in_node = String::from_utf8_lossy(&o.stdout).into_owned();
    assert_eq!(in_velt, in_node, "the shared components render differently");
    assert_eq!(in_velt.lines().count(), 6, "{in_velt}");
}
