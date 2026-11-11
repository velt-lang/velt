# package.velt — CONTRACT (M5)

The package manifest is `package.velt` at the package root, written in Velt and read without
compiling or running anything (design: [package-manifest.md](../design/package-manifest.md)).

```ts ignore
import type { Package } from "velt/package";

export const pkg: Package = {
  name: "hello",                // [a-z][a-z0-9]* words joined by single - or _; not std, rt…, sig…, native…
  version: "0.1.0",             // semver, required
  velt: "0.1",                  // optional: the toolchain versions that build it (additive)
  entry: "src/main.velt",        // optional, the default (relative to package.velt, inside the package)
  dependencies: {
    json: "1.2",                // registry package, semver requirement
    http: { version: "0.3" },   // object form
    util: { path: "../util" },  // local package (version optional)
  },
  paths: {                      // optional import aliases (additive)
    "@app/*": "src/*",          // import { x } from "@app/util"  →  src/util.velt (or src/util/index.velt)
    "@config": "src/config",    // exact alias
  },
  native: {                     // optional: a Rust crate, or C libraries (`c`), built into a native library (additive)
    path: "native",             // the crate's directory, one name in the package root (default)
    targets: ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"],  // published prebuilt
  },
  tsCompat: ["src/models"],     // optional: folders kept TypeScript-compatible (additive)
  lint: { perf: "warn" },       // optional: how loud the lint rules are (additive)
  imports: {                    // optional conditional imports (additive)
    "#io": { js: "./src/io/js.velt", default: "./src/io/native.velt" },
  },
  hostModules: { kv: "./src/kv.mjs" },  // optional: JS of `declare host("kv")` (additive)
  host: {                       // optional: a host package's platform (additive)
    target: "wasm32-unknown-unknown",
    condition: "edge",
    entry: "./src/entry.mjs",
    scaffold: { "edge.toml": "./src/edge.toml" },
  },
};
```

`velt/package` (`std/package/manifest.velt` and `workspace.velt`, re-exported by its index.velt) declares the `Package` type (documentation, and the import
resolves). The reader below is the definition of a valid manifest; its fields are one table,
`vpm::manifest::schema`, which a test checks against `std/package/index.velt`. The language server
does not analyze a file named `package.velt` as a program: its diagnostics are the reader's, and
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
- Errors are `velt_common::Diagnostic`s rendered as `package.velt:<line>:<col>: error: …`.
- A directory with a `velt.toml` and no `package.velt` is still found as a package root, and
  every command reading it fails with "`velt.toml` is no longer read; the manifest is
  `package.velt`" plus the converted file (`vpm::manifest::legacy`).
- No backward compatibility for published packages: a registry or cache copy, or an archive,
  that has a `velt.toml` cannot be installed or served, and the error says its author must
  publish a new version (`vpm::manifest::legacy::REPUBLISH`).
- `velt manifest --json` prints the validated manifest as JSON in this shape, with the defaults
  filled in (`entry`, `dependencies: {}`, `paths: {}`, and `native.path`/`targets`/`wasm` when
  `native` is present).

