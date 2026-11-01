//! The goldens on WebAssembly: every `tests/golden/{m1,m2,m3,m4,lang}/*.vlt` with a `.out`
//! file, except the ones using sockets or threads-only features (see [`UNSUPPORTED`]), built
//! with `--target wasm32-wasip1` (debug and `--release`) and run under wasmtime by
//! `velt run`, must print exactly that output and exit with its `.code` (default 0). The M1, M2
//! and M3 (async) goldens also run as browser modules (`wasm32-unknown-unknown`) through the JS
//! glue under node, in the turns it drives.
//!
//! Needs the rustup targets (`rustup target add wasm32-wasip1 wasm32-unknown-unknown`), LLVM's
//! `opt`/`llc` (`rustup component add llvm-tools`), wasmtime and node; each part is skipped with
//! a note when its tools are missing. Filter with `VELT_GOLDEN=<substring>` (several separated
//! by `,`). With `VELT_RT_WASM_PREBUILT=1` (CI's shards, which run this test from an archive of
//! another job's build: crates/xtask/src/golden_archive.rs) the runtime libraries are already
//! built, and a missing tool fails the test instead of skipping it.
//!
//! The wasm32-wasip1 goldens are split into [`SHARDS`] test functions (every `SHARDS`-th file
//! of the sorted list), so the test runner spreads them over its threads with everything else
//! instead of one test running them all at the end.

mod no_window;
mod work_dir;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Goldens that cannot run on WebAssembly: the runtime has no sockets (nor `velt/http`), a
/// `resolve` called later, or a shared promise settling, resumes its waiters at the executor's
/// next step instead of queueing them as microtasks (https://veltlang.com/docs/reference/async/).
const UNSUPPORTED: &[&str] = &[
    "tcp_echo",
    "http_server",
    "console_std_handles",
    "await_order_resolve",
    "promise_shared_inflight",
    "promise_shared_orphan",
    "promise_shared_orphan_task",
    "promise_shared_sync_main",
    "promise_shared_sync_main_module",
    // A shared promise settling resumes its waiters at the executor's next step (the order of
    // the log), and a converted promise's rejection is reported as its own (no views, #1682).
    "promise_identity_bounded",
    "promise_identity_converted_rejects",
    "promise_identity_defer",
    "promise_identity_defer_rejects",
    "promise_identity_defer_twice",
    "promise_identity_rejection",
];
/// Goldens that need a file system (not available to browser modules).
const NEEDS_FS: &[&str] = &["fs"];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

