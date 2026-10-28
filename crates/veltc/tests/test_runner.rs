//! `velt test` through the `velt` binary: the test files that only declare share one harness,
//! each file runs in a process of its own, files run in parallel with their output in file order
//! (`velt:test`), a failed assertion or a thrown error fails its test and the next one runs in
//! the same process, and a test that ends its process doesn't stop the tests after it.

use std::path::Path;
use std::process::Output;

mod no_window;
mod runtime_support;
mod test_dir;

fn velt_test(dir: &Path, args: &[&str]) -> Output {
    let mut command = crate::no_window::command(env!("CARGO_BIN_EXE_velt"));
    command
        .arg("test")
        .args(args)
        .current_dir(dir)
        .env("VELT_HOME", dir.join("home"));
    command.output().unwrap()
}

fn write(dir: &Path, rel: &str, text: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// The lines of `stdout` from `running <file>` up to the next `running` line.
fn block<'s>(stdout: &'s str, file: &str) -> Vec<&'s str> {
    let mut lines = stdout.lines().skip_while(|l| !l.ends_with(file));
    let head = lines
        .next()
        .unwrap_or_else(|| panic!("no `{file}` in:\n{stdout}"));
    std::iter::once(head)
        .chain(lines.take_while(|l| !l.starts_with("running ") && !l.is_empty()))
        .collect()
}

const A: &str = "\
export function test_same() {
  console.log(\"a same\");
}

export function test_panics() {
  assertEq(1, 2);
}

export function test_after_panic() {
  console.log(\"a after\");
}

export function test_throws() {
  throw new Error(\"boom\");
}

export function test_out_of_bounds() {
  const xs = [1];
  console.log(xs[3]);
}

export function test_last() {
  console.log(\"a last\");
}
";

const B: &str = "\
export function test_same() {
  console.log(\"b same\");
}

export async function test_async_fails() {
  await Promise.resolve(1);
  assert(false, \"async bad\");
}

export async function test_async_ok() {
  await Promise.resolve(2);
  console.log(\"b async ok\");
}
";

/// Runs code when evaluated: a harness of its own, so other files' processes don't print it.
const C: &str = "\
console.log(\"c top\");

export function test_c() {
  console.log(\"c ran\");
}
";

const E: &str = "\
export function test_exit() {
  process.exit(0);
}

export function test_e_after() {
  console.log(\"e after\");
}

class ParseError extends Error {}

export function test_custom_error() {
  throw new ParseError(\"bad port\");
}
";

#[test]
fn shared_harness_parallel_files_and_failures() {
    runtime_support::build_native_runtime(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    write(dir, "t/a.test.vlt", A);
    write(dir, "t/sub/b.test.vlt", B);
    write(dir, "t/c.test.vlt", C);
    write(dir, "t/e.test.vlt", E);

    let o = velt_test(dir, &["t"]);
    let stdout = String::from_utf8_lossy(&o.stdout).into_owned();
    assert_eq!(o.status.code(), Some(1), "{stdout}");
    assert_eq!(
        block(&stdout, "a.test.vlt")[1..12],
        [
            "a same",
            "ok test_same",
            "FAILED test_panics",
            "    AssertionError: assertEq failed at t/a.test.vlt:6:3",
            "      left: 1",
            "     right: 2",
            "a after",
            "ok test_after_panic",
            "FAILED test_throws",
            "    Error: boom at t/a.test.vlt:14:3",
            "FAILED test_out_of_bounds (exit code 101)",
        ],
        "{stdout}"
    );
    // A panic ends the process: the runner starts the file again after the test.
    let a = block(&stdout, "a.test.vlt");
    assert!(a[1..].len() >= 12, "{stdout}");
    assert!(
        a[12].starts_with("    panic: ") && a[12].contains("t/a.test.vlt:19:"),
        "{stdout}"
    );
    assert_eq!(a[a.len() - 2..], ["a last", "ok test_last"], "{stdout}");
    let b = block(&stdout, "b.test.vlt");
    for want in [
        "b same",
        "ok test_same",
        "FAILED test_async_fails",
        "b async ok",
        "ok test_async_ok",
    ] {
        assert!(
            b.iter().any(|l| l.starts_with(want)),
            "missing `{want}` in:\n{stdout}"
        );
    }
    // Its top-level line is in its own block alone (where in it: #1399, an `async main` prints
    // an imported module's top-level output last).
    let mut c = block(&stdout, "c.test.vlt")[1..].to_vec();
    c.sort_unstable();
    assert_eq!(c, ["c ran", "c top", "ok test_c"], "{stdout}");
    assert_eq!(stdout.matches("c top").count(), 1, "{stdout}");
    let e = block(&stdout, "e.test.vlt");
    assert_eq!(
        e[1..],
        [
            "FAILED test_exit (exit code 0)",
            "e after",
            "ok test_e_after",
            "FAILED test_custom_error",
            "    ParseError: bad port at t/e.test.vlt:12:3",
        ],
        "{stdout}"
    );
    assert!(stdout.contains("7 passed; 6 failed"), "{stdout}");

    // a, b and e share one harness, built once whatever failed; c has one of its own.
    let built: Vec<String> = std::fs::read_dir(dir.join("target/velt/test"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| !n.contains('.') || n.ends_with(".exe"))
        .collect();
    let mut stems: Vec<&str> = built.iter().map(|n| n.trim_end_matches(".exe")).collect();
    stems.sort();
    assert_eq!(stems, ["c_test_1", "harness_0"], "{built:?}");

    // The same output one file at a time.
    let one = velt_test(dir, &["t", "-j", "1"]);
    assert_eq!(String::from_utf8_lossy(&one.stdout), stdout);

    // A file that does not build fails its own tests; the others still run.
    write(
        dir,
        "t/d.test.vlt",
        "export function test_d() { const n: number = \"no\"; }\n",
    );
    let o = velt_test(dir, &["t"]);
    let stdout = String::from_utf8_lossy(&o.stdout);
    assert_eq!(
        block(&stdout, "d.test.vlt")[1..],
        ["FAILED test_d (build failed)"],
        "{stdout}"
    );
    assert!(stdout.contains("7 passed; 7 failed"), "{stdout}");
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert_eq!(stderr.matches("d.test.vlt:1:").count(), 1, "{stderr}");
}
