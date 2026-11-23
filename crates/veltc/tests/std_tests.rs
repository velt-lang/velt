//! The standard library's tests: its black-box behaviour tests, the test files of each
//! `tests/std/<module>/`, and its white-box tests next to the code, the
//! `std/<module>/<name>.test.velt` files (docs/internals/design/std-layout.md §9). Each file runs
//! with `velt test <file>` in a debug build that also checks that its process frees every block
//! it allocates (`VELT_RC_STATS=1`, as the goldens' `// check: no leaks`) and uses the checking
//! allocator (`VELT_RT_DEBUG_ALLOC=1`, as the goldens' debug runs), and in a release build. Each
//! run has a temporary directory of its own for its build output. A file where no test ran fails
//! too. The files are split into [`SHARDS`] tests (`std_modules_pass_their_tests_shard_<k>`).
//! CONTRIBUTING.md "Golden or `velt test`?" says which tests go here.
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

/// Does test file `file` declare a test: a `describe(` or `it(` block, or an exported `test_*`
/// function? A few files only hold what others import (std/net/tls/x509/parts.test.velt's
/// certificates), so they run none.
fn declares_tests(file: &Path) -> bool {
    let src = std::fs::read_to_string(file).unwrap_or_default();
    src.lines().map(str::trim_start).any(|l| {
        l.starts_with("describe(")
            || l.starts_with("it(")
            || l.starts_with("export function test_")
            || l.starts_with("export async function test_")
    })
}

/// `velt test` of `dir`, a test file or a directory (`--release`, or a debug build counting
/// blocks): `None` if all its tests passed and it ran at least one (or none, for a test file
/// that declares none, [`declares_tests`]), else the report.
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
    } else if passed_tests(&stdout).is_none_or(|n| n == 0) && (dir.is_dir() || declares_tests(dir))
    {
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

/// The shards the test files are split into (#3481): one test each, which nextest runs in
/// parallel, so none of them nears the slow-test limit (all of them in one test took 19 minutes on
/// the Windows runner), and a failure names a smaller set of files.
const SHARDS: usize = 8;

/// The `velt test` runs a shard keeps going at once.
const RUNS_AT_ONCE: usize = 2;

/// Is `p` a test file (`*.test.velt`, `.test.ts`, `.test.tsx`)?
fn is_test_file(p: &Path) -> bool {
    let name = p.file_name().unwrap_or_default().to_string_lossy();
    [".test.velt", ".test.ts", ".test.tsx"]
        .iter()
        .any(|s| name.ends_with(s))
}

/// The test files under `dir`, outside build output (`target/`), appended to `out`.
fn test_files_in(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for p in entries.flatten().map(|e| e.path()) {
        if p.is_dir() {
            if p.file_name().is_some_and(|n| n != "target") {
                test_files_in(&p, out);
            }
        } else if is_test_file(&p) {
            out.push(p);
        }
    }
}

/// Every test file of the modules [`modules`] gives, sorted.
fn test_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for m in modules(root) {
        test_files_in(&m, &mut files);
    }
    files.sort();
    files
}

/// The files of shard `shard`: every `SHARDS`-th one of the sorted list, so a module's files,
/// which sort together, are spread over the shards and their times stay close.
fn shard_of(files: &[PathBuf], shard: usize) -> Vec<PathBuf> {
    (files.iter().enumerate())
        .filter(|(i, _)| i % SHARDS == shard)
        .map(|(_, f)| f.clone())
        .collect()
}

/// Runs the test files of shard `shard`, each in a debug and a release build.
fn std_shard(shard: usize) {
    let root = root();
    runtime_support::build_native_runtime(&root);
    let files = test_files(&root);
    assert!(
        !files.is_empty(),
        "no test files under tests/std or std (VELT_STD_TESTS={:?})",
        std::env::var("VELT_STD_TESTS").unwrap_or_default()
    );
    let runs: Vec<(PathBuf, bool)> = shard_of(&files, shard)
        .into_iter()
        .flat_map(|f| [false, true].map(|release| (f.clone(), release)))
        .collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let failures = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..RUNS_AT_ONCE {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some((file, release)) = runs.get(i) else {
                    break;
                };
                if let Some(report) = run_tests(file, *release) {
                    failures.lock().unwrap().push(report);
                }
            });
        }
    });
    let failures = failures.into_inner().unwrap();
    assert!(
        failures.is_empty(),
        "std tests, shard {shard} of {SHARDS}: {} failures\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

macro_rules! std_shards {
    ($($name:ident = $shard:literal),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                std_shard($shard);
            }
        )*
        /// Every shard has a test function.
        const SHARD_TESTS: &[usize] = &[$($shard),*];
    };
}

std_shards! {
    std_modules_pass_their_tests_shard_0 = 0,
    std_modules_pass_their_tests_shard_1 = 1,
    std_modules_pass_their_tests_shard_2 = 2,
    std_modules_pass_their_tests_shard_3 = 3,
    std_modules_pass_their_tests_shard_4 = 4,
    std_modules_pass_their_tests_shard_5 = 5,
    std_modules_pass_their_tests_shard_6 = 6,
    std_modules_pass_their_tests_shard_7 = 7,
}

/// The shards together run every test file under tests/std and std exactly once: the list they
/// split is every test file found by a walk of its own (without the `VELT_STD_TESTS` filter), and
/// each file is in one shard.
#[test]
fn every_std_test_file_is_in_one_shard() {
    let root = root();
    assert_eq!(SHARD_TESTS, (0..SHARDS).collect::<Vec<_>>().as_slice());
    let mut all = Vec::new();
    test_files_in(&root.join("tests/std"), &mut all);
    test_files_in(&root.join("std"), &mut all);
    all.sort();
    if std::env::var_os("VELT_STD_TESTS").is_none() {
        assert_eq!(
            test_files(&root),
            all,
            "the shards' list misses or adds test files"
        );
    }
    let mut seen: Vec<PathBuf> = (0..SHARDS).flat_map(|k| shard_of(&all, k)).collect();
    seen.sort();
    assert_eq!(seen, all, "a test file is in no shard, or in two");
    assert!(all.len() > 100, "only {} std test files found", all.len());
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
