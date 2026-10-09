# Diagnostics

The compiler reports errors as `path:line:col: error: message`, followed by notes
(`= note: …`) and related locations (`--> path:line:col`). Compilation stops with exit code 1;
nothing is run. An internal compiler error exits with code 101 and starts with
`internal compiler error`; please [report it](../../CONTRIBUTING.md#reporting-bugs).

The wording below is stable: tests, editors and tools rely on it.

| Situation | Message |
|---|---|
| parse | `expected expression`, ``expected `;` ``, `expected <token>, found <token>` |
| unknown name or type | ``cannot find `name` in this scope``, ``cannot find type `T` in this scope`` |
| types | `mismatched types` + note `expected i64, found string` |
| conditions | ``an expression of type `void` cannot be tested for truthiness`` (also for a value of a generic type) |
| moves (promises, disposed values) | ``use of moved value `name` `` (+ where it moved) |
| const | ``cannot assign twice to const `name` `` |
| members | ``no field `x` on type `T` ``, ``` `x` is private ```, ``property `#x` is not accessible outside class `A` because it has a private name``, `private names are only allowed in class bodies`, ``the right operand of `in` may be null``, ``cannot assign to `x`: it is a readonly field``, ``cannot assign to `x`: it is a getter`` |
| unions | ``no field `r` on type `Shape` `` + "narrow it to one member first …" |
| switch | ``` `"square"` is not a possible value of `s.kind` ``` + `possible values: …`, ``` `continue` cannot target a `switch` ``` |
| exclusive access | ``cannot use `xs` here: this call may modify it through another argument`` |
| threads | "this async closure modifies captured `n`, so it must stay on the task that created it" (+ where it reaches `spawn`, a handler, `shared` or a channel, and the `shared` hint); "`r` is still used after `spawn`, so the task would get a copy, but `T` owns a resource (`[Symbol.dispose]`) and has no `clone()`" (or "… after `send`, so the receiving task would get a copy, …" for a channel, "`this.conn` stays where it is held, …", "`Pair` holds a `Conn`, which …"; + the fixes) |
| modules | ``` `x` is not exported ```, "mutable module-level state is not allowed", ``` `export default` is not supported: Velt has named exports only ```, ``` `T` is imported with `import type` and cannot be used as a value ```, ``` namespace `ns` has no exported member `x` ``` |
| async | ``` `await` is only allowed inside async functions ```, `floating promise: this promise is neither awaited nor spawned` (+ the `await` / `spawn` fixes) |
| errors | ``` `f` throws `E`, which its `throws` clause does not allow ```, ``` `C.m` throws `E`, but `I.m` does not allow it ```, "this function throws `E`, but the function type it is used as does not allow throwing", "the error type of this function is not known yet" |
| removed syntax | ``` `mut` is not needed: mutation is inferred ```, ``` `match` is not supported ```, "enum members cannot have payloads", ``` the `?` operator was removed ```, ``` `Result` was removed ```, ``` `undefined` is not part of Velt ``` (+ note ``use `null` ``) |

At run time:

| Situation | Output | Exit code |
|---|---|---|
| panic | `panic: <message> at file:line:col` | 101 |
| uncaught error | `Uncaught <Type>: <message> at file:line:col` | 1 |

The language server turns many of these diagnostics into quick fixes: removing `mut`,
replacing `undefined` with `null`, converting a `+` chain to a template literal, and adding
`await` or `spawn(...)` to a floating promise
([Editors](../tooling/editors.md)).
