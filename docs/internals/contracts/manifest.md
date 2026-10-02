# package.vlt — CONTRACT (M5)

The package manifest is `package.vlt` at the package root, written in Velt and read without
compiling or running anything (design: [package-manifest.md](../design/package-manifest.md)).

```ts ignore
import type { Package } from "velt:package";

export const pkg: Package = {
  name: "hello",                // [a-z][a-z0-9_-]*
  version: "0.1.0",             // semver, required
  entry: "src/main.vlt",        // optional, the default (relative to package.vlt, inside the package)
  dependencies: {
    json: "1.2",                // registry package, semver requirement
    http: { version: "0.3" },   // object form
    util: { path: "../util" },  // local package (version optional)
  },
  paths: {                      // optional import aliases (additive)
    "@app/*": "src/*",          // import { x } from "@app/util"  →  src/util.vlt (or src/util/index.vlt)
    "@config": "src/config",    // exact alias
  },
  native: {                     // optional: a Rust crate built into a native library (additive)
    path: "native",             // the crate's directory, one name in the package root (default)
    targets: ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"],  // published prebuilt
  },
};
```

`velt:package` (`std/package.vlt`) declares the `Package` type (documentation, and the import
resolves). The reader below is the definition of a valid manifest; its fields are one table,
`vpm::manifest::schema`, which a test checks against `std/package.vlt`. The language server
does not analyze a file named `package.vlt` as a program: its diagnostics are the reader's, and
completion and hover come from the schema (`vpm::manifest::ide`). With the package's registry
(`$VELT_REGISTRY` when it is a URL, else `registry`, else the local one, as `velt install` picks
it) it also completes versions and package names, explains dependencies and checks requirements
against the versions that are not yanked plus the one `velt.lock` pins
(`vpm::manifest::ide::registry`). The data is fetched in the background (at most four fetches at
once, kept five minutes); completion waits up to 400 ms for data on its way, and an unreachable
registry adds nothing.

## The data-only subset (`vpm::manifest::read`)
- At most one import, `import type { … } from "velt:package"` naming at least one type; then
  exactly one `export const pkg: Package = <value>;`, and nothing else.
- Values are string literals, number literals, `true`/`false`, array literals and object literals
  whose keys are identifiers or string literals. Everything else (`null`, template literals,
  names, calls, `new`, operators, `as`, spreads, shorthand properties, regular expressions,
  duplicate keys) is an error at its location. Comments and trailing commas are allowed.
- Unknown keys are errors everywhere (with a "did you mean" note). Missing `name` or `version`
  is an error.
- Limits, for untrusted input (the registry server reads uploaded manifests): at most 64 KiB
  (checked before reading further), the parser's nesting limit, at most 10,000 values.
- Errors are `velt_common::Diagnostic`s rendered as `package.vlt:<line>:<col>: error: …`.
- A directory with a `velt.toml` and no `package.vlt` is still found as a package root, and
  every command reading it fails with "`velt.toml` is no longer read; the manifest is
  `package.vlt`" plus the converted file (`vpm::manifest::legacy`).
- No backward compatibility for published packages: a registry or cache copy, or an archive,
  that has a `velt.toml` cannot be installed or served, and the error says its author must
  publish a new version (`vpm::manifest::legacy::REPUBLISH`).
- `velt manifest --json` prints the validated manifest as JSON in this shape, with the defaults
  filled in (`entry`, `dependencies: {}`, `paths: {}`, and `native.path`/`targets`/`wasm` when
  `native` is present).

## Fields
- `import ... from "json"` / `"json/sub"` resolve through `dependencies`; `"velt:x"` and `"./x"`
  never do.
- `paths`: a bare specifier matching a pattern resolves to the target, relative to the package
  root, before `dependencies` are consulted (`"velt:x"` and `"./x"` are never aliased). A
  pattern has at most one `*`, at its end, and so has its target (both or neither); the longest
  matching pattern wins (like TypeScript's `compilerOptions.paths`, but one target per pattern).
  Targets must stay inside the package (no `..`, not absolute). Each package's aliases apply to
  its own modules only (`vpm::PackageGraph::path_alias`).
- `entry` is a `/`-separated path inside the package (not empty, not absolute, no `..`).
- A library package exposes `src/lib.vlt` (entry for importers); `src/main.vlt` makes it runnable.
- `velt.lock.json` (generated JSON: `{ "version": 1, "packages": [...] }`, pretty-printed, stable
  key order, trailing newline) pins exact versions + content hashes; a package with only the
  former `velt.lock` (TOML) gets an error saying to run `velt install`. The registry for the POC is a
  local directory (`$VELT_REGISTRY`, default `~/.velt/registry/<name>/<version>/`), cache in
  `~/.velt/cache`.

## Remote registries (additive)
```ts ignore
registry: "https://registry.example.com",
```
- A root package's `registry` URL (or `$VELT_REGISTRY` set to an `http(s)://` URL, which takes
  precedence) replaces the local registry directory for resolution, `velt add`, `velt install`
  and `velt publish`. Protocol (`velt registry serve`, crates/vpm/src/remote.rs):
  `GET <url>/api/v1/<name>/index` → `index.json` (`{ "versions": [...] }`, `application/json`; a
  registry directory with an `index.toml` from an older velt is an error); `GET <url>/api/v1/<name>/<version>` → package
  archive; `PUT` the same path with the archive and `X-Velt-Checksum: sha256:…`.
