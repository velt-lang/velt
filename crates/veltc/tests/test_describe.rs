//! `velt test` with tests registered at run time (`describe`, `it`, `test` and the hooks of
//! `velt:test`, also imported as `vitest`) and `expect`: files whose top level only registers
//! share one harness with the other files, each file collects its tests in its own process and
//! lists them for the runner, `--filter` matches the full names, and a test that ends its
//! process is followed by a new one that collects again.

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

/// The test programs built in `dir`, without their extension, sorted.
fn built(dir: &Path) -> Vec<String> {
    let mut stems: Vec<String> = std::fs::read_dir(dir.join("target/velt/test"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| !n.contains('.') || n.ends_with(".exe"))
        .map(|n| n.trim_end_matches(".exe").to_string())
        .collect();
    stems.sort();
    stems
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

/// Nested groups, hooks in order, sync and `async` tests, `expect` and a `test_*` function.
const MATH: &str = "\
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, test } from \"velt:test\";

const log: string[] = [];

beforeEach(() => {
  log.push(\"file each\");
});

describe(\"math\", () => {
  beforeAll(() => {
    console.log(\"math setup\");
  });
  beforeEach(() => {
    log.push(\"math each\");
  });
  afterEach(() => {
    log.push(\"math after\");
  });
  afterAll(() => {
    console.log(\"math teardown\");
  });
  it(\"adds\", () => {
    expect(1 + 1).toBe(2);
  });
  it(\"fails\", () => {
    expect(1 + 1).toBe(3);
  });
  describe(\"async\", () => {
    test(\"awaits\", async () => {
      await Promise.resolve(1);
      expect([1, 2]).toEqual([1, 2]);
    });
    it(\"rejects\", async () => {
      throw new Error(\"nope\");
    });
  });
});

// The file's `beforeEach` runs for `test_function` (first) and for every test; the group's for
// its four.
it(\"hooks ran in order\", () => {
  const group = \"file each,math each,math after\";
  expect(log.join(\",\")).toBe(`file each,${group},${group},${group},${group},file each`);
});

export function test_function() {
  assert(true);
}
";

/// Imported as Vitest's module; a panic in the middle, after which a new process collects the
/// tests again and runs the group's `beforeAll` again before the next one.
const VITEST: &str = "\
import { beforeAll, describe, expect, it } from \"vitest\";

describe(\"crash\", () => {
  beforeAll(() => {
    console.log(\"crash setup\");
  });
  it(\"before\", () => {});
  it(\"panics\", () => {
    const xs: i64[] = [];
    console.log(xs[3]);
  });
  it(\"after\", () => {
    expect(\"after\").toMatch(/^aft/);
  });
});
";

/// A `describe` callback that throws: a test named after the group fails with the error, and
/// what it registered before still runs.
const COLLECT: &str = "\
import { describe, it } from \"velt:test\";

describe(\"setup\", () => {
  it(\"registered first\", () => {});
  throw new Error(\"cannot set up\");
});
";

#[test]
fn describe_it_and_expect() {
    runtime_support::build_native_runtime(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    write(dir, "t/math.test.vlt", MATH);
    write(dir, "t/vitest.test.vlt", VITEST);
    write(dir, "t/collect.test.vlt", COLLECT);

    let o = velt_test(dir, &["t"]);
    // Paths are printed as given, `t\math.test.vlt` on Windows.
    let stdout = String::from_utf8_lossy(&o.stdout).replace('\\', "/");
    assert_eq!(o.status.code(), Some(1), "{stdout}");
    assert_eq!(
        block(&stdout, "math.test.vlt")[1..],
        [
            "ok test_function",
            "math setup",
            "ok math > adds",
            "FAILED math > fails",
            "    AssertionError: expected 2 to be 3 // Object.is equality at t/math.test.vlt:26:5",
            "ok math > async > awaits",
            "FAILED math > async > rejects",
            "    Error: nope at t/math.test.vlt:34:7",
            "math teardown",
            "ok hooks ran in order",
        ],
        "{stdout}"
    );
    let crash = block(&stdout, "vitest.test.vlt");
    assert_eq!(
        crash[1..4],
        [
            "crash setup",
            "ok crash > before",
            "FAILED crash > panics (exit code 101)"
        ],
        "{stdout}"
    );
    assert!(
        crash[4].starts_with("    panic: index out of bounds"),
        "{stdout}"
    );
    assert_eq!(
        crash[crash.len() - 2..],
        ["crash setup", "ok crash > after"],
        "{stdout}"
    );
    assert_eq!(
        block(&stdout, "collect.test.vlt")[1..],
        [
            "ok setup > registered first",
            "FAILED setup",
            "    Error: cannot set up at t/collect.test.vlt:5:3",
        ],
        "{stdout}"
    );
    assert!(stdout.contains("7 passed; 4 failed"), "{stdout}");
    // Their top levels only register: one harness for the three, and the runner.
    assert_eq!(built(dir), ["harness_1", "runner_0"]);

    // `--filter` matches the full names, which only the test programs know.
    let o = velt_test(dir, &["t", "--filter", "math > async"]);
    let stdout = String::from_utf8_lossy(&o.stdout).replace('\\', "/");
    assert_eq!(
        block(&stdout, "math.test.vlt")[1..],
        [
            "math setup",
            "ok math > async > awaits",
            "FAILED math > async > rejects",
            "    Error: nope at t/math.test.vlt:34:7",
            "math teardown",
        ],
        "{stdout}"
    );
    assert!(stdout.contains("1 passed; 1 failed"), "{stdout}");
    let o = velt_test(dir, &["t", "--filter", "crash > after", "--exact"]);
    let stdout = String::from_utf8_lossy(&o.stdout);
    assert!(stdout.contains("1 passed; 0 failed"), "{stdout}");
}

/// Matchers added with `extend`, for every type or for one: they fail through `check` as the
/// built-in ones do, `.not` included, located at their `check` (not yet at the call in the test:
/// caller tracking is for standard-library functions).
const MATCHERS: &str = "\
import { Expect, expect, it } from \"velt:test\";

extend<T extends Comparable<T>> Expect<T> {
  toBeWithin(lo: T, hi: T) {
    this.check(this.actual >= lo && this.actual <= hi, `expected ${this.actual} to be within ${lo}..${hi}`);
  }
}

class Reply {
  status: i64 = 204;
}

extend Expect<Reply> {
  toHaveStatus(status: i64) {
    this.check(this.actual.status == status, `expected status ${status}, got ${this.actual.status}`);
  }
}

it(\"within\", () => {
  expect(5).toBeWithin(1, 9);
  expect(\"m\").toBeWithin(\"a\", \"z\");
});

it(\"not within\", () => {
  expect(5).not.toBeWithin(1, 9);
});

it(\"status\", () => {
  expect(new Reply()).toHaveStatus(200);
});
";

#[test]
fn custom_matchers() {
    runtime_support::build_native_runtime(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    write(dir, "t/matchers.test.vlt", MATCHERS);
    let o = velt_test(dir, &["t"]);
    let stdout = String::from_utf8_lossy(&o.stdout).replace('\\', "/");
    assert_eq!(
        block(&stdout, "matchers.test.vlt")[1..],
        [
            "ok within",
            "FAILED not within",
            "    AssertionError: expected 5 not to be within 1..9 at t/matchers.test.vlt:5:5",
            "FAILED status",
            "    AssertionError: expected status 200, got 204 at t/matchers.test.vlt:15:5",
        ],
        "{stdout}"
    );
}
