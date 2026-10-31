//! The repository's packages (`packages/<name>/`, libraries that leave std and later move to
//! repositories of their own, docs/internals/design/std-layout.md §3): `velt test` in each, as
//! their users run it, once in a debug build with the checking allocator (`VELT_RT_DEBUG_ALLOC=1`:
//! a use after free or a double free aborts, and the block counters on: `VELT_RC_STATS=1`, so a
//! test file that leaves a block unfreed fails) and once in a release build. A package with
//! native code builds its
//! crate from source with cargo first (into `packages/<name>/target/velt-native/`, kept between
//! runs, as `velt` does for any package with native code that it builds locally), and its crate's
//! own Rust tests run too (`cargo test`, outside the compiler's workspace). A package where no test
//! ran fails too.
//!
//! Tests that need a database server read its URL from the environment and do nothing without it:
//! `VELT_TEST_PG_URL` (packages/postgres), `VELT_TEST_REDIS_URL`, `VELT_TEST_REDISS_URL` and
//! `VELT_TEST_REDISS_CA` (packages/redis). `scripts/linux-check.sh --services` starts the servers
//! and sets them, as CI does; without them this test says which were not set.
//!
//! Filter with `VELT_PACKAGES=<name>` (several separated by `,`). Needs cargo for the packages
//! with native code; `VELT_SKIP_NATIVE_E2E=1` skips those.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

mod no_window;
mod runtime_support;
mod test_dir;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

/// The packages (`packages/<name>/package.vlt`), sorted, after the `VELT_PACKAGES` filter.
fn packages(root: &Path) -> Vec<PathBuf> {
    let filter = std::env::var("VELT_PACKAGES").unwrap_or_default();
    let wanted = |name: &str| filter.is_empty() || filter.split(',').any(|f| f.trim() == name);
    let mut packages: Vec<PathBuf> = std::fs::read_dir(root.join("packages"))
        .expect("packages")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("package.vlt").is_file())
        .filter(|p| wanted(&p.file_name().unwrap().to_string_lossy()))
        .collect();
    packages.sort();
    packages
}

/// Whether the package has native code (a `native/` crate).
fn native(package: &Path) -> bool {
    package.join("native").join("Cargo.toml").is_file()
}

/// `velt test` in `package` (`--release`, or a debug build with the checking allocator): `None`
/// if it ran at least one test and all passed, else the report.
fn run_tests(package: &Path, release: bool) -> Option<String> {
    let mut command = no_window::command(env!("CARGO_BIN_EXE_velt"));
    command.arg("test");
    if release {
        // No block counters in a release runtime: an exported `VELT_RC_STATS=1` would fail
        // every file.
        command.arg("--release").env_remove("VELT_RC_STATS");
    } else {
        command.env("VELT_RC_STATS", "1");
        if std::env::var_os("VELT_RT_DEBUG_ALLOC").is_none() {
            command.env("VELT_RT_DEBUG_ALLOC", "1");
        }
    }
    let out = command
        .current_dir(package)
        .env_remove("VELT_REGISTRY")
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
        "packages/{} ({mode}): {why}\n{stdout}{}",
        package.file_name().unwrap().to_string_lossy(),
        String::from_utf8_lossy(&out.stderr)
    ))
}

