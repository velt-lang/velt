//! Which changes the merge queue checks on Windows or macOS too, and which the push to `main`
//! checks there afterwards (`crates/xtask/src/os.rs`).

use super::*;

#[test]
fn main_adds_no_windows_or_macos_gate_after_a_queue_run() {
    // The queue checked what the change needs, and the scheduled run checks the rest: the push to
    // main adds nothing, with or without macOS in the queue.
    for path in [
        "crates/velt_loader/src/lib.rs",
        "crates/velt_rt/src/http.rs",
        "crates/veltc/src/main.rs",
        "std/fs/index.velt",
        ".github/workflows/ci.yml",
        "scripts/check-all.sh",
        "crates/xtask/src/plan.rs",
        "Cargo.toml",
    ] {
        let p = plan(&[path]);
        assert!(p.other_os.is_some(), "{path}");
        assert_eq!(p.after_merge, None, "{path}");
    }
    let p = plan(&[
        "crates/velt_sema/src/check.rs",
        "crates/velt_rt/src/http.rs",
    ]);
    assert_eq!(p.after_merge, None);
}

#[test]
fn main_leaves_windows_and_macos_to_the_scheduled_run() {
    for path in [
        "crates/velt_sema/src/check.rs",
        "crates/velt_lsp/src/hover.rs",
        "docs/internals/design/semantics.md",
        "CONTRIBUTING.md",
        "tests/golden/lang/foo.out",
    ] {
        let p = plan(&[path]);
        assert_eq!(p.other_os, None, "{path}");
        assert_eq!(p.after_merge, None, "{path}");
    }
    // Comments only in OS-specific code: the queue checked Linux only.
    let paths = vec!["crates/velt_rt/src/http/server.rs".to_string()];
    let p = Plan::for_changes(&graph(), &paths, &paths.iter().cloned().collect());
    assert_eq!(p.after_merge, None);
}

#[test]
fn main_runs_the_windows_and_macos_gate_when_the_build_cache_changes() {
    // Every OS in the queue, but only a push to main saves the caches the toolchain and the
    // dependencies key.
    for path in [
        "Cargo.lock",
        "rust-toolchain.toml",
        ".cargo/config.toml",
        ".github/actions/setup/action.yml",
    ] {
        let p = plan(&[path]);
        assert!(p.other_os.is_some(), "{path}");
        let reason = p.after_merge.unwrap_or_default();
        assert!(reason.contains("build cache"), "{path}: {reason}");
    }
    let p = plan(&["crates/velt_rt/src/http.rs", "Cargo.lock"]);
    assert!(p.after_merge.is_some());
}

#[test]
fn main_runs_the_windows_and_macos_gate_when_the_change_is_unknown() {
    // Nobody knows which OSes the queue checked.
    assert!(Plan::unknown_changes("no base revision")
        .after_merge
        .is_some());
    assert!(Plan::everything("--full").after_merge.is_some());
}

#[test]
fn os_specific_changes_check_windows_in_the_queue() {
    for path in [
        "crates/velt_rt/src/http.rs",
        "crates/veltc/src/main.rs",
        // Terminal echo, signals and file permissions differ by OS (#315).
        "crates/veltc/src/commands/registry.rs",
        "crates/vpm/src/credentials.rs",
        "crates/vpm/src/json_file.rs",
        "std/fs/index.velt",
        "std/net/tcp_stream.velt",
        "std/prelude/timers.velt",
        // The standard library's `velt test` suites run servers, sockets and files (#1892).
        "tests/std/http/responses.test.velt",
        "Cargo.lock",
        ".github/workflows/ci.yml",
    ] {
        assert!(plan(&[path]).other_os.is_some(), "{path}");
    }
    // A crate the rules don't know counts as OS-specific.
    let unknown = Graph::from_manifests(&[(
        "velt_new".to_string(),
        "[package]\nname = \"velt_new\"\n".to_string(),
    )]);
    let paths = vec!["crates/velt_new/src/lib.rs".to_string()];
    assert!(Plan::for_paths(&unknown, &paths).other_os.is_some());
}

#[test]
fn portable_changes_check_linux_only() {
    for path in [
        "crates/velt_sema/src/check.rs",
        "crates/velt_lsp/src/hover.rs",
        "crates/velt_fmt/src/jsx.rs",
        "docs/internals/design/semantics.md",
        "CONTRIBUTING.md",
        // Goldens, std code that doesn't talk to the OS, examples and packages: the full gate on
        // Windows and macOS in the scheduled run catches the exception.
        "tests/golden/lang/foo.out",
        "std/collections/map.velt",
        "std/half/index.velt",
        "examples/apps/shop-api/src/main.velt",
        "packages/sqlite/src/db.velt",
    ] {
        assert_eq!(plan(&[path]).other_os, None, "{path}");
    }
}

