# The Velt Reference

This is the precise description of the Velt language: what the compiler accepts, what it
rejects, and what the compiled program does. For a guided introduction, read
[the Book](../book/README.md); for imported modules, the [standard library](../std/README.md).

Velt is **TypeScript without the parts inherited from JavaScript that cause bugs**, compiled to
native code with Rust-level performance and no garbage collector. Three rules shape every
decision:

- adopt TypeScript's best parts, never JavaScript's bug sources (implicit coercions,
  `undefined`, loose truthiness, floating promises, …);
- one way of doing things;
- something that is not TypeScript exists only where TypeScript can't express it at native
  speed (integer types, `shared`, `extend`).

## Conventions

- This reference describes **what the compiler does today**. Every code block marked as Velt is
  compiled by the documentation tests, and the behavior is pinned by the end-to-end tests in
  `tests/golden/`.
- **Planned** marks a decided design that is not built yet. It always links the design note,
  and planned code is never shown as working.
- Error messages are quoted verbatim, so you can search for them.

## Contents

1. [Lexical structure](lexical.md) — programs, comments, literals, keywords, operators
2. [Types](types.md) — numbers, strings, equality, `null`, literal types, unions, discriminated
   unions, enums, objects, arrays, tuples, maps
3. [Variables and conditions](variables.md) — `const`/`let`, safe truthiness, module state
4. [Functions and closures](functions.md) — parameters, generics, arrows, captures
5. [Classes, structs, interfaces and generics](classes.md) — members, inheritance, dispatch,
   `extend`, `Comparable`
6. [Control flow](control-flow.md) — loops, `for...of`, `switch`
7. [Errors](errors.md) — typed `throw`/`try`/`catch`, `throws` clauses, panics
8. [Async and concurrency](async.md) — promises, `spawn`, `shared`, `Mutex`
9. [Memory model](memory.md) — ownership, inferred mutation, exclusive access, `using`
10. [Modules and packages](modules.md) — imports, exports, specifiers
11. [Built-ins](builtins.md) — what is in scope without an import
12. [Diagnostics](diagnostics.md) — the format and wording of compiler errors

How Velt differs from TypeScript, item by item, is in
[Velt for TypeScript developers](../book/ts-developers.md).

## Planned changes at a glance

Decided direction that is not implemented yet. Each chapter says where it applies.

| Change | Design |
|---|---|
| Semantics stage 3: `weak` references and a compile-time warning for reference cycles | [semantics — cycles](../internals/design/semantics.md#reference-cycles--without-a-collector) |
| The `struct` keyword removed (structs already behave as objects) | [semantics — JS fidelity](../internals/design/semantics.md#js-fidelity-decisions) |
| `extend`: retroactive `implements`, module scoping | [TypeScript alignment §4](../internals/design/ts-alignment.md#4-extend--full-power-zero-cost-module-scoped) |
| TSX: `children` and other element-typed props, faster templates (syntax, providers, `velt:jsx` and streaming already work) | [TSX](../internals/design/tsx.md) |
| `new Promise((resolve, reject) => …)` | [semantics — promises](../internals/design/semantics.md#promises) |
