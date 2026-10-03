//! The goldens on WebAssembly: every `tests/golden/{m1,m2,m3,m4,lang}/*.vlt` with a `.out`
//! file, except the ones using sockets or threads-only features (see [`UNSUPPORTED`]), built
//! with `--target wasm32-wasip1` (debug and `--release`) and run under wasmtime by
//! `velt run`, must print exactly that output and exit with its `.code` (default 0). The M1 and
//! M2 goldens also run as browser modules (`wasm32-unknown-unknown`) through the JS glue under
//! node.
//!
//! Needs the rustup targets (`rustup target add wasm32-wasip1 wasm32-unknown-unknown`), LLVM's
//! `opt`/`llc` (`rustup component add llvm-tools`), wasmtime and node; each part is skipped with
//! a note when its tools are missing. Filter with `VELT_GOLDEN=<substring>` (several separated
//! by `,`).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

/// Goldens that cannot run on WebAssembly: the runtime has no sockets, and on its one thread a
/// promise's result reaches another task without a copy (`velt_rt_fut_transfer` is a no-op).
const UNSUPPORTED: &[&str] = &["tcp_echo", "http_server", "spawn_promise_result_copied"];
/// Goldens that need a file system (not available to browser modules).
const NEEDS_FS: &[&str] = &["fs"];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn has_rust_target(triple: &str) -> bool {
    let Ok(out) = Command::new("rustc").args(["--print", "sysroot"]).output() else {
        return false;
    };
    let sysroot = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Path::new(&sysroot)
        .join("lib/rustlib")
        .join(triple)
        .join("lib")
        .is_dir()
}

fn runs(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Why this flavor cannot be tested here, if it cannot.
fn missing_tools(triple: &str, runner: &str) -> Option<String> {
    if velt_codegen_llvm::find_wasm_tools().is_none() {
        return Some("LLVM's opt/llc (rustup component add llvm-tools)".into());
    }
    if !has_rust_target(triple) {
        return Some(format!("the {triple} Rust target"));
    }
    (!runs(runner)).then(|| runner.to_string())
}

fn build_runtime(root: &Path, triple: &str) {
    let st = Command::new(env!("CARGO"))
        .args(["build", "-p", "velt_rt_wasm", "--target", triple])
        .current_dir(root)
        .status()
        .expect("cargo build -p velt_rt_wasm");
    assert!(st.success(), "building velt_rt_wasm for {triple} failed");
}

fn programs(root: &Path, dirs: &[&str], skip: &[&str]) -> Vec<PathBuf> {
    // `VELT_GOLDEN` as in tests/golden.rs: substrings separated by `,`, any of them matches.
    let filter = std::env::var("VELT_GOLDEN").unwrap_or_default();
    let mut filters: Vec<&str> = filter
        .split(',')
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .collect();
    if filters.is_empty() {
        filters.push("");
    }
    let mut files = vec![];
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(root.join("tests/golden").join(dir)) else {
            continue;
        };
        files.extend(rd.flatten().map(|e| e.path()).filter(|p| {
            let stem = p.file_stem().unwrap_or_default().to_string_lossy();
            p.extension().is_some_and(|e| e == "vlt")
                && p.with_extension("out").exists()
                && !skip.contains(&stem.as_ref())
                && {
                    let name = p.to_string_lossy().replace('\\', "/");
                    filters.iter().any(|f| name.contains(f))
                }
        }));
    }
    files.sort();
    files
}

/// Run one golden; `Err` describes the mismatch.
fn check(file: &Path, target: &str, mode: Option<&str>, work: &Path) -> Result<(), String> {
    let want = std::fs::read_to_string(file.with_extension("out"))
        .expect(".out")
        .replace("\r\n", "\n");
    let want_code: i32 = std::fs::read_to_string(file.with_extension("code"))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let o = Command::new(env!("CARGO_BIN_EXE_velt"))
        .args(["run", "--target", target])
        .args(mode)
        .arg(file)
        .current_dir(work)
        .output()
        .expect("run velt");
    let got = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
    let code = o.status.code().unwrap_or(-1);
    if got == want && code == want_code {
        return Ok(());
    }
    Err(format!(
        "{} [{target} {}]: exit {code} (want {want_code})\n--- want ---\n{want}--- got ---\n{got}--- stderr ---\n{}",
        file.display(),
        mode.unwrap_or("debug"),
        String::from_utf8_lossy(&o.stderr)
    ))
}

/// Run every (file, mode) pair on a few threads. Each pair builds in a directory of its own,
/// removed afterwards: a build never overwrites a module that has just run (on Windows a file
/// that was just in use can stay locked for a moment), whether it is the other mode of the same
/// program or a program with the same name from another directory.
fn run_all(files: &[PathBuf], target: &str, modes: &[Option<&str>], work: &Path) -> Vec<String> {
    let jobs: Vec<(&PathBuf, Option<&str>)> = files
        .iter()
        .flat_map(|f| modes.iter().map(move |m| (f, *m)))
        .collect();
    let next = Mutex::new(jobs.into_iter().enumerate());
    let failures = Mutex::new(vec![]);
    std::thread::scope(|s| {
        for _ in 0..4 {
            let (next, failures) = (&next, &failures);
            s.spawn(move || loop {
                let Some((i, (file, mode))) = next.lock().unwrap().next() else {
                    break;
                };
                let dir = work.join(format!("j{i}"));
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir).expect("work dir");
                let result = check(file, target, mode, &dir);
                let _ = std::fs::remove_dir_all(&dir);
                if let Err(e) = result {
                    failures.lock().unwrap().push(e);
                }
            });
        }
    });
    failures.into_inner().unwrap()
}

#[test]
fn goldens_under_wasmtime() {
    if let Some(missing) = missing_tools("wasm32-wasip1", "wasmtime") {
        eprintln!("note: {missing} not available; skipping the wasm32-wasip1 goldens");
        return;
    }
    let root = root();
    build_runtime(&root, "wasm32-wasip1");
    let dirs = ["m1", "m2", "m3", "m4", "lang"];
    let files = programs(&root, &dirs, UNSUPPORTED);
    let work = root.join("target/golden-work-wasi");
    let failures = run_all(&files, "wasm32-wasip1", &[None, Some("--release")], &work);
    let _ = std::fs::remove_dir_all(&work);
    println!(
        "wasm32-wasip1 goldens: {} files, {} failures",
        files.len(),
        failures.len()
    );
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

#[test]
fn goldens_in_the_browser_glue() {
    if let Some(missing) = missing_tools("wasm32-unknown-unknown", "node") {
        eprintln!("note: {missing} not available; skipping the wasm32-unknown-unknown goldens");
        return;
    }
    let root = root();
    build_runtime(&root, "wasm32-unknown-unknown");
    let skip: Vec<&str> = UNSUPPORTED.iter().chain(NEEDS_FS).copied().collect();
    let files = programs(&root, &["m1", "m2"], &skip);
    let work = root.join("target/golden-work-web");
    let failures = run_all(&files, "wasm32-unknown-unknown", &[None], &work);
    let _ = std::fs::remove_dir_all(&work);
    println!(
        "browser goldens: {} files, {} failures",
        files.len(),
        failures.len()
    );
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}
