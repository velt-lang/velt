//! Panic and uncaught-error messages name their source location (stderr; the goldens under
//! `tests/golden/lang/panic_*` check stdout and exit codes). Each program runs in debug and
//! release mode, plus release with the LLVM backend when clang is installed. An uncaught error is
//! reported once.

use std::path::{Path, PathBuf};

mod no_window;
mod runtime_support;
mod work_dir;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

/// (golden, expected exit code, expected stderr line).
const CASES: &[(&str, i32, &str)] = &[
    (
        "panic_index",
        101,
        "panic: index out of bounds: the len is 3 but the index is 5 at tests/golden/lang/panic_index.vlt:3:10",
    ),
    (
        "panic_div",
        101,
        "panic: division by zero at tests/golden/lang/panic_div.vlt:3:10",
    ),
    (
        "panic_unwrap",
        101,
        "panic: called unwrap() on a null value at tests/golden/lang/panic_unwrap.vlt:14:15",
    ),
    (
        "panic_builtin",
        101,
        "panic: too big: 3 at tests/golden/lang/panic_builtin.vlt:4:5",
    ),
    (
        "panic_uncaught",
        1,
        "Uncaught ParseError { message: 'bad digit: x' } at tests/golden/lang/panic_uncaught.vlt:13:3",
    ),
    (
        "panic_uncaught_async",
        1,
        "Uncaught ParseError: bad digit: x at tests/golden/lang/panic_uncaught_async.vlt:10:3",
    ),
    (
        "panic_uncaught_spawned",
        1,
        "Uncaught Overheated [Error]: over 150 at tests/golden/lang/panic_uncaught_spawned.vlt:8:5",
    ),
    (
        "panic_uncaught_finally_await",
        1,
        "Uncaught A [Error]: thrown before the await at tests/golden/lang/panic_uncaught_finally_await.vlt:14:5",
    ),
    (
        "panic_uncaught_handle_dropped",
        1,
        "Uncaught Bad [Error]: main failed at tests/golden/lang/panic_uncaught_handle_dropped.vlt:16:3",
    ),
    (
        "panic_uncaught_dropped_catcher",
        1,
        "Uncaught Bad [Error]: main failed at tests/golden/lang/panic_uncaught_dropped_catcher.vlt:22:3",
    ),
    (
        "spawn_handle_dropped_rejects",
        1,
        "Uncaught Failed [Error]: lost after 20 ms at tests/golden/lang/spawn_handle_dropped_rejects.vlt:8:3",
    ),
    (
        "spawn_handle_dropped_after_reject",
        1,
        "Uncaught Failed [Error]: rejected before the handle was dropped at tests/golden/lang/spawn_handle_dropped_after_reject.vlt:13:3",
    ),
    // Through a large standard-library function, which takes the call site as an argument.
    (
        "panic_uncaught_shared_std",
        1,
        "Uncaught RangeError: start offset of Uint32Array should be a multiple of 4 at tests/golden/lang/panic_uncaught_shared_std.vlt:6:16",
    ),
    (
        "panic_uncaught_std",
        1,
        "Uncaught IoError: ENOENT: no such file or directory, lstat 'velt-missing-file-b' at tests/golden/lang/panic_uncaught_std.vlt:7:3",
    ),
    // Reported once (#1497): the task that started the promise used to report it too.
    (
        "process_lifetime_sync_main_rejection",
        1,
        "Uncaught Bad [Error]: bad 3 at tests/golden/lang/process_lifetime_sync_main_rejection.vlt:7:5",
    ),
    (
        "panic_uncaught_two_tasks",
        1,
        "Uncaught Bad [Error]: bad 3 at tests/golden/lang/panic_uncaught_two_tasks.vlt:8:5",
    ),
];

#[test]
fn panics_report_their_source_location() {
    let root = root();
    runtime_support::build_native_runtime(&root);
    let mut modes: Vec<&[&str]> = vec![&[], &["--release"]];
    if velt_codegen_llvm::available() {
        modes.push(&["--release", "--backend", "llvm"]);
    }
    let mut failures = vec![];
    for (name, want_code, want) in CASES {
        // Run from the repo root with a relative path: messages show the path as given.
        let file = format!("tests/golden/lang/{name}.vlt");
        for mode in &modes {
            let out = work_dir::work_dir(&root, "golden-work-panics").join(mode.join("_"));
            let o = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
                .arg("build")
                .args(*mode)
                .arg(&file)
                .arg("-o")
                .arg(out.join(name))
                .current_dir(&root)
                .output()
                .expect("run velt build");
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
            let exe = if cfg!(windows) {
                out.join(format!("{name}.exe"))
            } else {
                out.join(name)
            };
            let r = crate::no_window::command(&exe)
                .output()
                .expect("run program");
            let stderr = String::from_utf8_lossy(&r.stderr).replace("\r\n", "\n");
            let code = r.status.code().unwrap_or(-1);
            // Reported once: a second report (another task failing at the same time) would
            // follow the first before the process ends.
            let reports = stderr
                .lines()
                .filter(|l| l.starts_with("Uncaught "))
                .count();
            if code != *want_code || !stderr.lines().any(|l| l == *want) || reports > 1 {
                failures.push(format!(
                    "{name} {mode:?}: exit {code} (want {want_code})\n  want: {want}\n  stderr: {stderr}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// Panic texts compiled into a program name a standard-library file relative to the std root
/// (`std/math.vlt:17:15`), never by the absolute path of this checkout or install: std code
/// compiles to the same bytes on every machine, so it can be compiled once and shared (#1728).
#[test]
fn std_locations_are_relative_to_the_std_root() {
    let root = root();
    let o = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "bench/compile/all_std.vlt", "--emit", "vir"])
        .current_dir(&root)
        .output()
        .expect("run velt build");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let vir = String::from_utf8_lossy(&o.stdout);
    let std_root = root.join("std").display().to_string().replace('\\', "/");
    let absolute = vir
        .lines()
        .find(|l| l.replace('\\', "/").contains(&std_root));
    assert_eq!(absolute, None, "an absolute std path in the program");
    // gcd's `x % y` (its division-by-zero check).
    assert!(vir.contains(" at std/math.vlt:"), "no std location at all");
}
