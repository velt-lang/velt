//! An async program's WebAssembly module links none of velt_rt_wasm's own executor (`task/`),
//! which no program runs there since the executor in Velt does (#3905), on WASI (a JS host's programs still run on it):
//! the runtime's start no longer runs its tasks after `main`, and the executor's start-up on one
//! thread calls none of its functions. Read in the `--release` module's name section (LLVM drops
//! the start-up's calls behind the constant; a debug build keeps them), when the runtime it links
//! keeps one. Each part is skipped with a note when its tools are missing.

mod no_window;
#[allow(dead_code)]
mod wasm_tools;
mod work_dir;

use wasm_tools::{build_runtime, missing_tools, root};

#[test]
fn wasm_modules_link_no_runtime_executor() {
    let root = root();
    for (target, runner) in [("wasm32-wasip1", "wasmtime")] {
        if let Some(missing) = missing_tools(target, runner) {
            eprintln!("note: {missing} not available; skipping the check on {target}");
            continue;
        }
        build_runtime(&root, target);
        // A directory of this process's own: a build leaves its object next to the program.
        let work = work_dir::work_dir(&root, "golden-work-no-rt-exec")
            .join(format!("{target}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work).expect("work dir");
        let module = work.join("async_basic.wasm");
        let o = no_window::command(env!("CARGO_BIN_EXE_velt"))
            .args(["build", "--release", "--target", target])
            .arg(root.join("tests/golden/m3/async_basic.velt"))
            .arg("-o")
            .arg(&module)
            .current_dir(&work)
            .output()
            .expect("run velt");
        assert!(
            o.status.success(),
            "{target}: velt build: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        let bytes = std::fs::read(&module).expect("the module");
        let _ = std::fs::remove_dir_all(&work);
        let has = |s: &[u8]| bytes.windows(s.len()).any(|w| w == s);
        if !has(b"velt_main") {
            eprintln!("note: {target}'s module has no name section; skipping the check");
            continue;
        }
        assert!(
            !has(b"velt_rt_wasm::task::"),
            "{target}: the module links velt_rt_wasm's executor"
        );
    }
}
