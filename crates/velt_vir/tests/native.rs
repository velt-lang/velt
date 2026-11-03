//! Native runs of lowered M3/M4 programs: HIR (golden sources loaded with the real std, which
//! prints numbers: #2197) → `velt_vir::lower` → Cranelift object → linked with the real
//! `velt_rt` (tokio executor, timers, TCP, JSON reader) → executed; stdout is compared with the
//! goldens.

#[path = "../src/tests/std_src.rs"]
mod std_src;

use std::path::{Path, PathBuf};
use std::sync::Once;

use velt_sema::hir;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Build the runtime staticlib once per process (it must sit next to the test binary's target
/// dir). Not under the gate: `cargo xtask check` builds the workspace first and sets
/// `VELT_RT_PREBUILT=1`. There `-p velt_rt` would resolve other features than `--workspace` and
/// rebuild `velt_rt.lib` while tests running in parallel processes link against it, which fails
/// on Windows (the file is open).
fn ensure_runtime() {
    static BUILD: Once = Once::new();
    BUILD.call_once(|| {
        let prebuilt = std::env::var_os("VELT_RT_PREBUILT").is_some_and(|v| v == "1");
        if cfg!(debug_assertions) && prebuilt {
            return;
        }
        let st = command(env!("CARGO"))
            .args(["build", "-p", "velt_rt"])
            .current_dir(root())
            .status()
            .expect("cargo build -p velt_rt");
        assert!(st.success(), "building velt_rt failed");
    });
}

/// Lower, compile, link and run; returns (stdout, exit code).
fn run_native(name: &str, p: &hir::Program) -> (String, i32) {
    let exe = build_native(name, p);
    let out = command(&exe).output().expect("run program");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.is_empty(), "{name} stderr: {stderr}");
    let stdout = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
    (stdout, out.status.code().unwrap_or(-1))
}

/// Lower, compile and link; returns the executable.
fn build_native(name: &str, p: &hir::Program) -> PathBuf {
    ensure_runtime();
    let v = velt_vir::lower(p);
    if let Err(e) = velt_vir::verify(&v) {
        panic!("verify failed:\n{}\n\n{v}", e.join("\n"));
    }
    let target = velt_codegen_cl::host_triple();
    let opts = velt_codegen_cl::CodegenOptions {
        target: target.clone(),
        optimize: false,
        cpu: None,
    };
    let obj = velt_codegen_cl::emit_object(&v, &opts).unwrap_or_else(|e| panic!("codegen: {e}"));
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("vir_native");
    std::fs::create_dir_all(&dir).expect("tmp dir");
    let obj_path = dir.join(format!("{name}.o"));
    std::fs::write(&obj_path, obj).expect("write object");
    let exe = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    let runtime = velt_link::find_runtime_lib(&target).expect("runtime library");
    velt_link::link(&velt_link::LinkRequest {
        target: &target,
        objects: &[obj_path],
        runtime_lib: &runtime,
        output: &exe,
        release: false,
        native: &[],
    })
    .unwrap_or_else(|e| panic!("link: {e}"));
    exe
}

fn golden(dir: &str, name: &str) -> String {
    let path = root().join(format!("tests/golden/{dir}/{name}.out"));
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .replace("\r\n", "\n")
}

/// The source of golden program `tests/golden/<dir>/<name>.vlt`, loaded with the real std: it
/// prints numbers through std's hooks (#2197).
fn golden_program(dir: &str, name: &str) -> hir::Program {
    let path = root().join(format!("tests/golden/{dir}/{name}.vlt"));
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    std_src::check_src(&src)
}

#[test]
fn async_basic() {
    let (out, code) = run_native("async_basic", &golden_program("m3", "async_basic"));
    assert_eq!(out, golden("m3", "async_basic"));
    assert_eq!(code, 0);
}

#[test]
fn tasks() {
    let (out, code) = run_native("tasks", &golden_program("m3", "tasks"));
    assert_eq!(out, golden("m3", "tasks"));
    assert_eq!(code, 0);
}

#[test]
fn tcp_echo() {
    let (out, code) = run_native("tcp_echo", &golden_program("m3", "tcp_echo"));
    assert_eq!(out, golden("m3", "tcp_echo"));
    assert_eq!(code, 0);
}

#[path = "../../../tests/common/command.rs"]
mod command;
use command::command;
