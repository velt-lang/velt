# velt:test

The test runner, written in Velt. `velt test` ([Testing](../book/testing.md)) compiles the test
files into a program whose generated `main` registers their tests with this module and hands it
control; everything after that happens here:

- **As the runner** (started by `velt test`), the program reads the run's plan from `velt test`,
  starts a process per test file (from the program that holds the file; several at once,
  `--jobs`), prints each file's report in file order and the summary, and exits with 1 if a
  test failed.
- **For one file** (in a process the runner started), it runs the file's tests one after
  another. A test fails when it throws: a failed `assert`, `assertEq` or `assertThrows` throws an
  `AssertionError`, and any other error it doesn't catch counts the same. The error is reported
  with where it was thrown, as an uncaught error would be (`Error: boom at
  tests/a.test.vlt:6:3`), after the last line of a message of several lines (as Vitest shows
  the location after the message), and the next test runs. A test that ends the process (a panic,
  `process.exit`) is reported with its exit code and stderr, and the runner starts the file
  again from the next test.
  After the last test the process exits, also when a failed test left a timer or a server
  running. A process that ends before its first test (in a module's top-level code) fails all
  of the file's tests.
- **By hand** (a test program from `velt test --no-run`, run directly or under a debugger), it
  runs every test of its files in this process and prints each test's result (`ok test_x`,
  `FAILED test_x` with the error), without the `running` lines and the summary.

The runner is a program of its own, without test files, so no test file's code runs in it.

## `describe`, `it` and `expect`

`describe`, `it`, `test`, `beforeAll`, `afterAll`, `beforeEach`, `afterEach`, `expect` and
`Expect` are Vitest's API ([Testing](../book/testing.md#describe-it-and-expect)); `"vitest"`
imports this module. How they run:

- Called at a test file's top level, `describe`, `it` and the hooks only queue what they
  register. A test program may hold several test files, and every module it imports is
  evaluated when it starts, so this is all their top level does in each process; such a file
  still counts as one that only declares and shares a program with the others. The generated
  `main` gives each file the registrations its top level queued, in import order (`__file(n)`).
- In the process for one file, its tests are collected: its `test_*` functions, then what its
  queued calls register, running the `describe` callbacks, whose calls register into their
  group at once. The process lists the tests to the runner (their full names, `group > test`)
  and runs them from the one the runner asks for, with the hooks of their groups. `--filter`
  is applied to the full names here.
- A `describe` callback that throws (or is `async`) is reported as a failed test named as its
  group, and the group's tests don't run. An `afterAll` that throws is printed where it ran and
  fails the run. A test's name is escaped on its report line (`\n` for a line break).
- `toEqual` compares with `__intrinsic_deep_eq`: `deepEqual`'s comparison, but a class instance
  that isn't the same object compares by its fields and numbers with `Object.is`. Cycles go by
  Vitest's rule: an object met again with the same partner while being compared is equal (a
  parent pointer, rings of one length), with another partner different (rings of two lengths).

`expect(actual)` returns an `Expect<T>`: `actual`, whether `.not` was used (`negated`) and the
matchers, which are methods (the generic ones) and `extend` blocks (those for arrays, strings,
`Comparable` types, functions and promises). `check(pass, message, notMessage?)` throws the
`AssertionError` of a failed matcher; custom matchers call it too. `.resolves` and `.rejects`
return a `Resolves<V, E>` or `Rejects<V, E>`, whose matchers are `async`; their `settled()` is
the `Expect` of the value or the error, for a custom matcher that awaits.

`toMatch` and `toThrow` take a `string | Pattern` (velt:regex/pattern), an interface `RegExp`
implements, so `expect` doesn't import velt:regex: a test file compiles the regex engine only
when it has a regex (a literal such as `/^h/`, or `RegExp` named), as any program does.

`velt:test/http` adds `toHaveStatus` and `toHaveHeader` to `Expect<Response>` (`velt:fetch`).

With `VELT_RC_STATS=1` (the debug runtime's block counters), the process for a file returns from
`main` after its tests instead of exiting at once, so the runtime frees the module state and
reports the blocks left (`rc stats: … blocks=A/F`); the runner fails the file if A ≠ F, or if
its process reported no counts (a runtime without the debug counters). Such a
process waits for what is still running, as a program does after `main`: the runner ends one
still running 120 s after its tests and fails the file.

The other exports (`__file`, `__test`, `__testAsync`, `__run`, `__claim`) are for the generated
`main` only.
