//! The standard library's behaviour tests: `velt test` over each `tests/std/<module>/`, in a
//! debug build that also checks that every test file's process frees every block it allocates
//! (`VELT_RC_STATS=1`, as the goldens' `// check: no leaks`) and uses the checking allocator
//! (`VELT_RT_DEBUG_ALLOC=1`, as the goldens' debug runs), and in a release build. Each run
//! has a temporary directory of its own for its build output. A module where no test ran fails
//! too. CONTRIBUTING.md "Golden or `velt test`?" says which tests go here.
//!
//! Filter with `VELT_STD_TESTS=<module>` (several separated by `,`).

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

/// The modules with tests (`tests/std/<module>/`), sorted, after the `VELT_STD_TESTS` filter.
fn modules(root: &Path) -> Vec<PathBuf> {
    let filter = std::env::var("VELT_STD_TESTS").unwrap_or_default();
    let wanted = |name: &str| filter.is_empty() || filter.split(',').any(|f| f.trim() == name);
    let mut modules: Vec<PathBuf> = std::fs::read_dir(root.join("tests/std"))
        .expect("tests/std")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && wanted(&p.file_name().unwrap().to_string_lossy()))
        .collect();
    modules.sort();
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
        .env_remove("CLICOLOR_FORCE")
        .env("NO_COLOR", "1")
        .output()
        .expect("run velt test");
    let stdout = String::from_utf8_lossy(&out.stdout);
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
        "no module with tests under tests/std (VELT_STD_TESTS={:?})",
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
