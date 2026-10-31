# package.vlt — CONTRACT (M5)

The package manifest is `package.vlt` at the package root, written in Velt and read without
compiling or running anything (design: [package-manifest.md](../design/package-manifest.md)).

```ts ignore
import type { Package } from "velt/package";

export const pkg: Package = {
  name: "hello",                // [a-z][a-z0-9]* words joined by single - or _; not std, rt…, sig…, native…
  version: "0.1.0",             // semver, required
  velt: "0.1",                  // optional: the toolchain versions that build it (additive)
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
  tsCompat: ["src/models"],     // optional: folders kept TypeScript-compatible (additive)
};
```

`velt/package` (`std/package/index.vlt`) declares the `Package` type (documentation, and the import
resolves). The reader below is the definition of a valid manifest; its fields are one table,
`vpm::manifest::schema`, which a test checks against `std/package/index.vlt`. The language server
does not analyze a file named `package.vlt` as a program: its diagnostics are the reader's, and
completion and hover come from the schema (`vpm::manifest::ide`). With the package's registry
(`$VELT_REGISTRY` when it is a URL, else `registry`, else the local one, as `velt install` picks
it) it also completes versions and package names, explains dependencies and checks requirements
against the versions that are not yanked plus the one `velt.lock.json` pins
(`vpm::manifest::ide::registry`). The data is fetched in the background (at most four fetches at
once, each within 5 s to connect and 15 s in all, kept five minutes); completion waits up to
400 ms for data on its way, and an unreachable registry adds nothing. A `registry` that is plain
`http://` to another machine is not asked from the editor (opening a checkout must not make it
talk to hosts the checkout names); `$VELT_REGISTRY` is always asked.

## The data-only subset (`vpm::manifest::read`)
- At most one import, `import type { … } from "velt/package"` naming at least one type; then
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

## Scoped names (additive, #929)
- `name` and the keys of `dependencies` may be scoped, `@scope/name`, each part lowercase
  letters and digits (starting with either) joined by single `-` or `_`, not a Windows device
  name (`vpm::manifest::is_valid_dependency_name`); `is_valid_package_name` (registry names)
  stays unscoped. A scoped dependency needs a `path` ("dependency `@x/y` needs a `path`:
  registries hold unscoped packages only"); a scoped package can't have `native`.
  `"@scope/name/sub"` imports the dependency's `src/sub`.

## `define` (additive, #929)
- `define: Record<string, string>`: name (`[A-Za-z_$][A-Za-z0-9_$]*`) → the source of a literal
  (`vpm::manifest::define_literal`: `true`, `false`, `null`, a non-negative number, a string in
  double quotes (JSON) or in single quotes without escapes). Only the root package's apply
  (`vpm::PackageGraph::defines`). The loader parses each non-std module with them
  (`velt_syntax::parse_with`, `ParseOptions::defines`): an identifier expression (or
  shorthand property) of a defined name that nothing in its scope declares
  (`velt_syntax::scope::free_values`) is the literal.
- Ambient declarations: `declare const|let|var NAME: T;` (also `export declare …`) and
  `declare global { [declare] var NAME: T; … }` are returned by `parse_with` in
  `Parsed::ambient`, not as items (`parse_file` keeps reporting them as errors, so the formatter
  never drops them). `declare global { … }` is parsed once, as an `ItemKind::Augment` holding
  its interfaces and namespaces of types (`ItemKind::Namespace`, `ast/blocks.rs`; sema declares
  such a namespace in its module, as a `declare namespace`); only its variables go to
  `Parsed::ambient`, and any other declaration in it is an error. `Ambient::global` marks
  those in `declare global`, and `Parsed::is_module` a source with a top-level `import` or
  `export`: the others of such a module are its own, and those of a declaration file that is a
  module are not read. The loader also reads them
  from the `.d.ts` files of each package it loads a module of (the nearest directory with a
  `package.json` or `package.vlt`: its own `.d.ts` files and those below `src/`, skipping
  `node_modules`, `target`, `dist`, hidden and symlinked directories and nested packages),
  ignoring those files' syntax errors. A name a module refers to (`free_values`; any module for
  a global declaration, its own module otherwise) without a `define` is an error at its first
  declaration ("`X` is declared here but has no value"); so is a `define`
  whose literal is not of a declared `boolean`, `string` or `number`.

