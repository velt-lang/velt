//! Whether a change needs the merge queue to check Windows or macOS too, or Linux is enough.
//!
//! The OS-specific code is in the runtime, the native crates, code generation and linking, the
//! `velt` command line and packages, and the parts of the standard library that talk to the OS; a
//! change there (or to the build, CI or scripts, which select everything) runs Windows in the
//! queue ([`other_os_reason`]). The front end, the tooling, the rest of the standard library, the
//! goldens, the examples and the packages behave the same everywhere nearly always: Linux checks
//! them, and the full gate on Windows and macOS once a day (`ci.yml`'s scheduled run) and before a
//! release (`release.yml`) catches the exception. Runners are the limit (15 at once, 5 of them
//! macOS), and a queue entry on every OS takes four times the machine time of one on Linux. A
//! push to `main` runs those OSes only for the build cache ([`after_merge_reason`]).
//!
//! macOS has a much narrower rule of its own ([`macos_reason`]): GitHub often can't supply a macOS
//! runner, and a job no runner picks up fails its queue entry after 40-50 minutes with no test
//! failing. So macOS blocks a merge only when the change touches code that really can differ
//! there. What that gives up: a break only macOS shows in any other code (`velt_loader`, most of
//! `veltc`, a dependency update, CI itself) appears in the scheduled run, after the merge, not in
//! the queue.

use crate::graph::Graph;

/// Crates without OS-specific code. A crate not listed here (a new one too) counts as
/// OS-specific.
pub(crate) const PORTABLE_CRATES: &[&str] = &[
    "velt_common",
    "velt_syntax",
    "velt_sema",
    "velt_vir",
    "velt_opt",
    "velt_fmt",
    "velt_lsp",
    "velt_doc",
    "velt_tscompat",
    "velt_lint",
    "velt_registry",
    "velt_http",
    "velt_rt_wasm",
];

/// Paths outside `crates/` whose behavior differs by OS: the parts of the standard library that
/// talk to the OS, and the standard library's `velt test` suites, which run servers, sockets and
/// files (#1892). A pull request that changes a package runs its tests on Windows
/// ([`windows_packages`]), so packages need no second look in the queue.
const OS_SPECIFIC_PATHS: &[&str] = &[
    "std/fs/",
    "std/host/",
    "std/native/",
    "std/net/",
    "std/os/",
    "std/process/",
    "std/prelude/timers.velt",
    "std/task/",
    "std/test/",
    "std/time/",
    "tests/std/",
];

/// Why Windows must be checked too, or `None` when Linux is enough. `full` is whether the paths
/// select everything (build, CI and script changes). macOS has its own rule, [`macos_reason`].
pub fn other_os_reason(graph: &Graph, paths: &[String], full: bool) -> Option<String> {
    if full {
        return Some("the change selects every check".into());
    }
    for path in paths {
        if let Some(rest) = path.strip_prefix("crates/") {
            let dir = rest.split('/').next().unwrap_or(rest);
            match graph.dirs.get(dir) {
                Some(name) if PORTABLE_CRATES.contains(&name.as_str()) => {}
                Some(name) => return Some(format!("{name} has OS-specific code")),
                None => return Some(format!("{path}: unknown crate")),
            }
        } else if OS_SPECIFIC_PATHS.iter().any(|p| path.starts_with(p)) {
            return Some(format!("{path} can behave differently by OS"));
        }
    }
    None
}

