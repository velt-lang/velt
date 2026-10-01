# `velt.toml`

Every package has a `velt.toml` at its root. `velt new` and `velt init` write one.

```toml
[package]
name = "hello"            # [a-z][a-z0-9_-]*
version = "0.1.0"         # semver, required
entry = "src/main.vlt"    # optional, default "src/main.vlt" (relative to velt.toml)

[dependencies]
json = "1.2"                   # registry package, semver requirement
http = { version = "0.3" }     # table form
util = { path = "../util" }    # local package (version optional)

[paths]                        # optional import aliases
"@app/*" = "src/*"             # import { x } from "@app/util"  →  src/util.vlt (or src/util/index.vlt)
"@config" = "src/config"       # exact alias

[native]                       # optional: the package includes a Rust crate
targets = ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]
```

## `[package]`

- `name`: lowercase letters, digits, `_` and `-`, starting with a letter.
- `version`: a semantic version.
- `entry`: the program's root file. A package with `src/main.vlt` is runnable; a package with
  `src/lib.vlt` is a library that other packages import. A package can have both.

## `[dependencies]`

`import … from "json"` and `"json/sub"` resolve through `[dependencies]`: the dependency's
`src/lib.vlt`, or `src/sub.vlt` / `src/sub/index.vlt`. Standard library imports (`"velt:x"`)
and relative imports (`"./x"`) never do. See [Packages](packages.md).

## `[paths]`

Import aliases, like TypeScript's `compilerOptions.paths`, replace long `../../` chains:

- A bare specifier that matches a pattern resolves to the target, relative to the package root,
  before `[dependencies]` are consulted. `"velt:x"` and `"./x"` are never aliased.
- A pattern has at most one `*`, at its end, and so does its target (both or neither). The
  longest matching pattern wins. Each pattern has one target.
- Targets must stay inside the package (no `..`, not absolute).
- Each package's aliases apply to its own modules only.

## `[native]`

The package includes a Rust crate (a `cdylib` + `staticlib` built on the `velt_native` crate)
whose functions its Velt code declares ([Packages with native code](packages.md#packages-with-native-code)).

- `path`: the crate's directory, a directory name in the package root (default `"native"`).
- `targets`: the targets `velt publish` publishes a prebuilt library for (it fails if one is
  missing): `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `x86_64-apple-darwin`,
  `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc`.
- `wasm`: must be `false` (the default); WebAssembly libraries are not supported yet.

## `registry`

```toml
registry = "https://registry.example.com"   # top level, before [package]
```

A root package's `registry` URL, or `VELT_REGISTRY` set to an `http(s)://` URL (which takes
precedence), replaces the local registry directory for `velt add`, `velt install` and
`velt publish` ([Registries](packages.md#registries)).

## `velt.lock`

`velt install` writes `velt.lock`, which pins the exact version and content hash of every
dependency: `version = 1` and one `[[package]]` entry per package with `name`, `version`,
`source` (`"registry"` or `"path+<relative path>"`), `checksum` and `dependencies`, and for a
package with native code a `[package.native]` table with the checksum of its prebuilt library
for every published target. Commit it for applications. `--locked` on `build`, `run`, `test` and `install` fails instead of changing
it.