- Users (additive): `<dir>/.auth/users.json` maps user names to the `sha256:` of their tokens.
  While the file exists (even listing no users), every write without `Authorization: Bearer <a
  user's token>` (scheme case-insensitive) is 401; clients send `$VELT_REGISTRY_TOKEN`. Only a
  registry without the file is open: every write is allowed. The file is replaced atomically
  (temporary file + rename); on Unix `.auth/` is mode 0700.
- Owners (additive): `<dir>/<name>/owners.json` (`{ "owners": ["alice"] }`). The first user to publish
  a new package owns it. Writes to a package by a user who doesn't own it are 403, also when an
  existing package has no owners (on a server with users): an administrator assigns them with
  `velt registry owner add`. `GET <url>/api/v1/<name>/owners` → one owner per line; `PUT`/`DELETE
  <url>/api/v1/<name>/owners/<user>` add/remove one (400 for a name or user that isn't
  `[a-z][a-z0-9_-]*`, 404 for an unknown user or a user who isn't an owner, 409 for the last
  owner, 400 on a server without users).
- Yank (additive): index entries gain `yanked = true` (absent when false). `PUT`/`DELETE
  <url>/api/v1/<name>/<version>/yank` set/clear it (owners). Resolution never selects a yanked
  version unless the lockfile pins it; yanked versions stay downloadable.
- Search (additive): `GET <url>/api/v1/search?q=<text>` →
  `{"packages":[{"name":"…","version":"…"}]}`: names containing the text (case-insensitive),
  exact match first, then prefix matches, then by name; at most 50; the version is the newest
  not yanked, and packages with only yanked versions are left out.
- Archives (`vpm::archive`) carry `package.vlt` + `src/**` (+ the `native` crate directory,
  without `target/` and `.git/`); an archive with any other path, such as a `velt.toml`, is
  rejected. Their checksum is the same content hash `velt.lock.json` records, and every download is
  verified against it before it enters the cache.
- `https://` uses rustls (`velt_http::tls`) with Mozilla's roots (`webpki-roots`) plus the PEM
  certificates in `$VELT_CA_FILE`; `http://` is plain TCP. No redirects, proxies (`HTTPS_PROXY`
  is not honored) or HTTP/2. `velt registry serve` is plain HTTP; deployments beyond localhost
  put a TLS-terminating reverse proxy in front of it.
- Yanked but locked (additive): `vpm::Resolution::yanked` / `vpm::Installed::yanked` list the
  selected registry versions that are yanked; the CLI prints `warning: `<name>` <version> is
  yanked (pinned by velt.lock.json)` for each.

## JSX import source (additive)
```ts ignore
jsx: { importSource: "sigx" },  // or "velt:jsx" (the default), an "@alias" from paths, or "./ui"
```
- The JSX runtime of the package's modules ([jsx.md](jsx.md) "Choosing the provider"): a module
  containing JSX imports `<importSource>/jsx-runtime`. A `// @jsxImportSource x` comment in a file
  wins; without either the source is `velt:jsx`.
- The value is a module specifier: a dependency (`"sigx"`), `"velt:x"`, a `paths` alias, or a
  path starting with `./` / `../`, which is relative to the package root (not to the importing
  file, unlike a pragma). Each package's `jsx` applies to its own modules only
  (`vpm::PackageGraph::jsx_import_source`).

## Native libraries (additive)
Contract: [native_abi.md](native_abi.md).
- `native`: `path` (default `"native"`, a directory name in the package root, not `src` or
  `target`), `targets` (triples from `vpm::manifest::NATIVE_TARGETS`: x86_64/aarch64 Linux GNU,
  macOS and Windows MSVC), `wasm` (must be `false`: not supported yet).
- Index entries gain `"native_abi": <n>` and `"native": { "<triple>": "sha256:…" }`. A published
  target is never replaced; a target may be **added** to a published version
  (`velt publish --native-only`). `velt publish` requires a bundle for every listed target.
- Local registry: `<registry>/<name>/<version>.native/<triple>/`. Remote:
  `GET`/`PUT <url>/api/v1/<name>/<version>/native/<triple>` (bundle archive, same checksum header
  and token); `PUT` answers 409 for a target already published, 404 for an unpublished version.
  The bundle's metadata is `native.json` (generated JSON); a bundle with the former `native.toml`
  is refused with an error saying to rebuild it.
- `velt.lock.json` entries of registry packages gain a `"native"` object: target triple →
  bundle checksum, for **every** published target (the lock is the same on every OS). Under
  `--locked`, a locked version keeps exactly its locked targets; a changed checksum of a locked
  target is an error.
- Cache: `<cache>/native/<name>-<version>/<triple>/` (verified prebuilt bundles) and
  `<triple>-source/` (built from source); `<cache>` is vpm's package cache (`$VELT_HOME/cache`).
