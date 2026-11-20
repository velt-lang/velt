## Design note: spreading a value of an array-bounded type parameter into a call's rest parameter

Part of this issue, for sigx's `serverFn` (#379: `server/src/index.ts:220` and `:230`).

### What is missing

```ts
function forward<A extends unknown[], R>(g: (...xs: unknown[]) => R): (...args: A) => R {
  return (...args: A): R => g(...args);
}
```

In a generic body, `args` has type `A`, a type parameter bounded by an array type. Velt refuses `g(...args)`: "cannot spread a value of type `A` into an array". TypeScript accepts it, and so does Node. #3226 made `A` inferable from a callback, and its lowering makes an instance's function type with a tuple rest the fixed parameters (`(Ctx, ...[f64, f64]) => R` is `(Ctx, f64, f64) => R`). So `args` is a tuple at one instance and an array at another.

The overload wrapper of sigx's `serverFn` needs exactly this. The implementation returns `(...args: unknown[]) => Promise<unknown>` and the signature promises `(...args: A) => Promise<R>`. The wrapper that converts it (an interface adapter's `adapt_fn`) is `(...args: A) => f(...args)`.

### Scope: call arguments only

A spread of a value of type `A` is accepted where it is an argument of a call whose matching parameter is a rest parameter of an array type `U[]`, and where the bound's element type converts to `U` (`A extends unknown[]` into `...xs: unknown[]`, or `A extends number[]` into `...xs: number[]`). It is still refused:

- in an array literal (`[...args]`);
- into fixed parameters (`f(...args)` for `f(a: number, b: number)`, whose arity would be known only per instance);
- for a type parameter without an array bound.

Calls *through* a function value whose own rest is `A` stay refused (#3226's call refusal, TS2345).

### Per instance

Sema types the spread against the bound and records the argument as a spread of a type-parameter value whose type stays `A`, with the rest's element type. Lowering substitutes `A` and then expands it:

- **A tuple** (`[f64, string]`): its elements, in order, each converted to `U` (boxed to `unknown` here) and pushed onto the rest array. Today a written spread of a concrete tuple into a call (`g(...t)` with `t: [number, string]`) is expanded in sema into element reads (`spread_args.rs`), which a generic body can't do, so lowering needs the same expansion per instance.
- **An array** (`number[]`): as a spread of that array does now.
- **An empty tuple** (`[]`): nothing.

**Contract change:** this needs one HIR form for "spread of a value of a type parameter into a rest", lowered per instance: an `Intrinsic` variant (`SpreadParam(value, elem_ty)`) rather than a new `ExprKind`, so the HIR shape changes as little as possible. It touches `hir` (a contract) and velt_vir's call lowering, so it needs your approval before code.

No run-time check is needed: the conversion is from each element's own type to the rest's, so it is checked statically per instance. If an instance's element doesn't convert, that is a sema-level error, reported at the instantiation as other per-instance requirements are (`TyOp::Convert`-style, `generic_spread.rs`). With an `unknown[]` rest it can't happen.

### Tests

Goldens from Node, and tsc `--strict` accepting them:

- `forward` above with tuple instances of different lengths and element types, the empty tuple, and an array instance;
- a spread after fixed arguments (`g(rq, ...args)`);
- the overload shape of sigx's `serverFn`, end to end: the direct form `(rq, ...args: A) => R` and the options form `[S] extends [void] ? [] : [S]`;
- error goldens for the cases still refused (array literal, fixed parameters, unbounded `T`).

The kit should then drop `index.ts:220` and `:230`.
