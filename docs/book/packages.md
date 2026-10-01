# Modules and packages

## Modules

Every `.vlt` file is a module, with ES module syntax and named exports only:

```ts ignore
// src/geometry.vlt
export type Point = { x: f64; y: f64 };

export function distance(a: Point, b: Point): f64 {
  return Math.sqrt((a.x - b.x) ** 2.0 + (a.y - b.y) ** 2.0);
}

const EPSILON = 1e-9;                // not exported: private to the file
```

```ts ignore
// src/main.vlt
import { distance, type Point } from "./geometry";
import * as path from "velt:path";

function main() {
  const a: Point = { x: 0.0, y: 0.0 };
  console.log(distance(a, { x: 3.0, y: 4.0 }), path.basename("/srv/app.vlt"));
}
```

- Relative imports drop the extension. A folder is a module through its `index.vlt`, which
  usually re-exports its parts: `export { Circle } from "./circle";`, `export * from "./square";`.
- `import * as ns` binds a namespace; `import type` imports names for type positions only.
- `export default` and default imports don't exist: the compiler's error suggests the named
  form.
- Modules hold declarations only. State that changes lives in values created by `main` and
  passed where they're needed ([why](../reference/variables.md#no-mutable-module-state)).
- Standard library modules use the `velt:` prefix: `"velt:fs"`, `"velt:collections/set"`.

All rules are in [Modules and packages](../reference/modules.md) in the Reference.

## Path aliases

In a package, `[paths]` in `velt.toml` replaces long `../../` chains, like TypeScript's
`compilerOptions.paths`:

```toml
[paths]
"@app/*" = "src/*"
```

```ts ignore
import { execute } from "@app/commands";        // src/commands.vlt or src/commands/index.vlt
```

## Packages

A package is a directory with a `velt.toml` ([reference](../tooling/manifest.md)). An
application has `src/main.vlt`; a library has `src/lib.vlt`, whose exports are what other
packages import.

```sh
velt new textkit --template lib     # a library: src/lib.vlt, tests, doc comments
cd textkit
velt test
velt doc                            # HTML docs in target/doc from the /// comments
velt publish                        # to the local registry, or the one in velt.toml
```

Using it from another package:

```sh
cd ../app
velt add textkit                    # the latest version, into velt.toml and velt.lock
velt add util --path ../util        # or a package from a local directory
```

```ts ignore
import { slugify } from "textkit";             // the package's src/lib.vlt
import { wrap } from "textkit/format";         // its src/format.vlt
```

`velt install` resolves dependencies and writes `velt.lock` with exact versions and content
hashes; `velt install --locked` (also on `build`, `run` and `test`) fails instead of changing
the lockfile, for reproducible builds. Commit `velt.lock` for applications.

## Documenting a library

`velt doc` documents exported items from the `///` comments right above them:

```ts ignore
/// A URL-friendly form of `text`: lower case, ASCII letters and digits, words joined by `-`.
///
/// `slugify("Hello, World!")` is `"hello-world"`.
export function slugify(text: string): string {
  // …
}
```

## Registries

Packages come from a registry: a local directory by default (`~/.velt/registry`), or an HTTP
registry you run with `velt registry serve`. A public registry doesn't exist yet. See
[Packages and registries](../tooling/packages.md).
