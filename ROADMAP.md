# Roadmap

Velt is pre-1.0. These are the milestones ahead, roughly in order; each is tracked as a GitHub
milestone with its issues. Breaking changes are expected until 1.0: old syntax is removed, not
deprecated, and the compiler explains how to update code.

| Milestone | What it brings |
|---|---|
| **Semantics stage 2** | Objects, arrays, maps and closures become shared references, exactly like JavaScript: no more "use of moved value", `.clone()` becomes an explicit deep copy, escaping closures may modify captures. Uniquely owned values keep today's code; every benchmark must stay within 3%. The `struct` keyword goes away and objects compare by identity. ([design](docs/internals/design/semantics.md)) |
| **TSX and server-side rendering** | JSX syntax with TypeScript's `jsxImportSource` model, a precompiling SSR mode, the `velt:jsx` provider, streaming async components. ([design](docs/internals/design/tsx.md)) |
| **JSON round** | `JSON.parse` into unions, literal types and string enums; recursive type aliases. |
| **Packages and a modular std** | A public package registry; the database drivers (`sqlite`, `postgres`, `redis`) move to versioned packages; module-scoped `extend` and retroactive `implements`. |
| **Platform** | Unwind information for JIT code on macOS and Linux, hot swap verified on every platform, Windows arm64, a released toolchain with installers. |
| **Performance** | Compile-time scaling of lowering and the optimizer, closure environments, `noalias` from the exclusivity rule, a batched PostgreSQL query API. |
| **Tooling** | A published VS Code extension, debugger support for JIT code, a warm front end in `velt dev`, `new Promise(...)`, the logical assignment operators. |
| **Semantics stage 3** | `weak` references and a compile-time warning for reference cycles, still without a collector. ([design](docs/internals/design/semantics.md#reference-cycles--without-a-collector)) |

Proposals for language changes start as issues; see
[CONTRIBUTING.md](CONTRIBUTING.md#proposing-a-language-change).