/// Paths that can really behave differently on macOS: the code with a macOS branch, or whose Unix
/// code meets a different kernel, C library or file system there, and the arm64 code, which of the
/// queue's runners only macOS runs. Code under `cfg(unix)` that does the same on both needs no
/// entry: Linux checks it. A new file with a macOS branch needs one.
pub(crate) const MACOS_PATHS: &[&str] = &[
    // The runtime, in all its builds: kqueue, signals, thread and memory calls, dylib names.
    "crates/velt_rt/",
    "crates/velt_rt_host/",
    "crates/velt_rt_shared/",
    // Linking and the toolchain: ld64, the SDK kit, the archives' target names.
    "crates/velt_link/",
    "crates/velt_toolchain/",
    // Code generation: Mach-O objects, symbol names, thread locals, unwinding, the arm64 ABI.
    "crates/velt_codegen_cl/",
    "crates/velt_codegen_llvm/",
    // The system functions' tables per OS and their arm64 lowering, and arm64's vector code.
    "crates/velt_vir/src/system/",
    "crates/velt_vir/src/lower/system.rs",
    "crates/velt_vir/src/simd/",
    "crates/velt_text/src/json_escape.rs",
    "std/simd/",
    // Native libraries are linked with Apple's flags and named `.dylib`.
    "crates/vpm/src/native/",
    // The files of the portable crates and of `veltc` that have a macOS branch.
    "crates/velt_fmt/src/print/thread_cpu.rs",
    "crates/velt_sema/tests/common/process_work.rs",
    "crates/veltc/src/commands/doctor/",
    "crates/veltc/src/dev/supervisor.rs",
    "crates/veltc/src/generate/process.rs",
    "crates/veltc/tests/debugger.rs",
    "crates/veltc/tests/install_layout.rs",
    "crates/veltc/tests/runtime_support/",
    // The standard library where it talks to the OS, and its suites of servers, sockets and files.
    "std/fs/",
    "std/host/",
    "std/native/",
    "std/net/",
    "std/os/",
    "std/process/",
    "std/prelude/timers.velt",
    "std/task/",
    "std/time/",
    "tests/std/",
    // Goldens with a macOS branch (`system_functions`, `system_function_views`, ...).
    "tests/golden/lang/system_function",
    // Packages with a macOS branch, or tests of file paths: `/tmp` and `/var` are symbolic links
    // on macOS, so `realpath` answers differently from Linux there.
    "packages/http-server/src/files/",
    "packages/http-server/tests/files.test.velt",
    "packages/http-server/tests/connections.test.velt",
];

/// Why macOS must be checked too, or `None` when it can wait for the scheduled run. Unlike
/// [`other_os_reason`] this asks only about the paths, not whether they select everything: the
/// build, CI, the scripts and the lockfile alone don't start a macOS job, and that includes the
/// macOS steps of `.github/actions/setup`. Documentation never does.
pub fn macos_reason(paths: &[String]) -> Option<String> {
    paths
        .iter()
        .filter(|p| !p.ends_with(".md"))
        .find(|p| MACOS_PATHS.iter().any(|m| p.starts_with(m)))
        .map(|p| format!("{p} can behave differently on macOS"))
}

/// Paths whose change runs every package's tests on Windows in a pull request (besides a change
/// to a package itself, which runs its own): the networking the HTTP server and the database
/// drivers stand on (std's and the runtime's), timers, and the test runner (std's `velt/test`
/// and the `velt test` command). Pull requests otherwise test on Linux only, and these behave
/// differently on Windows (sockets, timer resolution, process handling): the merge queue was
/// where they failed, for every pull request queued behind them.
const WINDOWS_PACKAGE_TESTS: &[&str] = &[
    "std/net/",
    "std/test/",
    "std/prelude/timers.velt",
    "crates/velt_rt/src/net/",
    "crates/velt_rt/src/tls.rs",
    "crates/velt_rt/src/timer.rs",
    "crates/veltc/src/commands/test/",
];

/// The packages whose tests a pull request runs on Windows (`VELT_PACKAGES` of veltc's `packages`
/// test): `""` for none, `"all"`, or their names separated by `,`. `skip` are files where only
/// comments changed; `exists` says whether a package is still there (a change may delete one).
pub fn windows_packages(
    paths: &[String],
    skip: &std::collections::BTreeSet<String>,
    exists: impl Fn(&str) -> bool,
) -> String {
    let mut names = std::collections::BTreeSet::new();
    for path in paths.iter().filter(|p| !skip.contains(*p)) {
        if WINDOWS_PACKAGE_TESTS.iter().any(|p| path.starts_with(p)) {
            return "all".into();
        }
        if let Some((name, rest)) = path
            .strip_prefix("packages/")
            .and_then(|r| r.split_once('/'))
        {
            // Documentation alone changes no test, and a deleted package has none.
            if !rest.ends_with(".md") && exists(name) {
                names.insert(name.to_string());
            }
        }
    }
    names.into_iter().collect::<Vec<_>>().join(",")
}