/// `path` as node takes it. `canonicalize` gives a verbatim path (`\\?\E:\…`) on Windows, which
/// node can't run as its main module (`EISDIR: illegal operation on a directory, lstat 'E:'`)
/// and `pathToFileURL` turns into a wrong URL.
fn for_node(path: &Path) -> PathBuf {
    match path.to_str().and_then(|p| p.strip_prefix(r"\\?\")) {
        Some(plain) if !plain.starts_with(r"UNC\") => PathBuf::from(plain),
        _ => path.to_path_buf(),
    }
}

fn has_rust_target(triple: &str) -> bool {
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

fn runs(program: &str) -> bool {
    crate::no_window::command(program)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Whether the runtime libraries are prebuilt (`VELT_RT_WASM_PREBUILT=1`).
fn prebuilt() -> bool {
    std::env::var_os("VELT_RT_WASM_PREBUILT").is_some_and(|v| v == "1")
}

/// Why this flavor cannot be tested here, if it cannot. A run with prebuilt runtimes exists to
/// run these goldens: there a missing tool fails.
fn missing_tools(triple: &str, runner: &str) -> Option<String> {
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
fn build_runtime(root: &Path, triple: &str) {
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

/// Does the program declare a system function? WebAssembly has none (declaring one is a
/// compile error there), so such a golden never runs on it.
fn declares_system_functions(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|s| s.contains("declare system function"))
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
                && !declares_system_functions(p)
                && {
                    // The path in the checkout, as in tests/golden.rs.
                    let rel = p.strip_prefix(root).unwrap_or(p);
                    let name = rel.to_string_lossy().replace('\\', "/");
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
    let o = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
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

/// Run every (file, mode) pair on `threads` threads. Each pair builds in a directory of its own,
/// removed afterwards: a build never overwrites a module that has just run (on Windows a file
/// that was just in use can stay locked for a moment), whether it is the other mode of the same
/// program or a program with the same name from another directory.
fn run_all(
    files: &[PathBuf],
    target: &str,
    modes: &[Option<&str>],
    work: &Path,
    threads: usize,
) -> Vec<String> {
    let jobs: Vec<(&PathBuf, Option<&str>)> = files
        .iter()
        .flat_map(|f| modes.iter().map(move |m| (f, *m)))
        .collect();
    let next = Mutex::new(jobs.into_iter().enumerate());
    let failures = Mutex::new(vec![]);
    std::thread::scope(|s| {
        for _ in 0..threads {
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

/// How many test functions share the wasm32-wasip1 goldens.
const SHARDS: usize = 8;

/// The wasm32-wasip1 goldens: every file with an `.out` in these directories,
const WASI_DIRS: &[&str] = &["m1", "m2", "m3", "m4", "lang"];
/// and these of `std/`: the SIMD programs, whose vectors are WebAssembly `v128` values there,
/// and the std code written with them.
const WASI_STD: &[&str] = &[
    "bytes_search",
    "simd_floats",
    "simd_generic",
    "simd_ints",
    "simd_lane_range",
    "simd_load_range",
    "simd_mul_add",
    "simd_names",
    "simd_positions",
    "simd_sha",
    "simd_shuffle",
    "simd_store_range",
    "simd_values",
    "typed_arrays_basic",
    "typed_arrays_edges",
    "utf8_valid",
];

/// The files of shard `shard`: every `SHARDS`-th one of the sorted list, so each shard gets a
/// mix of directories and about the same number of files.
fn shard_of(files: &[PathBuf], shard: usize) -> Vec<PathBuf> {
    files
        .iter()
        .enumerate()
        .filter(|(i, _)| i % SHARDS == shard)
        .map(|(_, f)| f.clone())
        .collect()
}

/// Runs one shard of the wasm32-wasip1 goldens, debug and `--release`.
fn wasi_shard(shard: usize) {
    if let Some(missing) = missing_tools("wasm32-wasip1", "wasmtime") {
        eprintln!("note: {missing} not available; skipping the wasm32-wasip1 goldens");
        return;
    }
    let root = root();
    build_runtime(&root, "wasm32-wasip1");
    let mut files = programs(&root, WASI_DIRS, UNSUPPORTED);
    let std_files = programs(&root, &["std"], &[]);
    files.extend(std_files.into_iter().filter(|p| {
        let stem = p.file_stem().unwrap_or_default().to_string_lossy();
        WASI_STD.contains(&stem.as_ref())
    }));
    files.sort();
    let files = shard_of(&files, shard);
    // A directory of this process's own: shards run at the same time, and a path that a
    // crashed earlier run may still hold is never reused.
    let work = work_dir::work_dir(&root, "golden-work-wasi")
        .join(format!("shard{shard}-{}", std::process::id()));
    let failures = run_all(
        &files,
        "wasm32-wasip1",
        &[None, Some("--release")],
        &work,
        2,
    );
    let _ = std::fs::remove_dir_all(&work);
    println!(
        "wasm32-wasip1 goldens, shard {shard} of {SHARDS}: {} files, {} failures",
        files.len(),
        failures.len()
    );
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

macro_rules! wasi_shards {
    ($($name:ident = $shard:literal),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                wasi_shard($shard);
            }
        )*
        /// Every shard has a test function.
        const SHARD_TESTS: &[usize] = &[$($shard),*];
    };
}

wasi_shards! {
    goldens_under_wasmtime_shard_0 = 0,
    goldens_under_wasmtime_shard_1 = 1,
    goldens_under_wasmtime_shard_2 = 2,
    goldens_under_wasmtime_shard_3 = 3,
    goldens_under_wasmtime_shard_4 = 4,
    goldens_under_wasmtime_shard_5 = 5,
    goldens_under_wasmtime_shard_6 = 6,
    goldens_under_wasmtime_shard_7 = 7,
}

/// The shards together run every wasm32-wasip1 golden exactly once.
#[test]
fn wasmtime_shards_cover_every_golden_once() {
    assert_eq!(SHARD_TESTS, (0..SHARDS).collect::<Vec<_>>());
    let root = root();
    let files = programs(&root, WASI_DIRS, UNSUPPORTED);
    let mut sharded: Vec<PathBuf> = (0..SHARDS).flat_map(|s| shard_of(&files, s)).collect();
    assert_eq!(sharded.len(), files.len(), "a golden is in two shards");
    sharded.sort();
    assert_eq!(sharded, files, "the shards miss a golden");
    if std::env::var_os("VELT_GOLDEN").is_none() {
        assert!(
            files.len() > SHARDS * 10,
            "found only {} wasm32-wasip1 goldens",
            files.len()
        );
    }
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
    let files = programs(&root, &["m1", "m2", "m3"], &skip);
    let work = work_dir::work_dir(&root, "golden-work-web");
    let failures = run_all(&files, "wasm32-unknown-unknown", &[None], &work, 4);
    let _ = std::fs::remove_dir_all(&work);
    println!(
        "browser goldens: {} files, {} failures",
        files.len(),
        failures.len()
    );
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

/// Host functions (https://veltlang.com/docs/reference/host-functions/) under node: tests/wasm_host/main.vlt, built
/// for wasm32-unknown-unknown in debug and release mode, calls the host functions host.mjs
/// passes with every type that crosses, holds and drops `HostRef`s (host.mjs checks the glue's
/// table is empty at the end), and fails to start without the functions.
#[test]
fn host_functions_under_node() {
    if let Some(missing) = missing_tools("wasm32-unknown-unknown", "node") {
        eprintln!("note: {missing} not available; skipping the host functions test");
        return;
    }
    let root = root();
    build_runtime(&root, "wasm32-unknown-unknown");
    let dir = root.join("crates/veltc/tests/wasm_host");
    let want = std::fs::read_to_string(dir.join("main.out"))
        .expect("main.out")
        .replace("\r\n", "\n");
    let work = work_dir::work_dir(&root, "golden-work-host");
    for mode in [None, Some("--release")] {
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work).expect("work dir");
        let module = work.join("main.wasm");
        let o = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
            .args(["build", "--target", "wasm32-unknown-unknown"])
            .args(mode)
            .arg(dir.join("main.vlt"))
            .arg("-o")
            .arg(&module)
            .output()
            .expect("run velt");
        assert!(
            o.status.success(),
            "velt build {mode:?}:\n{}",
            String::from_utf8_lossy(&o.stderr)
        );
        let o = crate::no_window::command("node")
            .arg(for_node(&dir.join("host.mjs")))
            .arg(for_node(&work.join("velt_web.mjs")))
            .arg(for_node(&module))
            .output()
            .expect("run node");
        let got = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
        assert!(
            o.status.success() && got == want,
            "{mode:?}: exit {:?}\n--- want ---\n{want}--- got ---\n{got}--- stderr ---\n{}",
            o.status.code(),
            String::from_utf8_lossy(&o.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&work);
}

/// Async host functions and the turns the host drives (rt_abi_async.md §1.4, §2.5) under node:
/// tests/wasm_host_async/main.vlt awaits host promises of every result type, a rejection as an
/// `IoError`, promises settled out of order and interleaved with timers, drops a disposable and
/// a `HostRef` held across awaits, and sleeps while the host's event loop runs (host.mjs counts
/// its ticks); uncaught.vlt ends with a rejection nothing catches after it waited; ends.vlt,
/// stuck.vlt and sync_main.vlt end in later turns with `main`'s code, `process.exit`, exit code
/// 13, a spawned task waiting for a host promise after `main`, and an uncaught error reported at
/// once while a host promise is pending;
/// many.vlt runs thousands of turns, after which the wasm stack
/// pointer and the memory are as they were (many.mjs); order.vlt's promises, microtasks and
/// timers print what the native build prints. Debug and release mode.
#[test]
fn async_host_functions_under_node() {
    if let Some(missing) = missing_tools("wasm32-unknown-unknown", "node") {
        eprintln!("note: {missing} not available; skipping the async host functions test");
        return;
    }
    let root = root();
    build_runtime(&root, "wasm32-unknown-unknown");
    let dir = root.join("crates/veltc/tests/wasm_host_async");
    let read = |name: &str| {
        std::fs::read_to_string(dir.join(name))
            .expect("expected output")
            .replace("\r\n", "\n")
    };
    let work = work_dir::work_dir(&root, "golden-work-host-async");
    let velt = || crate::no_window::command(env!("CARGO_BIN_EXE_velt"));
    // Built in the work directory: a build leaves its files next to the program.
    std::fs::create_dir_all(&work).expect("work dir");
    std::fs::copy(dir.join("order.vlt"), work.join("order.vlt")).expect("copy order.vlt");
    let native = velt()
        .arg("run")
        .arg(work.join("order.vlt"))
        .output()
        .expect("run velt");
    let order = String::from_utf8_lossy(&native.stdout).replace("\r\n", "\n");
    assert_eq!(
        order,
        read("order.out"),
        "the native build's order; stderr:\n{}",
        String::from_utf8_lossy(&native.stderr)
    );
    for mode in [None, Some("--release")] {
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work).expect("work dir");
        let host = || Some(dir.join("host.mjs"));
        // ends.vlt, by what `how()` returns: the end and the exit code.
        let ends = [
            ("return", "exit 7"),
            ("exit", "exit 3"),
            ("stuck", "exit 0"),
            ("spawn", "background got 42\nexit 0"),
            ("uncaught", "Uncaught Error: bad at ends.vlt:18:3\nexit 1"),
        ]
        .map(|(how, end)| {
            let want = format!("ending by {how}\n{end}, 0 values held\n");
            ("ends", host(), Some(how), want)
        });
        for (program, runner, how, want) in ends.into_iter().chain([
            ("main", host(), None, read("main.out")),
            ("uncaught", host(), None, read("uncaught.out")),
            ("sync_main", host(), None, read("sync_main.out")),
            ("stuck", host(), None, read("stuck.out")),
            ("many", Some(dir.join("many.mjs")), None, read("many.out")),
            ("order", None, None, order.clone()),
        ]) {
            let module = work.join(format!("{program}.wasm"));
            // Built where it was copied, so messages name it by its file name.
            let source = format!("{program}.vlt");
            std::fs::copy(dir.join(&source), work.join(&source)).expect("copy the program");
            let o = velt()
                .current_dir(&work)
                .args(["build", "--target", "wasm32-unknown-unknown"])
                .args(mode)
                .arg(&source)
                .arg("-o")
                .arg(&module)
                .output()
                .expect("run velt");
            assert!(
                o.status.success(),
                "velt build {program} {mode:?}:\n{}",
                String::from_utf8_lossy(&o.stderr)
            );
            let o = crate::no_window::command("node")
                .args(runner)
                .arg(work.join("velt_web.mjs"))
                .arg(&module)
                .args(how)
                .output()
                .expect("run node");
            let got = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
            assert!(
                o.status.success() && got == want,
                "{program} {how:?} {mode:?}: exit {:?}\n--- want ---\n{want}--- got ---\n{got}--- stderr ---\n{}",
                o.status.code(),
                String::from_utf8_lossy(&o.stderr)
            );
        }
    }
    let _ = std::fs::remove_dir_all(&work);
}
