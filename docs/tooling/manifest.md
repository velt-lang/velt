# `package.vlt`

Every package has a `package.vlt` at its root: the manifest, written in Velt. `velt new` and
`velt init` write one.

```ts ignore
import type { Package } from "velt:package";

export const pkg: Package = {
  name: "hello",
  version: "0.1.0",
  dependencies: {
    json: "1.2",                // registry package, semver requirement
    http: { version: "0.3" },   // object form
    util: { path: "../util" },  // local package (version optional)
  },
  paths: { "@app/*": "src/*" }, // import { x } from "@app/util"  →  src/util.vlt
};
```

The file is **data only**. `velt` reads it the way it would read JSON, without compiling or
running anything, so installing a package never runs its code. Besides the type import, it holds
one `export const pkg: Package = { … }` made of strings, numbers, `true`/`false`, arrays and
objects. Comments and trailing commas are fine; anything else (`null`, template literals,
variables, calls, spreads) is an error that points at it:

```text
package.vlt:5:13: error: `null` is not allowed; leave the key out
```

In an editor, the language server ([`velt lsp`](editors.md)) completes the fields valid where
you type, explains each on hover, and reports exactly the errors `velt` would, as you type. The
[`Package`](../std/package.md) type documents the same fields. Leave out the fields you don't
need. A key the manifest doesn't know is an error, with a
suggestion when it is close to one (`dependecies` → `dependencies`). `velt manifest` checks the
file and reports these errors without building anything.

## `name`, `version`, `entry`

- `name`: lowercase letters, digits, `_` and `-`, starting with a letter.
- `version`: a semantic version.
- `entry`: the program's root file, a path inside the package (default `"src/main.vlt"`). A
  package with `src/main.vlt` is runnable; a package with `src/lib.vlt` is a library that other
  packages import. A package can have both.

## `dependencies`

`import … from "json"` and `"json/sub"` resolve through `dependencies`: the dependency's
`src/lib.vlt`, or `src/sub.vlt` / `src/sub/index.vlt`. Standard library imports (`"velt:x"`)
and relative imports (`"./x"`) never do. Names that aren't identifiers are quoted
(`"my-lib": "1.0"`). `velt add` edits this object and keeps your comments. See
[Packages](packages.md).

## `paths`

Import aliases, like TypeScript's `compilerOptions.paths`, replace long `../../` chains:

- A bare specifier that matches a pattern resolves to the target, relative to the package root,
  before `dependencies` are consulted. `"velt:x"` and `"./x"` are never aliased.
- A pattern has at most one `*`, at its end, and so does its target (both or neither). The
  longest matching pattern wins. Each pattern has one target.
- Targets must stay inside the package (no `..`, not absolute).
- Each package's aliases apply to its own modules only.

## `jsx`

```ts ignore
jsx: { importSource: "sigx" },
```

The module whose `jsx-runtime` compiles the package's JSX: a dependency, `"velt:jsx"` (the
default), a `paths` alias, or `./dir` relative to the package root. A
`// @jsxImportSource x` comment at the top of a file wins ([JSX](../std/jsx.md)).

## `native`

```ts ignore
native: { targets: ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"] },
```

The package includes a Rust crate (a `cdylib` + `staticlib` built on the `velt_native` crate)
whose functions its Velt code declares ([Packages with native code](packages.md#packages-with-native-code)).

- `path`: the crate's directory, a directory name in the package root (default `"native"`).
- `targets`: the targets `velt publish` publishes a prebuilt library for (it fails if one is
  missing): `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `x86_64-apple-darwin`,
  `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc`.
- `wasm`: must be `false` (the default); WebAssembly libraries are not supported yet.

## `registry`

```ts ignore
registry: "https://registry.example.com",
```

A root package's `registry` URL, or `VELT_REGISTRY` set to an `http(s)://` URL (which takes
precedence), replaces the local registry directory for `velt add`, `velt install` and
`velt publish` ([Registries](packages.md#registries)).

## Other tools

`velt manifest --json` prints the validated manifest as JSON, with the defaults filled in, for
tools that can't read Velt:

```text
$ velt manifest --json
{
  "dependencies": {
    "json": "1.2"
  },
  "entry": "src/main.vlt",
  "name": "hello",
  "paths": {},
  "version": "0.1.0"
}
```

## Moving from `velt.toml`

Packages used to have a `velt.toml`. It is no longer read: every command in a package that
still has one stops with an error that prints the equivalent `package.vlt`. Save that as
`package.vlt` (copy over any comments) and delete `velt.toml`.

Package versions published with a `velt.toml` can't be installed any more; installing one says
so. Their authors publish a new version with a `package.vlt`.

## `velt.lock`

`velt install` writes `velt.lock`, which pins the exact version and content hash of every
dependency: `version = 1` and one `[[package]]` entry per package with `name`, `version`,
`source` (`"registry"` or `"path+<relative path>"`), `checksum` and `dependencies`, and for a
package with native code a `[package.native]` table with the checksum of its prebuilt library
for every published target. Commit it for applications. `--locked` on `build`, `run`, `test` and
`install` fails instead of changing it. It is generated, so it stays TOML.
