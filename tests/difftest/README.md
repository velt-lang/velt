# difftest — differential testing of `velt` against Node

Velt looks like TypeScript, so a program in the shared subset must print the same thing under
Node and under every `velt` build mode. `difftest` runs each program under
`node --experimental-transform-types` (the oracle, Node ≥ 22.7) and under `velt build` in three modes —
`debug` (Cranelift), `release` (velt_opt + LLVM when clang is found) and `release-cl`
(velt_opt + Cranelift) — and compares stdout and the exit status.

It is a standalone crate (its own `[workspace]`), not a member of the compiler workspace.

## Usage

```sh
tests/difftest/run.sh run tests/difftest/corpus          # check programs (files or directories)
tests/difftest/run.sh fuzz --seeds 0..1000 -j 8          # generate, check, group, shrink
tests/difftest/run.sh gen 42                             # print the program for seed 42
tests/difftest/run.sh shrink path/to/failing.vlt        # minimize a failing program
tests/difftest/run.sh fuzz --wild --seeds 0..1000        # backends against each other (below)
tests/difftest/run.sh fuzz --std --seeds 0..500          # std modules against Node (below)
```

`run.ps1` is the PowerShell twin. Options: `--modes debug,release,release-cl`, `-j <n>`,
`--timeout <secs>` (per program run; builds get 6×), `--velt <path>`, `--node <path>`,
`--out <dir>` (default `tests/difftest/out`, git-ignored), `--oracle node|debug`, `--wild`.

**Formatter mode.** `--modes …,fmt` also runs `velt fmt` on a copy of each program and checks
that the formatted program (debug build) behaves like the oracle: formatting must never change
meaning. With `--oracle debug` this works on any program, e.g. all goldens.

**Self-differential mode.** `--oracle debug` makes the debug build (unoptimized Cranelift) the
oracle: `release` and `release-cl` must match it, and Node isn't used. `--wild` (implies it)
generates programs outside the JS-compatible subset: full-range wrapping `i64` arithmetic,
i64 edge values, unguarded `/`, `%`, `<<`, `>>`, `>>>`, and casts through `i32`, `u8`, `u16`,
`u64` and from `f64`. That covers optimizer and backend code (overflow flags, shift masking,
saturating casts) that the Node oracle can't reach.

**Std mode.** `--std` generates programs that push random inputs through the deterministic
std modules — `std/url` (parsing, components, setters, `URLSearchParams`, `encodeURIComponent`
family), `std/encoding` (Base64, hex, UTF-8 strict/lossy), `std/crypto` (SHA-256, SHA-1, HMAC),
`std/csv` (parse with options, stringify, round trips), `std/regex` (a grammar of patterns ×
flags × `test`/`exec`/`matchAll`/`replace`/`split`) and `std/datetime` (epoch values,
rolled-over fields, ISO parsing, format patterns, calendar arithmetic, durations) — one
statement per line, each in its own `try`. Node runs them through `shims/std/*.ts` (below).

Verdicts:

| Verdict | Meaning |
|---|---|
| `ok` | every mode printed exactly what Node printed and exited the same way |
| `mismatch[mode]: stdout` / `exit …` / `timeout` / `signal` | a miscompile (or runtime bug) |
| `crash[mode]` | the compiler panicked, hit an ICE or died |
| `velt-rejected: <error>` | Node runs it, `velt` refuses it: a front-end bug, a language gap, or a program outside the subset |
| `node-rejected` | not TypeScript Node can run: nothing to conclude |

Failing programs are saved under `out/cases/<signature>/` with a `.txt` describing the difference;
`fuzz` also writes `seed-N.min.vlt`, shrunk while the signature stays the same (only the failing
mode is re-checked). Shrunk repros go to `tests/golden/bugs/` (with Node's output as `.out`) and
into an issue.

## The normalizer (`src/tsify.rs`)

The TypeScript twin is the `.vlt` source plus:

1. a call of `main()`; a numeric return becomes `process.exitCode`;
2. a `panic(msg)` shim when the program calls Velt's `panic` builtin (`panic: msg` on stderr, exit 101);
3. `util.inspect.defaultOptions.breakLength = Infinity`: Velt prints containers on one line, Node
   wraps them past 72 columns.
4. `import … from "velt:<module>"` loads `shims/std/<module>.ts`, the module's Node twin: the same
   API on Node's own implementation where there is one (`URL`/`URLSearchParams` (ada), `RegExp`
   (V8), `Buffer`/`TextDecoder`, `node:crypto`, `Date`), and small independent implementations
   written from the std docs where Node has none (CSV, `DateTime.format`, month arithmetic,
   `Duration.toString`). A std module without a shim makes the program `node-rejected`.