/// `cargo test` of a native package's crate (`native/`), in the cargo directory `velt` builds it
/// in: `None` if it passed, else the report.
fn native_unit_tests(package: &Path) -> Option<String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = no_window::command(&cargo)
        .arg("test")
        .arg("--manifest-path")
        .arg(package.join("native/Cargo.toml"))
        .arg("--target-dir")
        .arg(package.join("target/velt-native/cargo"))
        .output()
        .expect("run cargo test");
    if out.status.success() {
        return None;
    }
    Some(format!(
        "packages/{} (cargo test of native/): failed ({})\n{}{}",
        package.file_name().unwrap().to_string_lossy(),
        out.status,
        String::from_utf8_lossy(&out.stdout),
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
fn the_summary_line_counts_passed_tests() {
    assert_eq!(
        passed_tests("test result: ok. 7 passed; 0 failed\n"),
        Some(7)
    );
    assert_eq!(
        passed_tests("test result: FAILED. 1 passed; 1 failed"),
        None
    );
}

#[test]
fn every_package_passes_its_tests() {
    let root = root();
    runtime_support::build_native_runtime(&root);
    let skip_native = std::env::var_os("VELT_SKIP_NATIVE_E2E").is_some_and(|v| v == "1");
    let packages: Vec<PathBuf> = packages(&root)
        .into_iter()
        .filter(|p| {
            let skip = skip_native && native(p);
            if skip {
                eprintln!("skipped {} (VELT_SKIP_NATIVE_E2E=1)", p.display());
            }
            !skip
        })
        .collect();
    if !skip_native {
        assert!(
            !packages.is_empty(),
            "no package under packages/ (VELT_PACKAGES={:?})",
            std::env::var("VELT_PACKAGES").unwrap_or_default()
        );
    }
    for var in [
        "VELT_TEST_PG_URL",
        "VELT_TEST_REDIS_URL",
        "VELT_TEST_REDISS_URL",
    ] {
        if std::env::var_os(var).is_none() {
            eprintln!("note: {var} is not set: the tests that need its server do nothing");
        }
    }
    // Packages in parallel; a package's two builds one after the other (they share its
    // `target/`, and the native library's build).
    let failures: Vec<String> = std::thread::scope(|s| {
        let runs: Vec<_> = packages
            .iter()
            .map(|p| {
                s.spawn(move || {
                    let mut failures: Vec<String> = [false, true]
                        .into_iter()
                        .filter_map(|release| run_tests(p, release))
                        .collect();
                    if native(p) {
                        failures.extend(native_unit_tests(p));
                    }
                    failures
                })
            })
            .collect();
        runs.into_iter()
            .flat_map(|r| {
                r.join()
                    .unwrap_or_else(|_| vec!["a test thread panicked".into()])
            })
            .collect()
    });
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// A program that closes its Redis client ends (packages/redis): closing used to abort the
/// connection's driver task while its reader task, waiting on the socket, was left running, and
/// its pending read kept the process alive forever after `main` returned. Against a stand-in
/// server on a local port that answers every command with `+PONG`, so it needs no Redis.
#[test]
fn a_closed_redis_client_lets_the_program_end() {
    let root = root();
    let skip_native = std::env::var_os("VELT_SKIP_NATIVE_E2E").is_some_and(|v| v == "1");
    if skip_native || !packages(&root).iter().any(|p| p.ends_with("redis")) {
        eprintln!("skipped (VELT_SKIP_NATIVE_E2E=1 or VELT_PACKAGES)");
        return;
    }
    runtime_support::build_native_runtime(&root);
    let server = std::net::TcpListener::bind("127.0.0.1:0").expect("listen");
    let port = server.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for conn in server.incoming() {
            let Ok(mut conn) = conn else { return };
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                // One command per read: the client sends one PING and waits for its reply.
                while matches!(conn.read(&mut buf), Ok(n) if n > 0) {
                    if conn.write_all(b"+PONG\r\n").is_err() {
                        return;
                    }
                }
            });
        }
    });
    let tmp = test_dir::TestDir::new();
    let app = tmp.path().join("app");
    std::fs::create_dir_all(app.join("src")).unwrap();
    let dep = root
        .join("packages/redis")
        .to_string_lossy()
        .replace('\\', "/");
    std::fs::write(
        app.join("package.vlt"),
        format!(
            "import type {{ Package }} from \"velt/package\";\n\nexport const pkg: Package = {{\n  name: \"app\",\n  version: \"0.1.0\",\n  dependencies: {{ redis: {{ path: \"{dep}\" }} }},\n}};\n"
        ),
    )
    .unwrap();
    std::fs::write(
        app.join("src/main.vlt"),
        format!(
            "import {{ connect }} from \"redis\";\n\nasync function main() {{\n  const c = await connect(\"redis://127.0.0.1:{port}\");\n  console.log(await c.ping());\n  c.close();\n  console.log(\"closed\");\n}}\n"
        ),
    )
    .unwrap();
    // Built first (cargo may build the native library), then run with a deadline.
    let exe = app.join(if cfg!(windows) { "app.exe" } else { "app" });
    let built = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "-o"])
        .arg(&exe)
        .arg("src/main.vlt")
        .current_dir(&app)
        .output()
        .expect("run velt build");
    assert!(
        built.status.success(),
        "velt build failed:\n{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let mut child = no_window::command(&exe)
        .current_dir(&app)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("run the program");
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break Some(status);
        }
        if start.elapsed() > Duration::from_secs(30) {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut stdout = String::new();
    let _ = child.stdout.take().unwrap().read_to_string(&mut stdout);
    assert_eq!(stdout, "PONG\nclosed\n");
    let status = status.expect("the program did not end within 30 s of closing its client");
    assert!(status.success(), "{status}");
}
