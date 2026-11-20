Part of #943 and #379.

A value of a type parameter bounded by an array type can now be spread into a call's rest parameter of `unknown` or `any` values, as TypeScript allows:

```ts
function forward<A extends unknown[], R>(g: (...xs: unknown[]) => R): (...args: A) => R {
  return (...args: A): R => g(...args);
}
```

Before, Velt refused this with "cannot spread a value of type `A` into an array". sigx's `serverFn` overload wrapper needs it: the sigx kit drops from 136 to 135 (`server/src/index.ts:220`).

## How

The design note is on #943.
- **Sema:** the spread becomes `Intrinsic::SpreadParam`, an array of the rest's element type (`body/expr/spread_param.rs`). Only a call's packed rest argument takes it.
- **Lowering:** builds the array per instantiation (`lower/array/spread_param.rs`). A tuple gives its elements in order and an array its elements, each shared and boxed.
- **Contract:** one new `Intrinsic` variant, documented in `docs/internals/contracts/hir_encodings.md`. No new `ExprKind`.
- **A lock-holding element:** an instantiation whose element holds a lock is refused where the spread is written, as printing such a value is.

## Still refused

- **An array literal** (`[...args]`): tsc accepts it.
- **Fixed parameters** (`pair(...args)`): tsc refuses it too (TS2556).
- **A type parameter without an array bound:** tsc refuses it too (TS2488).

## Differences from Node

- **The array literal case above.**
- **A handler with fewer parameters than its type** (`handler: (rq) => …` for `handler(rq, input: S)`): still #3296. The sigx-shaped golden writes `input: void`.
- **`index.ts:230`:** still fails on throws inference (a `SigxError` thrown into a function type written without `throws`). That is outside this PR.

## Tests

- **`lang/spread_type_param_rest`:** tuple instances of several lengths and element types, the empty tuple, an array instance, a spread after fixed arguments, and a single evaluation of the spread value. Node prints the same.
- **`lang/overload_rest_result`:** the `serverFn` shape end to end, in its direct and options forms. Node prints the same.
- **`lang/errors/spread_type_param_lock`:** an instantiation whose element holds a lock.
- **`lang/errors/spread_type_param_refused`:** the cases above that stay refused.
- tsc 6.0 `--strict` accepts both run goldens.

Docs sync: DOCS_ISSUE.
