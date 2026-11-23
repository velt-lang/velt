//! The standard library's tests: its black-box behaviour tests, the test files of each
//! `tests/std/<module>/`, and its white-box tests next to the code, the
//! `std/<module>/<name>.test.velt` files (docs/internals/design/std-layout.md §9). They run by
//! directory ([`Unit`]), with `velt test <dir>`, in a debug build that also checks that each test
//! file's process frees every block it allocates (`VELT_RC_STATS=1`, as the goldens' `// check: no leaks`) and uses the checking
//! allocator (`VELT_RT_DEBUG_ALLOC=1`, as the goldens' debug runs), and in a release build, with
//! `--no-cache` (every test runs). The runs of one shard in one mode share a temporary directory
//! for their build output (see [`Work`]). A file where no test ran fails too. The directories are
//! split into [`SHARDS`] tests (`std_modules_pass_their_tests_shard_<k>`).
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

/// Where the `velt test` runs of one shard work. A run puts its runner (the program that starts
/// each test file's process, the same for every file) and its harnesses in the `target/` of the
/// package of the file, or else of its working directory, and builds nothing again that its
/// build stamp says is unchanged. So the runs of a shard share a working directory per mode: the
/// white-box tests build the runner once per shard and mode instead of once per run. The modes
/// don't share one, so a file's debug and release runs, which may run at once, never share a
/// working directory.
///
/// `VELT_HOME` is part of what the build stamp compares, so every run uses the same one: the
/// runner of `tests/std`'s package, which goes in `tests/std/target/`, is then built once for all
/// shards instead of once per run (with `--no-cache`, a run still runs every test).
struct Work {
    dir: test_dir::TestDir,
    home: PathBuf,
}

impl Work {
    fn new() -> Work {
        let dir = test_dir::TestDir::new();
        for mode in ["debug", "release"] {
            std::fs::create_dir(dir.path().join(mode)).expect("work dir");
        }
        let home = Path::new(env!("CARGO_TARGET_TMPDIR")).join("std_tests_home");
        std::fs::create_dir_all(&home).expect("VELT_HOME");
        Work { dir, home }
    }

    /// The working directory of the runs in debug or `release`.
    fn dir(&self, release: bool) -> PathBuf {
        let mode = if release { "release" } else { "debug" };
        self.dir.path().join(mode)
    }

    fn home(&self) -> &Path {
        &self.home
    }
}

