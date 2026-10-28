//! `velt test` through the `velt` binary: the test files that only declare share one harness,
//! each file runs in a process of its own, files run in parallel with their output in file order,
//! and a test that ends its process doesn't stop the tests after it.

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
        block(&stdout, "a.test.vlt")[1..],
        [
            "a same",
            "ok test_same",
            "FAILED test_panics (exit code 101)",
            "    assertion `left == right` failed",
            "      left: 1",
            "     right: 2",
            "    panic: assertEq failed at t/a.test.vlt:6:3",
            "a after",
            "ok test_after_panic",
            "FAILED test_throws (exit code 1)",
            "    Uncaught Error: boom at t/a.test.vlt:14:3",
        ],
        "{stdout}"
    );
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
    assert_eq!(
        block(&stdout, "c.test.vlt")[1..],
        ["c top", "c ran", "ok test_c"],
        "{stdout}"
    );
    assert_eq!(stdout.matches("c top").count(), 1, "{stdout}");
    let e = block(&stdout, "e.test.vlt");
    assert_eq!(
        e[1..],
        [
            "FAILED test_exit (exit code 0)",
            "e after",
            "ok test_e_after"
        ],
        "{stdout}"
    );
    assert!(stdout.contains("6 passed; 4 failed"), "{stdout}");

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
    assert!(stdout.contains("6 passed; 5 failed"), "{stdout}");
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert_eq!(stderr.matches("d.test.vlt:1:").count(), 1, "{stderr}");
}
