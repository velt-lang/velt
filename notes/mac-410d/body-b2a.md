Two conversions an overload signature makes to call its implementation, which TypeScript allows and Velt refused ("this overload signature is not compatible with its implementation signature", TS2394), now compile. Each converts with a check when the program runs.

## A callback whose rest parameter is typed by a type parameter

```ts
function wrap<A extends unknown[], R>(impl: (rq: Ctx, ...args: A) => R): (wire: unknown[]) => unknown;
function wrap(arg: ((rq: Ctx, ...args: unknown[]) => unknown) | Options): (wire: unknown[]) => unknown { … }

const add = wrap((rq: Ctx, a: number, b: number): number => a + b);
add([1, 2]);      // 3
add(["1", 2]);    // TypeError: the value is not a number (it is a string)   (Node: "12")
```

The signature's body passes the callback as `impl as unknown as <the implementation's function type>` (`overload_adapter.rs` `rest_param_cast`). What `A` is is known per call only, so no adapter can name its elements. The instance's value is the callback's own function: since #3226, its type has the tuple's elements as its parameters. Called through the wider type, its arguments are converted to what it takes.

The cast is made only when:
- the parameters before the rest are written alike;
- a `throws` clause is written alike;
- the callback isn't optional;
- the implementation's type names none of the implementation's own type parameters.

Otherwise the signature is refused as before (`errors/overload_rest_callback_forms`).

## A function result of `unknown` values

```ts
function make<S, R>(o: Options<S, R>): (input: S) => Promise<R>;
function make(o: Options<unknown, unknown>): (input: unknown) => Promise<unknown> { … }
```

When the implementation's result doesn't convert to the signature's, and it is a function or a callable object, it is converted as an interface adapter converts (`adapting`, `signature_result` → `checked_result`). The conversion is a wrapper whose arguments go in as they are and whose results are checked when they come. A promise's value is checked when it settles. A result of the wrong type is a TypeError, where Node goes on with it.

It is reached only when the types don't match. Unions keep their member check (`checked_narrow`). Arrays, objects and interfaces are left out: converted by an adapter they would be copies, where TypeScript returns the implementation's own value.

**Also:** a function member of a nullable or union result (`(() => R) | undefined`) converts the same way. An async callback passed where the signature takes a callback with a rest typed by a type parameter is refused at the call: the implementation calls it as a function of `unknown` values, which can't return a promise yet (#924, #1674 item 1).

## Differences from Node (velt-lang.github.io#278)

- An argument or result that isn't what the types say is a TypeError at the call or when the promise settles. Node carries the value on.
- The implementation gets the callback as a converted function value, not the caller's own (`===` against the caller's function is `false`), as for other adapted function values.
- A function or callable result is a wrapper, not the implementation's own value.

## What this does not finish

sigx's `serverFn` (`server/src/index.ts:220` and `:230`) is still refused, one step later. Both lines now fail on the implementation's result `(...args: unknown[]) => Promise<unknown>` as the signature's `(...args: A) => Promise<R>`. The wrapper for that has to spread a value of type `A` into a call, which Velt doesn't do yet ("cannot spread a value of type `A` into an array"). That is the next step, with a design note on #943. The sigx kit count is unchanged: 136.

## Tests

- **`lang/overload_rest_callback_cast`:** two parameters, one, none, and the options member of the implementation's union. The last call passes a string where the callback takes a number. Node prints the same first four lines; Velt's `.stderr` is the TypeError, `.code` 101.
- **`lang/overload_checked_result`:** a function result and a callable-object result (the field read through the copy), each with a synchronous and an async handler. The last call's implementation returns a string where a number is promised: the TypeError when the promise settles.
- **`lang/errors/overload_rest_callback_forms`:** the parameters before the rest not alike (tsc refuses it too, TS2394), and an optional callback (tsc accepts it; refused as before).
- **`lang/overload_nullable_result`:** `() => R` and `() => Promise<R>`, each `| undefined`. Node prints the same.
- **`lang/errors/overload_async_callback`:** an async callback through the cast, refused (tsc accepts it).
- tsc 6.0 `--strict` accepts both run goldens.
- **Broken on purpose** (one build, a switch per piece): without the cast, `overload_rest_callback_cast` is refused; without the result conversion, `overload_checked_result` is refused.

## Cost

Sema only. The cast is chosen while the signature bodies are built. The result retry runs only where a signature's result didn't convert, which used to be an error.

Part of #379.
