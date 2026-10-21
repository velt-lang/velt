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
- `import.meta` describes the module, as in Node, with Node's keys in Node's order:
  `import.meta.dirname` is the source file's directory, `import.meta.filename` its absolute
  path, `import.meta.main` is `true` in the program's root module (the file `velt run` or
  `velt build` is given) and `false` in the others, `import.meta.resolve(specifier)` resolves a
  specifier against the module's URL, and `import.meta.url` is the file's `file:` URL. They are
  fixed while compiling, so they name the source file, not the built program, wherever it runs.
  `console.log(import.meta)` prints it as Node does (`[Object: null prototype] { dirname: …,
  resolve: [Function: resolve], … }`), and `JSON.stringify` leaves `resolve` out.
- `resolve` resolves as Node does, without checking that the file exists: an absolute URL as
  the URL parser normalizes it (`HTTPS://Example.com/a` is `https://example.com/a`), a relative
  or absolute path (`./data.json`, `../a?x=1#h`, `/etc/hosts`, which keeps a Windows drive
  letter) against the module's URL, and one of Node's built-in modules as `node:<name>`
  (`fs` is `node:fs`, `_http_agent` is `node:_http_agent`). A package name throws an `Error`
  with Node's message (`Cannot find package 'x' imported from <file>`): packages are compiled
  into the program rather than looked up while it runs, so `try` / `catch` around
  `import.meta.resolve("optional-dep")` takes the `catch`. Its type is therefore
  `(specifier: string) => string throws Error`, and it can't be used where a function type
  that cannot throw is expected (`const r: (s: string) => string = import.meta.resolve` is an
  error): write that type with `throws Error`, or wrap it in an arrow,
  `const r = (s: string) => import.meta.resolve(s)`, whose type says it throws.
- `import.meta` can be passed where an object type with some of its fields is expected
  (`function f(p: { readonly url: string })`, `f(import.meta)`). That object is a copy of those
  fields, as for any object type of another layout: assigning one of its fields is an error, and
  it is not `===` to `import.meta`.
- `import.meta` used as a value of its own (`const m = import.meta`, `return import.meta`) is
  the same object. Its fields are read-only, also through such a value (`m.url = "x"` is an
  error, as `import.meta.url = "x"` is).
- A dynamic `import(…)` is an error: every module is loaded while compiling, so import it with
  a static `import` instead.

```ts
const here = import.meta.url;
console.log(here.startsWith("file:///")); // true
```

## Scripts: top-level statements

