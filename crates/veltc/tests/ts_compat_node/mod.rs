//! The behaviour claims of `velt check --ts-compat` under Node (docs/internals/design/tsx.md
//! "The oracle"): each behaviour sample with a `main` (tests/tscompat-oracle/behaviour) prints
//! one thing under `velt run` and another under Node, and with the lint's fixes applied (from
//! `--json`) the same under both. Node runs the sample with its types stripped
//! (`--experimental-transform-types`, Node 22.7 or newer) and a call of `main()` appended.
//!
//! Without Node the test is skipped with a message; `VELT_TSC_ORACLE=1` (the nightly oracle
//! job) makes that a failure.

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
    let required = std::env::var_os("VELT_TSC_ORACLE").is_some_and(|v| v == "1");
    if let Err(why) = node_ready() {
        assert!(!required, "VELT_TSC_ORACLE=1 but {why}");
        eprintln!("skipped: {why}");
        return;
    }
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
