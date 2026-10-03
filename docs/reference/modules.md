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
| `"./file"`, `"../dir/file"` | relative to the importing file, without the extension: `file.vlt`, `file.ts` or `file.tsx` ([TypeScript files](#typescript-files-ts-and-tsx)). A folder is a module through its `index` file: `"./shapes"` is `shapes.vlt` (or `.ts`, `.tsx`), else `shapes/index.vlt` (or `.ts`, `.tsx`) |
| `"velt:x"` | the [standard library](../std/README.md) module `x` (`"velt:fs"`, `"velt:collections/set"`) |
| an alias from `paths` in `package.vlt` | `"@app/*": "src/*"` makes `"@app/util/strings"` mean `src/util/strings.vlt` (or `.ts`, `.tsx`), resolved like a relative import ([`package.vlt`](../tooling/manifest.md)) |
| `"pkg"`, `"pkg/sub"` | a dependency from `package.vlt`: its `src/lib.vlt`, or `src/sub.vlt` / `src/sub/index.vlt` ([Packages](../tooling/packages.md)) |

The prelude (strings, arrays, `Map`, `Math`, `JSON`, `Error`, `Comparable`, `Mutex`, `assert`,
…) is always in scope without an import ([Built-ins](builtins.md)).

## TypeScript files (`.ts` and `.tsx`)

A module can be a `.vlt`, `.ts` or `.tsx` file, so a folder can be shared with a TypeScript
project: Velt reads all three as Velt source, and `tsc` reads the `.ts` and `.tsx` files. A
program may mix them freely, and `velt run app.ts` works like `velt run app.vlt`.

- `"./util"` tries `util.vlt`, `util.ts` and `util.tsx`, then `util/index.vlt`,
  `util/index.ts` and `util/index.tsx`. When two of the files tried together exist (`util.vlt`
  and `util.ts`), the import is an error that names them: rename or remove one.
- A relative import may name the extension, as TypeScript allows with
  `allowImportingTsExtensions`: `"./util.ts"` (or `.tsx`, `.vlt`) is exactly that file. As in
  TypeScript, `"./util.js"` means `util.ts` or `util.tsx`, and `"./card.jsx"` means `card.tsx`.
- JSX is allowed in `.tsx` and `.vlt` files. In a `.ts` file it is an error, as in TypeScript;
  rename the file to `.tsx`. The [JSX provider](../internals/contracts/jsx.md#choosing-the-provider) is chosen the same way for every file:
  the `// @jsxImportSource` comment, else the package's `jsx.importSource`, else `velt:jsx`.
- Two files whose paths differ only in the extension have the same module path, so a program
  can't load both (`velt check` in a package reports it).
- Declaration files (`.d.ts`) are not modules: Velt never loads them.
- Standard library modules and the modules of dependencies (`"pkg"`, `"pkg/sub"`) are `.vlt`
  files, named without an extension.

`velt check` in a package, `velt test` (`*.test.ts`, `*.test.tsx`), `velt fmt`, `velt doc`
and the language server take `.ts` and `.tsx` files along with `.vlt` ones
([tooling](../tooling/cli.md)).

## Runtime declarations

`declare function` / `declare async function` declare runtime (C ABI) functions; the standard
library uses them to bind `velt_rt_*`, and only it may declare those (they take raw runtime
handles). Outside the standard library, a `declare` may only name an export of the native
library of its own package, and must match that export exactly
([Packages with native code](../book/native-packages.md)). Anything else is a compile error: a
program or a package without native code can't declare C functions such as `free` or `memcpy`. Compiler intrinsics (`__intrinsic_*`) are reserved for
the standard library too.

Whether a module belongs to the standard library depends on where it was loaded from (the std
root), never on its name: a user module whose path would start with `std/` (a `./std/`
directory) is an error, and no package may be named `std`.

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
