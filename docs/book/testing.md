# Testing

`velt test` finds every `*.test.vlt` file and runs every exported function whose name starts
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
- `assert(cond, msg?)` and `assertEq(actual, expected, msg?)` are built in. A failed assertion
  panics, which fails the test with the message and its source location.
- A test also fails if it throws an error it doesn't catch.
- Helpers that are not exported, or don't start with `test_`, are not run.
- To check that something throws, catch it:

```ts
class ParseError extends Error {}

function parsePort(s: string): i64 throws ParseError {
  const n = Number(s);
  if (n != n) throw new ParseError(`bad port: ${s}`);
  return n as i64;
}

export function test_rejects_garbage() {
  try {
    parsePort("x");
    assert(false, "expected a ParseError");
  } catch (e) {
    assertEq(e.message, "bad port: x");
  }
}

export function test_parses() {
  assertEq(parsePort("8080"), 8080);
}
```

When a test fails, the runner shows why and where:

```
FAILED test_parses_twice (exit code 101)
    assertion `left == right` failed
      left: 2
     right: 3
    panic: assertEq failed at tests/port.test.vlt:14:3
FAILED test_uncaught (exit code 1)
    Uncaught ParseError: bad port: x at tests/port.test.vlt:5:15
```

## Running tests

```sh
velt test                       # every *.test.vlt of the package (or under the current directory)
velt test tests/api.test.vlt    # one file
velt test --release             # optimized build
velt test --watch               # rerun on every change
```

The runner prints `ok <name>` or `FAILED <name>` per test and a summary, and exits with 1 if any
test failed, so it works in CI as is. `--watch` reruns the tests whenever a file they import, a
test file or the manifest changes.

## Testing servers

Start the server on port 0 (any free port) inside the test and call it with `fetch`; close it
at the end. The `api` template (`velt new todo --template api`) has complete examples, and the
[HTTP guide](http-server.md#testing-it) walks through one.

## Testing command-line tools

Keep the logic in a function that takes the argument array and returns output lines, and test
that function directly; `main` only does the printing and the exit code. See the
[CLI guide](cli-app.md#testing).
