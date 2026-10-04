//! The benchmark programs keep compiling: every `bench/**/*.vlt` goes through parse, sema,
//! lowering and VIR verification (no codegen or linking, so the set checks in seconds).
//!
//! Nothing else builds `bench/`, so a change to the standard library or the checker that breaks
//! a benchmark (a driver function gaining an error type, say) would otherwise go unnoticed until
//! someone runs it. The gate runs this test whenever the compiler, the standard library or
//! `bench/` changes (crates/xtask/src/plan.rs).

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use veltc::cli::Emit;
use veltc::driver::{self, BuildError, BuildOptions, Session};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

/// The `.vlt` files under `dir`, recursively, sorted.
fn programs(dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            out.extend(programs(&path));
        } else if path.extension().is_some_and(|e| e == "vlt") {
            out.push(path);
        }
    }
    out.sort();
    out
}

/// Compiles one program to verified VIR; `Some(message)` when it fails.
fn compile(path: &Path) -> Option<String> {
    let opts = BuildOptions {
        input: path.to_path_buf(),
        emit: Emit::Vir,
        ..Default::default()
    };
    let mut sess = Session::new();
    match catch_unwind(AssertUnwindSafe(|| driver::compile(&mut sess, &opts))) {
        Ok(Ok(_)) => None,
        Ok(Err(BuildError::Diagnostics)) => Some(sess.render_diagnostics()),
        Ok(Err(BuildError::Failed(msg))) => {
            Some(format!("{}\nerror: {msg}", sess.render_diagnostics()))
        }
        Ok(Err(BuildError::Ice(msg))) => Some(format!("internal compiler error: {msg}")),
        Err(_) => Some("the compiler panicked".to_string()),
    }
}

#[test]
fn bench_programs_compile() {
    let root = &root();
    let all = programs(&root.join("bench"));
    assert!(!all.is_empty(), "no programs under bench/");
    let failures: Vec<String> = std::thread::scope(|scope| {
        let workers: Vec<_> = all
            .chunks(all.len().div_ceil(8))
            .map(|chunk| {
                scope.spawn(move || {
                    chunk
                        .iter()
                        .filter_map(|p| {
                            let rel = p.strip_prefix(root).unwrap_or(p).display().to_string();
                            compile(p).map(|m| format!("{rel}:\n{m}"))
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|w| w.join().expect("worker"))
            .collect()
    });
    println!("bench: {} programs checked", all.len());
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}
