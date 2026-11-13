//! End-to-end golden tests: every `tests/golden/**/*.velt` is compiled and run with the real `velt` binary.
//!
//! - `foo.velt` + `foo.out`  → `velt run foo.velt` must exit 0 (or the code in `foo.code`) and stdout
//!   must equal `foo.out` (CRLF normalized). Half of the debug runs build with `velt build` first,
//!   which describes variables for debuggers (`with_variables`).
//! - `foo.velt` + `foo.err`  → `velt build foo.velt` must fail and stderr must contain every non-empty
//!   line of `foo.err`.
//! - `foo.stderr` next to a `foo.out`: the run's stderr must contain every non-empty line of it
//!   (for programs that report an error at run time). A `.stderr` without a `.out` fails.
//!
//! - `examples/*.velt` with a sibling `.out` are included as well.
//! - Files whose name starts with `_` are helper modules imported by other goldens; not run directly.
//!   So are all files in a directory whose name starts with `_` (folder modules, `dir/index.velt`).
//!
//! - Goldens under a directory containing `.pending` (milestone in progress) are run, but their
//!   failures only fail the test with `VELT_GOLDEN_STRICT=1`.
//! - Goldens listed in `golden_known_failing.txt` (next to this file: the path, the issue that
//!   tracks the failure, and optionally the OSes it fails on) are run too, but their failures only
//!   fail the test with `VELT_GOLDEN_STRICT=1`: the executor in Velt's fix list since #2649's
//!   switch. One that passes is reported, to be taken off the list.
//!
//! - A golden whose first lines contain `// requires-env: NAME ...` is skipped unless those
//!   environment variables are set (for tests needing a database or other external service).
//! - A golden whose first lines contain `// requires: symlinks` is skipped where this process
//!   cannot create symbolic links (Windows without Developer Mode or administrator rights);
//!   `// requires: unix` is skipped on Windows (POSIX system functions, `velt/fs`'s fd API), and
//!   `// requires: windows` everywhere else (Windows system functions).
//! - A golden whose first lines contain `// check: no leaks` must free every block it allocates
//!   (`VELT_RC_STATS=1` with the debug runtime: `blocks=A/F` with A = F) in its debug run. Tasks
//!   still running when `main` returns are waited for (up to a minute) while blocks are unfreed;
//!   the count is taken at a moment when every block was freed, or once no task can run any
//!   more (blocks held by tasks that wait forever, or by pending `unref()`'d timers, count as
//!   unfreed).
//! - A golden whose first lines contain `// check: unfreed blocks at most N` is checked the same
//!   way, but may leave up to N blocks unfreed: for programs that keep memory on purpose until
//!   they exit.
//! - A golden whose first lines contain `// check: heap strings at most N` allocates at most N
//!   heap string buffers in its debug run (`alloc=` of the same report): a count that does not
//!   depend on timing, for programs that must not copy a string per step (e.g. appends).
//! - A golden whose first lines contain `// check: heap blocks at most N` allocates at most N
//!   heap blocks in its debug run (`blocks=A/F` of the same report, A <= N): for programs that
//!   must not allocate per step (e.g. a callback per call).
//! - A golden whose first lines contain `// check: also with NAME=VALUE` runs once more in each
//!   mode with that environment variable set, and must print the same: for programs whose paths
//!   an environment switch selects (`VELT_SIMD_DISABLE=sha`: `velt/simd`'s emulation and std's
//!   portable code on a CPU that has the instructions, #1330).
//!
//! Filter with `VELT_GOLDEN=<substring>` (several separated by `,`: a file matching any of them
//! runs). `VELT_GOLDEN_SHARD=<i>/<n>` checks only the i-th of n interleaved shards (1-based), so
//! CI can spread the goldens over several machines. A program running longer than
//! `VELT_GOLDEN_TIMEOUT` seconds (default 120) is killed, with every process it started, and
//! fails. Builds go to `target/golden-work` (or `VELT_GOLDEN_WORK`); each program's outputs are
//! deleted after it runs. Programs are checked on `VELT_GOLDEN_JOBS` worker threads (default:
//! one per core, at most 16; `1` = sequential);
//! `VELT_GOLDEN_MODES=debug` (or `release`) checks one build mode only.
//! The debug-mode run of each program uses the debug runtime's checking allocator
//! (`VELT_RT_DEBUG_ALLOC=1`, `crates/velt_rt/src/debug_alloc.rs`): a use after free, double free
//! or buffer overflow aborts with `velt debug-alloc: ...`. Set `VELT_RT_DEBUG_ALLOC=0` to turn it
//! off, or `=1` to check the release-mode runs too.

