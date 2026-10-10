# `package.vlt`

Every package has a `package.vlt` at its root: the manifest, written in Velt. `velt new` and
`velt init` write one.

```ts ignore
import type { Package } from "velt:package";

export const pkg: Package = {
  name: "hello",
  version: "0.1.0",
  velt: "0.1",                  // the velt versions that build it: the newest 0.1.x
  dependencies: {
    json: "1.2",                // registry package, semver requirement
    http: { version: "0.3" },   // object form
    util: { path: "../util" },  // local package (version optional)
  },
  paths: { "@app/*": "src/*" }, // import { x } from "@app/util"  →  src/util.vlt (or .ts, .tsx)
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
you type, explains each on hover, and reports exactly the errors `velt` would, as you type. It
also asks the package's registry: it completes package names and versions in `dependencies`,
shows a dependency's newest and locked versions on hover, and flags requirements that no
published version matches or that leave out a newer one (with a fix). The
[`Package`](../std/package.md) type documents the same fields. Leave out the fields you don't
need. A key the manifest doesn't know is an error, with a
suggestion when it is close to one (`dependecies` → `dependencies`). `velt manifest` checks the
file and reports these errors without building anything.

## `name`, `version`, `description`, `keywords`, `entry`

- `name`: lowercase letters and digits, starting with a letter, with single `-` or `_` between
  words (`text-kit`, `text_kit`; not `text--kit`, `text_-kit` or `textkit-`). Names that differ
  only in `-` versus `_` count as the same in a registry. `std` is reserved for the standard
  library, and names whose first word is `rt`, `sig` or `native` (`rt`, `rt-str`) because a
  package's native functions are named `velt_<name>__…` and those prefixes are Velt's own
  (`velt_rt_…` is the runtime).
- `version`: a semantic version.
- `description`: one line about the package, shown by `velt search` and registry listings: at most
  300 characters, no line breaks, no surrounding whitespace.
- `keywords`: search words, such as `["json", "parser"]`: at most 10, each lowercase letters,
  digits and `-` (at most 32 characters, starting with a letter or digit). `velt search` matches
  every word of its text against names, keywords and descriptions, names first.

  The registry keeps both per published version, so changing them means publishing a new
  version.
- `entry`: the program's root file, a path inside the package (default `"src/main.vlt"`). A
  package with `src/main.vlt` is runnable; a package with `src/lib.vlt` is a library that other
  packages import. A package can have both. Without an `entry`, `src/main.ts` or
  `src/main.tsx` (`src/lib.ts`, `src/lib.tsx`) work like `src/main.vlt` (`src/lib.vlt`); two of
  them at once are an error.

## `velt`

The velt versions that build the package. `velt new` and `velt init` write the `major.minor` of
the velt that created the package (a pre-release writes its whole version, such as
`"0.2.0-rc.1"`), so installing a newer velt doesn't change how it builds: pre-1.0 minors may
change the language, and a toolchain's standard library and runtime belong to it.

Choosing the toolchain by this field comes with the velt launcher, which installs versions side
by side (#948). Until then the field records the version, the velt that runs checks that it is a
requirement, and it warns when its own version is not one the field accepts:

```text
warning: package.vlt asks for velt "0.2", but this is velt 0.1.0; it may not build the package
```

| `velt` | builds with |
|---|---|
| `"0.1"` | the newest installed `0.1.x` |
| `"0.1.3"` | `0.1.3` or a newer `0.1.x` |
| `"=0.1.3"` | exactly `0.1.3` |
| `">=0.1, <0.3"` | a requirement with an operator means what it means in `dependencies` |

A version without an operator stays within its minor version, also after 1.0: `"1.2"` accepts
`1.2.5` but not `1.3.0`, unlike the same requirement in `dependencies`. A major version alone
(`"1"`) accepts any `1.x`. Pre-releases match only
a requirement that names one (`"0.2.0-rc.1"`). Leave the field out to build with any velt.
`velt manifest` and every other command check that the value is a requirement.

## `dependencies`

`import … from "json"` and `"json/sub"` resolve through `dependencies`: the dependency's
`src/lib.vlt`, or `src/sub.vlt` / `src/sub/index.vlt` (or `.ts`, `.tsx`, tried like a relative
import's files). Standard library imports (`"velt:x"`)
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
`/** @jsxImportSource x */` comment at the top of a file wins ([JSX](../std/jsx.md)); Velt also
reads it from a `//` comment, `tsc` only from a block comment.

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

## `tsCompat`

```ts ignore
tsCompat: ["src/components", "src/models"],
```

Folders whose modules a TypeScript client shares, so they must stay in the
[common subset](../internals/design/tsx.md#the-common-subset) of TypeScript and Velt (like the
client's `tsconfig.json` `include`). `velt check --ts-compat` without paths lints every `.vlt`,
`.ts` and `.tsx` file in them ([`velt check`](cli.md#code-shared-with-typescript---ts-compat)),
and the editor shows the findings, with their fixes, as you type
([editors](editors.md#code-shared-with-typescript)). Nothing else changes: `velt build` and a
plain `velt check` never lint.

- Each entry is a `/`-separated path relative to the package root (`src/models`; not
  `./src/models`, `../shared` or `/abs`, no trailing `/`).
- Each folder once: a folder listed twice, or inside another listed one, is an error. Case
  doesn't matter here (`src/models` and `src/Models/sub` overlap): on macOS and Windows they
  are the same folder, so such a list would mean something else on Linux.
- The files in a folder are found as `velt check` finds a package's: `node_modules/`,
  `target/`, hidden and symlinked directories are skipped, and so is a package nested in the
  folder (a directory with its own `package.vlt`), which lints its own `tsCompat`.
- Not empty: leave the field out instead of writing `[]`.
- A folder that doesn't exist is a warning in the editor and an error for
  `velt check --ts-compat`.

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
  "velt": "0.1",
  "version": "0.1.0"
}
```

## Moving from `velt.toml`

Packages used to have a `velt.toml`. It is no longer read: every command in a package that
still has one stops with an error that prints the equivalent `package.vlt`. Save that as
`package.vlt` (copy over any comments) and delete `velt.toml`.

Package versions published with a `velt.toml` can't be installed any more; installing one says
so. Their authors publish a new version with a `package.vlt`.

## `velt.lock.json`

`velt install` writes `velt.lock.json`, which pins the exact version and content hash of every
dependency. It is generated JSON, pretty-printed with a stable key order so diffs stay readable:
`"version": 1` and a `"packages"` array with one entry per package (`name`, `version`, `source`:
`"registry"` or `"path+<relative path>"`, `checksum`, `dependencies`, and for a package with
native code a `"native"` object with the checksum of its prebuilt library for every published
target). Commit it for applications. `--locked` on `build`, `run`, `test` and `install` fails
instead of changing it. A package that still has the former `velt.lock` (TOML) gets an error:
delete it and run `velt install`.
