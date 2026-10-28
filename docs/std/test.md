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
  with where it was thrown, as an uncaught error would be (`AssertionError: assertEq failed at
  tests/a.test.vlt:6:3`), and the next test runs. A test that ends the process (a panic,
  `process.exit`) is reported with its exit code and stderr, and the runner starts the file
  again from the next test.
  After the last test the process exits, also when a failed test left a timer or a server
  running. A process that ends before its first test (in a module's top-level code) fails all
  of the file's tests.
- **By hand** (a test program from `velt test --no-run`, run directly or under a debugger), it
  runs every test of its files in this process and prints each test's result (`ok test_x`,
  `FAILED test_x` with the error), without the `running` lines and the summary.

The runner is a program of its own, without test files, so no test file's code runs in it.
Its exports (`__file`, `__test`, `__testAsync`, `__run`) are for the generated `main` only.
`describe`, `it` and `expect` will be added here
([#928](https://github.com/velt-lang/velt/issues/928)).
