//! End-to-end golden tests: every `tests/golden/**/*.vlt` is compiled and run with the real `velt` binary.
//!
//! - `foo.vlt` + `foo.out`  → `velt run foo.vlt` must exit 0 (or the code in `foo.code`) and stdout
//!   must equal `foo.out` (CRLF normalized).
//! - `foo.vlt` + `foo.err`  → `velt build foo.vlt` must fail and stderr must contain every non-empty
//!   line of `foo.err`.
//! - `foo.stderr` next to a `foo.out`: the run's stderr must contain every non-empty line of it
//!   (for programs that report an error at run time). A `.stderr` without a `.out` fails.
//!
//! - `examples/*.vlt` with a sibling `.out` are included as well.
//! - Files whose name starts with `_` are helper modules imported by other goldens; not run directly.
//!   So are all files in a directory whose name starts with `_` (folder modules, `dir/index.vlt`).
//!
//! - Goldens under a directory containing `.pending` (milestone in progress) are run, but their
//!   failures only fail the test with `VELT_GOLDEN_STRICT=1`.
//!
//! - A golden whose first lines contain `// requires-env: NAME ...` is skipped unless those
//!   environment variables are set (for tests needing a database or other external service).
//! - A golden whose first lines contain `// check: no leaks` must free every block it allocates
//!   (`VELT_RC_STATS=1` with the debug runtime: `blocks=A/F` with A = F) in its debug run.
//!
//! Filter with `VELT_GOLDEN=<substring>` (several separated by `,`: a file matching any of them
//! runs). `VELT_GOLDEN_SHARD=<i>/<n>` checks only the i-th of n interleaved shards (1-based), so
//! CI can spread the goldens over several machines. A program running longer than `VELT_GOLDEN_TIMEOUT`
//! seconds (default 120) is killed and fails. Builds go to `target/golden-work` (or
//! `VELT_GOLDEN_WORK`); each program's outputs are deleted after it runs. Programs are checked
//! on `VELT_GOLDEN_JOBS` worker threads (default: half the cores, at most 8; `1` = sequential);
//! `VELT_GOLDEN_MODES=debug` (or `release`) checks one build mode only.
//! The debug-mode run of each program uses the debug runtime's checking allocator
//! (`VELT_RT_DEBUG_ALLOC=1`, `crates/velt_rt/src/debug_alloc.rs`): a use after free, double free
//! or buffer overflow aborts with `velt debug-alloc: ...`. Set `VELT_RT_DEBUG_ALLOC=0` to turn it
//! off, or `=1` to check the release-mode runs too.

use std::path::{Path, PathBuf};
use std::process::Command;

mod no_window;
mod runtime_support;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        let helper = p
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with('_'));
        if p.is_dir() && !helper {
            collect(&p, out);
        } else if p.is_file() && p.extension().is_some_and(|e| e == "vlt")
            // `_name.vlt` files are imported modules and `package.vlt` is a manifest, not programs.
            && !helper
            && !p.ends_with(vpm::manifest::MANIFEST_FILE)
        {
            out.push(p);
        }
    }
}

fn norm(s: &str) -> String {
    s.replace("\r\n", "\n")
}