#[test]
fn most_os_specific_changes_check_windows_but_not_macos() {
    for path in [
        "crates/velt_loader/src/lib.rs",
        "crates/velt_loader/Cargo.toml",
        "crates/veltc/src/main.rs",
        "crates/xtask/src/plan.rs",
        "crates/vpm/src/credentials.rs",
        "Cargo.lock",
        "Cargo.toml",
        "rust-toolchain.toml",
        ".github/workflows/ci.yml",
        ".github/actions/setup/action.yml",
        "scripts/check.sh",
        "std/test/run.velt",
    ] {
        let p = plan(&[path]);
        assert!(p.other_os.is_some(), "{path}");
        assert_eq!(p.macos, None, "{path}");
    }
    // A crate the rules don't know: Windows, not macOS.
    let unknown = Graph::from_manifests(&[(
        "velt_new".to_string(),
        "[package]\nname = \"velt_new\"\n".to_string(),
    )]);
    let paths = vec!["crates/velt_new/src/lib.rs".to_string()];
    let p = Plan::for_paths(&unknown, &paths);
    assert!(p.other_os.is_some());
    assert_eq!(p.macos, None);
}

#[test]
fn code_that_can_differ_on_macos_checks_it() {
    for path in [
        "crates/velt_rt/src/http.rs",
        "crates/velt_rt_host/src/lib.rs",
        "crates/velt_rt_shared/tests/install_name.rs",
        "crates/velt_link/src/system.rs",
        "crates/velt_toolchain/src/release.rs",
        "crates/velt_codegen_cl/src/isa.rs",
        "crates/velt_codegen_llvm/src/target.rs",
        "crates/velt_vir/src/c_abi.rs",
        "crates/velt_vir/src/simd/ext.rs",
        "crates/velt_text/src/json_escape.rs",
        "crates/vpm/src/native/build.rs",
        "crates/veltc/src/dev/supervisor.rs",
        "std/fs/index.velt",
        "std/net/tcp_stream.velt",
        "std/os/machine.velt",
        "std/process/child/exec.velt",
        "std/native/index.velt",
        "std/host/index.velt",
        "std/task/index.velt",
        "std/time/index.velt",
        "std/simd/bytes.velt",
        "std/prelude/timers.velt",
        "tests/std/net/x.test.velt",
        "std/fs/fd/darwin.velt",
        "packages/http-server/src/files/resolve.velt",
        "packages/http-server/tests/files.test.velt",
    ] {
        let reason = plan(&[path]).macos.unwrap_or_default();
        assert!(reason.contains(path), "{path}: {reason}");
    }
}

#[test]
fn macos_is_decided_apart_from_windows_and_from_everything() {
    // Code in a package: macOS, not Windows.
    let p = plan(&["packages/http-server/src/files/resolve.velt"]);
    assert!(p.macos.is_some());
    assert_eq!(p.other_os, None);
    // Runtime code next to a path that selects everything, in either order.
    for paths in [
        [".github/workflows/ci.yml", "crates/velt_rt/src/http.rs"],
        ["crates/velt_rt/src/http.rs", ".github/workflows/ci.yml"],
    ] {
        let p = plan(&paths);
        assert!(p.full);
        assert!(p.macos.is_some(), "{paths:?}");
    }
    // No change known, or `--full`, or the label `ci:full`: every OS.
    assert!(Plan::everything("--full").macos.is_some());
    assert!(Plan::unknown_changes("no base").macos.is_some());
}

#[test]
fn what_does_not_change_such_code_checks_no_macos() {
    for path in [
        "crates/velt_rt/NATIVE_LIBS.md",
        "crates/velt_rt_wasm/src/lib.rs",
        "docs/internals/maintaining.md",
        "CONTRIBUTING.md",
        "tests/golden/lang/foo.out",
        "std/collections/map.velt",
        "packages/sqlite/src/db.velt",
        "packages/http-server/src/serve.velt",
    ] {
        assert_eq!(plan(&[path]).macos, None, "{path}");
    }
    // Comments only, in the runtime, also next to a CI change.
    let runtime = "crates/velt_rt/src/http/server.rs".to_string();
    let comments = [runtime.clone()].into_iter().collect();
    let alone = Plan::for_changes(&graph(), std::slice::from_ref(&runtime), &comments);
    assert_eq!(alone.macos, None);
    let paths = vec![runtime, ".github/workflows/ci.yml".to_string()];
    assert_eq!(Plan::for_changes(&graph(), &paths, &comments).macos, None);
}

