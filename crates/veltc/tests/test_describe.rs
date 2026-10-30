//! `velt test` with tests registered at run time (`describe`, `it`, `test` and the hooks of
//! `velt/test`, also imported as `vitest`) and `expect`: files whose top level only registers
//! share one harness with the other files, each file collects its tests in its own process and
//! lists them for the runner, `--filter` matches the full names, and a test that ends its
//! process is followed by a new one that collects again.

use std::path::Path;
use std::process::Output;

mod no_window;
mod runtime_support;
mod test_dir;

/// `velt test` in `dir`, without kept results: these tests are about what the runner prints, which
/// a kept result (`(cached)`, or a note on what a test reaches) would change from run to run.
fn velt_test(dir: &Path, args: &[&str]) -> Output {
    let mut command = crate::no_window::command(env!("CARGO_BIN_EXE_velt"));
    command
        .args(["test", "--no-cache"])
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
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, test } from \"velt/test\";

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
/// none of the group's tests run, what it registered before the throw included (as in Vitest).
const COLLECT: &str = "\
import { describe, it } from \"velt/test\";

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
            "FAILED setup",
            "    Error: cannot set up at t/collect.test.vlt:5:3",
        ],
        "{stdout}"
    );
    assert!(stdout.contains("6 passed; 4 failed"), "{stdout}");
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
import { Expect, expect, it } from \"velt/test\";

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

/// Every built-in matcher passing, failing and with `.not`, with Vitest's messages (checked
/// against Vitest 3.2.4 for the same file: they differ only in Vitest's `+0` for a zero and in
/// the "promise rejected / resolved instead of …" messages being plain `Error`s there).
const EVERY_MATCHER: &str = r#"import { describe, expect, it } from "velt/test";

class P {
  x: number;
  constructor(x: number) {
    this.x = x;
  }
}
function boom(): number {
  throw new Error("boom here");
}
async function three(): Promise<number> {
  return 3;
}
async function failing(): Promise<number, Error> {
  throw new Error("went wrong");
}
const nothing: string | null = null;
const missing: number | undefined = undefined;

describe("pass", () => {
  it("each matcher", async () => {
    expect(nothing).toBeNull();
    expect(missing).toBeUndefined();
    expect(1).toBeDefined();
    expect("x").toBeTruthy();
    expect(0).toBeFalsy();
    expect([1, 2]).toContain(2);
    expect("hello").toContain("ell");
    expect([1, 2]).toHaveLength(2);
    expect("hello").toHaveLength(5);
    expect("hello").toMatch("ll");
    expect("hello").toMatch(/^h/);
    expect(2).toBeGreaterThan(1);
    expect(2).toBeGreaterThanOrEqual(2);
    expect(1).toBeLessThan(2);
    expect(2).toBeLessThanOrEqual(2);
    expect(() => boom()).toThrow();
    expect(() => boom()).toThrow("boom");
    expect(() => boom()).toThrow(/here$/);
    expect(() => 1).not.toThrow();
    expect(() => boom()).not.toThrow("other");
    expect(new P(1)).toEqual(new P(1));
    await expect(three()).resolves.toBe(3);
    await expect(Promise.reject(new Error("r"))).rejects.toThrow("r");
  });
  it("not on each block", () => {
    expect([1]).not.toContain(2);
    expect("a").not.toMatch("b");
    expect(1).not.toBeGreaterThan(2);
    expect(1).not.toBeNull();
  });
});

