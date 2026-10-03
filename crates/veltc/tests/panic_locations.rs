//! Panic and uncaught-error messages name their source location (stderr; the goldens under
//! `tests/golden/lang/panic_*` check stdout and exit codes). Each program runs in debug and
//! release mode, plus release with the LLVM backend when clang is installed.

use std::path::{Path, PathBuf};

mod no_window;
mod runtime_support;

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
        "Uncaught ParseError: bad digit: x at tests/golden/lang/panic_uncaught.vlt:13:3",
    ),
    (
        "panic_uncaught_std",
        1,
        "Uncaught IoError: ENOENT: no such file or directory, lstat 'velt-missing-file-b' at tests/golden/lang/panic_uncaught_std.vlt:7:3",
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
            let out = root.join("target/golden-work-panics").join(mode.join("_"));
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
            if code != *want_code || !stderr.lines().any(|l| l == *want) {
                failures.push(format!(
                    "{name} {mode:?}: exit {code} (want {want_code})\n  want: {want}\n  stderr: {stderr}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