A program starts at `function main()` (or `async function main()`) in its root file, or, as in
a TS or JS file, at its top-level statements: when the root file has statements outside
declarations, they run in order in a generated `main` (`async` when one of them awaits). A
top-level `const` or `let` moves into that `main` with them, unless a function, class, struct,
`extend` block or module constant refers to it; then it stays at module level
([module state](variables.md#module-state)).

```ts
const GREETING = "hello";               // a function uses it: a module constant

function greet(name: string): string {
  return `${GREETING} ${name}`;
}

const names = ["ann", "bob"];           // only statements use it: a local of `main`
names.push("cy");
for (const n of names) {
  console.log(greet(n));
}
```

A file with top-level statements cannot also declare `main`.

An imported module's top-level statements run once, when the module is evaluated, as in Node:
the modules are evaluated before the root file's statements run, each after the modules it
imports (in the order of its `import`s, depth first) and before the module that imports it. A
circular import finds the module that is still being evaluated and goes on without it. A
module imported only for its types (`import type { … }`, or an import whose every name is
`type`) is not evaluated: neither its statements nor its variables' initializers run. A
module's statements run in source order with its module-level variables
([module state](variables.md#module-state)): each after the initializers above it and before
those below it (a variable that statements on both sides of a module-level variable use must
be declared with one name and an initializer, for now). An error they throw and don't catch prints `Uncaught …` and ends the program
with exit code 1. In the root file, a promise statement (`renderAll();`) starts the promise as
in Node: it runs up to its first `await`, the statements after it run, and the program waits for
it before exiting (a rejection nobody handles ends it with exit code 1); in a function it is a
floating promise, an error. Top-level `await` is only allowed in the root file, and an imported module's
statements can't start asynchronous work yet (a timer, a promise chain, `queueMicrotask`,
`spawn`): they run before the scheduler starts (#846).

```ts
// registry.ts
export const registry: string[] = [];
export function register(name: string): void {
  registry.push(name);
}
register("trim"); // runs when a module imports registry.ts
```

## Re-exports

Re-exports make one module the public face of others: `export { x, y as z } from "…"`,
`export type { T } from "…"`, and `export * from "…"` (every export of that module; a name the
module exports itself or lists by name wins). Re-exports bind nothing in the re-exporting
module; importers get the original declaration, and go-to-definition lands there too.

## Namespaces

A `namespace` groups types under one name, as in TypeScript: its members are written `N.T`
outside it and `T` inside it (a type parameter of the same name hides a member). Namespaces
nest (`A.B.T`), `namespace A.B { … }` declares `A` holding `B`, blocks with the same name are
one namespace, and a namespace may share its name with an interface or a type
(`interface Shape` and `Shape.Circle` are different types).

```ts
namespace Geo {
  export interface Point {
    x: number;
    y: number;
  }
  type Pair = [Point, Point]; // without `export`: only inside `Geo`
  export type Segment = { ends: Pair; length: number };
}

const a: Geo.Point = { x: 0, y: 0 };
const b: Geo.Point = { x: 3, y: 4 };
const s: Geo.Segment = { ends: [a, b], length: 5 };
console.log(s.ends[1].x, s.length); // 3 5
```

- Only members declared with `export` are visible outside (`Geo.Pair` is TypeScript's TS2694,
  ``namespace `Geo` has no exported member `Pair` ``); in a `declare namespace` every member is.
  A member declared without `export` is visible only in its own block, not in another block
  of the same namespace; two blocks may each declare their own unexported member of the same
  name (two interfaces declared so do not merge).
- Inside a namespace, a name used as a type means a type or interface (a nested namespace of
  the same name does not hide an outer type), and the first part of `B.T` a namespace.
- A namespace named like an imported type or value merges with it, as in TypeScript: the bare
  name is the import, `N.T` the namespace's member (`import { N } from "./lib"` with
  `export type N = number` there, next to `namespace N { export type T = string }`). A
  namespace can't share its name with an imported namespace, `import * as N` or the import of
  another module's `namespace N` (TypeScript's TS2440: ``import declaration conflicts with
  local declaration of `N` ``).
- An exported namespace is imported and re-exported by its name, like a type
  (`import { type Geo } from "./geo"`, `export type { Geo } from "./geo"`), and through
  `import * as m` as `m.Geo.Point`.
- `declare global { namespace JSX { … } }` declares the namespace in its module, as a
  `declare namespace` (a global visible in other modules is not supported yet).
- A namespace holds type aliases, interfaces and namespaces. Values in a namespace (`const`,
  `function`, `class`, `enum`: a run-time object in TypeScript) are not supported yet.

## Module specifiers

| Specifier | Resolves to |
|---|---|
| `"./file"`, `"../dir/file"` | relative to the importing file, without the extension: `file.vlt`, `file.ts` or `file.tsx` ([TypeScript files](#typescript-files-ts-and-tsx)). A folder is a module through its `index` file: `"./shapes"` is `shapes.vlt` (or `.ts`, `.tsx`), else `shapes/index.vlt` (or `.ts`, `.tsx`) |
| `"velt:x"` | the [standard library](../std/README.md) module `x` (`"velt:fs"`, `"velt:collections/set"`) |
| an alias from `paths` in `package.vlt` | `"@app/*": "src/*"` makes `"@app/util/strings"` mean `src/util/strings.vlt` (or `.ts`, `.tsx`), resolved like a relative import ([`package.vlt`](../tooling/manifest.md)) |
| `"pkg"`, `"pkg/sub"` | a dependency from `package.vlt`: its `src/lib.vlt`, or `src/sub.vlt` / `src/sub/index.vlt` (or `.ts`, `.tsx`, as for a relative import) ([Packages](../tooling/packages.md)) |
| `"@scope/pkg"`, `"pkg/sub"` of an npm package | when no dependency has that name: the npm package in `node_modules` or the workspace, compiled from the TypeScript source its `package.json` `exports` point back to ([npm packages](../tooling/packages.md#npm-packages)) |

A specifier must spell file and directory names exactly as they are on disk, case included, on
every OS, as TypeScript's `forceConsistentCasingInFileNames` requires: `"./Util"` does not load
`util.vlt` on Windows or macOS either, since it would fail on Linux. The error names the file on
disk and the specifier to write (`import it as "./util"`). Files whose names differ only in case
(`Util.ts` next to `util.vlt`) are different modules everywhere.

The prelude (strings, arrays, `Map`, `Math`, `JSON`, `Error`, `Comparable`, `Mutex`, `assert`,
…) is always in scope without an import ([Built-ins](builtins.md)).

## TypeScript files (`.ts` and `.tsx`)

A module can be a `.vlt`, `.ts` or `.tsx` file, so a folder can be shared with a TypeScript
project: Velt reads all three as Velt source, and `tsc` reads the `.ts` and `.tsx` files. A
program may mix them freely, and `velt run app.ts` works like `velt run app.vlt`.

- `"./util"` tries `util.vlt`, `util.ts` and `util.tsx`, then `util/index.vlt`,
  `util/index.ts` and `util/index.tsx`. When two of the files tried together exist (`util.vlt`
  and `util.ts`), the import is an error that names them: rename or remove one. A file wins over
  a folder, as in TypeScript; when the folder's `index` file has another extension (`util.ts`
  next to `util/index.vlt`), the import gets a warning, since the folder module is easy to
  mistake for the one loaded.
- A relative import may name the extension, as TypeScript allows with
  `allowImportingTsExtensions`: `"./util.ts"` (or `.tsx`, `.vlt`) is exactly that file. As in
  TypeScript, `"./util.js"` means `util.ts` or `util.tsx`, and `"./card.jsx"` means `card.tsx`.
- JSX is allowed in `.tsx` and `.vlt` files. In a `.ts` file it is an error, as in TypeScript;
  rename the file to `.tsx`. The [JSX provider](../internals/contracts/jsx.md#choosing-the-provider) is chosen the same way for every file:
  the `/** @jsxImportSource x */` comment before the first token, else the package's
  `jsx.importSource`, else `velt:jsx`. Velt also reads the comment as `// @jsxImportSource x`;
  `tsc` reads only the block comment, so code shared with TypeScript uses that form.
- Two files in one directory whose names differ only in the extension (`dup.vlt` and
  `dup.ts`) have the same module path: an import without an extension can't tell them apart,
  and one program can't load both. `velt check` in a package reports every such pair under
  `src/` and `tests/` as an error, whether or not anything imports them.
- Declaration files (`.d.ts`) are not modules: Velt never loads them. `"./types.d"` does not
  find `types.d.ts`, and `"./types.d.ts"` is an error.
- TypeScript's older type assertion `<T>x` is an error in a `.ts` file (in `.tsx` and `.vlt`
  files `<T>` starts JSX, as in a `.tsx` file): narrow with `typeof` or `instanceof`, or
  annotate the variable's type.
- A package can be written in TypeScript: the modules of a dependency (`"pkg"`, `"pkg/sub"`)
  are found like relative imports' files (`src/lib.ts`, `src/sub.tsx`, `src/sub/index.ts`, …),
  and without an `entry` in `package.vlt` a package's default entry is `src/main.vlt`,
  `src/main.ts` or `src/main.tsx` (`src/lib.*` for a library). Standard library modules are
  `.vlt` files.

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