describe("fail", () => {
  it("toBeNull", () => expect(1).toBeNull());
  it("toBeUndefined", () => expect(1).toBeUndefined());
  it("toBeDefined", () => expect(missing).toBeDefined());
  it("toBeTruthy", () => expect(0).toBeTruthy());
  it("toBeFalsy", () => expect(1).toBeFalsy());
  it("toContain array", () => expect([1, 2]).toContain(3));
  it("toContain string", () => expect("abc").toContain("x"));
  it("toHaveLength array", () => expect([1]).toHaveLength(2));
  it("toHaveLength string", () => expect("ab").toHaveLength(3));
  it("toMatch string", () => expect("abc").toMatch("x"));
  it("toMatch regexp", () => expect("abc").toMatch(/x/i));
  it("toBeGreaterThan", () => expect(1).toBeGreaterThan(2));
  it("toBeGreaterThanOrEqual", () => expect(1).toBeGreaterThanOrEqual(2));
  it("toBeLessThan", () => expect(2).toBeLessThan(1));
  it("toBeLessThanOrEqual", () => expect(2).toBeLessThanOrEqual(1));
  it("toThrow", () => expect(() => 1).toThrow());
  it("toThrow string", () => expect(() => boom()).toThrow("x"));
  it("toThrow regexp", () => expect(() => boom()).toThrow(/x/));
  it("not.toThrow", () => expect(() => boom()).not.toThrow());
  it("not.toThrow string", () => expect(() => boom()).not.toThrow("boom"));
  it("not.toContain", () => expect([1]).not.toContain(1));
  it("not.toContain string", () => expect("abc").not.toContain("b"));
  it("not.toMatch", () => expect("a").not.toMatch(/a/));
  it("not.toBeLessThan", () => expect(1).not.toBeLessThan(2));
  it("resolves rejected", async () => {
    await expect(failing()).resolves.toBe(1);
  });
  it("rejects resolved", async () => {
    await expect(three()).rejects.toThrow();
  });
  it("rejects other message", async () => {
    await expect(Promise.reject(new Error("r"))).rejects.toThrow("x");
  });
  it("resolves value", async () => {
    await expect(three()).resolves.toBe(4);
  });
});
describe("negated", () => {
  it("not.toBe", () => expect(1).not.toBe(1));
  it("not.toEqual", () => expect([1]).not.toEqual([1]));
  it("not.toBeNull", () => expect(null).not.toBeNull());
  it("not.toBeUndefined", () => expect(undefined).not.toBeUndefined());
  it("not.toBeDefined", () => expect(1).not.toBeDefined());
  it("not.toBeTruthy", () => expect(1).not.toBeTruthy());
  it("not.toBeFalsy", () => expect(0).not.toBeFalsy());
  it("not.toHaveLength", () => expect([1]).not.toHaveLength(1));
  it("not.toMatch string", () => expect("abc").not.toMatch("b"));
  it("not.toBeGreaterThan", () => expect(2).not.toBeGreaterThan(1));
  it("not.toBeGreaterThanOrEqual", () => expect(2).not.toBeGreaterThanOrEqual(1));
  it("not.toBeLessThanOrEqual", () => expect(1).not.toBeLessThanOrEqual(2));
  it("toEqual", () => expect({ a: 1 }).toEqual({ a: 2 }));
});
"#;

