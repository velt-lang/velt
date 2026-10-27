//! Running WebAssembly builds (`velt run --target wasm32-…`):
//! - `wasm32-wasip1`: `$VELT_WASM_RUNNER <module> <args>`, else `wasmtime run --dir=. <module>
//!   <args>` (the current directory is the program's file-system view);
//! - `wasm32-unknown-unknown`: `node velt_web.mjs <module> <args>`, with the JS glue that every
//!   browser build writes next to its module ([`write_glue`]).
//!
//! A `--host workers` build writes the Worker glue instead: `velt_worker.mjs`, `worker.mjs` (the
//! Worker's entry) and a `wrangler.toml` (kept once it exists, so it can be edited).

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

/// The JS glue for browser builds (see the file's header).
const GLUE: &str = include_str!("../../../velt_rt_wasm/js/velt_web.mjs");
/// File name of the glue next to a browser module.
pub const GLUE_FILE: &str = "velt_web.mjs";
/// The JS glue of a Worker (`--host workers`; see the file's header).
const WORKER_GLUE: &str = include_str!("../../../velt_rt_wasm/js/velt_worker.mjs");
/// File name of the Worker glue next to the module.
pub const WORKER_GLUE_FILE: &str = "velt_worker.mjs";
/// The Worker's entry, which `wrangler.toml` names.
pub const WORKER_ENTRY_FILE: &str = "worker.mjs";
/// The compatibility date a new `wrangler.toml` gets.
const COMPATIBILITY_DATE: &str = "2026-10-01";

/// Whether `target` is a WebAssembly target.
pub fn is_wasm(target: &str) -> bool {
    velt_codegen_llvm::is_wasm(target)
}

fn is_browser(target: &str) -> bool {
    target.trim() == "wasm32-unknown-unknown"
}

/// After a browser build: write the glue next to the module (other targets: nothing); the
/// Worker files for a `--host workers` build.
pub fn write_glue(target: &str, host: Option<&str>, module: &Path) -> Result<(), String> {
    if host == Some("workers") {
        return write_worker(module);
    }
    if !is_browser(target) {
        return Ok(());
    }
    write_file(&module.with_file_name(GLUE_FILE), GLUE)
}

/// The Worker files next to `module`: the glue, the entry, and `wrangler.toml` if there is none.
fn write_worker(module: &Path) -> Result<(), String> {
    let file = module
        .file_name()
        .map_or_else(|| "main.wasm".into(), |f| f.to_string_lossy().into_owned());
    write_file(&module.with_file_name(WORKER_GLUE_FILE), WORKER_GLUE)?;
    let entry = format!(
        "// The Worker's entry (written by `velt build --host workers`).\n\
         import module from \"./{file}\";\n\
         import {{ createWorker }} from \"./{WORKER_GLUE_FILE}\";\n\
         export default createWorker(module);\n"
    );
    write_file(&module.with_file_name(WORKER_ENTRY_FILE), &entry)?;
    let config = module.with_file_name("wrangler.toml");
    if config.exists() {
        return Ok(());
    }
    let stem = module
        .file_stem()
        .map_or_else(|| "main".into(), |s| s.to_string_lossy().into_owned());
    write_file(
        &config,
        &format!(
            "name = \"{}\"\nmain = \"{WORKER_ENTRY_FILE}\"\ncompatibility_date = \"{COMPATIBILITY_DATE}\"\n",
            worker_name(&stem)
        ),
    )
}

/// A Worker name from a file stem: lowercase letters, digits and `-`.
fn worker_name(stem: &str) -> String {
    let name: String = stem
        .chars()
        .map(|c| match c.to_ascii_lowercase() {
            c @ ('a'..='z' | '0'..='9') => c,
            _ => '-',
        })
        .collect();
    let name = name.trim_matches('-');
    if name.is_empty() {
        "velt-worker".into()
    } else {
        name.into()
    }
}

fn write_file(path: &Path, text: &str) -> Result<(), String> {
    std::fs::write(path, text).map_err(|e| format!("cannot write `{}`: {e}", path.display()))
}

/// The command that runs `module` with `args`.
pub fn runner(target: &str, module: &Path, args: &[OsString]) -> Result<Command, String> {
    if is_browser(target) {
        let mut cmd = Command::new("node");
        cmd.arg(vpm::relpath::plain(&module.with_file_name(GLUE_FILE)))
            .arg(vpm::relpath::plain(module))
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
        write_glue("wasm32-wasip1", None, &module).unwrap();
        assert!(!dir.path().join(GLUE_FILE).exists());
        write_glue("wasm32-unknown-unknown", None, &module).unwrap();
        let glue = std::fs::read_to_string(dir.path().join(GLUE_FILE)).unwrap();
        assert!(glue.contains("export async function runVelt"));
        let cmd = runner("wasm32-unknown-unknown", &module, &["x".into()]).unwrap();
        assert_eq!(cmd.get_program(), "node");
        assert_eq!(cmd.get_args().count(), 3);
    }

    #[test]
    fn worker_files_for_the_workers_host() {
        let dir = tempfile::tempdir().unwrap();
        let module = dir.path().join("My_App.wasm");
        write_glue("wasm32-unknown-unknown", Some("workers"), &module).unwrap();
        assert!(!dir.path().join(GLUE_FILE).exists());
        let entry = std::fs::read_to_string(dir.path().join(WORKER_ENTRY_FILE)).unwrap();
        assert!(entry.contains("import module from \"./My_App.wasm\";"));
        let glue = std::fs::read_to_string(dir.path().join(WORKER_GLUE_FILE)).unwrap();
        assert!(glue.contains("export function createWorker"));
        let config = dir.path().join("wrangler.toml");
        assert!(std::fs::read_to_string(&config)
            .unwrap()
            .starts_with("name = \"my-app\"\nmain = \"worker.mjs\"\n"));
        // An edited wrangler.toml is kept.
        std::fs::write(&config, "name = \"mine\"\n").unwrap();
        write_glue("wasm32-unknown-unknown", Some("workers"), &module).unwrap();
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            "name = \"mine\"\n"
        );
    }
}
