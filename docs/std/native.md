# velt/native

`import { IoResult, IoStatus, unwrapIo, checkIo } from "velt/native"`. What the Velt side of a
[package with native code](../book/native-packages.md) binds its Rust functions with:

- `IoResult<T>`: the C layout a native function that produces a `T` returns (in Rust,
  `Result<T, Error>`), and `IoStatus` for one that produces nothing (`Result<(), Error>`).
- `unwrapIo(r)`: the value of a successful `IoResult`; throws an
  [`IoError`](io.md) `{ code, message }` otherwise. `checkIo(s)` throws for a failed `IoStatus`.
- `ioError(code, message)`: the `IoError` for a failed result.

```ts ignore
import { IoResult, unwrapIo } from "velt/native";

declare function velt_greet__parse(text: string): IoResult<i64>;

export function parse(text: string): i64 {
  return unwrapIo(velt_greet__parse(text));
}
```