#[test]
fn every_matcher_passes_and_fails_as_in_vitest() {
    runtime_support::build_native_runtime(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    write(dir, "t/matchers.test.vlt", EVERY_MATCHER);
    let o = velt_test(dir, &["t"]);
    let stdout = String::from_utf8_lossy(&o.stdout).replace('\\', "/");
    let block = block(&stdout, "matchers.test.vlt");
    assert_eq!(
        block[1..3],
        ["ok pass > each matcher", "ok pass > not on each block"],
        "{stdout}"
    );
    // The messages, without their locations; each is at its test's line.
    let messages: Vec<&str> = block
        .iter()
        .filter_map(|l| l.strip_prefix("    "))
        .map(|l| l.split(" at t/matchers.test.vlt:").next().unwrap())
        .collect();
    assert_eq!(
        messages,
        [
            r#"AssertionError: expected 1 to be null"#,
            r#"AssertionError: expected 1 to be undefined"#,
            r#"AssertionError: expected undefined to be defined"#,
            r#"AssertionError: expected 0 to be truthy"#,
            r#"AssertionError: expected 1 to be falsy"#,
            r#"AssertionError: expected [ 1, 2 ] to include 3"#,
            r#"AssertionError: expected 'abc' to contain 'x'"#,
            r#"AssertionError: expected [ 1 ] to have a length of 2 but got 1"#,
            r#"AssertionError: expected 'ab' to have a length of 3 but got 2"#,
            r#"AssertionError: expected 'abc' to match 'x'"#,
            r#"AssertionError: expected 'abc' to match /x/i"#,
            r#"AssertionError: expected 1 to be greater than 2"#,
            r#"AssertionError: expected 1 to be greater than or equal to 2"#,
            r#"AssertionError: expected 2 to be less than 1"#,
            r#"AssertionError: expected 2 to be less than or equal to 1"#,
            r#"AssertionError: expected [Function] to throw an error"#,
            r#"AssertionError: expected [Function] to throw error including 'x' but got 'boom here'"#,
            r#"AssertionError: expected [Function] to throw error matching /x/ but got 'boom here'"#,
            r#"AssertionError: expected [Function] to not throw an error but 'Error: boom here' was thrown"#,
            r#"AssertionError: expected [Function] to throw error not including 'boom'"#,
            r#"AssertionError: expected [ 1 ] to not include 1"#,
            r#"AssertionError: expected 'abc' not to contain 'b'"#,
            r#"AssertionError: expected 'a' not to match /a/"#,
            r#"AssertionError: expected 1 to be not less than 2"#,
            r#"AssertionError: promise rejected "Error: went wrong" instead of resolving"#,
            r#"AssertionError: promise resolved "3" instead of rejecting"#,
            r#"AssertionError: expected [Function] to throw error including 'x' but got 'r'"#,
            r#"AssertionError: expected 3 to be 4 // Object.is equality"#,
            r#"AssertionError: expected 1 not to be 1 // Object.is equality"#,
            r#"AssertionError: expected [ 1 ] to not deeply equal [ 1 ]"#,
            r#"AssertionError: expected null not to be null"#,
            r#"AssertionError: expected undefined not to be undefined"#,
            r#"AssertionError: expected 1 to be undefined"#,
            r#"AssertionError: expected 1 to not be truthy"#,
            r#"AssertionError: expected 0 to not be falsy"#,
            r#"AssertionError: expected [ 1 ] to not have a length of 1"#,
            r#"AssertionError: expected 'abc' not to match 'b'"#,
            r#"AssertionError: expected 2 to be not greater than 1"#,
            r#"AssertionError: expected 2 to be not greater than or equal to 1"#,
            r#"AssertionError: expected 1 to be not less than or equal to 2"#,
            r#"AssertionError: expected { a: 1 } to deeply equal { a: 2 }"#,
        ],
        "{stdout}"
    );
    assert!(
        block.contains(&"    AssertionError: expected 1 to be null at t/matchers.test.vlt:56:24"),
        "{stdout}"
    );
    assert!(stdout.contains("2 passed; 41 failed"), "{stdout}");
}

/// Vitest's semantics where they are easy to get wrong:
/// - `afterEach` and `afterAll` run last-registered first, inner groups' first.
/// - When an outer `beforeAll` throws, the inner groups' hooks don't run (the outer `afterAll`
///   does).
/// - An `async` `describe` and a hook that returns a teardown function fail loudly: Velt can't
///   await the one or run the other yet.
/// - A failing `beforeEach` still runs `afterEach`; a failing `afterEach` or a rejecting
///   `beforeAll` fails the test; a failing `afterAll` fails the run.
/// - A name with a line break is escaped.
/// - With registrations, a `--filter` that matches nothing says so.
const SEMANTICS: &str = r#"import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it } from "velt/test";

describe("order", () => {
  afterEach(() => console.log("outer ae1"));
  afterEach(() => console.log("outer ae2"));
  afterAll(() => console.log("outer aa1"));
  afterAll(() => console.log("outer aa2"));
  describe("inner", () => {
    afterEach(() => console.log("inner ae1"));
    afterEach(() => console.log("inner ae2"));
    afterAll(() => console.log("inner aa1"));
    afterAll(() => console.log("inner aa2"));
    it("runs", () => {});
  });
});

describe("outer setup fails", () => {
  beforeAll(() => {
    throw new Error("outer ba");
  });
  afterAll(() => console.log("outer aa"));
  describe("inner", () => {
    beforeAll(() => console.log("inner ba"));
    afterAll(() => console.log("inner aa"));
    it("x", () => {});
  });
});

describe("async", async () => {
  await Promise.resolve(1);
  it("late", () => {});
});

describe("teardown", () => {
  beforeEach(() => () => console.log("teardown"));
  it("t", () => {});
});

describe("failing hooks", () => {
  describe("beforeEach", () => {
    beforeEach(() => {
      throw new Error("be");
    });
    afterEach(() => console.log("ae after a failed be"));
    it("t", () => {});
  });
  describe("afterEach", () => {
    afterEach(() => {
      throw new Error("ae");
    });
    it("t", () => {});
  });
  describe("async beforeAll", () => {
    beforeAll(async () => {
      await Promise.resolve(1);
      throw new Error("ba rejected");
    });
    it("t", () => {});
  });
  describe("afterAll", () => {
    afterAll(() => {
      throw new Error("aa");
    });
    it("t", () => {
      expect(1).toBe(1);
    });
  });
});

it("multi\nline", () => {});
"#;

#[test]
fn hooks_and_groups_as_in_vitest() {
    runtime_support::build_native_runtime(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    write(dir, "t/semantics.test.vlt", SEMANTICS);
    let o = velt_test(dir, &["t"]);
    // Paths are printed as given (`t\semantics.test.vlt` on Windows); the name `multi\nline`
    // keeps its backslash.
    let stdout = String::from_utf8_lossy(&o.stdout).replace("t\\semantics", "t/semantics");
    assert_eq!(o.status.code(), Some(1), "{stdout}");
    assert_eq!(
        block(&stdout, "semantics.test.vlt")[1..],
        [
            r#"inner ae2"#,
            r#"inner ae1"#,
            r#"outer ae2"#,
            r#"outer ae1"#,
            r#"ok order > inner > runs"#,
            r#"inner aa2"#,
            r#"inner aa1"#,
            r#"outer aa2"#,
            r#"outer aa1"#,
            r#"FAILED outer setup fails > inner > x"#,
            r#"    Error: outer ba at t/semantics.test.vlt:19:5"#,
            r#"outer aa"#,
            r#"FAILED async"#,
            r#"    Error: a `describe` callback can't be `async` yet (Vitest awaits it): register the group's tests without awaiting, and await in a `beforeAll`"#,
            r#"FAILED teardown > t"#,
            r#"    Error: a hook returned a function: Vitest runs it as the hook's teardown, Velt doesn't yet; clean up in `afterEach` or `afterAll`"#,
            r#"ae after a failed be"#,
            r#"FAILED failing hooks > beforeEach > t"#,
            r#"    Error: be at t/semantics.test.vlt:42:7"#,
            r#"FAILED failing hooks > afterEach > t"#,
            r#"    Error: ae at t/semantics.test.vlt:49:7"#,
            r#"FAILED failing hooks > async beforeAll > t"#,
            r#"    Error: ba rejected at t/semantics.test.vlt:56:7"#,
            r#"ok failing hooks > afterAll > t"#,
            r#"afterAll of `failing hooks > afterAll` failed: Error: aa at t/semantics.test.vlt:62:7"#,
            r#"ok multi\nline"#,
            r#"FAILED (test harness exited with code 1 after all tests ran)"#,
        ],
        "{stdout}"
    );
    assert!(stdout.contains("3 passed; 7 failed"), "{stdout}");

    let o = velt_test(dir, &["t", "--filter", "nomatch"]);
    assert_eq!(o.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&o.stdout);
    assert!(!stdout.contains("running "), "{stdout}");
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(stderr.contains("no tests match `nomatch`"), "{stderr}");
}

/// `toEqual` as Vitest's: class instances by their fields (also inside a `Map` and through a
/// nullable field), a `Date` by its time, a `Set` by its members in any order, `0` unequal to
/// `-0` and `NaN` equal to itself; cycles by Vitest's rule (the same pair met again is equal, an
/// object met again with another partner differs: rings of two lengths), through classes and
/// object literals; `toBe` still compares identity.
const TO_EQUAL: &str = r#"import { expect, it } from "velt/test";

class P {
  x: number;
  y?: number;
  constructor(x: number) {
    this.x = x;
  }
}
class Node2 {
  v: number;
  next: Node2 | null = null;
  constructor(v: number) {
    this.v = v;
  }
}

it("class instances by fields", () => expect(new P(1)).toEqual(new P(1)));
it("class instances differ", () => expect(new P(1)).toEqual(new P(2)));
it("optional field present on one", () => {
  const a = new P(1);
  a.y = 2;
  expect(a).toEqual(new P(1));
});
it("dates by time", () => expect(new Date(1)).toEqual(new Date(1)));
it("dates differ", () => expect(new Date(1)).toEqual(new Date(2)));
it("sets in any order", () => expect(new Set([1, 2])).toEqual(new Set([2, 1])));
it("sets differ", () => expect(new Set([1, 2])).toEqual(new Set([1, 3])));
it("0 and -0 differ", () => expect(0).toEqual(-0));
it("NaN", () => expect(NaN).toEqual(NaN));
it("nested nullable", () => {
  const a = new Node2(1);
  a.next = new Node2(2);
  const b = new Node2(1);
  b.next = new Node2(2);
  expect(a).toEqual(b);
  expect([a, null]).toEqual([b, null]);
});
it("maps of classes", () => {
  const m1 = new Map<string, P>([["a", new P(1)]]);
  const m2 = new Map<string, P>([["a", new P(1)]]);
  expect(m1).toEqual(m2);
});
it("toBe still by identity", () => expect(new P(1)).not.toBe(new P(1)));

class Ring {
  v: number;
  next: Ring | null = null;
  constructor(v: number) {
    this.v = v;
  }
}
it("cycles equal", () => {
  const a = new Ring(1);
  a.next = a;
  const b = new Ring(1);
  b.next = b;
  expect(a).toEqual(b);
});
it("rings equal", () => {
  const a = new Ring(1);
  a.next = new Ring(2);
  a.next.next = a;
  const b = new Ring(1);
  b.next = new Ring(2);
  b.next.next = b;
  expect(a).toEqual(b);
});
it("cycles differ", () => {
  const a = new Ring(1);
  a.next = a;
  const b = new Ring(1);
  b.next = new Ring(2);
  b.next.next = b;
  expect(a).toEqual(b);
});

function ring(len: number): Ring {
  const first = new Ring(1);
  let last = first;
  for (let i = 1; i < len; i++) {
    const n = new Ring(1);
    last.next = n;
    last = n;
  }
  last.next = first;
  return first;
}
it("rings of two lengths differ", () => expect(ring(1)).toEqual(ring(2)));
it("rings of two lengths differ either way", () => expect(ring(3)).toEqual(ring(2)));
it("rings of one length", () => expect(ring(3)).toEqual(ring(3)));
type L = { v: number; next: L | null };
it("object literal cycles", () => {
  const a: L = { v: 1, next: null };
  a.next = a;
  const b: L = { v: 1, next: null };
  b.next = b;
  expect(a).toEqual(b);
});
"#;

#[test]
fn to_equal_compares_as_vitest_does() {
    runtime_support::build_native_runtime(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    write(dir, "t/eq.test.vlt", TO_EQUAL);
    let o = velt_test(dir, &["t"]);
    let stdout = String::from_utf8_lossy(&o.stdout).replace("t\\eq", "t/eq");
    let lines: Vec<&str> = block(&stdout, "eq.test.vlt")[1..]
        .iter()
        .map(|l| l.split(" at t/eq.test.vlt:").next().unwrap())
        .collect();
    assert_eq!(
        lines,
        [
            r#"ok class instances by fields"#,
            r#"FAILED class instances differ"#,
            r#"    AssertionError: expected P { x: 1, y: undefined } to deeply equal P { x: 2, y: undefined }"#,
            r#"FAILED optional field present on one"#,
            r#"    AssertionError: expected P { x: 1, y: 2 } to deeply equal P { x: 1, y: undefined }"#,
            r#"ok dates by time"#,
            r#"FAILED dates differ"#,
            r#"    AssertionError: expected 1970-01-01T00:00:00.001Z to deeply equal 1970-01-01T00:00:00.002Z"#,
            r#"ok sets in any order"#,
            r#"FAILED sets differ"#,
            r#"    AssertionError: expected Set(2) { 1, 2 } to deeply equal Set(2) { 1, 3 }"#,
            r#"FAILED 0 and -0 differ"#,
            r#"    AssertionError: expected 0 to deeply equal -0"#,
            r#"ok NaN"#,
            r#"ok nested nullable"#,
            r#"ok maps of classes"#,
            r#"ok toBe still by identity"#,
            r#"ok cycles equal"#,
            r#"ok rings equal"#,
            r#"FAILED cycles differ"#,
            r#"    AssertionError: expected <ref *1> Ring { v: 1, next: [Circular *1] } to deeply equal <ref *1> Ring { v: 1, next: Ring { v: 2, next: [Circular *1] } }"#,
            r#"FAILED rings of two lengths differ"#,
            r#"    AssertionError: expected <ref *1> Ring { v: 1, next: [Circular *1] } to deeply equal <ref *1> Ring { v: 1, next: Ring { v: 1, next: [Circular *1] } }"#,
            r#"FAILED rings of two lengths differ either way"#,
            r#"    AssertionError: expected <ref *1> Ring {"#,
            r#"      v: 1,"#,
            r#"      next: Ring { v: 1, next: Ring { v: 1, next: [Circular *1] } }"#,
            r#"    } to deeply equal <ref *1> Ring { v: 1, next: Ring { v: 1, next: [Circular *1] } }"#,
            r#"ok rings of one length"#,
            r#"ok object literal cycles"#,
        ],
        "{stdout}"
    );
}