#[test]
fn the_plan_says_why_macos_is_in_or_out() {
    let oses = |path: &str| {
        let text = plan(&[path]).describe();
        let line = text.lines().find(|l| l.contains("merge queue OSes"));
        line.unwrap_or_default().to_string()
    };
    let line = oses("crates/velt_rt/src/http.rs");
    assert!(line.contains("Windows ("), "{line}");
    assert!(
        line.contains("macOS (crates/velt_rt/src/http.rs can behave differently"),
        "{line}"
    );
    let line = oses("crates/velt_loader/src/lib.rs");
    assert!(line.contains("Windows ("), "{line}");
    assert!(
        line.contains("no macOS (") && line.contains("scheduled run"),
        "{line}"
    );
    let line = oses("crates/velt_sema/src/check.rs");
    assert!(line.contains("Linux; no macOS"), "{line}");
}

/// A path that moved would stop selecting macOS without anyone noticing. (`serveFile`'s files in
/// http-server are named ahead of arriving.)
#[test]
fn the_macos_paths_exist() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for path in crate::os::MACOS_PATHS {
        if path.starts_with("packages/http-server/") {
            continue;
        }
        assert!(root.join(path).exists(), "no {path}");
    }
}

/// The `wasm_os` line of the queue's plan (`$GITHUB_OUTPUT`): the OSes whose `queue-golden` job
/// gets the WebAssembly goldens' shards (ci.yml).
fn wasm_os(plan: &Plan) -> String {
    let text = crate::github_output_text(plan);
    let line = text.lines().find_map(|l| l.strip_prefix("wasm_os="));
    line.unwrap_or_else(|| panic!("no wasm_os in\n{text}"))
        .to_string()
}

/// A merge train runs `--full`, which knows no changes: every OS checks the goldens, but the
/// WebAssembly goldens run on Linux only (the module is the same on every OS; the nightly runs
/// them everywhere).
#[test]
fn a_full_queue_run_checks_the_webassembly_goldens_on_linux_only() {
    let full = Plan::everything("--full");
    let text = crate::github_output_text(&full);
    assert!(
        text.contains("\nos=[\"ubuntu-latest\",\"windows-latest\",\"macos-latest\"]\n"),
        "{text}"
    );
    assert_eq!(wasm_os(&full), r#"["ubuntu-latest"]"#);
    // The nightly reads every OS's shards from the same plan.
    let shards = text.lines().find(|l| l.starts_with("wasm_shards="));
    assert!(
        shards.is_some_and(|l| l.contains("windows-latest") && l.contains("macos-latest")),
        "{text}"
    );
    let summary = full.describe();
    assert!(
        summary.contains("WebAssembly goldens in the queue: Linux only"),
        "{summary}"
    );
    let mut skipped = Plan::everything("--full");
    skipped.skip_wasm_goldens();
    assert_eq!(wasm_os(&skipped), "[]");
}

/// What differs by OS is the host's tooling, not the module: a change to it runs the WebAssembly
/// goldens on every OS the queue checks; any other WebAssembly change on Linux only.
#[test]
fn only_webassemblys_host_side_runs_its_goldens_on_windows_and_macos() {
    let every = r#"["ubuntu-latest","windows-latest","macos-latest"]"#;
    let windows = r#"["ubuntu-latest","windows-latest"]"#;
    let linux = r#"["ubuntu-latest"]"#;
    let cases: &[(&str, &str)] = &[
        // Finding rust-lld and wasi-libc; LLVM's tools (Homebrew's on macOS): every OS is checked.
        ("crates/velt_link/src/wasm.rs", every),
        ("crates/velt_codegen_llvm/src/llc.rs", every),
        // Starting wasmtime, the test harness, the toolchain: macOS isn't checked, Windows is.
        ("crates/veltc/src/commands/wasm.rs", windows),
        ("crates/veltc/tests/wasm_goldens.rs", windows),
        ("crates/veltc/tests/wasm_tools/mod.rs", windows),
        ("rust-toolchain.toml", windows),
        // The guest: the same module everywhere, though Windows and macOS check the rest.
        ("crates/velt_codegen_llvm/src/target.rs", linux),
        ("crates/velt_rt_wasm/src/fs/paths.rs", linux),
        ("std/process/cwd_wasm.velt", linux),
        // No WebAssembly at all.
        (".github/workflows/ci.yml", "[]"),
        ("crates/velt_rt/src/timer.rs", "[]"),
    ];
    for (path, oses) in cases {
        assert_eq!(wasm_os(&plan(&[path])), *oses, "{path}");
    }
    let p = plan(&["crates/veltc/src/commands/wasm.rs"]);
    assert!(
        p.describe()
            .contains("Linux and the other OSes it checks (crates/veltc/src/commands/wasm.rs"),
        "{}",
        p.describe()
    );
    // Documentation of the host side changes no tool.
    let docs = plan(&["crates/veltc/tests/wasm_tools/README.md"]);
    assert_eq!(docs.wasm_host, None);
}

/// A path that moved would stop running the WebAssembly goldens on Windows and macOS.
#[test]
fn the_webassembly_host_paths_exist() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for path in crate::os::WASM_HOST_PATHS {
        assert!(root.join(path).exists(), "no {path}");
    }
}
