# Design: lint profiles, and performance findings for native code

Status: proposed. Related: #1247 (`velt check --fix`), #51 (fix-its for TS idioms), #991
(ts-compat follow-ups); the performance issues the first rules come from are listed under
[Rules](#rules).

## Problem

One package often holds code with different jobs. The sigx example (`examples/apps/sigx-ssr/app`)
has all three kinds:

| Code | Example | Who compiles it | What the author wants to hear |
|---|---|---|---|
| Shared with TypeScript | `src/shared/*.tsx` (components), a `models/` folder | `tsc`/Vite and `velt` | what leaves the common subset; what is slow *and* has a fix inside it |
| Native | `src/server.vlt`, `src/api.server.vlt` | `velt` only | what is slow, and the Velt way to write it |
| A dependency | `sigx` (`node_modules/@sigx/velt/velt`) | `velt` only | nothing, unless they ask (to report it upstream) |

Today only the first kind has a lint (`velt check --ts-compat`, `velt_tscompat`, folders listed in
`tsCompat`). Native code gets no help moving from TypeScript habits to Velt: a counter that
stays a double, `s += x` in a loop (#379), `Record<string, any>` (#2058), a class value that stays
on the heap (#2119). The optimizer knows about most of these; the only one it tells users about is
`velt build --report numbers`.

There is no shared machinery either: `Diagnostic` has no code and no fix, so the language server
finds each quick fix by matching message text (`velt_lsp/src/code_actions/mod.rs`), there are no
levels or suppression comments, and `velt_tscompat` has its own `Finding`, `Fix` and `Severity`.

## Proposal

### 1. Every source file has a profile

A file's **profile** says which rules apply and which fixes may be offered:

- **`portable`**: the file must compile with `tsc` and behave the same. The folders in
  `tsCompat`, as today. The ts-compat rules apply, and so do the performance rules whose fixes stay
  inside the common subset.
- **`native`**: every other file of the package (the root package, or a workspace member being
  checked). Velt-only code. Performance rules apply with Velt fixes (`int` types, `struct`,
  `StringBuilder`, …).
- **`dependency`**: files of other packages, std included. No findings. `velt check --perf --deps`
  lists the evidence findings (§4) inside them as notes, so an author can report them upstream,
  but never offers a fix.

So no new configuration is needed for the common case: `tsCompat` already marks the portable
folders, and the rest is native. The sigx app gets the right behaviour from its existing
`package.vlt`.

### 2. A rule's fixes say where they are valid

A rule detects a pattern once and offers fixes per profile:

```rust
pub struct Fix {
    pub title: String,
    pub edits: Vec<(Span, String)>,
    pub safety: Safety,       // Safe: same behaviour, proven; Review: behaviour may change
    pub valid_in: ValidIn,    // Portable (also valid TypeScript) or NativeOnly
}
```

In a portable file only `Portable` fixes are offered. A rule with none is reported there as a
note, never as a warning, because the author can't act on it without breaking the subset. A
portable fix is also held to the subset: the `tsc` oracle (`velt_tscompat/tests/oracle.rs`) gets
each portable fix's output as a sample.

Example: a class that only holds data and is never compared by identity.
- native: `struct` (Review: copies instead of shares, sema checks the uses first).
- portable: a field-only interface (shared-models.md §1: stored inline, no vtable) and object
  literals at the construction sites (Review).

### 3. Fixes never cross a stricter boundary

A finding's cause may be in another file: a `number` field of a model in `src/models`
(portable) makes a counter in `src/server.vlt` (native) stay a double. The rule is that **a fix
only edits files whose profile allows it**:
- it never edits a dependency;
- it never puts a Velt-only construct into a portable file.

So the finding is placed at the native use, with a native fix there: convert once where the value
crosses into native code (`const n = m.count as i64` before the loop). A note names the portable
cause and the portable fix, if there is one.

### 4. Two kinds of rules

- **Pattern rules** use the syntax tree and the checker's types (`velt_sema::ide`, as the typed
  ts-compat rules do). They are cheap and run in `velt check` and in the language server.
- **Evidence rules** report what the optimizer actually failed to do. They run the release
  pipeline up to optimized VIR (no codegen), for the target the build would use (`host.target`,
  `--target`; wasm and native can differ), and map the place back to the source (`velt_opt`'s
  `srclocs`). Since the optimizer has already handled everything it could, an evidence finding is
  never about code that is already fast.

  Evidence findings are ranked by loop depth, and by whether the place is reachable from the
  entry (for the SSR server: from the handler that renders the shared components). Ranking by
  sampled time (`velt run --profile`) can come later.

  `velt build --report numbers` becomes a view of the first evidence rule (`perf/number-double`).

### 5. Configuration, levels and suppression

```ts
tsCompat: ["src/shared", "src/islands"],   // portable (exists)
lint: {                                     // optional (additive)
  perf: "warn",                             // native perf findings: "hint" (default) | "warn" | "deny" | "off"
  rules: { "perf/string-append-loop": "deny", "perf/number-double": "off" },
},
```

- **Default `hint`:** `velt check` prints nothing new; `velt check --perf` and the editor show the
  findings (the language server sends them as hints). A package opts in to warnings or a failing
  check.
- **Suppression:** `// velt-allow(perf/number-double): reason` on the line before, or at the top
  of a file for the whole file. The reason is required, so the suppression explains itself; an
  allow that suppresses nothing is a warning.
- **Codes** have a family prefix: `perf/…` for the new rules, `ts-compat/…` for the existing ones
  (shown as `ts-compat(<code>)` today, which stays accepted).

### 6. Commands and the editor

- `velt check --perf [paths]`: runs the evidence rules too (the pipeline costs about as much as a
  release build without codegen).
- `velt check --fix`: applies the Safe fixes of all findings, ts-compat included (#1247);
  `--fix=review` also applies the Review ones. It prints a diff summary and changes no file it
  can't parse back.
- Language server: pattern findings as you type, their fixes as quick fixes (by code, not by
  message text), Safe ones in "fix all". Evidence findings when the file is saved, in the
  background, with a setting to turn that off.
- Multi-file refactors in the language server, each checked by sema before it is offered:
  class → `struct`, class → field-only interface, `number` → an integer type for a whole function
  when the numbers report shows the values are integers.

## Rules

The first rules each come from a measured cost. Each rule lands with a benchmark showing what its
fix gains (CONTRIBUTING.md), and a test that fails without the rule.

| Code | Kind | Finds | Native fix | Portable fix | From |
|---|---|---|---|---|---|
| `perf/string-append-loop` | pattern | `s += x` / `s = s + x` in a loop | `StringBuilder` (Safe) | note only | #379 |
| `perf/dynamic-record` | pattern | `Record<string, any>` or `any` data read in a loop | a typed object type or `Map<K, V>` (Review) | a field-only interface (Review) | #2058 |
| `perf/class-as-data` | pattern | a class with only fields, never compared by identity | `struct` (Review) | field-only interface (Review) | shared-models.md |
| `perf/number-double` | evidence | a `number` in a loop that stays a double, with the optimizer's reason | an integer type (Review: overflow) | note only | #732, #769 |
| `perf/heap-value` | evidence | a value that stays on the heap, with why (e.g. passed to a union parameter) | per cause | note only | #2119 |
| `perf/dynamic-call` | evidence | a call the optimizer could not make direct | note | note | `vtable_loads` |
| `perf/kept-clone` | evidence | a copy the optimizer could not remove | note | note | `gated_clones` |

## Compiler changes

1. **Contract change, `velt_common::Diagnostic`:** gains `code: Option<&'static str>` and
   `fixes: Vec<Fix>` (the type above). `--json` (contracts/cli.md) already prints `code` and
   `fix` for ts-compat findings; it moves to the general fields, and the old field stays for one
   release.
2. **A lint crate:** `velt_tscompat` becomes `velt_lint`, with a rule registry (code, family,
   kind, default level), the file-to-profile map (`vpm::sources::in_folder`, as `tsCompat` uses
   today), levels from `lint`, and the suppression comments. The ts-compat rules move in
   unchanged.
3. **`velt_opt`:** a `remarks(program) -> Vec<Remark>` entry point beside `number_report`, which
   collects the optimizer's failures (narrowing, heap SROA, devirtualization, clone removal)
   with their VIR places. The lint crate turns them into findings; `velt_opt` doesn't know about
   profiles.
4. **Manifest (additive):** `lint` in `package.vlt` (contracts/manifest.md, `vpm::manifest`).
5. **Language server:** code actions by code; evidence findings on save.

## Implementation order

Each step is useful alone.

1. `Diagnostic` codes and fixes; `velt check --fix`; the language server's code actions by code
   (closes #1247).
2. The lint crate: registry, profiles, levels, suppression, `lint` in the manifest. ts-compat
   moves in.
3. The pattern rules: `perf/string-append-loop`, `perf/class-as-data`, `perf/dynamic-record`.
4. Evidence: `velt_opt::remarks`, `velt check --perf`, `perf/number-double` (replacing the
   report's renderer), placement across boundaries (§3), `--deps`.
5. The other evidence rules, and the portable fixes in the `tsc` oracle.
6. The multi-file refactors in the language server.

## Open questions

1. **Names:** `native`/`portable`, or `velt`/`shared`? `perf/` as the family name?
2. **Default level:** `hint` (proposed) or `warn` for native code in new packages (`velt new` could
   write `lint: { perf: "warn" }`).
3. **Evidence in the editor:** on save (proposed), or only on demand through a code lens, given
   the pipeline's cost on large packages.
4. **A `profile` key** for folders that are neither `tsCompat` nor the rest (generated code that
   should get no findings), or is suppression at the top of a file enough?
