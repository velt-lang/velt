//! `velt test` for every example application: each `examples/apps/<app>/` with a `tests/`
//! directory is copied to a temporary directory (so its build output stays out of the source
//! tree) and its tests must pass, with an unchanged lock file (`--locked`); an app where no test
//! ran fails too. The apps' `demo.vlt` programs run as goldens; this covers their `*.test.vlt`
//! files.
//!
//! Filter with `VELT_EXAMPLE_APPS=<name>` (several separated by `,`).

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

/// The apps with tests, sorted, after the `VELT_EXAMPLE_APPS` filter.
fn apps(root: &Path) -> Vec<PathBuf> {
    let filter = std::env::var("VELT_EXAMPLE_APPS").unwrap_or_default();
    let wanted = |name: &str| filter.is_empty() || filter.split(',').any(|f| f.trim() == name);
    let mut apps: Vec<PathBuf> = std::fs::read_dir(root.join("examples/apps"))
        .expect("examples/apps")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("tests").is_dir() && p.join("package.vlt").is_file())
        .filter(|p| wanted(&p.file_name().unwrap().to_string_lossy()))
        .collect();
    apps.sort();
    apps
}

/// Copies an app's sources: everything but build output (`target/`).
fn copy_app(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let (path, name) = (entry.path(), entry.file_name());
        if name == "target" {
            continue;
        }
        if entry.file_type().unwrap().is_dir() {
            copy_app(&path, &to.join(&name));
        } else {
            std::fs::copy(&path, to.join(&name)).unwrap();
        }
    }
}

/// `velt test --locked` in a copy of `app`: `None` if it ran at least one test and all passed,
/// else the report.
fn run_tests(app: &Path) -> Option<String> {
    let name = app.file_name().unwrap().to_string_lossy().into_owned();
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path().join(&name);
    copy_app(app, &dir);
    let out = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["test", "--locked"])
        .current_dir(&dir)
        .env("VELT_HOME", tmp.path().join("home"))
        .env_remove("VELT_REGISTRY")
        .env_remove("CLICOLOR_FORCE")
        .env("NO_COLOR", "1")
        .output()
        .expect("run velt test");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let why = if !out.status.success() {
        format!("`velt test` failed ({})", out.status)
    } else if passed_tests(&stdout).is_none_or(|n| n == 0) {
        "`velt test` ran no tests (no `test result: ok. N passed; 0 failed` with N > 0)".into()
    } else {
        return None;
    };
    Some(format!(
        "examples/apps/{name}: {why}\n{stdout}{}",
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

/// The report for an app whose test thread panicked.
fn panicked(app: &Path, payload: Box<dyn std::any::Any + Send>) -> String {
    let msg = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied())
        .unwrap_or("(no message)");
    format!(
        "examples/apps/{}: the test thread panicked: {msg}",
        app.file_name().unwrap().to_string_lossy()
    )
}

#[test]
fn the_summary_line_counts_passed_tests() {
    assert_eq!(
        passed_tests("x\n\ntest result: ok. 7 passed; 0 failed\n"),
        Some(7)
    );
    assert_eq!(passed_tests("test result: ok. 0 passed; 0 failed"), Some(0));
    assert_eq!(
        passed_tests("test result: FAILED. 1 passed; 1 failed"),
        None
    );
    assert_eq!(passed_tests("no test files found\n"), None);
}

#[test]
fn every_example_app_passes_its_tests() {
    let root = root();
    runtime_support::build_native_runtime(&root);
    let apps = apps(&root);
    match std::env::var("VELT_EXAMPLE_APPS") {
        Ok(filter) if !filter.is_empty() => assert!(
            !apps.is_empty(),
            "VELT_EXAMPLE_APPS={filter} matches no app with tests under examples/apps"
        ),
        _ => assert!(!apps.is_empty(), "no app with tests under examples/apps"),
    }
    let failures: Vec<String> = std::thread::scope(|s| {
        let runs: Vec<_> = apps
            .iter()
            .map(|app| (app, s.spawn(move || run_tests(app))))
            .collect();
        runs.into_iter()
            .filter_map(|(app, r)| r.join().unwrap_or_else(|p| Some(panicked(app, p))))
            .collect()
    });
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
