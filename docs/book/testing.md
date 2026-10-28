# Testing

`velt test` finds every `*.test.vlt` file (and `*.test.ts`, `*.test.tsx`) and runs every exported function whose name starts
with `test_`. There is no framework to install and no configuration.

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
test failed, so it works in CI as is. Each test file runs in a process of its own, several at
once (`--jobs`), and its output is printed in file order, whatever finishes first. The tests of
a file run one after another, in source order, in that process; one that ends it (a panic,
`process.exit`) is reported, and a new process runs the tests after it. The runner itself is
written in Velt: the standard library's `velt:test`, compiled into the test program. Run a test
program by hand (`velt test --no-run` prints its path) and it runs its tests itself. `--watch` reruns the tests whenever a file they import, a
test file or the manifest changes. `--no-run` prints the paths of the test programs, to run one
under a debugger (`--json` describes them for tools). In VS Code, **▶ Run test | Debug test**
above a test runs or debugs just that test ([Debugging](../tooling/debugging.md#vs-code)).

## How tests are built

The test files are compiled into one program, so the compiler runs once for all of them. Every
module a program imports is evaluated when it starts
([Modules](../reference/modules.md#scripts-top-level-statements)): in that program, each test
file's process also evaluates the other test files and the modules they import. That is harmless
for a file that only declares (imports, functions, classes, types, and constants made of
literals), and most test files do. A test file whose evaluation runs code (statements at the top
level, or a constant or static field initialized with a call, `const Page = component(...)`)
gets a program of its own, so other files never see what it does. The modules under `src/` that
test files import are evaluated in every file's process: keep their top level free of side
effects, as for any module.

## Testing servers

Start the server on port 0 (any free port) inside the test and call it with `fetch`; close it
at the end. The `api` template (`velt new todo --template api`) has complete examples, and the
[HTTP guide](http-server.md#testing-it) walks through one.

## Testing command-line tools

Keep the logic in a function that takes the argument array and returns output lines, and test
that function directly; `main` only does the printing and the exit code. See the
[CLI guide](cli-app.md#testing).
