# Modules and packages

## Exports

- Each file is a module. `export` marks functions, classes, structs, interfaces, types, enums
  and constants as importable; everything else is private to the file
  (``` `x` is not exported ```).
- `export { a, b as c };` exports names declared (or imported) in the file under the listed
  names.
- Named exports only: `export default` is an error whose fix is a named export
  (`export function name`, or `export { name };`), and so is a default import
  (`import x from`).
- `extend` blocks cannot be exported: they apply wherever their module is loaded.

## Imports

- `import { a, b as c } from "…"` binds exported names.
- `import * as ns from "…"` binds the module as a namespace: `ns.f()`, `ns.LIMIT`,
  `new ns.Class()`, `ns.Enum.Member`, `ns.Class.make()`, and `ns.Type` in type positions. `ns`
  itself is not a value; a local variable named `ns` shadows it.
- `import type { T } from "…"` (or `import { type T, f }`) imports names for type positions
  only; using one as a value is an error. Types are compile-time only either way, so this
  changes no code, as in TypeScript.
- `import "…";` loads a module without binding names.

## Re-exports

Re-exports make one module the public face of others: `export { x, y as z } from "…"`,
`export type { T } from "…"`, and `export * from "…"` (every export of that module; a name the
module exports itself or lists by name wins). Re-exports bind nothing in the re-exporting
module; importers get the original declaration, and go-to-definition lands there too.

## Module specifiers

| Specifier | Resolves to |
|---|---|
| `"./file"`, `"../dir/file"` | relative to the importing file, without the `.vlt` extension. A folder is a module through its `index.vlt`: `"./shapes"` is `shapes.vlt` or `shapes/index.vlt` |
| `"velt:x"` | the [standard library](../std/README.md) module `x` (`"velt:fs"`, `"velt:collections/set"`) |
| an alias from `[paths]` in `velt.toml` | `"@app/*" = "src/*"` makes `"@app/util/strings"` mean `src/util/strings.vlt` ([`velt.toml`](../tooling/manifest.md)) |
| `"pkg"`, `"pkg/sub"` | a dependency from `velt.toml`: its `src/lib.vlt`, or `src/sub.vlt` / `src/sub/index.vlt` ([Packages](../tooling/packages.md)) |

The prelude (strings, arrays, `Map`, `Math`, `JSON`, `Error`, `Comparable`, `Mutex`, `assert`,
…) is always in scope without an import ([Built-ins](builtins.md)).

## Runtime declarations

`declare function` / `declare async function` declare runtime (C ABI) functions; the standard
library uses them to bind `velt_rt_*`. A package with native code declares its own library's
functions the same way; each such `declare` must match the library's export exactly, or it is a
compile error ([Packages with native code](../book/native-packages.md)). Compiler intrinsics
(`__intrinsic_*`) are reserved for the standard library.

**Planned** ([TypeScript alignment §4](../internals/design/ts-alignment.md#4-extend--full-power-zero-cost-module-scoped)):
extensions scoped to the modules that import them.

## Examples

```ts
import { join, basename } from "velt:path";
import { gcd as greatestDivisor } from "velt:math";

export function describe(path: string): string {
  return `${basename(path)} in ${join("/srv", "app")}`;
}

console.log(describe("/tmp/a.txt"), greatestDivisor(12, 18));
```

```ts
import * as path from "velt:path";
import type { Stats } from "velt:fs";

function size(s: Stats): u64 {
  return s.size;
}

export { size as statSize };

console.log(path.join("a", "b"), path.extname("x.vlt"));
```

A folder module re-exporting its parts (`shapes/index.vlt`):

```ts ignore
export { Circle, area } from "./circle";
export * from "./square";
export type { Shape } from "./shape";
```