use std::path::{Path, PathBuf};
use std::process::Command;

mod no_window;
mod process_tree;
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
        } else if p.is_file() && p.extension().is_some_and(|e| e == "velt")
            // `_name.velt` files are imported modules and `package.velt` is a manifest, not programs.
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
            // Matched against the path in the checkout, so a checkout whose own path contains
            // the filter (`branches/uncaught-flush`) doesn't run every file.
            let rel = f.strip_prefix(&root).unwrap_or(f);
            let name = rel
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            filters.iter().any(|filter| name.contains(filter.as_str()))
        })
        .collect();
    let files = shard(files);

    let strict = std::env::var("VELT_GOLDEN_STRICT").is_ok();
    let mut skipped: Vec<String> = vec![];
    let mut runnable = vec![];
    for f in &files {
        let rel = f.strip_prefix(&root).unwrap().display().to_string();
        match (missing_required_env(f), missing_capability(f, &work)) {
            (Some(missing), _) => skipped.push(format!("{rel} (needs ${missing})")),
            (None, Some(missing)) => skipped.push(format!("{rel} (needs {missing})")),
            (None, None) => runnable.push((f.clone(), rel)),
        }
    }
    let known = known_failing();
    let results = run_parallel(velt, &runnable, &work);
    let mut failures = vec![];
    let mut pending_failures = 0;
    let mut known_failures = 0;
    for ((f, rel), errs) in runnable.iter().zip(results) {
        let issue = known.get(rel.replace('\\', "/").as_str());
        if errs.is_empty() {
            if let Some(issue) = issue {
                println!("golden: {rel} passes: take it off golden_known_failing.txt ({issue})");
            }
            continue;
        }
        if !strict && is_pending(f, &root) {
            pending_failures += 1;
        } else if let (false, Some(issue)) = (strict, issue) {
            println!("golden: {rel} fails, known ({issue})");
            known_failures += 1;
        } else {
            failures.extend(errs);
        }
    }
    for s in &skipped {
        println!("golden: skipped {s}");
    }
    println!(
        "golden: {} files, {} failures, {} failures in pending milestones and {} known (VELT_GOLDEN_STRICT=1 to fail on them)",
        files.len(),
        failures.len(),
        pending_failures,
        known_failures
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

/// The goldens known to fail on this OS (`golden_known_failing.txt`): path from the repository
/// root, then the issue tracking it.
fn known_failing() -> std::collections::HashMap<String, String> {
    parse_known_failing(
        include_str!("golden_known_failing.txt"),
        std::env::consts::OS,
    )
}

/// The entries of known-failing list `text` that apply on `os`. Each line not blank and not a
/// `#`-comment is `<path> #<issue> [os=<os>,...]`; anything else is a mistake in the list.
fn parse_known_failing(text: &str, os: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut words = line.split_whitespace();
        let (Some(path), Some(issue)) = (words.next(), words.next()) else {
            panic!("golden_known_failing.txt: `{line}` needs a path and an issue");
        };
        assert!(
            issue.len() > 1
                && issue[1..].bytes().all(|b| b.is_ascii_digit())
                && issue.starts_with('#'),
            "golden_known_failing.txt: `{line}`: the issue is `#<number>`"
        );
        let applies = match words.next() {
            None => true,
            Some(w) => match w.strip_prefix("os=") {
                Some(list) => list.split(',').any(|o| o == os),
                None => panic!("golden_known_failing.txt: `{line}`: after the issue only `os=...`"),
            },
        };
        assert!(
            words.next().is_none(),
            "golden_known_failing.txt: `{line}`: too many words"
        );
        if applies {
            out.insert(path.to_string(), issue.to_string());
        }
    }
    out
}

