//! Which changes the merge queue checks on Windows and macOS too, and which the push to `main`
//! checks there afterwards (`crates/xtask/src/os.rs`).

use super::*;

#[test]
fn main_skips_the_windows_and_macos_gate_the_queue_already_ran() {
    // The queue checked every OS: the push to main doesn't check them again.
    for path in [
        "crates/velt_rt/src/http.rs",
        "crates/veltc/src/main.rs",
        "std/fs/index.vlt",
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
fn os_specific_changes_check_windows_and_macos_in_the_queue() {
    for path in [
        "crates/velt_rt/src/http.rs",
        "crates/veltc/src/main.rs",
        // Terminal echo, signals and file permissions differ by OS (#315).
        "crates/veltc/src/commands/registry.rs",
        "crates/vpm/src/credentials.rs",
        "crates/vpm/src/json_file.rs",
        "std/fs/index.vlt",
        "std/net/tcp_stream.vlt",
        "std/prelude/timers.vlt",
        // The standard library's `velt test` suites run servers, sockets and files (#1892).
        "tests/std/http/responses.test.vlt",
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
        // Windows and macOS after the push to `main` catches the exception.
        "tests/golden/lang/foo.out",
        "std/collections/map.vlt",
        "std/half/index.vlt",
        "examples/apps/shop-api/src/main.vlt",
        "packages/sqlite/src/db.vlt",
    ] {
        assert_eq!(plan(&[path]).other_os, None, "{path}");
    }
}
