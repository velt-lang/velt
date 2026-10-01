//! Running WebAssembly builds (`velt run --target wasm32-…`):
//! - `wasm32-wasip1`: `$VELT_WASM_RUNNER <module> <args>`, else `wasmtime run --dir=. <module>
//!   <args>` (the current directory is the program's file-system view);
//! - `wasm32-unknown-unknown`: `node velt_web.mjs <module> <args>`, with the JS glue that every
//!   browser build writes next to its module ([`write_glue`]).

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

/// The JS glue for browser builds (see the file's header).
const GLUE: &str = include_str!("../../../velt_rt_wasm/js/velt_web.mjs");
/// File name of the glue next to a browser module.
pub const GLUE_FILE: &str = "velt_web.mjs";

/// Whether `target` is a WebAssembly target.
pub fn is_wasm(target: &str) -> bool {
    velt_codegen_llvm::is_wasm(target)
}

fn is_browser(target: &str) -> bool {
    target.trim() == "wasm32-unknown-unknown"
}

/// After a browser build: write the glue next to the module (other targets: nothing).
pub fn write_glue(target: &str, module: &Path) -> Result<(), String> {
    if !is_browser(target) {
        return Ok(());
    }
    let path = module.with_file_name(GLUE_FILE);
    std::fs::write(&path, GLUE).map_err(|e| format!("cannot write `{}`: {e}", path.display()))
}

/// The command that runs `module` with `args`.
pub fn runner(target: &str, module: &Path, args: &[OsString]) -> Result<Command, String> {
    if is_browser(target) {
        let mut cmd = Command::new("node");
        cmd.arg(module.with_file_name(GLUE_FILE))
            .arg(module)
            .args(args);
        return Ok(cmd);
    }
    if let Some(runner) = std::env::var_os("VELT_WASM_RUNNER").filter(|r| !r.is_empty()) {
        let mut cmd = Command::new(runner);
        cmd.arg(module).args(args);
        return Ok(cmd);
    }
    let found = Command::new("wasmtime")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !found {
        return Err(
            "running wasm32-wasip1 programs needs wasmtime (https://wasmtime.dev: \
                    `brew install wasmtime`, or `curl https://wasmtime.dev/install.sh -sSf | \
                    bash`), or set $VELT_WASM_RUNNER"
                .into(),
        );
    }
    let mut cmd = Command::new("wasmtime");
    cmd.args(["run", "--dir=."]).arg(module).args(args);
    Ok(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glue_only_for_browser_builds() {
        let dir = tempfile::tempdir().unwrap();
        let module = dir.path().join("p.wasm");
        write_glue("wasm32-wasip1", &module).unwrap();
        assert!(!dir.path().join(GLUE_FILE).exists());
        write_glue("wasm32-unknown-unknown", &module).unwrap();
        let glue = std::fs::read_to_string(dir.path().join(GLUE_FILE)).unwrap();
        assert!(glue.contains("export async function runVelt"));
        let cmd = runner("wasm32-unknown-unknown", &module, &["x".into()]).unwrap();
        assert_eq!(cmd.get_program(), "node");
        assert_eq!(cmd.get_args().count(), 3);
    }
}