#[test]
fn golden() {
    let root = root();
    // The runtime staticlib must exist next to the velt binary before we can link anything.
    runtime_support::build_native_runtime(&root);

    let velt = env!("CARGO_BIN_EXE_velt");
    let filters = name_filters();
    // `VELT_GOLDEN_WORK` moves the build directory (e.g. to a disk with more space).
    let work = std::env::var_os("VELT_GOLDEN_WORK")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target/golden-work"));
    std::fs::create_dir_all(&work).unwrap();

    let mut files = vec![];
    collect(&root.join("tests/golden"), &mut files);
    // Example programs with an `.out` file are goldens too, so they keep working.
    let mut examples = vec![];
    collect(&root.join("examples"), &mut examples);
    files.extend(
        examples
            .into_iter()
            .filter(|f| f.with_extension("out").exists()),
    );
    let files: Vec<_> = files
        .into_iter()
        .filter(|f| {
            let name = f.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/");
            filters.iter().any(|filter| name.contains(filter.as_str()))
        })
        .collect();
    let files = shard(files);

    let strict = std::env::var("VELT_GOLDEN_STRICT").is_ok();
    let mut skipped: Vec<String> = vec![];
    let mut runnable = vec![];
    for f in &files {
        let rel = f.strip_prefix(&root).unwrap().display().to_string();
        match missing_required_env(f) {
            Some(missing) => skipped.push(format!("{rel} (needs ${missing})")),
            None => runnable.push((f.clone(), rel)),
        }
    }
    let results = run_parallel(velt, &runnable, &work);
    let mut failures = vec![];
    let mut pending_failures = 0;
    for ((f, _), errs) in runnable.iter().zip(results) {
        if errs.is_empty() {
            continue;
        }
        if !strict && is_pending(f, &root) {
            pending_failures += 1;
        } else {
            failures.extend(errs);
        }
    }
    for s in &skipped {
        println!("golden: skipped {s}");
    }
    println!(
        "golden: {} files, {} failures, {} failures in pending milestones (VELT_GOLDEN_STRICT=1 to fail on them)",
        files.len(),
        failures.len(),
        pending_failures
    );
    if !failures.is_empty() {
        panic!(
            "
{}
",
            failures.join(
                "

"
            )
        );
    }
}

/// The `VELT_GOLDEN` substrings, split at `,`; no filter matches every file.
fn name_filters() -> Vec<String> {
    let all = std::env::var("VELT_GOLDEN").unwrap_or_default();
    let filters: Vec<String> = all
        .split(',')
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .map(String::from)
        .collect();
    if filters.is_empty() {
        vec![String::new()]
    } else {
        filters
    }
}

/// The files of shard `i` of `n` (`VELT_GOLDEN_SHARD=i/n`, 1-based): every n-th file, so slow
/// directories spread over the shards.
fn shard(files: Vec<PathBuf>) -> Vec<PathBuf> {
    let Ok(spec) = std::env::var("VELT_GOLDEN_SHARD") else {
        return files;
    };
    let parsed = spec.split_once('/').and_then(|(i, n)| {
        Some((
            i.trim().parse::<usize>().ok()?,
            n.trim().parse::<usize>().ok()?,
        ))
    });
    let Some((i, n)) = parsed.filter(|&(i, n)| i >= 1 && i <= n) else {
        panic!("VELT_GOLDEN_SHARD must be <i>/<n> with 1 <= i <= n, not `{spec}`");
    };
    files
        .into_iter()
        .enumerate()
        .filter(|(k, _)| k % n == i - 1)
        .map(|(_, f)| f)
        .collect()
}

/// A golden is pending when a directory between it and `tests/golden` contains a `.pending` file:
/// the milestone is in progress, so its failures are reported but don't fail the build.
fn is_pending(file: &Path, root: &Path) -> bool {
    let top = root.join("tests/golden");
    file.ancestors()
        .skip(1)
        .take_while(|d| d.starts_with(&top))
        .any(|d| d.join(".pending").exists())
}