/// `velt test` of `dir`, a directory of test files ([`Unit`]; `--release`, or a debug build
/// counting blocks): `None` if all its tests passed and each of `files` ran at least one (or
/// none, for a test file that declares none, [`declares_tests`]), else the report.
fn run_tests(dir: &Path, files: &[PathBuf], release: bool, work: &Work) -> Option<String> {
    let mut command = no_window::command(env!("CARGO_BIN_EXE_velt"));
    command.arg("test").arg(dir).arg("--no-cache");
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
        .current_dir(work.dir(release))
        .env("VELT_HOME", work.home())
        .env("VELT_REGISTRY", work.home().join("registry"))
        .env_remove("CLICOLOR_FORCE")
        .env("NO_COLOR", "1")
        .output()
        .expect("run velt test");
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() && only_known_leaks(&stdout) {
        return None;
    }
    let idle: Vec<String> = (files.iter())
        .filter(|f| passed_in(&stdout, f) == 0 && declares_tests(f))
        .map(|f| f.display().to_string())
        .collect();
    let why = if !out.status.success() {
        format!("`velt test` failed ({})", out.status)
    } else if passed_tests(&stdout).is_none_or(|n| n == 0) {
        "`velt test` ran no tests".into()
    } else if !idle.is_empty() {
        format!("`velt test` ran no tests of {}", idle.join(", "))
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

/// How many tests of test file `file` passed in the run of `velt test` printing `stdout`: the
/// `ok <test>` lines after its `running <file>` line, up to the next file's.
fn passed_in(stdout: &str, file: &Path) -> usize {
    let slashes = |s: &str| s.replace('\\', "/");
    let header = format!("running {}", slashes(&file.display().to_string()));
    let mut lines = stdout.lines().map(str::trim);
    if !lines.any(|l| {
        let l = slashes(l);
        l == header || l.starts_with(&format!("{header} ("))
    }) {
        return 0;
    }
    lines
        .take_while(|l| !l.starts_with("running "))
        .filter(|l| l.starts_with("ok "))
        .count()
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

/// A directory that one `velt test` run tests, and its test files (every test file under it).
///
/// `velt test <dir>` builds one harness for all of its files that only declare (and one of its
/// own for each file whose top level runs code), and the runner once; each file still runs in a
/// process of its own. A run per file built both for every file: 172 files took about seven
/// times the CPU of the same files run by directory (#3635).
#[derive(Debug, PartialEq)]
struct Unit {
    dir: PathBuf,
    files: Vec<PathBuf>,
}

/// The directories the test files `files` (sorted) are run by: the shallowest directory above
/// each file, below `tests/std/` or `std/`, that holds a test file itself. Its subdirectories'
/// test files are its own, so no two units share a file.
fn units(root: &Path, files: &[PathBuf]) -> Vec<Unit> {
    let tops = [root.join("tests/std"), root.join("std")];
    let holds: std::collections::HashSet<&Path> = files.iter().filter_map(|f| f.parent()).collect();
    let mut units: std::collections::BTreeMap<PathBuf, Vec<PathBuf>> = Default::default();
    for f in files {
        let top = tops.iter().find(|t| f.starts_with(t));
        let dir = (f.ancestors().skip(1))
            .take_while(|d| top.is_some_and(|t| *d != t.as_path()))
            .filter(|d| holds.contains(d))
            .last()
            .unwrap_or_else(|| f.parent().expect("ICE: a test file without a directory"));
        units.entry(dir.to_path_buf()).or_default().push(f.clone());
    }
    (units.into_iter())
        .map(|(dir, files)| Unit { dir, files })
        .collect()
}

/// The units of shard `shard`. The units are dealt out largest first (by their number of test
/// files) to the shard with the fewest files so far, so the shards' times stay close.
fn shard_of(units: &[Unit], shard: usize) -> Vec<&Unit> {
    let mut order: Vec<&Unit> = units.iter().collect();
    order.sort_by(|a, b| b.files.len().cmp(&a.files.len()).then(a.dir.cmp(&b.dir)));
    let mut load = [0usize; SHARDS];
    let mut mine = vec![];
    for unit in order {
        let k = (0..SHARDS).min_by_key(|&k| (load[k], k)).unwrap();
        load[k] += unit.files.len();
        if k == shard {
            mine.push(unit);
        }
    }
    mine
}

/// Runs the test directories of shard `shard`, each in a debug and a release build.
fn std_shard(shard: usize) {
    let root = root();
    runtime_support::build_native_runtime(&root);
    let files = test_files(&root);
    assert!(
        !files.is_empty(),
        "no test files under tests/std or std (VELT_STD_TESTS={:?})",
        std::env::var("VELT_STD_TESTS").unwrap_or_default()
    );
    let units = units(&root, &files);
    let runs: Vec<(&Unit, bool)> = shard_of(&units, shard)
        .into_iter()
        .flat_map(|u| [false, true].map(|release| (u, release)))
        .collect();
    let work = Work::new();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let failures = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..RUNS_AT_ONCE {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some((unit, release)) = runs.get(i) else {
                    break;
                };
                if let Some(report) = run_tests(&unit.dir, &unit.files, *release, &work) {
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
/// split is every test file found by a walk of its own (without the `VELT_STD_TESTS` filter), each
/// file is in one unit of one shard, and a unit's files are exactly the test files under its
/// directory, which `velt test <dir>` runs.
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
    let units = units(&root, &all);
    for unit in &units {
        let mut under = Vec::new();
        test_files_in(&unit.dir, &mut under);
        under.sort();
        assert_eq!(under, unit.files, "{}: not its files", unit.dir.display());
    }
    let mut seen: Vec<PathBuf> = (0..SHARDS)
        .flat_map(|k| shard_of(&units, k))
        .flat_map(|u| u.files.clone())
        .collect();
    seen.sort();
    assert_eq!(seen, all, "a test file is in no shard, or in two");
    assert!(all.len() > 100, "only {} std test files found", all.len());
    assert!(
        (0..SHARDS).all(|k| !shard_of(&units, k).is_empty()),
        "a shard has no tests"
    );
}

/// A file's passing tests are counted from its `running` line to the next file's, whatever the
/// note after its path and the slashes the path is printed with.
#[test]
fn passing_tests_are_counted_per_file() {
    let out = "running /r/a.test.velt\nok one\nok two\nrunning C:\\r\\b.test.velt (ran: not \
               cacheable, it uses the network)\nFAILED three\nrunning /r/c.test.velt\nok four\n\n\
               test result: FAILED. 3 passed; 1 failed\n";
    assert_eq!(passed_in(out, Path::new("/r/a.test.velt")), 2);
    assert_eq!(passed_in(out, Path::new("C:/r/b.test.velt")), 0);
    assert_eq!(passed_in(out, Path::new("/r/c.test.velt")), 1);
    assert_eq!(passed_in(out, Path::new("/r/d.test.velt")), 0);
}

/// A file's unit is the shallowest directory above it that holds a test file, so a directory's
/// subdirectories go with it, and two sibling directories are units of their own.
#[test]
fn test_files_run_by_the_shallowest_directory_holding_one() {
    let root = Path::new("/r");
    let files: Vec<PathBuf> = [
        "std/a/b/x.test.velt",
        "std/a/y.test.velt",
        "std/c/d/z.test.velt",
        "std/c/e/w.test.velt",
        "tests/std/m/v.test.velt",
    ]
    .iter()
    .map(|f| root.join(f))
    .collect();
    let dirs: Vec<(PathBuf, usize)> = (units(root, &files).into_iter())
        .map(|u| (u.dir, u.files.len()))
        .collect();
    let want: Vec<(PathBuf, usize)> = [
        ("std/a", 2),
        ("std/c/d", 1),
        ("std/c/e", 1),
        ("tests/std/m", 1),
    ]
    .iter()
    .map(|(d, n)| (root.join(d), *n))
    .collect();
    assert_eq!(dirs, want);
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
