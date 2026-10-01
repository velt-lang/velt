//! The M1 goldens through the LLVM release backend: every `tests/golden/m1/*.vlt` with a
//! `.out` file, run with `velt run --release --backend llvm`, must print exactly that output
//! (and exit with the `.code`, default 0). Skipped with a note when clang is unavailable.
//! `velt build --emit llvm` must print IR without needing clang.

use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn m1_programs(root: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(root.join("tests/golden/m1"))
        .expect("tests/golden/m1")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "vlt") && p.with_extension("out").exists())
        .collect();
    files.sort();
    files
}

fn build_runtime(root: &Path) {
    let st = Command::new(env!("CARGO"))
        .args(["build", "-p", "velt_rt"])
        .current_dir(root)
        .status()
        .expect("cargo build -p velt_rt");
    assert!(st.success(), "building velt_rt failed");
}

#[test]
fn m1_goldens_with_llvm() {
    if !velt_codegen_llvm::available() {
        eprintln!("note: clang not available; skipping the LLVM goldens");
        return;
    }
    let root = root();
    build_runtime(&root);
    let work = root.join("target/golden-work-llvm");
    std::fs::create_dir_all(&work).expect("work dir");
    let files = m1_programs(&root);
    assert!(!files.is_empty(), "no M1 goldens found");
    let mut failures = vec![];
    for f in &files {
        let want = std::fs::read_to_string(f.with_extension("out"))
            .expect(".out")
            .replace("\r\n", "\n");
        let want_code: i32 = std::fs::read_to_string(f.with_extension("code"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        let o = Command::new(env!("CARGO_BIN_EXE_velt"))
            .args(["run", "--release", "--backend", "llvm"])
            .arg(f)
            .current_dir(&work)
            .output()
            .expect("run velt");
        let got = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
        let code = o.status.code().unwrap_or(-1);
        if got != want || code != want_code {
            failures.push(format!(
                "{}: exit {code} (want {want_code})\n--- want ---\n{want}--- got ---\n{got}--- stderr ---\n{}",
                f.display(),
                String::from_utf8_lossy(&o.stderr)
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

#[test]
fn emit_llvm_prints_ir() {
    let root = root();
    let o = Command::new(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "--emit", "llvm"])
        .arg(root.join("tests/golden/m1/functions.vlt"))
        .output()
        .expect("run velt");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let ir = String::from_utf8_lossy(&o.stdout);
    assert!(ir.contains("target triple = "), "{ir}");
    assert!(ir.contains("define dso_local i32 @\"velt_main\"()"), "{ir}");
}