#[test]
fn the_known_failing_list_reads_its_entries_by_os() {
    let text = "# comment\n\ntests/golden/a.velt #12\ntests/golden/b.velt #34 os=windows,linux\n";
    let mac = parse_known_failing(text, "macos");
    assert_eq!(
        mac.get("tests/golden/a.velt").map(String::as_str),
        Some("#12")
    );
    assert!(!mac.contains_key("tests/golden/b.velt"));
    let linux = parse_known_failing(text, "linux");
    assert_eq!(
        linux.get("tests/golden/b.velt").map(String::as_str),
        Some("#34")
    );
    // Every entry of the real list names a golden that is there, on any OS.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let all = include_str!("golden_known_failing.txt");
    for os in ["macos", "linux", "windows"] {
        for path in parse_known_failing(all, os).keys() {
            assert!(
                root.join(path).is_file(),
                "golden_known_failing.txt: no golden {path}"
            );
        }
    }
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

/// Whether the debug run of the golden at `rel` goes through `velt build`, which describes variables
/// for debuggers as it does for its users, instead of `velt run`, which doesn't. Half of them, by a
/// hash of the path: the same compile then also checks that path through to the running program,
/// where `every_golden_compiles_with_variables` samples the programs and stops at VIR.
fn with_variables(rel: &str) -> bool {
    let hash = rel.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    (hash >> 8) % 2 == 0
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
    // A golden in a package is built with `-o` into its work directory: without it a package's
    // build goes to the package's `target/velt`, in the source tree, where the debug and release
    // runs (and `cargo xtask repeat`'s copies of the test running at once) would share one
    // executable, which Windows keeps locked for a moment after it ran.
    let package = f.parent().and_then(vpm::manifest::find_package_root);
    let had_target = package.as_ref().map(|p| p.join("target").exists());
    let stem = f
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    if err_file.exists() {
        let mut cmd = crate::no_window::command(velt);
        cmd.arg("build");
        if package.is_some() {
            cmd.arg("-o").arg(work.join("bin").join(&stem));
        }
        let o = cmd.arg(f).current_dir(work).output().unwrap();
        let stderr = norm(&String::from_utf8_lossy(&o.stderr));
        if o.status.success() {
            failures.push(format!(
                "{rel}: expected compile error, but build succeeded"
            ));
            return failures;
        }
        // An expected path is written with `/`, and Windows prints `\` in a diagnostic.
        let same_slashes = |s: &str| {
            if cfg!(windows) {
                s.replace('\\', "/")
            } else {
                s.to_string()
            }
        };
        let stderr_slashes = same_slashes(&stderr);
        for line in norm(&std::fs::read_to_string(&err_file).unwrap()).lines() {
            let line = line.trim();
            if !line.is_empty() && !stderr_slashes.contains(&same_slashes(line)) {
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
        // Debug (unoptimized) and release (velt_opt Speed + Cranelift speed) must agree, and so
        // must each with a `// check: also with` variable set.
        let also = also_with_env(f);
        let runs: Vec<(Option<&str>, Option<EnvVar>)> = modes()
            .into_iter()
            .flat_map(|m| std::iter::once((m, None)).chain(also.clone().map(|kv| (m, Some(kv)))))
            .collect();
        for (mode, env) in runs {
            // Separate output dirs per mode: on Windows a just-built exe can stay locked briefly
            // (Defender scan), so the release link must not overwrite the debug executable.
            let dir = work.join(if mode.is_some() { "release" } else { "debug" });
            std::fs::create_dir_all(&dir).unwrap();
            // In a package, and half of the debug runs: `velt build -o` into this directory, then
            // the program itself.
            let mut built_stderr = String::new();
            let through_build = package.is_some() || (mode.is_none() && with_variables(rel));
            let mut cmd = match through_build {
                false => {
                    let mut cmd = crate::no_window::command(velt);
                    cmd.arg("run").args(mode).arg(f);
                    cmd
                }
                true => {
                    // `-o` with the platform's suffix: a stem with a dot (`a.b`) would get no
                    // `.exe` on Windows otherwise.
                    let exe = dir.join(format!("{stem}{}", std::env::consts::EXE_SUFFIX));
                    let mut build = crate::no_window::command(velt);
                    build.arg("build").args(mode);
                    // As `velt run` builds to run here: a debug build for this CPU. (`velt run`
                    // leaves out the variable descriptions for debuggers, which `velt build`
                    // has no flag for; they must not change what a program does.)
                    if mode.is_none() && std::env::var_os("VELT_CPU").is_none() {
                        build.args(["--cpu", "native"]);
                    }
                    build.arg("-o").arg(&exe).arg(f).current_dir(&dir);
                    let o = run_with_timeout(build);
                    built_stderr = norm(&String::from_utf8_lossy(&o.stderr));
                    if !o.status.success() {
                        failures.push(format!(
                            "{rel} [{}]: the build failed\n--- stderr ---\n{built_stderr}",
                            mode.unwrap_or("debug")
                        ));
                        continue;
                    }
                    let mut cmd = crate::no_window::command(exe);
                    // What `velt run` tells the program it runs (`process.argv[1]`).
                    cmd.env(
                        "VELT_SCRIPT",
                        std::path::absolute(f).unwrap_or(f.to_path_buf()),
                    );
                    cmd
                }
            };
            cmd.current_dir(&dir);
            if let Some((name, value)) = &env {
                cmd.env(name, value);
            }
            // The debug run checks every allocation (use after free, double free, overflow)
            // unless the caller chose otherwise (`VELT_RT_DEBUG_ALLOC=0`).
            if mode.is_none() && std::env::var_os("VELT_RT_DEBUG_ALLOC").is_none() {
                cmd.env("VELT_RT_DEBUG_ALLOC", "1");
            }
            let leak_check = mode.is_none().then(|| leak_allowance(f)).flatten();
            let str_limit = mode.is_none().then(|| heap_string_limit(f)).flatten();
            let block_limit = mode.is_none().then(|| heap_block_limit(f)).flatten();
            if leak_check.is_some() || str_limit.is_some() || block_limit.is_some() {
                cmd.env("VELT_RC_STATS", "1");
            }
            let o = run_with_timeout(cmd);
            let stdout = norm(&String::from_utf8_lossy(&o.stdout));
            if let Some(allowed) = leak_check {
                if let Some(leak) = leaked_blocks(&String::from_utf8_lossy(&o.stderr), allowed) {
                    failures.push(format!("{rel} [debug]: {leak}"));
                }
            }
            if let Some(limit) = str_limit {
                if let Some(e) = too_many_strings(&String::from_utf8_lossy(&o.stderr), limit) {
                    failures.push(format!("{rel} [debug]: {e}"));
                }
            }
            if let Some(limit) = block_limit {
                if let Some(e) = too_many_blocks(&String::from_utf8_lossy(&o.stderr), limit) {
                    failures.push(format!("{rel} [debug]: {e}"));
                }
            }
            let stderr = built_stderr + &norm(&String::from_utf8_lossy(&o.stderr));
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
                    "{rel} [{}{}]: exit {code} (want {want_code})
--- want ---
{want}--- got ---
{stdout}--- stderr ---
{}",
                    mode.unwrap_or("debug"),
                    env.as_ref()
                        .map_or(String::new(), |(n, v)| format!(", {n}={v}")),
                    String::from_utf8_lossy(&o.stderr)
                ));
            }
        }
    } else {
        failures.push(format!("{rel}: no .out or .err expectation file"));
    }
    // Nothing a golden builds lands in the source tree.
    if let (Some(root), Some(false)) = (&package, had_target) {
        if root.join("target").exists() {
            failures.push(format!(
                "{rel}: the build wrote `{}` into the source tree",
                root.join("target").display()
            ));
        }
    }
    failures
}

