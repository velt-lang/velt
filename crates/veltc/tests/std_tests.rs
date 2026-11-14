//! The standard library's tests: its black-box behaviour tests, `velt test` over each
//! `tests/std/<module>/`, and its white-box tests next to the code, `velt test std/` (the
//! `std/<module>/<name>.test.velt` files, docs/internals/design/std-layout.md §9). Each runs in a
//! debug build that also checks that every test file's process frees every block it allocates
//! (`VELT_RC_STATS=1`, as the goldens' `// check: no leaks`) and uses the checking allocator
//! (`VELT_RT_DEBUG_ALLOC=1`, as the goldens' debug runs), and in a release build. Each run
//! has a temporary directory of its own for its build output. A module where no test ran fails
//! too. CONTRIBUTING.md "Golden or `velt test`?" says which tests go here.
//!
//! Filter with `VELT_STD_TESTS=<module>` (several separated by `,`; `std` names the white-box
//! tests).

use std::path::{Path, PathBuf};

mod no_window;
mod runtime_support;
mod test_dir;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

/// The directories to run `velt test` in, after the `VELT_STD_TESTS` filter: the modules with
/// tests (`tests/std/<module>/`), sorted, then `std/` itself (its white-box tests).
fn modules(root: &Path) -> Vec<PathBuf> {
    let filter = std::env::var("VELT_STD_TESTS").unwrap_or_default();
    let wanted = |name: &str| filter.is_empty() || filter.split(',').any(|f| f.trim() == name);
    let mut modules: Vec<PathBuf> = std::fs::read_dir(root.join("tests/std"))
        .expect("tests/std")
        .flatten()
        .map(|e| e.path())
        // `target/`: the build output of `velt test` in the package tests/std is (its
        // `package.velt` depends on the HTTP server for the clients' tests).
        .filter(|p| p.is_dir() && p.file_name().is_some_and(|n| n != "target"))
        .filter(|p| wanted(&p.file_name().unwrap().to_string_lossy()))
        .collect();
    modules.sort();
    if wanted("std") {
        modules.push(root.join("std"));
    }
    modules
}

/// `velt test` of `dir` (`--release`, or a debug build counting blocks): `None` if it ran at
/// least one test and all passed, else the report.
fn run_tests(dir: &Path, release: bool) -> Option<String> {
    let tmp = test_dir::TestDir::new();
    let mut command = no_window::command(env!("CARGO_BIN_EXE_velt"));
    command.arg("test").arg(dir);
    if release {
        // No block counters there: an exported `VELT_RC_STATS=1` would fail every file.
        command.arg("--release").env_remove("VELT_RC_STATS");
    } else {
        command.env("VELT_RC_STATS", "1");
        // As the goldens' debug runs: the checking allocator (a use after free aborts), unless
        // the caller chose otherwise (`VELT_RT_DEBUG_ALLOC=0`).
        if std::env::var_os("VELT_RT_DEBUG_ALLOC").is_none() {
            command.env("VELT_RT_DEBUG_ALLOC", "1");
        }
    }
    let out = command
        .current_dir(tmp.path())
        .env("VELT_HOME", tmp.path().join("home"))
        .env("VELT_REGISTRY", tmp.path().join("home").join("registry"))
        .env_remove("CLICOLOR_FORCE")
        .env("NO_COLOR", "1")
        .output()
        .expect("run velt test");
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() && only_known_leaks(&stdout) {
        return None;
    }
    let why = if !out.status.success() {
        format!("`velt test` failed ({})", out.status)
    } else if passed_tests(&stdout).is_none_or(|n| n == 0) {
        "`velt test` ran no tests".into()
    } else {
        return None;
    };
    let mode = if release { "release" } else { "debug" };
    Some(format!(
        "{} ({mode}): {why}\n{stdout}{}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    ))
}

/// Test files known to leave blocks unfreed under the executor in Velt since #2649's switch, with
/// the issue that tracks it: their leak is reported, but doesn't fail the run.
const KNOWN_LEAKS: &[(&str, &str)] = &[
    ("tests/std/fetch/h2.test.velt", "#2842"),
    ("tests/std/fetch/wire.test.velt", "#2842"),
    ("tests/std/websocket/client.test.velt", "#2842"),
    ("std/net/tls/rfc8448.test.velt", "#2924"),
    ("std/net/tls/tls_engine.test.velt", "#2924"),
    ("std/sync/hand_off.test.velt", "#2924"),
];

/// Did the run of `velt test` printing `stdout` fail only by known leaks ([`KNOWN_LEAKS`])? Every
/// `FAILED` line is then a leak line of a listed file; each is reported.
fn only_known_leaks(stdout: &str) -> bool {
    let failed: Vec<&str> = (stdout.lines().map(str::trim))
        .filter(|l| l.starts_with("FAILED "))
        .collect();
    let known = |line: &str| {
        let path = line.trim_start_matches("FAILED ").replace('\\', "/");
        line.ends_with(" blocks not freed)")
            && KNOWN_LEAKS
                .iter()
                .find(|(f, _)| path.contains(f))
                .inspect(|(f, issue)| {
                    println!("std tests: {f} leaks, known ({issue})");
                })
                .is_some()
    };
    !failed.is_empty() && failed.iter().all(|l| known(l))
}

/// N from the summary line `test result: ok. N passed; 0 failed`, if it is there.
fn passed_tests(stdout: &str) -> Option<usize> {
    stdout.lines().find_map(|line| {
        line.trim()
            .strip_prefix("test result: ok. ")?
            .strip_suffix(" passed; 0 failed")?
            .parse()
            .ok()
    })
}

#[test]
fn std_modules_pass_their_tests() {
    let root = root();
    runtime_support::build_native_runtime(&root);
    let modules = modules(&root);
    assert!(
        !modules.is_empty(),
        "no module with tests under tests/std or std (VELT_STD_TESTS={:?})",
        std::env::var("VELT_STD_TESTS").unwrap_or_default()
    );
    let failures: Vec<String> = std::thread::scope(|s| {
        let runs: Vec<_> = modules
            .iter()
            .flat_map(|m| [false, true].map(|release| (m, release)))
            .map(|(m, release)| s.spawn(move || run_tests(m, release)))
            .collect();
        runs.into_iter()
            .filter_map(|r| {
                r.join()
                    .unwrap_or_else(|_| Some("a test thread panicked".into()))
            })
            .collect()
    });
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn navigator_names_this_toolchain_version() {
    // `navigator.userAgent` is `"Velt/<version>"`, written in std (it has no other source of the
    // version): a release that bumps the version updates it here.
    let src = std::fs::read_to_string(root().join("std/lang/navigator/navigator.velt"))
        .expect("navigator.velt");
    let ua = format!("return \"Velt/{}\";", env!("CARGO_PKG_VERSION"));
    assert!(
        src.contains(&ua),
        "std/lang/navigator/navigator.velt: expected `{ua}`"
    );
}
