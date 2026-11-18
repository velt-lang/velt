# Lint profiles and performance findings

Status: partly implemented. Issue: [#1247](https://github.com/velt-lang/velt/issues/1247).

## Summary

Every source file of a package has a **profile**, `portable`, `native` or `dependency`, which
decides which lint rules apply to it and which fixes may be offered. On top of the TypeScript
compatibility rules, `perf/…` rules point native code at the Velt way to write what is slow in its
TypeScript form, with machine-applicable fixes. `velt check --perf` runs them, `velt check --fix`
applies the fixes, and the editor shows the findings as hints with quick fixes.

## Motivation

One package often holds code with different jobs. The sigx example
(`examples/apps/sigx-ssr/app`) has all three kinds:

| Code | Example | Who compiles it | What the author wants to hear |
|---|---|---|---|
| Shared with TypeScript | `src/shared/*.tsx`, `src/islands/` (components) | `tsc`/Vite and `velt` | what leaves the common subset; what is slow *and* has a fix inside it |
| Native | `src/server.velt`, `src/api.server.velt` | `velt` only | what is slow, and the Velt way to write it |
| A dependency | `sigx` (`node_modules/@sigx/velt/velt`) | `velt` only | nothing, unless they ask (to report it upstream) |

Code ported from TypeScript keeps TypeScript habits that cost speed in native code: a counter that
stays a double, `Record<string, any>` read with constant keys, a data class whose instances stay on
the heap. The optimizer knows about many of these; the lint tells the author, at the source, with
a fix.

## Design

### 1. Profiles

- **`portable`**: the file must compile with `tsc` and behave the same: the folders listed in the
  manifest's `tsCompat`. The ts-compat rules apply, and so do the performance rules, with only the
  fixes that stay inside the common subset.
- **`native`**: every other file of the package (the root package, or a workspace member being
  checked). Performance rules apply with Velt fixes (integer types, `struct`, `StringBuilder`, …).
- **`dependency`**: files of other packages, std included. No findings. `velt check --perf --deps`
  lists the optimizer's findings inside them as notes ("in a dependency: report it to its
  authors"), never with a fix.

No new configuration is needed for the common case: `tsCompat` marks the portable folders, and
the rest is native.

### 2. Fixes and where they are valid

A rule detects a pattern once and offers fixes, each saying whether it keeps the program's
behaviour (`Applicability::Safe`) or may change it (`Applicability::Review`: a value's type,
sharing, overflow), and whether the code after it is still valid TypeScript. In a portable file
only portable fixes are offered, and a finding left without one is a note whatever its level: the
author cannot act on it without leaving the subset.

**A fix only edits files whose profile allows it**: it never edits a dependency, and it never puts
a Velt-only construct into a portable file. A finding is placed in a file whose profile allows a
fix there.

### 3. Kinds of rules

- **Pattern rules** use the syntax tree and the checker's types (`velt_sema::ide`). They are cheap
  and run in `velt check --perf` and in the language server.
- **Evidence rules** report what the optimizer failed to do. They run the release pipeline up to
  optimized VIR (no code generation) for the target the build would use, and map the place back
  to the source. Since the optimizer has already handled everything it could, an evidence finding
  is never about code that is already fast.

### 4. Rules

Each rule comes from a measured cost: the TypeScript form against the Velt form, release builds
with the release runtime, instructions retired (cachegrind) on Linux x86-64. The program pairs are
in `bench/perf_lint/`. Each rule has a test that fails without it (`crates/veltc/tests/lint.rs`,
`crates/velt_lsp/src/tests/perf.rs`).

| Code | Kind | Finds | Native fix | Portable fix | Instructions |
|---|---|---|---|---|---|
| `perf/dynamic-record` | pattern | a `Record<string, any>` (or `any`) parameter read with constant keys | an object type of the keys read, with `as T` (Review) | the same (TypeScript) | 1,894M → 7M |
| `perf/class-as-data` | pattern | a class with only fields and a constructor that stores its parameters, whose instances nothing could tell from copies (no field assigned outside the constructor, no comparison, `instanceof`, `Map`/`Set` key or use as a value) | `type C = { … }`, each `new C(…)` → `{ … } as C`, in every file (Review) | the same (TypeScript) | 259M → 79M |
| `perf/foreach-capture` | pattern | `xs.forEach(cb)` whose callback assigns a variable of the enclosing function, which then lives in a heap cell | `for (const x of xs) { … }` (Review), not offered when the callback has a `return` | the same (TypeScript) | 111M → 33M |
| `perf/number-double` | evidence | a `number` in a loop that the optimizer keeps as a double, with its reason | `let k: i64` when every value is a whole number nothing bounds (Review) | note only (`\| 0` named) | 1,101M → 621M |

The `forEach` rewrite is always Review: a body can add to the array without a call in its text (a
getter of the element, `toString` or `valueOf` behind `+` or a template, an iterator, a
destructuring), and `for…of` then visits the new elements where `forEach` does not.

Measured and left without a rule, because Velt makes the TypeScript form as fast:
`JSON.parse(s) as T` (the same instructions as `JSON.parse<T>(s)`), string appends in a loop
(`s += x`, `` s = `${s}…` ``, `this.out += s`), `xs.forEach` whose callback assigns nothing outside
it, and `xs.reduce` (as fast as `for…of`).

### 5. Levels and suppression

```ts
tsCompat: ["src/shared", "src/islands"],   // portable
lint: {                                     // optional
  perf: "warn",                             // "off" | "hint" (default) | "warn" | "deny"
  rules: { "perf/foreach-capture": "deny", "perf/number-double": "off" },
},
```

- **Default `hint`:** `velt check` prints nothing new; `velt check --perf` and the editor show the
  findings as notes, which the language server sends as hints. A package opts in to warnings or a
  failing check.
- **Suppression:** `// velt-allow(perf/number-double): reason` alone on a line covers the next
  line with code; after code, its own line. `// velt-allow-file(<code>): reason` covers the file.
  Several codes are separated by commas, and `perf/*` covers a family. The reason is required, so
  a suppression explains itself; an allow without one, of an unknown code, or that covers nothing
  is a warning (`crates/velt_lint/src/allow.rs`).
- **Codes** have a family prefix: `perf/…` and `ts-compat/…`. The JSON `code` is the whole code,
  the same string `velt-allow` and `lint.rules` take; the human-readable output and the editor
  show it as `perf(foreach-capture)`, and `ts-compat(<rule>)` stays accepted.

### 6. Commands and the editor

- `velt check --perf [<file>]` runs the pattern and the evidence rules; the evidence pipeline costs
  about as much as a release build without code generation. It builds with line information
  (`-g`), which changes no optimization, and places a finding at the local's declaration.
- `velt check --fix` applies the Safe fixes of all findings, ts-compat included; `--fix=review`
  also applies the Review ones. It then checks the program again: if the check fails, the fixes
  are undone and applied one at a time, keeping each after which the program still checks, and it
  prints what it fixed and what it undid. Review fixes such as an `i64` that meets a `number` are
  offered without proof and kept only if the program still checks.
- The language server publishes pattern findings with the document's diagnostics and offers their
  fixes as quick fixes (edits in other files go to those files), Safe ones preferred and in "fix
  all". Evidence findings appear only in `velt check --perf`: they build the program, and the
  editor's analysis is of one document and its imports. For the same reason the editor reports an
  exported data class without the fix that rewrites its uses across the package, and names
  `velt check --perf --fix=review`, which loads every module.

## Compilation

- **`velt_common::Diagnostic`** has `code: Option<Box<str>>` and `fixes: Vec<Fix>`
  (`Fix { title, edits, applicability }`). A `Box<str>` keeps `Diagnostic` small enough for the
  `Result`s it travels in. `--json` ([CLI contract](../contracts/cli.md)) prints every fix as
  `fixes`, and `fix` holds the first fix when it is one edit.
- **`velt_lint`** (`crates/velt_lint/src/`): the rule registry (`PERF_RULES` in `rules.rs`), the
  file-to-profile map (`profile.rs`, using `vpm::sources::in_folder` as `tsCompat` does), levels
  from the manifest's `lint` (`Levels`), suppression comments (`allow.rs`), fix selection and
  application (`fix.rs`), the pattern rules (`perf/`) and the evidence rules (`evidence.rs`, on
  plain records veltc makes from the optimizer's reports, so the crate needs no optimizer).
  `velt_tscompat` keeps its rules and its `Finding`; `velt_lint::ts_compat` turns its findings
  into diagnostics with codes and levels, so the `tsc` oracle tests stay where they are.
- **`velt_opt`**: `Unnarrowed` carries the kind of reason (`UnnarrowedWhy`), so a rule can tell an
  unbounded integer (an `i64` holds it) from a fraction.
- **Manifest**: `lint` in `package.velt` (`std/package/manifest.velt`: `Lint`, `LintLevel`;
  [manifest contract](../contracts/manifest.md)).
- **veltc**: `velt check --perf [--deps]` and `--fix[=review]` (`crates/veltc/src/commands/
  lint.rs`); the check driver returns the loaded modules with its outcome
  (`driver::check_with_modules`), so the lint reuses the check's front end.

## Alternatives considered

**A severity level for hints.** `Severity` has no hint level; a `perf/…` finding at the default
level is a note, and the editor shows notes from the lint as hints. A fourth severity would touch
every consumer of diagnostics for one presentation choice.

**Quick fixes found by message text.** The language server recognizes most compiler diagnostics by
message to build a fix. Lint findings carry their fixes instead, so the editor and `--fix` apply
the same edits and a message can change without breaking a fix.

## Not yet implemented

- **More evidence rules.** The optimizer reports where it gave up on keeping a class value off
  the heap, on devirtualizing a call and on removing a copy, and each becomes a `perf/…` evidence
  rule.
- **Portable fixes in the `tsc` oracle.** Each portable fix's output becomes a sample of the
  oracle (`crates/velt_tscompat/tests/oracle.rs`), so a fix offered in a portable file is proven
  to stay valid TypeScript.
- **Multi-file refactors in the language server:** a data class to a `struct`, and `number` to an
  integer type for a whole function when the numbers report shows its values are integers.