## npm packages (additive, #929)
`vpm::npm::NpmResolver`. The loader asks it about a bare specifier (one `resolve_spec`
classifies as a package, or rejects, other than `std…` and `velt:…`) that no `paths` alias
matches and that is not a Velt dependency of the importing package. Its answer is a file,
loaded like a relative import's (the importer's origin). Order: `node_modules/<name>/package.json`
in the importer's directory and each ancestor (skipping `node_modules` directories
themselves; a symlinked package directory is replaced by its real path), the nearest ancestor whose `package.json` has that `name`, then the first ancestor
that is a workspace root (`pnpm-workspace.yaml` `packages:`, or `package.json` `workspaces`;
globs with `*`, `**` and `!`; `**` does not enter symlinks; at most
`vpm::npm::workspace::MAX_DIRS` directories are listed, and when a walk stops there, an unresolved import's error gets `NpmResolver::workspace_note`). Entry: `exports` (Node's PACKAGE_EXPORTS_RESOLVE: the exact
subpath, then the `*` pattern with the longest prefix; conditions in written order under
`vpm::npm::SERVER_CONDITIONS` plus `default`: the targets of `types` and `velt` are hints, and
besides them only the first other matching condition's target is kept, as Node takes it
(`exports::Targets::runtime`, named by the no-source error); targets are tried `velt` hints
first, then the runtime target, then `types` hints; `null` under a matching condition
is not exported), else `types`, `typings`, `main`, `./index`;
without `exports` a subpath names that path. Each target maps to source candidates
(`vpm::npm::source_candidates`) and the first existing file wins; when none exists, the root
subpath (`.`, never another) falls back to `src/index.ts`, then `src/index.tsx`, unless a
target of `.` under any condition (`exports::root_targets_any_condition`) has a source. Errors: a subpath not
exported, an `exports` mixing subpaths and conditions, a target without a source.

