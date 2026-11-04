# Design: lint profiles, and performance findings for native code

Status: steps 1 to 4 built (`velt_lint`, `velt check --perf` and `--fix`, the editor's hints and
quick fixes); steps 5 and 6 proposed. Related: #1247 (`velt check --fix`), #51 (fix-its for TS
idioms), #991 (ts-compat follow-ups); the performance issues the first rules come from are listed
under [Rules](#rules). What changed while building it is in [Decisions](#decisions).

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

Example: a class that only holds data and is never compared by identity becomes an object type
(stored inline) with object literals at the construction sites (Review: copies instead of
shares; the rule checks the uses first). That is TypeScript too, so a portable file gets it; a
`struct` (step 6) would be native only.

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

- `velt check --perf [<file>]`: runs the evidence rules too (the pipeline costs about as much
  as a release build without codegen).
- `velt check --fix`: applies the Safe fixes of all findings, ts-compat included;
  `--fix=review` also applies the Review ones. It checks the program again and undoes a fix
  after which it no longer checks, then prints what it fixed and what is left.
- Language server: pattern findings as you type, their fixes as quick fixes (from the
  diagnostics' fixes, not their message text), Safe ones in "fix all". Evidence findings only in
  `velt check --perf` for now (open question 3).
- Multi-file refactors: the class → object type fix rewrites every `new` in the package. Still
  proposed (step 6): class → `struct`, and `number` → an integer type for a whole function when
  the numbers report shows the values are integers.

## Rules

Each rule comes from a measured cost: the TypeScript form against the Velt form, release builds
with the release runtime, instructions retired (cachegrind) on Linux x86-64. A measured pattern
that turned out to be fast already got no rule (below). Each rule has a test that fails without
it (`crates/veltc/tests/lint.rs`, `crates/velt_lsp/src/tests/perf.rs`).

| Code | Kind | Finds | Native fix | Portable fix | Measured |
|---|---|---|---|---|---|
| `perf/dynamic-record` | pattern | a `Record<string, any>` (or `any`) parameter read with constant keys | an object type of the keys read with `as T` (Review) | the same (TypeScript) | 1 894M → 7M (#2058) |
| `perf/class-as-data` | pattern | a class with only fields and a constructor that stores its parameters, whose instances nothing could tell from copies (no field assigned outside the constructor, no comparison, `instanceof`, `Map`/`Set` key or use as a value) | `type C = { … }`, each `new C(…)` → `{ … } as C`, in every file (Review) | the same (TypeScript) | 259M → 79M |
| `perf/foreach-capture` | pattern | `xs.forEach(cb)` on an array whose callback assigns a variable of the enclosing function, which then lives in a heap cell | `for (const x of xs) { … }` (Review: `for…of` also visits elements the body adds, which a call, a getter, `toString`/`valueOf`, an iterator or a destructuring can do; none with a `return`) | the same (TypeScript) | 111M → 33M |
| `perf/number-double` | evidence | a `number` in a loop that the optimizer keeps as a double, with its reason | `let k: i64` when every value is a whole number nothing bounds (Review) | note only (`\| 0` named) | 1 101M → 621M |

Measured and left without a rule, because Velt already makes the TypeScript form as fast:
`JSON.parse(s) as T` (the same instructions as `JSON.parse<T>(s)`), `s += x`, `` s = `${s}…` ``
and `this.out += s` in a loop (linear since #381), `xs.forEach` whose callback assigns nothing
outside it and `xs.reduce` (as fast as `for…of`). Not yet: values kept on the heap (#2119), calls
left dynamic and copies left in place, which need the optimizer to say where it gave up (step 5).

## Compiler changes

1. **Contract change, `velt_common::Diagnostic`:** gains `code: Option<Box<str>>` and
   `fixes: Vec<Fix>` (`Fix { title, edits, applicability }`, `Applicability::{Safe, Review}`;
   docs/internals/contracts/README.md). `--json` (contracts/cli.md) prints every fix as
   `fixes`; `fix` stays, the first fix when it is one edit.
2. **`velt_lint`:** the rule registry (`PERF_RULES`: code, kind, summary), the file-to-profile
   map (`Profiles`, `vpm::sources::in_folder` as `tsCompat` uses it), levels from `lint`
   (`Levels`), the suppression comments (`allow`), fix selection and application (`fix`), the
   pattern rules (`perf`) and the evidence rules (`evidence`, on plain records veltc makes from
   the optimizer's reports, so the crate needs no optimizer). `velt_tscompat` is unchanged.
3. **`velt_opt`:** `Unnarrowed::why` says the reason's kind (`UnnarrowedWhy`), so a rule can tell
   an unbounded integer (an `i64` holds it) from a fraction. Step 5 adds `remarks(program)` for
   the other passes.
4. **Manifest (additive):** `lint` in `package.vlt` (contracts/manifest.md, `vpm::manifest`,
   std's `Lint` and `LintLevel` types).
5. **Language server:** lint findings published with the document's diagnostics, quick fixes
   from the diagnostics' fixes (edits in other files go to those files), Safe ones preferred.
6. **veltc:** `velt check --perf [--deps]`, `--fix[=review]` (`commands/lint.rs`); the check
   driver returns the loaded modules with its outcome (`driver::check_with_modules`), so the
   lint reuses the check's front end.

## Implementation order

Each step is useful alone. Steps 1 to 4 are built.

1. `Diagnostic` codes and fixes; `velt check --fix`; the language server's code actions from
   the diagnostics' fixes (#1247's machinery; its `undefined` migration fixes are its own).
2. The lint crate: registry, profiles, levels, suppression, `lint` in the manifest. ts-compat
   moves in.
3. The pattern rules: `perf/string-append-loop`, `perf/class-as-data`, `perf/dynamic-record`.
4. Evidence: `velt_opt::remarks`, `velt check --perf`, `perf/number-double` (replacing the
   report's renderer), placement across boundaries (§3), `--deps`.
5. The other evidence rules (`velt_opt::remarks`: heap SROA, devirtualization, clone removal
   saying where they gave up), and the portable fixes in the `tsc` oracle.
6. The multi-file refactors in the language server.

## Decisions

Made while building steps 1 to 4:

- **No new crate rename.** `velt_tscompat` keeps its rules and `Finding`; `velt_lint` turns its
  findings into diagnostics (`velt_lint::ts_compat`) and adds profiles, levels, suppression and
  the `perf/…` rules, so the oracle tests and the rules stay where they are.
- **`Diagnostic::code` is `Option<Box<str>>`.** A `String` grew `Diagnostic` past clippy's
  `result_large_err` limit for the `Result`s it travels in.
- **The JSON `code` is the whole code**, `ts-compat/bool-type` (was `bool-type`): the same string
  `velt-allow` and `lint.rules` take. The editor keeps `velt ts-compat` / `velt perf` as the
  source and the rule as the code, as the note shows it (`perf(foreach-capture)`).
- **Hints are notes.** `Severity` has no hint; the editor shows a `perf/…` note as a hint.
- **The `forEach` rewrite is always Review.** A body can add to the array without a call in
  its text (a getter of the element, `toString` or `valueOf` behind `+` or a template, an
  iterator, a destructuring), and `for…of` then visits the new elements where `forEach` does
  not; no reading of the body proves the two the same.
- **`--fix` checks again.** A fix that makes the program fail to check is undone (all are tried
  together first, then one at a time): Review fixes such as an `i64` that meets a `number` are
  offered without proof and kept only if the program still checks.
- **Evidence only in `velt check --perf`**, not in the editor, for now (open question 3): it
  builds the program, and the editor's analysis is of one document and its imports.
- **A fix that rewrites the uses of an exported definition needs the whole package.** The editor
  loads a document's imports, not the files that import it, so it reports an exported data
  class without the fix and names `velt check --perf --fix=review`, which loads every module.
- **Evidence placement.** The optimized program names a source line and column; the finding is
  placed at the local's declaration (from the checker's analysis), the loop's line in a note.
  `velt check --perf` builds with line information (`-g`), which changes no optimization.

## Open questions

1. **Names:** `native`/`portable`, or `velt`/`shared`? `perf/` as the family name?
2. **Default level:** `hint` (proposed) or `warn` for native code in new packages (`velt new` could
   write `lint: { perf: "warn" }`).
3. **Evidence in the editor:** on save (proposed), or only on demand through a code lens, given
   the pipeline's cost on large packages.
4. **A `profile` key** for folders that are neither `tsCompat` nor the rest (generated code that
   should get no findings), or is suppression at the top of a file enough?