/// Files that key or fill the build cache (Swatinem/rust-cache in `.github/actions/setup`): the
/// toolchain and the dependencies. Only the pushes to `main` save the cache. A dependency change
/// reaches `Cargo.lock`. Once #1135 C5 keys the cache by `rust-toolchain.toml` and `Cargo.lock`'s
/// external packages only, an edit to a `Cargo.toml` alone changes nothing the cache holds; until
/// then such an edit changes the key without refreshing the Windows and macOS caches.
const CACHE_INPUTS: &[&str] = &[
    "Cargo.lock",
    "rust-toolchain.toml",
    ".cargo/",
    ".github/actions/setup/",
];

/// Why the whole gate runs on Windows and macOS right after a change lands on `main`, or `None`
/// when it can wait for `ci.yml`'s scheduled run. On the push, `cargo xtask affected --base
/// HEAD^1` plans the same change the queue planned (the queue tests each entry on its `base_sha`,
/// which becomes `HEAD^1` on `main`). The gate runs when the change touches the toolchain or the
/// dependencies: the run refreshes the Windows and macOS build caches, which only runs on `main`
/// save.
pub fn after_merge_reason(paths: &[String]) -> Option<String> {
    paths
        .iter()
        .find(|p| CACHE_INPUTS.iter().any(|c| p.starts_with(c)))
        .map(|p| format!("{p} changed: refresh the build cache"))
}

/// The runners the merge queue checks on: Linux always (first), the others when a change needs
/// them ([`other_os_reason`], [`macos_reason`]).
pub const QUEUE_OSES: &[&str] = &["ubuntu-latest", "windows-latest", "macos-latest"];

/// How many jobs check the goldens in parallel on each OS in the merge queue
/// (.github/workflows/queue-goldens.yml), after one job built them. Windows, whose goldens take
/// longest, gets the most. macOS gets two: in one job its goldens grew to the job's 30-minute
/// limit (2026-10). With 5 macOS runners for everything, more shards would let queue entries
/// starve each other.
/// Every runner in [`QUEUE_OSES`] needs an entry: the workflow fails when an OS has no shards.
pub const GOLDEN_SHARDS: &[(&str, usize)] = &[
    ("ubuntu-latest", 2),
    ("windows-latest", 3),
    ("macos-latest", 2),
];

/// How many jobs check the WebAssembly goldens (crates/veltc/tests/wasm_goldens.rs) in parallel
/// on each OS, in the merge queue when a change touches WebAssembly code and in the nightly, from
/// the build of the goldens' job (.github/workflows/queue-goldens.yml). They take about as long
/// as all the other tests together: on a 4-vCPU Windows runner about 45 minutes in one job. The
/// test splits them into 8 functions of about 330 s there, which the shards share (two each on
/// Windows). Every runner in [`QUEUE_OSES`] needs an entry, as for [`GOLDEN_SHARDS`].
pub const WASM_SHARDS: &[(&str, usize)] = &[
    ("ubuntu-latest", 2),
    ("windows-latest", 4),
    ("macos-latest", 2),
];

/// The queue's golden shards (`VELT_GOLDEN_SHARD` values) on each of the runners `oses`, as a
/// JSON object: `{"ubuntu-latest":["1/2","2/2"],…}`. A runner without a [`GOLDEN_SHARDS`] entry
/// gets none, which fails the queue (never a silent skip of its goldens).
pub fn golden_shards_json(oses: &[&str]) -> String {
    shards_json(GOLDEN_SHARDS, oses)
}

