# velt:package

`import type { Package } from "velt:package"`. The type of the package manifest,
[`package.vlt`](../tooling/manifest.md). The module contains only types; nothing imports it at
run time.

```ts ignore
import type { Package } from "velt:package";

export const pkg: Package = {
  name: "todo-api",
  version: "0.3.0",
  dependencies: { sqlite: "^0.1", util: { path: "../util" } },
  paths: { "@app/*": "src/*" },
};
```

`velt` never compiles `package.vlt`: it reads it as data. The language server does the same, so
the editor's completion, hover and errors in a manifest come from the manifest's rules, not from
type-checking the file; these types document the same fields (a test keeps them in line).

| Type | What it describes |
|---|---|
| `Package` | the manifest: `name`, `version`, and the optional `entry`, `registry`, `dependencies`, `paths`, `jsx`, `native` |
| `Dependency` | `string` (a semver requirement) or a `DependencySource` |
| `DependencySource` | `{ version?: string; path?: string }` |
| `Jsx` | `{ importSource?: string }` |
| `Native` | `{ path?: string; targets?: string[]; wasm?: boolean }` |

The fields and their rules are in [`package.vlt`](../tooling/manifest.md).
