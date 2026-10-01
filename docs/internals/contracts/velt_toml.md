# velt.toml — CONTRACT (M5)

```toml
[package]
name = "hello"            # [a-z][a-z0-9_-]*
version = "0.1.0"         # semver, required
entry = "src/main.vlt"   # optional, default "src/main.vlt" (relative to velt.toml)

[dependencies]
json = "1.2"                   # registry package, semver requirement
http = { version = "0.3" }     # table form
util = { path = "../util" }    # local package (version optional)

[paths]                        # optional import aliases (additive)
"@app/*" = "src/*"             # import { x } from "@app/util"  →  src/util.vlt (or src/util/index.vlt)
"@config" = "src/config"       # exact alias

[native]                       # optional: a Rust crate built into a native library (additive)
path = "native"                # the crate's directory, one name in the package root (default)
targets = ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]  # published prebuilt
```
- `import ... from "json"` / `"json/sub"` resolve through `[dependencies]`; `"std/x"` and `"./x"` never do.
- `[paths]`: a bare specifier matching a pattern resolves to the target, relative to the package
  root, before `[dependencies]` are consulted (`"std/x"` and `"./x"` are never aliased). A
  pattern has at most one `*`, at its end, and so has its target (both or neither); the longest
  matching pattern wins (like TypeScript's `compilerOptions.paths`, but one target per pattern).
  Targets must stay inside the package (no `..`, not absolute). Each package's aliases apply to
  its own modules only (`vpm::PackageGraph::path_alias`).
- A library package exposes `src/lib.vlt` (entry for importers); `src/main.vlt` makes it runnable.
- `velt.lock` pins exact versions + content hashes; registry for the POC is a local directory
  (`$VELT_REGISTRY`, default `~/.velt/registry/<name>/<version>/`), cache in `~/.velt/cache`.

## Remote registries (additive)
```toml
registry = "https://registry.example.com"   # top level, before [package]
```
- A root package's `registry` URL (or `$VELT_REGISTRY` set to an `http(s)://` URL, which takes
  precedence) replaces the local registry directory for resolution, `velt add`, `velt install`
  and `velt publish`. Protocol (`velt registry serve`, crates/vpm/src/remote.rs):
  `GET <url>/api/v1/<name>/index` → `index.toml`; `GET <url>/api/v1/<name>/<version>` → package
  archive; `PUT` the same path with the archive, `X-Velt-Checksum: sha256:…` and, when the server
  has a token, `Authorization: Bearer $VELT_REGISTRY_TOKEN`.
- Archives (`vpm::archive`) carry `velt.toml` + `src/**` (+ the `[native]` crate directory,
  without `target/` and `.git/`); their checksum is the same content hash `velt.lock` records,
  and every download is verified against it before it enters the cache.
- `https://` goes through the system `curl`; `http://` is built in.

## JSX import source (additive)
```toml
[jsx]
importSource = "sigx"    # or "velt:jsx" (the default), an "@alias" from [paths], or "./ui"
```
- The JSX runtime of the package's modules ([jsx.md](jsx.md) "Choosing the provider"): a module
  containing JSX imports `<importSource>/jsx-runtime`. A `// @jsxImportSource x` comment in a file
  wins; without either the source is `velt:jsx`.
- The value is a module specifier: a dependency (`"sigx"`), `"std/x"`, a `[paths]` alias, or a
  path starting with `./` / `../`, which is relative to the package root (not to the importing
  file, unlike a pragma). Each package's `[jsx]` applies to its own modules only
  (`vpm::PackageGraph::jsx_import_source`). Unknown keys in `[jsx]` are errors.

## Native libraries (additive)
Contract: [native_abi.md](native_abi.md).
- `[native]`: `path` (default `"native"`, a directory name in the package root, not `src` or
  `target`), `targets` (triples from `vpm::manifest::NATIVE_TARGETS`: x86_64/aarch64 Linux GNU,
  macOS and Windows MSVC), `wasm` (must be `false`: not supported yet). Unknown keys are errors.
- Index entries gain `native_abi = <n>` and `native = { "<triple>" = "sha256:…" }`. A published
  target is never replaced; a target may be **added** to a published version
  (`velt publish --native-only`). `velt publish` requires a bundle for every listed target.
- Local registry: `<registry>/<name>/<version>.native/<triple>/`. Remote:
  `GET`/`PUT <url>/api/v1/<name>/<version>/native/<triple>` (bundle archive, same checksum header
  and token); `PUT` answers 409 for a target already published, 404 for an unpublished version.
- `velt.lock` entries of registry packages gain a `[package.native]` table: target triple →
  bundle checksum, for **every** published target (the lock is the same on every OS). Under
  `--locked`, a locked version keeps exactly its locked targets; a changed checksum of a locked
  target is an error.
- Cache: `<cache>/native/<name>-<version>/<triple>/` (verified prebuilt bundles) and
  `<triple>-source/` (built from source); `<cache>` is vpm's package cache (`$VELT_HOME/cache`).
