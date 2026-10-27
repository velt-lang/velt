//! Cloudflare Workers (`--host workers`, docs/internals/design/cloudflare-workers.md): each
//! `tests/workers/*.vlt` is built with `velt build --host workers` (debug and `--release`) and
//! driven by `tests/workers/drive.mjs` under node, through the Worker glue the build wrote
//! (`velt_worker.mjs`) as a Worker would run it, with the requests (and the fake upstream) of the
//! file's `.mjs`; what it prints must match the file's `.out`.
//!
//! Needs what the browser goldens need (tests/wasm_goldens.rs): LLVM's `opt`/`llc`, the
//! wasm32-unknown-unknown Rust target and node; skipped with a note when one is missing, and a
//! failure instead with `VELT_RT_WASM_PREBUILT=1` (the runtime library is prebuilt there).

mod no_window;
mod work_dir;

use std::path::{Path, PathBuf};

const TRIPLE: &str = "wasm32-unknown-unknown";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn prebuilt() -> bool {
    std::env::var_os("VELT_RT_WASM_PREBUILT").is_some_and(|v| v == "1")
}

fn runs(program: &str) -> bool {
    no_window::command(program)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn has_rust_target() -> bool {
    let Ok(out) = no_window::command("rustc")
        .args(["--print", "sysroot"])
        .output()
    else {
        return false;
    };
    let sysroot = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Path::new(&sysroot)
        .join("lib/rustlib")
        .join(TRIPLE)
        .join("lib")
        .is_dir()
}

/// Why the test can't run here, if it can't.
fn missing_tools() -> Option<&'static str> {
    let missing = if velt_codegen_llvm::find_wasm_tools().is_none() {
        Some("LLVM's opt/llc (rustup component add llvm-tools)")
    } else if !has_rust_target() {
        Some("the wasm32-unknown-unknown Rust target")
    } else {
        (!runs("node")).then_some("node")
    };
    if let Some(missing) = missing {
        assert!(
            !prebuilt(),
            "VELT_RT_WASM_PREBUILT=1, but {missing} is not available"
        );
    }
    missing
}

/// Build `file` for the Workers host into `out` and drive it; `Err` describes the mismatch.
fn check(root: &Path, file: &Path, release: bool, out: &Path) -> Result<(), String> {
    let module = out.join(file.file_stem().expect("stem"));
    let build = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "--host", "workers"])
        .args(release.then_some("--release"))
        .arg(file)
        .arg("-o")
        .arg(&module)
        .output()
        .expect("run velt build");
    let mode = if release { "release" } else { "debug" };
    if !build.status.success() {
        return Err(format!(
            "{} [{mode}]: build failed\n{}",
            file.display(),
            String::from_utf8_lossy(&build.stderr)
        ));
    }
    let wasm = module.with_extension("wasm");
    let run = no_window::command("node")
        .arg(root.join("crates/veltc/tests/workers/drive.mjs"))
        .arg(out)
        .arg(wasm.file_name().expect("module file"))
        .arg(file.with_extension("mjs"))
        .output()
        .expect("run node");
    let got = String::from_utf8_lossy(&run.stdout).replace("\r\n", "\n");
    let want = std::fs::read_to_string(file.with_extension("out"))
        .expect(".out")
        .replace("\r\n", "\n");
    if run.status.success() && got == want {
        return Ok(());
    }
    Err(format!(
        "{} [{mode}]: exit {:?}\n--- want ---\n{want}--- got ---\n{got}--- stderr ---\n{}",
        file.display(),
        run.status.code(),
        String::from_utf8_lossy(&run.stderr)
    ))
}

#[test]
fn workers_programs() {
    if let Some(missing) = missing_tools() {
        println!("workers: skipped (needs {missing})");
        return;
    }
    let root = root();
    if !prebuilt() {
        let st = no_window::command(env!("CARGO"))
            .args(["build", "-p", "velt_rt_wasm", "--target", TRIPLE])
            .current_dir(&root)
            .status()
            .expect("cargo build -p velt_rt_wasm");
        assert!(st.success(), "building velt_rt_wasm for {TRIPLE} failed");
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(root.join("crates/veltc/tests/workers"))
        .expect("tests/workers")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "vlt"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no tests/workers/*.vlt");
    let work = work_dir::work_dir(&root, "workers-tests");
    let mut failures = vec![];
    for file in &files {
        for release in [false, true] {
            let mode = if release { "release" } else { "debug" };
            let stem = file.file_stem().expect("stem").to_string_lossy();
            let out = work.join(format!("{stem}-{mode}"));
            let _ = std::fs::remove_dir_all(&out);
            std::fs::create_dir_all(&out).expect("work dir");
            if let Err(e) = check(&root, file, release, &out) {
                failures.push(e);
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