/// An environment variable a golden sets for a run: name and value.
type EnvVar = (String, String);

/// The variable of a `// check: also with NAME=VALUE` line among the golden's first lines.
fn also_with_env(file: &Path) -> Option<EnvVar> {
    let src = std::fs::read_to_string(file).ok()?;
    let rest = src
        .lines()
        .take(10)
        .find_map(|l| l.trim().strip_prefix("// check: also with "))?;
    let (name, value) = rest.trim().split_once('=')?;
    Some((name.to_string(), value.to_string()))
}

/// The blocks a golden may leave unfreed when it asks for a leak check: 0 for `// check: no
/// leaks`, N for `// check: unfreed blocks at most N` (on one of its first lines).
fn leak_allowance(file: &Path) -> Option<u64> {
    let src = std::fs::read_to_string(file).ok()?;
    src.lines().take(10).find_map(|l| match l.trim() {
        "// check: no leaks" => Some(0),
        l => l
            .strip_prefix("// check: unfreed blocks at most ")
            .map(|n| {
                n.trim()
                    .parse()
                    .expect("a number after `unfreed blocks at most`")
            }),
    })
}

/// The N of a `// check: heap strings at most N` line among the golden's first lines.
fn heap_string_limit(file: &Path) -> Option<u64> {
    let src = std::fs::read_to_string(file).ok()?;
    let limit = src
        .lines()
        .take(10)
        .find_map(|l| l.trim().strip_prefix("// check: heap strings at most "))?;
    Some(
        limit
            .trim()
            .parse()
            .expect("a number after `heap strings at most`"),
    )
}

