//! The command-line arguments on WebAssembly, apart from wasm_goldens.rs (its size).

use super::own_build_dir;
use super::wasm_tools::{build_runtime, missing_tools, root};

/// The command-line arguments on WebAssembly (std/process/args_wasi.velt, args_none.velt):
/// under WASI the host's, the module's path first, so `process.argv` is that path twice and
/// `argv()` has it once; in the browser none at all, so `process.argv` is `["", ""]` (Node's
/// shape) and `argv()` and `args()` are empty, as the runtime's `std::env::args_os()` was there.
#[test]
fn arguments_on_webassembly() {
    let root = root();
    let main = root.join("crates/veltc/tests/wasm_args/main.velt");
    for (target, runner, want) in [
        ("wasm32-wasip1", "wasmtime", "[2,true,1,0]\n"),
        ("wasm32-unknown-unknown", "node", "[2,true,0,0]\n"),
    ] {
        if let Some(missing) = missing_tools(target, runner) {
            eprintln!("note: {missing} not available; skipping the arguments on {target}");
            continue;
        }
        build_runtime(&root, target);
        let work = own_build_dir(&root, "golden-work-args", target);
        let o = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
            .args(["run", "--target", target])
            .arg(&main)
            .current_dir(&work)
            .output()
            .expect("run velt");
        let _ = std::fs::remove_dir_all(&work);
        assert!(
            o.status.success(),
            "{target}: exit {:?}\n{}",
            o.status,
            String::from_utf8_lossy(&o.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n"),
            want,
            "{target}"
        );
    }
}