## Workspaces (additive, #1855)
`vpm::workspace` (docs/internals/design/workspaces.md). `velt.workspace.vlt` (`WORKSPACE_FILE`) at
a workspace root is read by the manifest reader (`manifest::read::read_workspace`: the same data
subset and bounds; one `export const workspace: Workspace = { … }` with `members` (required,
non-empty `/`-separated relative globs: `*`, `**` as a whole segment, `!` to exclude, `"."` for
the root; no `..`, `./`, `\`, duplicates), `velt`, `registry`, `dependencies` (no `"workspace"`
values; no key naming a member)). `Workspace::find` takes the nearest workspace file at or above a
directory; `Workspace::load` lists members with `npm::workspace::dirs_with` (marker
`package.vlt`, never entering `target`; literal segments match entry names exactly, as `*` ones do,
on every OS; each positive glob must list a package) and checks: distinct member names; no `velt`
or `registry` in a member; a dependency named like a member is `"workspace"`; every
`"workspace"` names a member or a workspace dependency; no cycle of `"workspace"` dependencies
between members. Each error is located (`file:line:col: error: …`, via `manifest::ide::locate`
and the globs' spans, `WorkspaceFile::member_spans`). Membership of a package below a workspace
file is decided by its globs alone (`velt_toolchain::members::lists`, also the launcher's): a
package they don't list installs on its own (`Installed::notes` says so) and keeps its own `velt`. `Dependency::Version("workspace")`
(`manifest::WORKSPACE_REQUIREMENT`, `Dependency::is_workspace`) is valid in any `package.vlt`;
resolving it outside a workspace is an error. `Workspace::effective_manifest` replaces it with the
member's directory (an absolute `path`) or the workspace's requirement (a `path` made absolute),
and sets the workspace's `velt` and `registry`. `vpm::install` of a member (and
`install::workspace::install` of the whole workspace) resolves all members together
(`resolve::resolve_members`: every member is a path requirement of the root; one version per
name), refuses a member's own `velt.lock.json`, writes the lockfile at the workspace root (members
as `"source": "workspace+<dir>"`, `lockfile::WORKSPACE_SOURCE`; path sources relative to the
workspace root), and returns the member's graph: the member first, then the packages it uses
transitively. A package below a workspace file that no glob lists installs on its own. The
launcher's pin (`velt_toolchain::pin::find_pin`) is the nearer package's `velt`, else its
workspace file's, with the lock beside the file that holds it. A build without `-o` writes to
`<package of the input>/target/velt/` (relative when that is the current directory); its stamp
records the workspace file, the globs' listings, the members' manifests and the root lockfile.

## Git dependencies (additive, #1860)
`vpm::git`. `DetailedDependency` gains `git`, `branch`, `tag`, `rev` (`Dependency::git` gives the
`GitSpec`); `git::check_fields`: `branch`/`tag`/`rev` need `git` and at most one is given, `rev`
is a full lowercase commit hash (`git::is_commit`, 40 or 64 digits), `git` passes
`git::check_url` (`https`, `http`, `ssh`, `git`, `file` URLs or `user@host:path`; no leading `-`,
whitespace, `?` or `#`; no password, and no user at all in `http(s)`), and with `git` a `path` is
`/`-separated plain directory names (no `..`, root, drive or `:`). A git package's own `path`
dependencies become `Want::Git` of the same URL at `GitRef::Rev(<commit>)` with their directory
relative to the checkout, and must stay inside it (lexically and through symbolic links); so must
the package's directory. A checked-out commit with a symbolic link that resolves outside the
checkout (followed through the checkout's own links, at most 40, its missing parts taken as
written; `git::check_links`) is refused before it enters the cache. The scratch repository is
made with `--template=`, and the checkout runs with `core.hooksPath` at an empty directory. git
runs without `git rev-parse --local-env-vars`'s variables and never prompts; the checkout runs with `GIT_CONFIG_NOSYSTEM=1`, an empty global configuration,
`core.autocrlf=false`, `core.eol=lf`, `core.longpaths=true`, and `core.symlinks=true`
(`false` on Windows: links are files holding the stored text, which hash as a link's text does
elsewhere; Git for Windows would write link targets with `\`). The commit's links (`git ls-tree`
mode `120000`) are listed in `<commit>.links` beside the checkout, outside the hashed tree; on
Windows, resolving a git package warns about each link inside it (`git::link_warnings`, printed
with the yanked versions by `velt install` and builds). Lock query
values percent-encode `%`, `&`, `=`, `#` and `?`. Resolution (`resolve::Want::Git`,
`Source::Git { spec, commit, checksum, dir }`) takes the commit the lockfile records for an equal
spec, else `git ls-remote` of the ref (an annotated tag's `^{}` commit), and checks it out with
`git::checkout` into `<cache>/git/<sha256(url)[..10] hex>/<commit>/` (a shallow fetch of the
commit into a scratch bare repository, checked out into a staging directory, renamed into place;
`GIT_TERMINAL_PROMPT=0`). The lock's `source` is `git+<url>[?branch=|tag=|rev=…][&path=…]#<commit>`
(`GitSpec::source`/`parse_source`), and its `checksum` is `git::tree_checksum` of the whole
checkout (every file, path order, `contents::checksum`'s encoding; symbolic links by target),
verified on every use. Native code of a git package builds only with `VELT_NATIVE_FROM_SOURCE=1`,
from a copy of the package in `<cache>/git-native/<name>/<commit>/`. `registry::publish` refuses a dependency with only
`git` (a `version` beside it is published as the requirement). A target without `package.vlt` is
an npm source (`npm::provided::packages_at`): the `package.json` there, plus a workspace root's
packages (its globs, each a `package.json`; one without a `name` is named after its directory, as
npm's `map-workspaces` does: `@scope/name` under a scope directory); a `package.json` that can't
be read or doesn't parse and a duplicate name are errors (paths relative to the target), and a
walk cut at `MAX_DIRS` is named when a package isn't found; the dependency's name must be one
of them (else "provides no npm package named …"), its version is that package's (`0.0.0` without
a valid one), and it has no dependencies of its own. It is `Source::Git` with `npm` (name →
directory), locked like any git package, and no package of the graph (two git dependencies providing one npm name from different
directories are an error, `install::check_npm_overlap`; from one checkout they agree): each
dependent's
`GraphPackage::npm` gets the packages, and the loader resolves a bare import of one in its
directory (`NpmResolver::resolve_in`) before Node's search.

## Fields
- `import ... from "json"` / `"json/sub"` resolve through `dependencies`; `"velt/x"` and `"./x"`
  never do.
- `paths`: a bare specifier matching a pattern resolves to the target, relative to the package
  root, before `dependencies` are consulted (`"velt/x"` and `"./x"` are never aliased). A
  pattern has at most one `*`, at its end, and so has its target (both or neither); the longest
  matching pattern wins (like TypeScript's `compilerOptions.paths`, but one target per pattern).
  Targets must stay inside the package (no `..`, not absolute). Each package's aliases apply to
  its own modules only (`vpm::PackageGraph::path_alias`).
- `entry` is a `/`-separated path inside the package (not empty, not absolute, no `..`).
- `description` (additive, #184): one line of at most 300 characters (Unicode scalar values), no
  control characters, no leading or trailing whitespace, not empty. `keywords`: at most 10
  distinct entries, each `[a-z0-9][a-z0-9-]{0,31}`, order kept, not empty. Both optional; an empty
  string or array is an error saying to remove the field. `to_vlt` writes them after `version`,
  and `velt manifest --json` prints them when present.
- A library package exposes `src/lib.vlt` (entry for importers); `src/main.vlt` makes it runnable.
- `velt.lock.json` (generated JSON: `{ "version": 1, "velt": "<version>", "packages": [...] }`,
  pretty-printed, stable key order, trailing newline) pins exact versions + content hashes, and
  (additive, #960; optional, so still format 1) the toolchain version that builds the package:
  for a package with a `velt` requirement only (none otherwise, and a recorded one is dropped):
  `vpm::install` writes the running version into a new lock, keeps the recorded one unless
  `update` runs or the requirement no longer accepts it (then it writes the running one), and
  under `--locked` neither adds nor changes it and fails when they differ: "`velt.lock.json` pins
  velt <locked>, but this is velt <running>, and --locked was passed: …", or, when the
  requirement no longer accepts the locked version, "… which the package's velt pin "<req>" no
  longer accepts, and --locked was passed: run `velt install` (or `velt update`) without
  --locked to record velt <running>". A `velt` that is not a semver version is an error when the
  lock is read; a package with only the
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
  user's token>` (scheme case-insensitive) is 401; clients send the token `velt login` stored
  for that registry, or `$VELT_REGISTRY_TOKEN` (`cli.md`), only to an `https://` registry or an
  `http://` one on a loopback host (`localhost`, `127.0.0.0/8`, `[::1]`), and fail otherwise.
  Only a registry without the file is open: every write is allowed. The registry's files are
  replaced atomically (temporary file, synced, then renamed); on Unix `.auth/` is mode 0700.
  Server writes and the administrator's `velt registry user|owner` commands hold an advisory lock
  on `<dir>/.lock`. Removing a user removes it from every `owners.json`. Package and user names
  are never Windows device names (`con`, `prn`, `aux`, `nul`, `com0`–`com9`, `lpt0`–`lpt9`).
- Owners (additive): `<dir>/<name>/owners.json` (`{ "owners": ["alice"] }`). The first user to publish
  a new package owns it (recorded before the version is stored, and taken back if storing fails).
  Writes to a package by a user who doesn't own it are 403, also when an existing package has no
  owners (on a server with users): an administrator assigns them with
  `velt registry owner add`. `GET <url>/api/v1/<name>/owners` → one owner per line; `PUT`/`DELETE
  <url>/api/v1/<name>/owners/<user>` add/remove one (400 for an invalid package name, or a user
  name that isn't `[a-z][a-z0-9_-]*`, 404 for an unknown user or a user who isn't an owner, 409 for the last
  owner, 400 on a server without users).
- Yank (additive): index entries gain `yanked = true` (absent when false). `PUT`/`DELETE
  <url>/api/v1/<name>/<version>/yank` set/clear it (owners). Resolution never selects a yanked
  version unless the lockfile pins it; yanked versions stay downloadable.
- Search (additive): `GET <url>/api/v1/search?q=<text>` →
  `{"packages":[{"name":"…","version":"…","description"?:"…","keywords"?:["…"]}]}`. The text is
  lowercased and split on whitespace into terms; a package matches when every term is a
  substring of its name, one of its keywords or its description. Ranked by the best tier any
  term reaches (0 the name equals the whole text, 1 the name starts with it, 2 a keyword equals a
  term, 3 the name contains a term, 4 a keyword starts with a term, 5 the description contains a
  term), then by how many terms reach that tier, then by name; at most 50. The version is the one
  `velt add` picks (newest stable not yanked, else newest pre-release not yanked), with that
  version's `description` and `keywords`; packages with only yanked versions are left out.
- Index entries (additive, #184) carry the version's `"description"` and `"keywords"` after
  `"checksum"` (omitted when absent), taken from the published `package.vlt`, also by the server
  from the uploaded archive (never from request headers).
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
jsx: { importSource: "sigx" },  // or "velt/jsx" (the default), an "@alias" from paths, or "./ui"
```
- The JSX runtime of the package's modules ([jsx.md](jsx.md) "Choosing the provider"): a module
  containing JSX imports `<importSource>/jsx-runtime`. A `/** @jsxImportSource x */` comment in a
  file (or `// @jsxImportSource x`, which `tsc` ignores) wins; without either the source is `velt/jsx`.
- The value is a module specifier: a dependency (`"sigx"`), `"velt/x"`, a `paths` alias, or a
  path starting with `./` / `../`, which is relative to the package root (not to the importing
  file, unlike a pragma). Each package's `jsx` applies to its own modules only
  (`vpm::PackageGraph::jsx_import_source`).

## TypeScript-compatible folders (additive)
```ts ignore
tsCompat: ["src/components", "src/models"],
```
- `vpm::Manifest::ts_compat` (`Vec<String>`, in the order written; empty when absent): folders
  whose modules `velt check --ts-compat` without paths and the language server lint
  ([cli.md](cli.md), docs/internals/design/tsx.md). A folder's modules are its source files as
  `vpm::sources::walks_into` walks it (no `target/`, `node_modules/`, hidden, symlinked or
  nested package directories); `vpm::sources::in_folder` decides membership the same way for one
  file.
- The reader's errors, at the value: not an array; an empty array ("remove the field
  instead"); an entry that is not a string; an entry that is not a `/`-separated relative path
  of non-empty parts other than `.` and `..` without `\` or `:` (so no leading or trailing
  `/`); an entry equal to, inside or containing an earlier one, compared ignoring case (an
  overlap on a case-insensitive file system is one on every OS).
- The reader doesn't look at the disk. `vpm::manifest::read::missing_ts_compat_dirs(file, src,
  root)` returns a warning, at its string, for each entry that is not a directory under `root`
  (the language server shows them in `package.vlt`); `velt check --ts-compat` fails on one.
- `to_vlt` writes `tsCompat` last; `velt manifest --json` prints it when present.

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

## Toolchain requirement (additive, #948)
```ts ignore
velt: "0.1",
```
- `velt` is a `velt_toolchain::Requirement`: a version without an operator (`X`, `X.Y`,
  `X.Y.Z`, optionally with a pre-release) means `~` that version (the newest patch of its minor;
  `"1"` is any `1.x`); any other text is a `semver::VersionReq` (`=0.1.3`, `>=0.1, <0.3`, `^1.2`).
  A value that is neither is an error at the value: "velt `<text>` is not a version requirement"
  (an empty or blank one: "velt the version requirement is empty …").
- `velt new` / `velt init` write `velt: "<major>.<minor>"` of the toolchain that runs them, or
  its whole `<major>.<minor>.<patch>-<pre>` when it is a pre-release
  (`velt_toolchain::requirement::pin_for`, `vpm::scaffold::this_toolchain_pin`): the pin always
  accepts the toolchain that wrote it. `to_vlt` writes the field after `version`, and
  `velt manifest --json` prints it as `"velt"` when present.
- Every command that reads the package (`Project::open`, `velt manifest`) warns on stderr,
  `package.vlt asks for velt "<req>", but this is velt <version>; it may not build the package`,
  when the requirement doesn't accept the running velt's version and `$VELT_LAUNCHER` is unset.
- The reader only checks the syntax. Choosing the toolchain is the launcher's job (#948), which
  reads this one field with `velt_toolchain::pin::find_pin` (the nearest `package.vlt` above the
  working directory; a manifest that does not parse counts as no pin, and the toolchain then
  reports it).