/// The N of a `// check: heap blocks at most N` line among the golden's first lines.
fn heap_block_limit(file: &Path) -> Option<u64> {
    let src = std::fs::read_to_string(file).ok()?;
    let limit = src
        .lines()
        .take(10)
        .find_map(|l| l.trim().strip_prefix("// check: heap blocks at most "))?;
    Some(
        limit
            .trim()
            .parse()
            .expect("a number after `heap blocks at most`"),
    )
}

/// More heap blocks allocated than `limit` in a `rc stats: ... blocks=A/F` report, or no
/// report.
fn too_many_blocks(stderr: &str, limit: u64) -> Option<String> {
    let allocs = stderr
        .lines()
        .rev()
        .find(|l| l.starts_with("rc stats:"))
        .and_then(|l| l.split_whitespace().find_map(|w| w.strip_prefix("blocks=")))
        .and_then(|b| b.split_once('/'))
        .and_then(|(a, _)| a.parse::<u64>().ok());
    match allocs {
        Some(n) if n <= limit => None,
        Some(n) => Some(format!(
            "{n} heap blocks allocated, at most {limit} expected"
        )),
        None => Some("heap block check: no `rc stats` report on stderr".into()),
    }
}

/// More heap string buffers allocated than `limit` in a `rc stats: ... alloc=N` report, or no
/// report.
fn too_many_strings(stderr: &str, limit: u64) -> Option<String> {
    let allocs = stderr
        .lines()
        .rev()
        .find(|l| l.starts_with("rc stats:"))
        .and_then(|l| l.split_whitespace().find_map(|w| w.strip_prefix("alloc=")))
        .and_then(|n| n.parse::<u64>().ok());
    match allocs {
        Some(n) if n <= limit => None,
        Some(n) => Some(format!(
            "{n} heap strings allocated, at most {limit} expected"
        )),
        None => Some("heap string check: no `rc stats` report on stderr".into()),
    }
}