fn check_file(velt: &str, f: &Path, rel: &str, work: &Path) -> Vec<String> {
    let mut failures = vec![];
    let err_file = f.with_extension("err");
    let out_file = f.with_extension("out");
    if f.with_extension("stderr").exists() && !out_file.exists() {
        failures.push(format!(
            "{rel}: a .stderr expectation needs a .out file next to it"
        ));
        return failures;
    }
    if err_file.exists() {
        let o = crate::no_window::command(velt)
            .arg("build")
            .arg(f)
            .current_dir(work)
            .output()
            .unwrap();
        let stderr = norm(&String::from_utf8_lossy(&o.stderr));
        if o.status.success() {
            failures.push(format!(
                "{rel}: expected compile error, but build succeeded"
            ));
            return failures;
        }
        for line in norm(&std::fs::read_to_string(&err_file).unwrap()).lines() {
            let line = line.trim();
            if !line.is_empty() && !stderr.contains(line) {
                failures.push(format!(
                    "{rel}: stderr missing `{line}`
--- stderr ---
{stderr}"
                ));
            }
        }
    } else if out_file.exists() {
        let want_code: i32 = std::fs::read_to_string(f.with_extension("code"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        let want = norm(&std::fs::read_to_string(&out_file).unwrap());
        // Debug (unoptimized) and release (velt_opt Speed + Cranelift speed) must agree.
        for mode in modes() {
            // Separate output dirs per mode: on Windows a just-built exe can stay locked briefly
            // (Defender scan), so the release link must not overwrite the debug executable.
            let dir = work.join(if mode.is_some() { "release" } else { "debug" });
            std::fs::create_dir_all(&dir).unwrap();
            let mut cmd = crate::no_window::command(velt);
            cmd.arg("run").args(mode).arg(f).current_dir(&dir);
            // The debug run checks every allocation (use after free, double free, overflow)
            // unless the caller chose otherwise (`VELT_RT_DEBUG_ALLOC=0`).
            if mode.is_none() && std::env::var_os("VELT_RT_DEBUG_ALLOC").is_none() {
                cmd.env("VELT_RT_DEBUG_ALLOC", "1");
            }
            let leak_check = mode.is_none() && checks_leaks(f);
            if leak_check {
                cmd.env("VELT_RC_STATS", "1");
            }
            let o = run_with_timeout(cmd);
            let stdout = norm(&String::from_utf8_lossy(&o.stdout));
            if leak_check {
                if let Some(leak) = leaked_blocks(&String::from_utf8_lossy(&o.stderr)) {
                    failures.push(format!("{rel} [debug]: {leak}"));
                }
            }
            let stderr = norm(&String::from_utf8_lossy(&o.stderr));
            for line in std::fs::read_to_string(f.with_extension("stderr"))
                .unwrap_or_default()
                .lines()
                .filter(|l| !l.is_empty() && !stderr.contains(l))
            {
                failures.push(format!(
                    "{rel}: stderr missing `{line}`\n--- stderr ---\n{stderr}"
                ));
            }
            let code = o.status.code().unwrap_or(-1);
            if code != want_code || stdout != want {
                failures.push(format!(
                    "{rel} [{}]: exit {code} (want {want_code})
--- want ---
{want}--- got ---
{stdout}--- stderr ---
{}",
                    mode.unwrap_or("debug"),
                    String::from_utf8_lossy(&o.stderr)
                ));
            }
        }
    } else {
        failures.push(format!("{rel}: no .out or .err expectation file"));
    }
    failures
}

/// Does the golden ask for a leak check (`// check: no leaks` on one of its first lines)?
fn checks_leaks(file: &Path) -> bool {
    std::fs::read_to_string(file).is_ok_and(|src| {
        src.lines()
            .take(10)
            .any(|l| l.trim() == "// check: no leaks")
    })
}

/// The leak in a `rc stats: ... blocks=A/F` report: blocks allocated but not freed, or no report.
fn leaked_blocks(stderr: &str) -> Option<String> {
    let counts = stderr
        .lines()
        .rev()
        .find(|l| l.starts_with("rc stats:"))
        .and_then(|l| l.split_whitespace().find_map(|w| w.strip_prefix("blocks=")))
        .and_then(|b| b.split_once('/'));
    match counts {
        Some((a, f)) if a == f => None,
        Some((a, f)) => Some(format!("leak: {a} blocks allocated, {f} freed")),
        None => Some("leak check: no `rc stats` report on stderr".into()),
    }
}

/// The build modes to check: `VELT_GOLDEN_MODES=debug` or `release` (the fast gate checks debug
/// only); both by default.
fn modes() -> Vec<Option<&'static str>> {
    match std::env::var("VELT_GOLDEN_MODES").as_deref() {
        Ok("debug") => vec![None],
        Ok("release") => vec![Some("--release")],
        _ => vec![None, Some("--release")],
    }
}

/// Checks `files` on `VELT_GOLDEN_JOBS` worker threads (default: half the cores, at most 8),
/// each program in a work directory of its own (its current directory, so files programs write
/// never collide; and no build ever overwrites an executable that has just run, which on Windows
/// can stay locked for a moment, even when two programs share a name). Returns each file's
/// errors, in `files` order.
fn run_parallel(velt: &str, files: &[(PathBuf, String)], work: &Path) -> Vec<Vec<String>> {
    let jobs = std::env::var("VELT_GOLDEN_JOBS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or_else(|| {
            std::thread::available_parallelism().map_or(1, |n| (n.get() / 2).clamp(1, 8))
        })
        .max(1);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results = std::sync::Mutex::new(vec![Vec::new(); files.len()]);
    std::thread::scope(|scope| {
        for _ in 0..jobs {
            let (next, results) = (&next, &results);
            scope.spawn(move || loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some((f, rel)) = files.get(i) else {
                    break;
                };
                let dir = work.join(format!("p{i}"));
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir).unwrap();
                let errs = check_file(velt, f, rel, &dir);
                // Delete this program's build outputs (exe, obj, pdb): kept around they add up
                // to tens of GB.
                let _ = std::fs::remove_dir_all(&dir);
                results.lock().unwrap()[i] = errs;
            });
        }
    });
    results.into_inner().unwrap()
}

