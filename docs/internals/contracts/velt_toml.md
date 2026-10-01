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
  (`$VELT_REGISTRY`, default `~/.vlt/registry/<name>/<version>/`), cache in `~/.vlt/cache`.

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
- Archives (`vpm::archive`) carry `velt.toml` + `src/**`; their checksum is the same content hash
  `velt.lock` records, and every download is verified against it before it enters the cache.
- `https://` goes through the system `curl`; `http://` is built in.
