//! The M1 goldens through the LLVM release backend: every `tests/golden/m1/*.vlt` with a
//! `.out` file, run with `velt run --release --backend llvm`, must print exactly that output
//! (and exit with the `.code`, default 0). Skipped with a note when clang is unavailable.
//! A few goldens also run split into codegen units (`VELT_CODEGEN_UNITS=3`).
//! `velt build --emit llvm` must print IR without needing clang.

use std::path::{Path, PathBuf};
use std::process::Command;

mod runtime_support;

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

#[test]
fn m1_goldens_with_llvm() {
    if !velt_codegen_llvm::available() {
        eprintln!("note: clang not available; skipping the LLVM goldens");
        return;
    }
    let root = root();
    runtime_support::build_native_runtime(&root);
    let work = root.join("target/golden-work-llvm");
    std::fs::create_dir_all(&work).expect("work dir");
    let files = m1_programs(&root);
    assert!(!files.is_empty(), "no M1 goldens found");
    let failures: Vec<String> = files
        .iter()
        .filter_map(|f| run_golden(f, &work, None))
        .collect();
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

/// Goldens that depend on symbols shared between codegen units, forced into three units: a
/// static's address compared across units (the JSON writer's vtable checks), interface and
/// override dispatch through vtables, escaping closures with shared captured variables, and a
/// recursive class tree freed through recursive drop glue (kept in one unit). Each
/// build must have written one object per unit (`VELT_CODEGEN_UNITS` is capped at the core count).
#[test]
fn goldens_split_into_codegen_units() {
    if !velt_codegen_llvm::available() {
        eprintln!("note: clang not available; skipping the codegen-unit goldens");
        return;
    }
    let root = root();
    runtime_support::build_native_runtime(&root);
    let work = root.join("target/golden-work-llvm-units");
    std::fs::create_dir_all(&work).expect("work dir");
    let failures: Vec<String> = [
        "lang/json_dynamic_generic",
        "lang/errors_dispatch",
        "lang/share_closure_cells",
        "lang/narrow_nullable_field",
    ]
    .iter()
    .map(|name| root.join("tests/golden").join(format!("{name}.vlt")))
    .filter_map(|f| {
        let unit = |i: usize| {
            let stem = f.file_stem().expect("stem").to_string_lossy();
            let ext = if cfg!(windows) { "obj" } else { "o" };
            work.join(format!("target/velt/{stem}.cgu{i}.{ext}"))
        };
        let units = std::thread::available_parallelism().map_or(1, |n| n.get().min(3));
        for i in 1..=3 {
            let _ = std::fs::remove_file(unit(i));
        }
        run_golden(&f, &work, Some(3)).or_else(|| {
            let written = (1..=3).filter(|&i| unit(i).exists()).count() + 1;
            (written != units)
                .then(|| format!("{}: {written} unit object(s), want {units}", f.display()))
        })
    })
    .collect();
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

/// Runs one golden in a release build through LLVM (with `units` codegen units when given);
/// returns the mismatch report, if any.
fn run_golden(f: &Path, work: &Path, units: Option<usize>) -> Option<String> {
    let want = std::fs::read_to_string(f.with_extension("out"))
        .expect(".out")
        .replace("\r\n", "\n");
    let want_code: i32 = std::fs::read_to_string(f.with_extension("code"))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_velt"));
    cmd.args(["run", "--release", "--backend", "llvm"])
        .arg(f)
        .current_dir(work);
    match units {
        Some(n) => cmd.env("VELT_CODEGEN_UNITS", n.to_string()),
        None => cmd.env_remove("VELT_CODEGEN_UNITS"),
    };
    let o = cmd.output().expect("run velt");
    let got = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
    let code = o.status.code().unwrap_or(-1);
    (got != want || code != want_code).then(|| {
        format!(
            "{}: exit {code} (want {want_code})\n--- want ---\n{want}--- got ---\n{got}--- stderr ---\n{}",
            f.display(),
            String::from_utf8_lossy(&o.stderr)
        )
    })
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