/// A golden that needs an external service declares it on one of its first lines:
/// `// requires-env: VELT_TEST_PG_URL` (several names separated by spaces). The golden is skipped
/// (and listed as skipped) when any of them is unset or empty; otherwise the program sees them
/// through `std/process` `env(...)`. Returns the first missing name.
fn missing_required_env(file: &Path) -> Option<String> {
    let src = std::fs::read_to_string(file).ok()?;
    for line in src.lines().take(10) {
        if let Some(names) = line.trim().strip_prefix("// requires-env:") {
            for name in names.split_whitespace() {
                if std::env::var(name).map_or(true, |v| v.is_empty()) {
                    return Some(name.to_string());
                }
            }
        }
    }
    None
}

/// `velt run` output, killing the program after `VELT_GOLDEN_TIMEOUT` seconds (default 120): a
/// hanging golden fails instead of stalling the whole run.
fn run_with_timeout(mut cmd: Command) -> std::process::Output {
    use std::io::Read;
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let limit = Duration::from_secs(
        std::env::var("VELT_GOLDEN_TIMEOUT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(120),
    );
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Drain the pipes on threads so a chatty program cannot block on a full pipe. After a kill
    // the program `velt run` started may still hold them open: collect with a grace period.
    let drain = |mut r: Box<dyn Read + Send>| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut v = vec![];
            let _ = r.read_to_end(&mut v);
            let _ = tx.send(v);
        });
        move || rx.recv_timeout(Duration::from_secs(5)).unwrap_or_default()
    };
    let out = drain(Box::new(child.stdout.take().unwrap()));
    let err = drain(Box::new(child.stderr.take().unwrap()));
    let start = Instant::now();
    let mut timed_out = false;
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if start.elapsed() > limit {
            timed_out = true;
            let _ = child.kill();
            break child.wait().unwrap();
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut stderr = err();
    if timed_out {
        stderr.extend_from_slice(format!("\n(killed after {} s)\n", limit.as_secs()).as_bytes());
    }
    std::process::Output {
        status,
        stdout: out(),
        stderr,
    }
}
