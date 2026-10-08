//! `velt test` for every example application: each `examples/apps/<app>/` with a `tests/`
//! directory is copied to a temporary directory (so its build output stays out of the source
//! tree) and its tests must pass, with an unchanged lock file (`--locked`). The apps' `demo.vlt`
//! programs run as goldens; this covers their `*.test.vlt` files.
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

/// `velt test --locked` in a copy of `app`: `None` if it passed, else the report.
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
        .output()
        .expect("run velt test");
    if out.status.success() {
        return None;
    }
    Some(format!(
        "examples/apps/{name}: `velt test` failed ({})\n{}{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    ))
}

#[test]
fn every_example_app_passes_its_tests() {
    let root = root();
    runtime_support::build_native_runtime(&root);
    let apps = apps(&root);
    let failures: Vec<String> = std::thread::scope(|s| {
        let runs: Vec<_> = apps
            .iter()
            .map(|app| s.spawn(move || run_tests(app)))
            .collect();
        runs.into_iter()
            .filter_map(|r| r.join().expect("ICE: test thread panicked"))
            .collect()
    });
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