/// The WebAssembly goldens' shards (`VELT_WASM_SHARD` values) on each of the runners `oses`, as
/// [`golden_shards_json`] gives the goldens'.
pub fn wasm_shards_json(oses: &[&str]) -> String {
    shards_json(WASM_SHARDS, oses)
}

fn shards_json(table: &[(&str, usize)], oses: &[&str]) -> String {
    let mut entries = vec![];
    for os in oses {
        let Some(&(_, n)) = table.iter().find(|(name, _)| name == os) else {
            continue;
        };
        let shards: Vec<String> = (1..=n).map(|i| format!("\"{i}/{n}\"")).collect();
        entries.push(format!("\"{os}\":[{}]", shards.join(",")));
    }
    format!("{{{}}}", entries.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_and_networking_changes_test_packages_on_windows() {
        let none = std::collections::BTreeSet::new();
        let p = |paths: &[&str]| {
            let paths: Vec<String> = paths.iter().map(|s| s.to_string()).collect();
            windows_packages(&paths, &none, |name| name != "gone")
        };
        assert_eq!(p(&["crates/velt_sema/src/lib.rs", "docs/std/net.md"]), "");
        assert_eq!(p(&["packages/http-server/README.md"]), "");
        assert_eq!(
            p(&[
                "packages/sqlite/src/db.velt",
                "packages/http-server/tests/h2.test.velt"
            ]),
            "http-server,sqlite"
        );
        // A change that deletes a package leaves nothing of it to test.
        assert_eq!(
            p(&["packages/gone/package.velt", "packages/gone/src/a.velt"]),
            ""
        );
        for every in [
            "std/net/tcp_stream.velt",
            "std/test/run.velt",
            "crates/velt_rt/src/net/tcp.rs",
            "crates/veltc/src/commands/test/mod.rs",
        ] {
            assert_eq!(p(&["packages/sqlite/src/db.velt", every]), "all", "{every}");
        }
        let skip = std::collections::BTreeSet::from(["std/net/listen.velt".to_string()]);
        assert_eq!(
            windows_packages(&["std/net/listen.velt".into()], &skip, |_| true),
            ""
        );
    }

    #[test]
    fn every_os_gets_its_shards() {
        assert_eq!(
            golden_shards_json(&["ubuntu-latest"]),
            r#"{"ubuntu-latest":["1/2","2/2"]}"#
        );
        let all = golden_shards_json(QUEUE_OSES);
        let total: usize = GOLDEN_SHARDS.iter().map(|(_, n)| n).sum();
        assert_eq!(all.matches('/').count(), total);
        assert!(
            all.contains(r#""windows-latest":["1/3","2/3","3/3"]"#),
            "{all}"
        );
        assert!(all.contains(r#""macos-latest":["1/2","2/2"]"#), "{all}");
        assert_eq!(golden_shards_json(&["other"]), "{}");
        assert_eq!(golden_shards_json(&[]), "{}");
    }

    #[test]
    fn every_queue_os_has_at_least_one_golden_shard() {
        for table in [GOLDEN_SHARDS, WASM_SHARDS] {
            for os in QUEUE_OSES {
                let n = table.iter().find(|(name, _)| name == os).map(|e| e.1);
                assert!(n.is_some_and(|n| n >= 1), "{os} has no shards");
            }
        }
    }

    #[test]
    fn the_webassembly_goldens_get_shards_of_their_own() {
        let all = wasm_shards_json(QUEUE_OSES);
        assert!(
            all.contains(r#""windows-latest":["1/4","2/4","3/4","4/4"]"#),
            "{all}"
        );
        assert!(all.contains(r#""macos-latest":["1/2","2/2"]"#), "{all}");
        assert_eq!(
            wasm_shards_json(&["ubuntu-latest"]),
            r#"{"ubuntu-latest":["1/2","2/2"]}"#
        );
        assert_eq!(wasm_shards_json(&["other"]), "{}");
    }
}
