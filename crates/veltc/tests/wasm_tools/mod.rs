//! What the tests that build WebAssembly and run it need (tests/wasm_goldens.rs,
//! tests/workers.rs): the tools, the runtime library, and paths node takes.

use std::path::{Path, PathBuf};

pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

/// `path` as node takes it. `canonicalize` gives a verbatim path (`\\?\E:\…`) on Windows, which
/// node can't run as its main module (`EISDIR: illegal operation on a directory, lstat 'E:'`)
/// and `pathToFileURL` turns into a wrong URL.
pub fn for_node(path: &Path) -> PathBuf {
    match path.to_str().and_then(|p| p.strip_prefix(r"\\?\")) {
        Some(plain) if !plain.starts_with(r"UNC\") => PathBuf::from(plain),
        _ => path.to_path_buf(),
    }
}

pub fn has_rust_target(triple: &str) -> bool {
    let Ok(out) = crate::no_window::command("rustc")
        .args(["--print", "sysroot"])
        .output()
    else {
        return false;
    };
    let sysroot = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Path::new(&sysroot)
        .join("lib/rustlib")
        .join(triple)
        .join("lib")
        .is_dir()
}

pub fn runs(program: &str) -> bool {
    crate::no_window::command(program)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Whether the runtime libraries are prebuilt (`VELT_RT_WASM_PREBUILT=1`).
pub fn prebuilt() -> bool {
    std::env::var_os("VELT_RT_WASM_PREBUILT").is_some_and(|v| v == "1")
}

/// Why this flavor cannot be tested here, if it cannot. A run with prebuilt runtimes exists to
/// run these goldens: there a missing tool fails.
pub fn missing_tools(triple: &str, runner: &str) -> Option<String> {
    let missing = if velt_codegen_llvm::find_wasm_tools().is_none() {
        Some("LLVM's opt/llc (rustup component add llvm-tools)".into())
    } else if !has_rust_target(triple) {
        Some(format!("the {triple} Rust target"))
    } else {
        (!runs(runner)).then(|| runner.to_string())
    };
    if let Some(missing) = &missing {
        assert!(
            !prebuilt(),
            "VELT_RT_WASM_PREBUILT=1, but {missing} is not available"
        );
    }
    missing
}

/// Builds the runtime library the `velt run --target` builds link. Every shard calls this in a
/// process of its own, at the same time: cargo locks the build directory, so the first call
/// builds it and the others wait and then find it up to date; none returns before the library is
/// complete. Prebuilt (`VELT_RT_WASM_PREBUILT=1`), it is not built again: in a checkout that
/// only unpacked the build, cargo would find every source newer than its record of the build.
pub fn build_runtime(root: &Path, triple: &str) {
    if prebuilt() {
        return;
    }
    let st = crate::no_window::command(env!("CARGO"))
        .args(["build", "-p", "velt_rt_wasm", "--target", triple])
        .current_dir(root)
        .status()
        .expect("cargo build -p velt_rt_wasm");
    assert!(st.success(), "building velt_rt_wasm for {triple} failed");
}

/// `from`'s files below `to`, its `target/` left out.
pub fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("work dir");
    for entry in std::fs::read_dir(from).expect("fixture").flatten() {
        let path = entry.path();
        if path.is_dir() {
            if entry.file_name() != "target" {
                copy_dir(&path, &to.join(entry.file_name()));
            }
        } else {
            std::fs::copy(&path, to.join(entry.file_name())).expect("copy");
        }
    }
}
