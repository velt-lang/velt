# The Velt Book

The Book teaches Velt from the ground up, with an eye on what is new if you come from
TypeScript. Every Velt code block in it is compiled by the documentation tests.

## Getting started

1. [Getting started](getting-started.md): install, your first program, project layout
2. [A tour of Velt](tour.md): the whole language in one page
3. [Velt for TypeScript developers](ts-developers.md): every difference, and why

## Guides

- [Building an HTTP server](http-server.md): routing, JSON, validation, shared state, tests
- [Building a command-line tool](cli-app.md): arguments, files, exit codes, tests
- [Async and concurrency](async.md): promises, timeouts, `spawn`, shared state
- [Error handling](errors.md): typed `catch`, `throws` clauses, errors as values, panics
- [Memory without a garbage collector](memory.md): ownership today, JavaScript semantics next
- [Modules and packages](packages.md): organizing code, dependencies, publishing
- [Testing](testing.md): `velt test`
- [Hot reload with `velt dev`](hot-reload.md): edit a running server

When you need the precise rules, go to [the Reference](../reference/README.md); for module
APIs, to [the standard library](../std/README.md).
