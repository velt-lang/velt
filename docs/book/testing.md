# Testing

`velt test` finds every `*.test.vlt` file (and `*.test.ts`, `*.test.tsx`) and runs every exported function whose name starts
with `test_`, and the tests registered with `describe` and `it`
([below](#describe-it-and-expect)). There is no framework to install and no configuration.

```ts ignore
// tests/greet.test.vlt
import { greet } from "../src/greet";

export function test_greets_by_name() {
  assertEq(greet("Ada"), "Hello, Ada!");
}

export function test_blank_name_greets_the_world() {
  assertEq(greet("  "), "Hello, world!");
}
```

```
$ velt test
running tests/greet.test.vlt
ok test_greets_by_name
ok test_blank_name_greets_the_world

test result: ok. 2 passed; 0 failed
```

## Writing tests

- A test is an `export function test_*()` with no parameters, or an
  `export async function test_*()`, which the runner awaits.
- `assert(cond, msg?)`, `assertEq(actual, expected, msg?)` and `assertThrows(() => …)` are
  built in. A failed one throws an `AssertionError` (as Node's `assert` does), which fails the
  test with the message and its source location; the next test runs.
- A test also fails if it throws an error it doesn't catch, or panics (an index out of bounds),
  which ends its process: the runner starts the file again from the next test.
- Helpers that are not exported, or don't start with `test_`, are not run.
- To check that something throws, `assertThrows` returns the error (and fails the test if
  nothing was thrown):

```ts
class ParseError extends Error {}

function parsePort(s: string): i64 throws ParseError {
  const n = Number(s);
  if (n != n) throw new ParseError(`bad port: ${s}`);
  return n as i64;
}

export function test_rejects_garbage() {
  const e = assertThrows(() => {
    parsePort("x");
  });
  assertEq(e.message, "bad port: x");
}

export function test_parses() {
  assertEq(parsePort("8080"), 8080);
}
```

When a test fails, the runner shows why and where:

```
FAILED test_parses_twice
    AssertionError: assertEq failed at tests/port.test.vlt:14:3
      left: 2
     right: 3
FAILED test_uncaught
    ParseError: bad port: x at tests/port.test.vlt:5:15
FAILED test_out_of_range (exit code 101)
    panic: index out of bounds: the len is 1 but the index is 3 at tests/port.test.vlt:20:15
```

## `describe`, `it` and `expect`

Tests can also be written as in Vitest, with `describe`, `it` (or `test`) and `expect` from
`velt:test`. A file written for Vitest imports them from `"vitest"`, which is `velt:test` in
Velt, and runs as it is:

```ts
import { beforeEach, describe, expect, it } from "velt:test";

class Cart {
  items: string[] = [];
  add(item: string) {
    this.items.push(item);
  }
}

describe("cart", () => {
  let cart = new Cart();
  beforeEach(() => {
    cart = new Cart();
  });

  it("starts empty", () => {
    expect(cart.items).toHaveLength(0);
  });

  it("adds items", () => {
    cart.add("tea");
    expect(cart.items).toEqual(["tea"]);
    expect(cart.items).not.toContain("coffee");
  });

  describe("async", () => {
    it("waits", async () => {
      await expect(Promise.resolve(cart.items.length)).resolves.toBe(0);
    });
  });
});
```

```
running tests/cart.test.vlt
ok cart > starts empty
ok cart > adds items
ok cart > async > waits
```

- A test's name is its groups' names and its own, joined by ` > ` as Vitest prints them;
  `--filter` and `--exact` match that full name.
- A callback may be `async`; it is awaited, and a rejection fails the test as a throw does.
- `beforeAll` and `afterAll` run once around a group's tests (or a file's, at the top level),
  `beforeEach` and `afterEach` around each of them: the outer groups' `beforeEach` first, the
  inner groups' `afterEach` first, and `afterEach` also after a failure. A group's `afterEach`
  and `afterAll` hooks run last-registered first, as in Vitest. A `beforeAll` that throws fails
  the group's tests with its error, and the hooks of the groups inside it don't run.
- A `describe` callback runs when the file's tests are collected, before the first test runs.
  If it throws, none of the group's tests run: a test named as the group fails with the error.
- Not yet as in Vitest: a `describe` callback can't be `async`, and a hook can't return a
  teardown function (`beforeEach(() => () => cleanup())`). Both fail with a message saying so;
  await in a `beforeAll` instead, and clean up in `afterEach` or `afterAll`.
- Matchers: `toBe` (`Object.is`), `toEqual` (deep equality as Vitest's: contents, recursively,
  class instances by their fields, a `Date` by its time, a `Set` by its members in any order,
  numbers with `Object.is`), `toBeNull`, `toBeUndefined`, `toBeDefined`, `toBeTruthy`,
  `toBeFalsy`, `toContain` and `toHaveLength` (an array or a string), `toMatch` (a string: a
  `RegExp`, or a string it contains), `toBeGreaterThan`, `toBeGreaterThanOrEqual`,
  `toBeLessThan`, `toBeLessThanOrEqual` (a `Comparable` type: numbers, strings), and `toThrow` on
  a function (`expect(() => parse("x")).toThrow("bad")`). `.not` checks the opposite. `await
  expect(p).resolves` and `.rejects` check what a promise resolves to, or its error
  (`.rejects.toThrow("boom")`). Failure messages are Vitest's (`expected 2 to be 3 // Object.is
  equality`).
- A matcher only fits the types it checks: `expect(1).toContain(1)` is a compile error.
- `export function test_*` and `describe`/`it` can be in one file: the functions run first.
- When a test ends its process (a panic), the runner starts the file again from the next test.
  The new process collects the tests again and runs the `beforeAll` hooks of the groups that
  are left.

### Custom matchers

A matcher is a method of `Expect<T>`, so an `extend` block adds one: for every type a bound
allows, or for one type, where a call on any other type is a compile error. It checks with
`this.check(pass, message)`, as the built-in matchers do; `.not` turns it round, writing the
message's first ` to ` as ` not to ` (or pass the message for `.not` as a third argument).

```ts
import { Expect, expect, it } from "velt:test";

extend<T extends Comparable<T>> Expect<T> {
  toBeWithin(lo: T, hi: T) {
    this.check(
      this.actual >= lo && this.actual <= hi,
      `expected ${this.actual} to be within ${lo}..${hi}`,
    );
  }
}

class Reply {
  status: i64 = 204;
}

// Only for a `Reply`: `expect(42).toHaveStatus(204)` doesn't compile.
extend Expect<Reply> {
  toHaveStatus(status: i64) {
    this.check(this.actual.status == status, `expected status ${status}, got ${this.actual.status}`);
  }
}

it("checks a reply", () => {
  expect(new Reply()).toHaveStatus(204);
  expect(7).toBeWithin(1, 9);
  expect(12).not.toBeWithin(1, 9);
});
```

`velt:test/http` has matchers for a `Response`: `import "velt:test/http"`, then
`expect(res).toHaveStatus(204)` and `expect(res).toHaveHeader("etag", "\"v1\"")` (without the
value: that the header is there; with `.not`: that it isn't).

The calls are direct, so a custom matcher costs what a built-in one does. An `extend` block
applies wherever its module is loaded, so a library of matchers is a module that test files
import (`import "./matchers"`). A custom matcher's failure is located at its `check` call;
the built-in matchers' at the `expect` in the test.

## Running tests

```sh
velt test                       # every *.test.vlt (.ts, .tsx) of the package (or under the current directory)
velt test tests/api.test.vlt    # one file
velt test --filter parse        # the tests whose name contains `parse`
velt test --filter test_parse --exact  # just `test_parse`
velt test --release             # optimized build
velt test --watch               # rerun on every change
velt test --jobs 1              # one test file at a time (default: one per core)
velt test --no-run              # build the test programs without running them
```

The runner prints `ok <name>` or `FAILED <name>` per test and a summary, and exits with 1 if any
test failed, so it works in CI as is. A `--filter` that matches no test says so and builds
nothing. Each test file runs in a process of its own, several at
once (`--jobs`), and its output is printed in file order, whatever finishes first. The tests of
a file run one after another, in source order, in that process; one that ends it (a panic, a
crash, `process.exit`) is reported, and a new process runs the tests after it. The runner itself
is written in Velt: the standard library's `velt:test`, compiled into the test program. Run a
test program by hand (`velt test --no-run` prints its path) and it runs its tests itself.
`--watch` reruns the tests whenever a file they import, a test file or the manifest changes. `--no-run` prints the paths of the test programs, to run one
under a debugger (`--json` describes them for tools). In VS Code, **▶ Run test | Debug test**
above a test runs or debugs just that test ([Debugging](../tooling/debugging.md#vs-code)).

### Checking for leaks

`VELT_RC_STATS=1 velt test` also checks that each test file's process frees every block it
allocates (debug builds; the standard library's own tests run this way). A file whose tests leave
something allocated fails with `FAILED <file> (N blocks not freed)`, and one whose process
reported no counts (a runtime without the debug counters) with `no block counts on its stderr`.
Close every server and
connection a test opens: one still running 120 s after the file's tests fails the file as
`still running … after its tests`.

## How tests are built

The test files are compiled into one program, so the compiler runs once for all of them. Every
module a program imports is evaluated when it starts
([Modules](../reference/modules.md#scripts-top-level-statements)): in that program, each test
file's process also evaluates the other test files and the modules they import. That is harmless
for a file that only declares (imports, functions, classes, types, and constants made of
literals), and most test files do. A test file whose evaluation runs code (statements at the top
level, or a constant or static field initialized with a call, `const Page = component(...)`,
or with something that can panic: `n!`, a cast like `raw as string`, an integer `/` or `%`)
gets a program of its own, so other files never see what it does. So does a test file that
imports such a module by a relative path (`import { DATA } from "./fixtures"`), directly or
through other modules: if that code fails, only that file's tests fail ("exit code 1 before its
tests ran"). Imports of packages are not followed; keep a package's top level free of side
effects, as for any module. When a test file doesn't compile, its tests fail ("build failed")
and the other files are built together again without it.

## Testing servers

Start the server on port 0 (any free port) inside the test and call it with `fetch`; close it
at the end. The `api` template (`velt new todo --template api`) has complete examples, and the
[HTTP guide](http-server.md#testing-it) walks through one.

## Testing command-line tools

Keep the logic in a function that takes the argument array and returns output lines, and test
that function directly; `main` only does the printing and the exit code. See the
[CLI guide](cli-app.md#testing).