Types are transformed, not only stripped, because TS `enum`s need code. Both sides see the same
local time zone: every run gets `TZ=UTC`, except on Windows, where Velt reads the system zone and
ignores `TZ` (the Node shim then uses the system zone too). `DateTime.parse` reads a date-time
without a zone as UTC on both sides.

Output is captured through files, not pipes (Node writes pipes asynchronously on macOS).

## Writing programs for the shared subset

Differences between the languages that a program must avoid (the generator does all of this):

- **Integer arithmetic.** Untyped integer literals are `i64` in Velt but doubles in JS: keep
  values small (no overflow, no precision loss), write integer division as `Math.trunc(a / k)`
  (one integer division in Velt, the truncated quotient in JS), keep operands of bitwise
  operators within 32 bits (JS converts to int32), never divide by zero (Velt panics). JS
  produces `-0` from `*`, `%`, `/`, negation on integers and `console.log` prints it: normalize
  with `| 0`.
- **Floats.** `console.log(-0.0)` prints `-0` in Node and `0` in Velt (documented): print floats
  through a template (`${x}`) or `JSON.stringify`. `Math.hypot` isn't correctly rounded in V8.
- **Printing containers.** Node groups arrays of more than 6 elements into columns (not
  switchable): print `xs.slice(0, 6)`, `JSON.stringify(xs)` or `join`. Keep nesting shallow
  (Node shows `[Object]` past depth 2).
- **Strings.** ASCII only (`length` is bytes in Velt, UTF-16 units in JS).
- **Ownership.** `let b = a`, storing, pushing and returning move non-Copy values in Velt but
  alias in JS: move only fresh values (`` `${s}` ``, `xs.slice(0)`, `new K(...)`). `slice`,
  `filter`, `concat` clone elements in Velt but share them in JS: don't mutate objects reachable
  from two arrays. A stored closure captures numbers by copy in Velt (see bug
  `lower_closure_capture_copy`): only capture `const`s.
- **Library differences.** `sort()` on numbers is numeric in Velt, lexicographic in JS; `Map.keys()`
  is an array in Velt, an iterator in JS (use `[...m.keys()]`); `sort()` / `reverse()` return
  nothing in Velt (don't chain them); `join` isn't on string-enum or literal-union arrays yet.
- **Tagged types.** Discriminated unions, literal unions and enums work as in TS, with Velt's
  checks: a `switch` without `default` must be exhaustive, and a tag test the flow has already
  ruled out (`v.kind === "a"` inside `case "b":`) is an error in both languages. Write object
  literals of a union where the union type is expected (annotation, parameter), with fields in
  declaration order (JSON output follows insertion order in JS). Known bugs to steer around:
  string methods on literal unions (`l.length`), a narrowed literal used as its union type
  (VIR verification failure), `x.acc++` on an `f64` accessor.
- **Std modules.** Regex subjects must be ASCII (offsets are bytes) without `\r`; patterns that
  can match the empty string differ after a non-empty match; JS returns `undefined` for a
  `split` group that didn't participate (Velt `""`). Floats from `Math.pow` and `Math.hypot`
  aren't correctly rounded everywhere: print them only when exact.
- **Exceptions.** An uncaught throw exits with 1 in both; only stdout and the code are compared.
- **Async.** JS starts an async function eagerly, Velt lazily (at `await`) on a multi-threaded
  runtime: only `await` calls directly, never store a promise, and pass to `Promise.all` only
  calls that neither print nor await. Async parameters are moved into the future, so an async
  function must not modify its arguments (the caller would see it in JS only).