## Scoped names (additive, #929)
- `name` and the keys of `dependencies` may be scoped, `@scope/name`, each part lowercase
  letters and digits (starting with either) joined by single `-` or `_`, not a Windows device
  name (`vpm::manifest::is_valid_dependency_name`); `is_valid_package_name` stays unscoped. A
  registry holds scoped packages too (the first publisher of one claims its scope, #1740), so a
  scoped dependency may be a `version`; a scoped package can't have `native`.
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
  `package.json` or `package.velt`: its own `.d.ts` files and those below `src/`, skipping
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
`vpm::workspace` (docs/internals/design/workspaces.md). `velt.workspace.velt` (`WORKSPACE_FILE`) at
a workspace root is read by the manifest reader (`manifest::read::read_workspace`: the same data
subset and bounds; one `export const workspace: Workspace = { … }` with `members` (required,
non-empty `/`-separated relative globs: `*`, `**` as a whole segment, `!` to exclude, `"."` for
the root; no `..`, `./`, `\`, duplicates), `velt`, `registry`, `dependencies` (no `"workspace"`
values; no key naming a member)). `Workspace::find` takes the nearest workspace file at or above a
directory; `Workspace::load` lists members with `npm::workspace::dirs_with` (marker
`package.velt`, never entering `target`; literal segments match entry names exactly, as `*` ones do,
on every OS; each positive glob must list a package) and checks: distinct member names; no `velt`
or `registry` in a member; a dependency named like a member is `"workspace"`; every
`"workspace"` names a member or a workspace dependency; no cycle of `"workspace"` dependencies
between members. Each error is located (`file:line:col: error: …`, via `manifest::ide::locate`
and the globs' spans, `WorkspaceFile::member_spans`). Membership of a package below a workspace
file is decided by its globs alone (`velt_toolchain::members::lists`, also the launcher's): a
package they don't list installs on its own (`Installed::notes` says so) and keeps its own `velt`. `Dependency::Version("workspace")`
(`manifest::WORKSPACE_REQUIREMENT`, `Dependency::is_workspace`) is valid in any `package.velt`;
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
checked on every use as "Fetched trees" below says. Native code of a git package builds only with `VELT_NATIVE_FROM_SOURCE=1`,
from a copy of the package in `<cache>/git-native/<name>/<commit>/`. `registry::publish` refuses a dependency with only
`git` (a `version` beside it is published as the requirement). A target without `package.velt` is
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

## Archive dependencies (additive, #2367)
`vpm::url_source`. `DetailedDependency` gains `url`, `sha256` and `strip` (a whole number, 0-16;
the reader's `ValueKind::Num` keeps an unsuffixed integer's value); `Dependency::url_spec` gives
the `UrlSpec { url, sha256, strip, subdir }` (`subdir` is `path`). `url_source::check_fields`:
`sha256` and `strip` need `url`; `url` needs `sha256` (64 lowercase hex digits), not with `git`,
`branch`, `tag` or `rev`; `path` is a `/`-separated directory inside the archive; `url` passes
`url_source::check_url`: a lowercase scheme, `source_url::check_downloadable` (what an
`allowNative` prefix comparison would refuse) and `https`, or plain `http` to this machine, which
only a manifest the root controls may name (`dependency_requirements` refuses it in a fetched
one). The resolver's `Want::Url`/`Source::Url { spec, checksum, dir }`: `url_source::checkout`
downloads the archive (`url_source::fetch`: each redirect hop's scheme, host, credentials and
path checked as the first URL, not its query, which is the server's, such as a storage service's
signed one; plain `http` only after `http`; a `Location` absolute only when it starts with a
scheme, otherwise resolved; no fragment sent; at most `MAX_ARCHIVE` = 512 MiB read) unless
`<cache>/url/<sha256>/` holds it for this URL, checks the SHA-256 of the bytes, extracts it there
with `vpm::extract` (`extract::write_then`), and
records in `<sha256>.verified` the raw tree's checksum, each URL whose bytes hashed to it, and its
links, before the tree is renamed into place, so a raw tree never lacks its record. One found
without (from before records, or the record removed) is replaced only once this URL's bytes are
downloaded, checked and extracted (`extract::Place::Replace`: the old tree moved aside in one
rename). A cache hit from a URL not recorded downloads and checks once (the raw tree is keyed by
hash alone, and `allowNative` matches the URL as written); without a lock, a cached raw tree is
read in full against the recorded checksum, so a changed tree never reaches a new lock. All but a
hit for this URL run under the archive's lock (`<sha256>.lock`, `url_source::lock_raw`) and look
at the cache again once they hold it: a concurrent install waits for the first one's tree, and no
recorded URL is lost. On a filesystem without locks they run anyway: two installs that meet there
fail with "already exists", or one URL goes unrecorded and costs one more download. Staging directories a killed extraction left beside the tree
(`.<name>.partial-…`) are removed by the next extraction there once a week old. The extractor
refuses `.git` in any spelling, a directory git would take for a repository by its contents
(`HEAD` with `objects` and `refs`, or with `commondir`) under any name, a name over 255 bytes,
Windows short names (`A~B~CD~1` too) and device names (`CLOCK$` too), names that differ only by
case, normalisation or a code point HFS+ ignores (`extract::names::clash_key`), and a gzip stream
of more than 1,024 members holding under 4 KiB each on average (`extract::tar::Members`). A query
parameter named like a credential (`token`, `sig`, `X-Amz-Signature`, …) gets a warning
(`url_source::credential_warning`), once, when the lock first records the URL: it is written to
the manifest and the lock. Then it goes
`strip` levels down (each the only entry at its level) and into `path`, which must stay inside;
`git::check_links` checks that directory's links again from it. The lock's `source` is
`url+<url>[?strip=n][&path=…]#sha256:<hex>` (`UrlSpec::source`/`parse_source`, `&=#?%` escaped),
its `checksum` the chosen directory's `git::tree_checksum`, checked as "Fetched trees" below
says. A url package's
`path` dependencies are other directories of the same archive (`Want::Url` with the same hash),
never outside it. Native code builds only with `VELT_NATIVE_FROM_SOURCE=1`, from a copy in
`<cache>/url-native/<name>/<sha256>-<strip>[-<path>]/`. `registry::publish` refuses a dependency
with only `url`. A tree without `package.velt` is an error (no npm sources from archives yet).
`velt add <url> [--path <dir>]` (`url_source::entry_for`) downloads once and writes `{ url, sha256,
strip?, path? }` under the package's own name, refusing to replace an entry of that name from
another source or hash (`url_source::check_replace`), and prints the install's warnings. Any
`<scheme>://` argument is a URL: `http`/`https` in another case reaches `check_url`'s "lowercase"
error, another scheme is refused by the CLI, never looked up in the registry.

**Fetched trees** (git checkouts, archives' package directories) are checked against the lock's
checksum through a stamp in `<cache>/stamps/` (`vpm::tree_stamp`): the checksum with fingerprints
of the tree's metadata, taken before it is read, and not written for a tree changed within 3 s
of now. `InstallOptions::tree_check` says how much is looked at:
- `TreeCheck::Files` (`velt install`, `update`, `add`, the default): every entry's name, type,
  size, mtime and, on Unix, ctime; the tree is read in full when they differ.
- `TreeCheck::Directories` (a build, `Project::open`): only the directories' times, so a file
  added, removed or renamed is seen but an edit in place is not. Whenever the program is loaded
  again (a build that is no longer a no-op, `velt check`), the driver runs
  `PackageGraph::verify_fetched` (every file) first.
- `TreeCheck::Bytes` (`velt install --locked`): every byte, whatever a stamp says.

A changed tree's error names what to delete to fetch it again: the git checkout, or for an
archive its raw tree `<cache>/url/<sha256>/` (`tree_stamp::Fetched::fetched_at`), never only the
package directory inside it, which the record would still vouch for.

## One source per package, and `allowNative` by source (additive, #2367)
`vpm::resolve`, `vpm::install::allow_native`, `vpm::manifest::allow_native`, `vpm::source_url`.
- **Who wins.** A requirement is the root's when a manifest the root controls wrote it: the
  root's, a workspace member's, or a path package's (only those name a `path`: a git package's
  paths become its repository, a registry package has none). The solver takes the root's
  requirements first (`Requirement::root`). A transitive requirement of the same name from
  another source (`same_source`: registry, the same directory, the same git spec, or the same
  archive by hash with the same `strip` and `path`, from whichever URL) is replaced
  by the root's entry when its version still fits (else "this package's entry for `x` gives …,
  which … does not accept"), and `Resolution::overrides` records it (`Override { by, wanted }`).
  Two of the root's entries that disagree, or two transitive ones the root doesn't settle, are an
  error: "`x` is asked for from two sources (…)", each requirement with its chain.
- **The lock** records an override as the package's `"overrides": ["<package that asked>", …]`
  (sorted, optional); `velt install` prints a note per overridden package when it resolves
  (`Installed::notes`), not builds.
- **`allowNative`** entries are a name (`AllowedFrom::Root`: the registry or a path source) or
  `{ name, git }` (that repository at any commit and directory) or `{ name, url }` (archives
  under an https prefix, matched by `source_url::prefix_matches` against the url source's URL as
  written; a refusal suggests the URL's directory, and none when that can't be a prefix, such as
  plain `http` to this machine). Read with each check at its value: `git`
  through `source_url::git_identity`, `url` through `source_url::check_prefix` (a path under the
  host, no credentials, query, fragment, characters past ASCII, `.`/`..` segments or one
  starting with `..` and empty segments but a trailing `/` (each judged before a `;`), backslash,
  a host with `%` or a `:` outside `[…]`, or an escape that isn't two hex digits or decodes to a
  control byte, `.`, `/`, `\`, `;`, `?`, `#`, `%` or a byte past ASCII (`%20` is allowed); a
  matched URL is held to the same); an
  object needs `name` and exactly one of the two. After resolution, a package with native code
  whose entries all name another source is an error naming both sources and the entry to add;
  an unlisted one's error suggests `{ name, git }` for a git source. Git URLs compare in one form
  (scheme and host lowercase, default port dropped, `user@host:/path` as `ssh://user@host/path`
  and a home-relative `user@host:path` as `ssh://user@host/~/path`, a trailing `/` and a `.git`
  ending the last segment's name removed); prefixes match whole path segments, compared as written in
  the manifest, before redirects or mirrors. `velt.lock.json`'s `allowNative` holds the entries
  as written (a string, or an object with `name` first), sorted. The compiler's check
  (`PackageGraph::allows_native`) stays by name: install has checked the sources.
- **Changed native code.** `velt install` and `velt update` print "native code of `x` changes:
  <old> → <new>" for each package with native code whose locked version, source or checksum
  changed (`install::native_changes`).

## Fields
- `import ... from "json"` / `"json/sub"` resolve through `dependencies`; `"velt/x"` and `"./x"`
  never do.
- `paths`: a bare specifier matching a pattern resolves to the target, relative to the package
  root, before `dependencies` are consulted (`"velt/x"` and `"./x"` are never aliased). A
  pattern has at most one `*`, at its end, and so has its target (both or neither); the longest
  matching pattern wins (like TypeScript's `compilerOptions.paths`, but one target per pattern).
  Targets must stay inside the package (no `..`, not absolute). Each package's aliases apply to
  its own modules only (`vpm::PackageGraph::path_alias`).
- `imports` (additive): conditional imports, Node's `imports` field (`vpm::imports`). A key is
  `#` and `/`-separated segments of `[A-Za-z0-9_-]`; a value is a file or an object of
  conditions to files, kept in the order written (`Manifest::imports`, `GraphPackage::imports`).
  Conditions are the built-in `vpm::imports::CONDITIONS` (`native`, `darwin`, `linux`, `win32`,
  `wasm`, `wasi`, `js`, `default`) or any name spelled as a host package's condition (a lowercase
  letter, then lowercase letters, digits and `-`; Node's custom conditions), which only a
  `--host` build for that host package has (the root package's condition that is neither
  built-in nor a dependency's host condition is warned about when the project opens,
  `vpm::host::unreachable_conditions`; std's map takes only built-in ones); `default` comes last; files are `./`-relative, below
  `./src/`, with a source extension. A build's conditions come from its target and, for
  `velt build --host <package>`, the host package's `condition` (`vpm::imports::conditions`);
  the first variant whose condition it has is the module a `#name` import of the package's own
  modules loads (`loader::conditional`). The standard library's map is `std/imports.velt`
  (`export const imports: Imports = { … }`, read by `vpm::manifest::read::read_std_imports`,
  files relative to `std/`). Not supported, each a manifest error: `#x/*` patterns, nested
  condition objects, arrays, `null` and bare-package targets, misspelled conditions. An npm
  package's `#name` resolves through its `package.json` `imports` (`NpmResolver::resolve_import`,
  Node's PACKAGE_IMPORTS_RESOLVE, `SERVER_CONDITIONS`; a target naming another package is an
  error), never a Velt package's map. The importer's package scope is its nearest `package.json`
  (`NpmResolver::package_scope`); it is an npm package's (`loader::conditional::npm_scope`)
  outside every Velt package, and inside one unless it is the Velt root's own or a nameless
  folder file without `exports` or `imports` that no workspace lists
  (`NpmResolver::is_npm_package`); below `node_modules` and a git dependency's provided packages
  always. Variants are compared through relative re-exports, local `export { a }` of imported
  bindings and `export *` (expanded breadth first, `default` left out; a name two stars bring in
  with different signatures, or a star from outside the package, is an error). `velt manifest
  --json` prints a name's variants as
  `[condition, file]` pairs, in order.
- `hostModules` (additive, #1736): the JavaScript of the host modules the package's `declare
  host("m")` functions import, `Record<string, string>` (`Manifest::host_modules`,
  `GraphPackage::host_modules`, `vpm::host_modules`). A key is a module name: not empty, no
  control characters, not `__proto__`, not `velt` and not `velt:…` (the runtime glue's own). A value is one ES
  module, `./`-relative, `/`-separated, below `./src/` (an archive carries only `src/`), named
  `.mjs` or `.js`; its default export is a function that makes the module's functions
  ([workers.md](workers.md)). Used by `velt build --host <package>`, which copies the files of
  the modules the program calls into its directory; a package provides only modules its own
  modules declare, and each module comes from one package (checked by the build, not the
  reader). `to_velt` writes it after `imports`; `velt manifest --json` prints it when present.
- `host` (additive, #1736): makes the package a host package, which `velt build --host
  <package>` builds for (`Manifest::host`, `GraphPackage::host`, `vpm::host::HostConfig`). The
  compiler knows no platform; this object is all a platform is: `target` (a triple; the build
  checks it is one the JavaScript glue runs), `condition` (spelled as a condition, not a
  built-in one), `entry` (a `.mjs`/`.js` module below `./src/` whose file name is not one the
  build writes: `program.wasm`, `velt_web.mjs`, `velt_serve.mjs`, `velt_modules.mjs`, `hosts`,
  `.velt-host`, and the by-products `program.o`, `program.entry.o`, `program.wasm.build-stamp`,
  `program.wasm.link-stamp`; `vpm::host::BUILD_FILES`), and `scaffold` (optional: a
  `/`-separated path inside the build's directory, every component a name (no trailing `/`),
  whose first component is neither one the build writes nor the entry's name (both compared
  without ASCII case), → a file below `./src/`; `{{name}}` in it
  becomes the program's name). Missing `target`, `condition` or `entry`, or an unknown key, is
  an error. `to_velt` writes it after `hostModules`; `velt manifest --json` prints it.
- `entry` is a `/`-separated path inside the package (not empty, not absolute, no `..`).
- `description` (additive, #184): one line of at most 300 characters (Unicode scalar values), no
  control characters, no leading or trailing whitespace, not empty. `keywords`: at most 10
  distinct entries, each `[a-z0-9][a-z0-9-]{0,31}`, order kept, not empty. Both optional; an empty
  string or array is an error saying to remove the field. `to_velt` writes them after `version`,
  and `velt manifest --json` prints them when present.
- A library package exposes `src/lib.velt` (entry for importers); `src/main.velt` makes it runnable.
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
- Scopes (additive, registry-core servers, #1740): `@scope/name` packages at
  `<url>/api/v1/@scope/name/…`. The first user to publish a package in a scope claims it; only its
  owners add packages to it (`GET <url>/api/v1/scopes/<scope>/owners`, `PUT`/`DELETE
  …/owners/<user>`, with the rules above). A registry directory keeps them at
  `<dir>/@scope/name/`. `velt registry serve` (the Rust server) doesn't serve them.
- Yank (additive): index entries gain `yanked = true` (absent when false). `PUT`/`DELETE
  <url>/api/v1/<name>/<version>/yank` set/clear it (owners). Resolution never selects a yanked
  version unless the lockfile pins it; yanked versions stay downloadable.
- READMEs (additive, registry-core servers, #1740): `PUT <url>/api/v1/<name>/<version>/readme`
  with UTF-8 text (owners; 512 KiB by default, 413 above) stores a published version's README by
  its checksum (`blobs/sha256/<hex>`, beside the archives), and the version's index entry gains
  `readme: "sha256:<hex>"` (absent when it has none; clients ignore it). Sent again, it replaces
  the earlier one. `GET` the same path → the text, `text/markdown`, 404 when there is none. It is
  not one of the package's files and changes no checksum. `velt publish` to a URL sends the
  package's `README.md` (any case) after the archive; a server without the request (404, 405) is
  skipped silently, and any other failure is a warning (the version stays published).
  registry-core servers answer every `GET` with `Access-Control-Allow-Origin: *`, so a web page
  (the registry's site) reads them from the browser.
- Browser sign-in (additive, registry-core servers, #1740; client: `vpm::login`, `velt login` in
  `cli.md`). All answers are JSON with `cache-control: no-store`:
  - `GET <url>/api/v1/login` → 200 `{"browser": "<page>", "device": <bool>}`. `browser` is the
    page the loopback flow opens (absolute; it may be on another host, e.g. the website); clients
    open only `https://` (or loopback `http://`) pages without `#`. Any other answer (404 from
    `velt registry serve`), or JSON without `browser`: the registry has no browser sign-in, and
    `velt login` reads a token from stdin.
  - Loopback flow: the client makes a `verifier` (32 random bytes, lowercase hex), its
    `challenge` (base64url without padding of the SHA-256 of the verifier's ASCII text: PKCE S256,
    RFC 7636) and a `state` (16 random bytes, lowercase hex), listens on `127.0.0.1:<port>` (any
    free port) and opens `<page>?port=<port>&challenge=<challenge>&state=<state>` (`&` when the page
    has a query). After the sign-in the registry sends the browser to
    `http://127.0.0.1:<port>/callback?code=<code>&state=<state>`, or on refusal
    `…/callback?error=<word>&state=<state>` with `access_denied` (cancelled), `refused` (this
    account can't sign in; the registry's page says why) or `server_error`. The client answers
    only `GET /callback` with its own `state` (compared in constant time), once, and only after
    the exchange below, with a page saying `Signed in to <url> as <user>` or `Signing in failed:
    see the terminal.`; anything else gets 404 and is ignored.
  - `POST <url>/api/v1/login/token` `{"code": "…", "verifier": "…"}` → 200 `{"token": "velt_…",
    "user": "<name>", "expires": "YYYY-MM-DD"}`, or 400 `{"error": "invalid_grant"}` (an unknown,
    used or expired code, or a verifier that doesn't match its challenge). A code is single-use and
    lives 60 seconds.
  - `POST <url>/api/v1/login/device` (empty body) → 200 `{"device_code": "<64 hex, secret>",
    "user_code": "XXXX-XXXX", "verification_uri": "…", "verification_uri_complete": "…",
    "expires_in": 600, "interval": 5}` (RFC 8628 names; the user code is 8 letters from
    `BCDFGHJKLMNPQRSTVWXZ`).
  - `POST <url>/api/v1/login/device/token` `{"device_code": "…"}` → 200 with the token as above
    (once), or 400 `{"error": …}`: `authorization_pending` (poll again after `interval`),
    `slow_down` (polled within 4 s; the client adds 5 s to its interval), `access_denied`
    (declined or refused) or `expired_token` (expired or unknown). Clients keep the interval
    between 1 and 60 s and stop polling after `expires_in` (at most 3600 s).
  - Clients show the `user`, `expires` and `user_code` only when they are 1-64 ASCII letters,
    digits, `-`, `_` or `.`, and refuse a token that is empty, longer than 4096 bytes or contains
    whitespace or control characters; a sign-in otherwise fails without storing anything.
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
  `"checksum"` (omitted when absent), taken from the published `package.velt`, also by the server
  from the uploaded archive (never from request headers).
- Archives (`vpm::archive`) carry `package.velt` + `src/**` (+ `c-headers.json`, + the `native`
  crate directory or, for C `sources`, `native/`, without `target/` and `.git/`); an archive with any other path, such as a `velt.toml`, is
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
  (the language server shows them in `package.velt`); `velt check --ts-compat` fails on one.
- `to_velt` writes `tsCompat` after the other fields but `lint` and `build`; `velt manifest
  --json` prints it when present.

## Lint levels (additive)
```ts ignore
lint: { perf: "warn", rules: { "perf/number-double": "off", "ts-compat/default-sort": "warn" } },
```
- `vpm::Manifest::lint` (`Option<LintConfig>`): `perf` (`Option<LintLevel>`, every `perf/…`
  rule; `hint` when absent) and `rules` (code → `LintLevel`, which wins over `perf`; a
  `ts-compat/<rule>` level replaces that finding's severity). `LintLevel` is `off`, `hint`,
  `warn` or `deny` ([cli.md](cli.md) "Lint rules"); std's type is `Lint` with `LintLevel`.
- The reader's errors, at the value: `lint`, or `rules`, not an object; a level that is not a
  string or not one of the four; an unknown key in `lint`. It doesn't check that a code names a
  rule (`velt check` warns about one that doesn't).
- `to_velt` writes `lint` before `build`; `velt manifest --json` prints `{"perf", "rules"}`, each
  only when set.

## Native libraries (additive)
Contract: [native_abi.md](native_abi.md).
- `native`: `path` (default `"native"`, a directory name in the package root, not `src` or
  `target`), `targets` (triples from `vpm::manifest::NATIVE_TARGETS`: x86_64/aarch64 Linux GNU,
  macOS and Windows MSVC), `wasm` (must be `false`: not supported yet).
- `native.c.<library>` (c-ffi.md "Where libraries come from"): `system`, `headers`, `unchecked`,
  and `sources` (`.c` files under `native/`; not with `system`) with `defines` (macro → value).
  With `sources`, `native` has only `c` and `targets`, which is what `velt publish` builds with
  the pinned zig; `path` and `wasm` are a crate's, an error next to them. A library's own headers
  are then under `native/`, all of which the package ships.
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
  accepts the toolchain that wrote it. `to_velt` writes the field after `version`, and
  `velt manifest --json` prints it as `"velt"` when present.
- Every command that reads the package (`Project::open`, `velt manifest`) warns on stderr,
  `package.velt asks for velt "<req>", but this is velt <version>; it may not build the package`,
  when the requirement doesn't accept the running velt's version and `$VELT_LAUNCHER` is unset.
- The reader only checks the syntax. Choosing the toolchain is the launcher's job (#948), which
  reads this one field with `velt_toolchain::pin::find_pin` (the nearest `package.velt` above the
  working directory; a manifest that does not parse counts as no pin, and the toolchain then
  reports it).