/// The leak in a `rc stats: ... blocks=A/F` report: more than `allowed` blocks allocated but
/// not freed, or no report.
fn leaked_blocks(stderr: &str, allowed: u64) -> Option<String> {
    let counts = stderr
        .lines()
        .rev()
        .find(|l| l.starts_with("rc stats:"))
        .and_then(|l| l.split_whitespace().find_map(|w| w.strip_prefix("blocks=")))
        .and_then(|b| b.split_once('/'));
    let unfreed = |a: &str, f: &str| a.parse::<u64>().ok()?.checked_sub(f.parse().ok()?);
    match counts {
        Some((a, f)) if unfreed(a, f).is_some_and(|n| n <= allowed) => None,
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

/// Checks `files` on `VELT_GOLDEN_JOBS` worker threads (default: one per core, at most 16: a
/// program spends much of its time waiting for the linker and for process start-up, and on CI's
/// 3- and 4-core runners half the cores took up to twice as long, #1135),
/// each program in a work directory of its own (its current directory, so files programs write
/// never collide; and no build ever overwrites an executable that has just run, which on Windows
/// can stay locked for a moment, even when two programs share a name). Returns each file's
/// errors, in `files` order.
fn run_parallel(velt: &str, files: &[(PathBuf, String)], work: &Path) -> Vec<Vec<String>> {
    let jobs = std::env::var("VELT_GOLDEN_JOBS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get().min(16)))
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
/// through `process.env.NAME`. Returns the first missing name.
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

/// A golden that needs something of the machine declares it on one of its first lines:
/// `// requires: symlinks`, `// requires: unix` or `// requires: windows`. Returns what is
/// missing, if anything.
fn missing_capability(file: &Path, work: &Path) -> Option<&'static str> {
    let src = std::fs::read_to_string(file).ok()?;
    let wants = |what: &str| {
        src.lines()
            .take(10)
            .filter_map(|l| l.trim().strip_prefix("// requires:"))
            .any(|names| names.split_whitespace().any(|n| n == what))
    };
    if wants("symlinks") && !can_symlink(work) {
        return Some("symlinks: on Windows, Developer Mode or administrator rights");
    }
    if wants("unix") && cfg!(windows) {
        return Some("unix: POSIX system functions");
    }
    if wants("windows") && !cfg!(windows) {
        return Some("windows: Windows system functions");
    }
    None
}

/// Can this process create symbolic links? Always on Unix; on Windows only with Developer Mode
/// or the symlink privilege, so try once.
fn can_symlink(work: &Path) -> bool {
    #[cfg(windows)]
    {
        static CAN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *CAN.get_or_init(|| {
            let link = work.join(format!("symlink-probe-{}", std::process::id()));
            let _ = std::fs::create_dir_all(work);
            let _ = std::fs::remove_file(&link);
            let ok = std::os::windows::fs::symlink_file("target", &link).is_ok();
            let _ = std::fs::remove_file(&link);
            ok
        })
    }
    #[cfg(not(windows))]
    {
        let _ = work;
        true
    }
}

/// `velt run` output, killing it and the program it started after `VELT_GOLDEN_TIMEOUT` seconds
/// (default 120): a hanging golden fails instead of stalling the whole run or running on.
fn run_with_timeout(mut cmd: Command) -> std::process::Output {
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let limit = Duration::from_secs(
        std::env::var("VELT_GOLDEN_TIMEOUT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(120),
    );
    let mut tree =
        process_tree::ProcessTree::spawn(cmd.stdout(Stdio::piped()).stderr(Stdio::piped()))
            .unwrap();
    // Drain the pipes on threads so a chatty program cannot block on a full pipe. A process the
    // program started may still hold them open after it exits: collect with a grace period.
    let out = Drain::start(Box::new(tree.child().stdout.take().unwrap()));
    let err = Drain::start(Box::new(tree.child().stderr.take().unwrap()));
    let start = Instant::now();
    let mut timed_out = false;
    let status = loop {
        if let Some(s) = tree.child().try_wait().unwrap() {
            break s;
        }
        if start.elapsed() > limit {
            timed_out = true;
            // `velt run` and the program it started (and anything that started).
            tree.kill();
            break tree.child().wait().unwrap();
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let grace = Duration::from_secs(5);
    let (mut stderr, err_open) = err.finish(grace);
    let (stdout, out_open) = out.finish(grace);
    if timed_out {
        stderr.extend_from_slice(format!("\n(killed after {} s)\n", limit.as_secs()).as_bytes());
    }
    // Said in the failure: what was read so far is not all the program printed, perhaps.
    for (open, name) in [(out_open, "stdout"), (err_open, "stderr")] {
        if open {
            let note = format!(
                "\n({name} still open {} s after the exit)\n",
                grace.as_secs()
            );
            stderr.extend_from_slice(note.as_bytes());
        }
    }
    std::process::Output {
        status,
        stdout,
        stderr,
    }
}

/// A pipe read to its end on a thread of its own.
struct Drain {
    read: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    ended: std::sync::mpsc::Receiver<()>,
}

impl Drain {
    fn start(mut r: Box<dyn std::io::Read + Send>) -> Drain {
        let read = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
        let (tx, ended) = std::sync::mpsc::channel();
        let into = read.clone();
        std::thread::spawn(move || {
            let mut chunk = [0; 8192];
            while let Ok(n @ 1..) = r.read(&mut chunk) {
                into.lock().unwrap().extend_from_slice(&chunk[..n]);
            }
            let _ = tx.send(());
        });
        Drain { read, ended }
    }

    /// What was read once the pipe ended or `grace` passed, and whether it is still open then.
    fn finish(self, grace: std::time::Duration) -> (Vec<u8>, bool) {
        let open = self.ended.recv_timeout(grace).is_err();
        let read = self.read.lock().unwrap().clone();
        (read, open)
    }
}

#[test]
fn what_a_pipe_gave_is_kept_when_it_stays_open() {
    use std::sync::mpsc::{channel, Receiver, Sender};
    /// Gives its bytes, then waits; the test reads once `given` says the bytes are in.
    struct Gives(Vec<u8>, Sender<()>, Receiver<()>);
    impl std::io::Read for Gives {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.0.is_empty() {
                let _ = self.1.send(());
                let _ = self.2.recv();
                return Ok(0);
            }
            let n = self.0.len();
            buf[..n].copy_from_slice(&self.0);
            self.0.clear();
            Ok(n)
        }
    }
    let (given_tx, given) = channel();
    let (_hold, wait) = channel();
    let drain = Drain::start(Box::new(Gives(b"started".to_vec(), given_tx, wait)));
    given.recv().unwrap();
    assert_eq!(
        drain.finish(std::time::Duration::ZERO),
        (b"started".to_vec(), true)
    );

    let drain = Drain::start(Box::new(&b"all of it"[..]));
    assert_eq!(
        drain.finish(std::time::Duration::from_secs(60)),
        (b"all of it".to_vec(), false)
    );
}

/// A timed-out run kills the program `velt run` started too, not only `velt run` (#475): the
/// program printed a line, so it is running; once the tree is killed nothing holds the output
/// pipe open any more, so reading it ends.
#[test]
fn killing_a_run_kills_the_program_it_started() {
    use std::io::{BufRead, BufReader, Read};
    use std::process::Stdio;
    let root = root();
    runtime_support::build_native_runtime(&root);
    let work = std::env::var_os("VELT_GOLDEN_WORK")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target/golden-work"))
        .join(format!("kill-tree-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    let src = work.join("forever.velt");
    std::fs::write(
        &src,
        "async function main() {\n  console.log(\"started\");\n  await sleep(600000);\n}\n",
    )
    .unwrap();
    let mut cmd = no_window::command(env!("CARGO_BIN_EXE_velt"));
    cmd.arg("run").arg(&src).current_dir(&work);
    let mut tree = process_tree::ProcessTree::spawn(cmd.stdout(Stdio::piped())).unwrap();
    let mut out = BufReader::new(tree.child().stdout.take().unwrap());
    let mut line = String::new();
    out.read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "started");
    tree.kill();
    tree.child().wait().unwrap();
    // Hang guard: the program would hold the pipe for ten minutes if it survived.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut rest = vec![];
        let _ = tx.send(out.read_to_end(&mut rest).map(|_| rest));
    });
    let rest = rx
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("the program `velt run` started is still running")
        .unwrap();
    assert!(rest.is_empty());
    drop(tree);
    let _ = std::fs::remove_dir_all(&work);
}
