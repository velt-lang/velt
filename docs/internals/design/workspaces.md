# Workspaces: monorepos of Velt packages

Status: partly implemented. Issue: [#1855](https://github.com/velt-lang/velt/issues/1855).

## Summary

A workspace is a repository of several Velt packages (an app, its libraries, a CLI sharing them)
that share one resolution, one `velt.lock.json`, one toolchain pin and one registry. A
`velt.workspace.velt` at the root lists the member packages; members depend on each other with
`"workspace"`; and `velt check`, `test`, `build`, `fmt` and `doc` run on all members, on members
selected by name, or on the members a change affects. Test results of programs the compiler can
show reach nothing outside themselves are kept, so an unchanged workspace tests in milliseconds.

## Motivation

Without workspaces, a repository of packages is wired by hand: every package has its own
`velt.lock.json` and resolution, a library is a `{ path: "../core" }` in each package that uses
it, and the `velt` pin and `registry` are repeated in every `package.velt`. Two packages can lock
different versions of the same dependency without anyone noticing, and checking, testing or
building all of them, or only those a change touched, means a CI script that loops over
directories.

pnpm with Turborepo is the bar, and much of what it does is work Velt does not need:

- **No linking.** pnpm's install builds `node_modules` trees of links per project. Velt imports
  resolve through the package graph straight to the content-addressed download cache or to a
  member's directory, so an install is a resolution plus downloads of what is missing.
- **One shared code cache.** The machine's build cache stores compiled functions by content, so a
  library that five members import is compiled once for all of them.
- **Strict by construction.** pnpm's strictness (no phantom dependencies) comes from its link
  layout. Velt's package graph resolves an import only through the importing package's own
  `dependencies` (`PackageGraph::dependency_root`).

Velt also reads pnpm, npm and Yarn workspaces, but only to find the TypeScript sources of npm
packages; that is separate from Velt workspaces.

## Design

### The workspace file

`velt.workspace.velt` at the repository root is data only and read by the same reader as
`package.velt` ([package-manifest.md](package-manifest.md)): no code runs, and errors have
locations.

```ts ignore
import type { Workspace } from "velt/package";

export const workspace: Workspace = {
  members: ["packages/*", "apps/*", "tools/cli"],
  velt: "0.2",                                  // the toolchain of every member
  registry: "https://registry.example.com",     // the registry of every member
  dependencies: {                               // requirements members share
    json: "1.2",
    http: "0.3",
  },
};
```

- `members`: `/`-separated globs relative to the workspace root, matching directories that hold
  a `package.velt`. `*` matches one directory name, `**` any number (not entering symbolic links
  or `target`); a `!` prefix excludes (`"!packages/legacy"`). A glob that matches no package is
  an error, so a typo does not silently drop a member. A `package.velt` in the root itself is a
  member when `"."` is listed.
- `velt`, `registry`: as in `package.velt`, for every member. Setting them in a member is an
  error that points at the workspace file: one toolchain and one registry per resolution is what
  makes one lock possible.
- `dependencies`: requirements (the same forms as a package's `dependencies`) that members refer
  to with `"workspace"`. A key that names a member is an error.

The workspace is a separate file, not a `workspace` field in a root `package.velt`, so that
`package.velt` always means "a package" with `name` and `version` required, and the root can be
just a directory, as with `pnpm-workspace.yaml`.

### Members depend on each other with `"workspace"`

```ts ignore
// packages/server/package.velt
import type { Package } from "velt/package";

export const pkg: Package = {
  name: "server",
  version: "0.4.0",
  dependencies: {
    core: "workspace",     // the member named `core`, whatever its version
    json: "workspace",     // the workspace's requirement for `json`
    log: "0.2",            // a requirement of this member's own
  },
};
```

`"workspace"` names the member of that name, else the workspace's `dependencies` entry, else it
is an error. It is not a semver requirement, so it cannot be mistaken for one. A member listed
with a version or a `path` is an error with the fix `core: "workspace"`: there is one way to say
it. A member keeps its own requirements for anything else; they join the one resolution. Members
that depend on each other in a cycle are an error, since they are built in dependency order.
`"workspace"` in a package that no workspace lists is an error that says so.

### One resolution, one lock

The workspace root holds the only `velt.lock.json`. `velt install` anywhere in the workspace
resolves every member's dependencies together, and the resolver keeps one version per package
name. Requirements no single version satisfies are an error naming who requires what, not two
copies.

The lock lists members as `"source": "workspace+packages/core"` entries with their direct
dependencies, and every other package as for a single package. A member with a `velt.lock.json`
of its own is an error that says to delete it, or to move it to the root when the workspace has
no lock yet, which keeps its versions. A package below a workspace file but outside every
`members` glob stays on its own, and `velt install` there says the workspace does not list it.

### Running commands on several members

`velt check`, `velt test`, `velt build`, `velt fmt` and `velt doc` take a member selection
(`crates/veltc/src/commands/members/`):

| Selection | Members |
|---|---|
| (none, in a member) | that member |
| (none, at the root, in no package) | all of them |
| `--workspace` | all of them |
| `-p <glob>` (repeatable) | members whose name matches (`-p 'server*'`) |
| `--affected [<base>]` | members with a file changed since `<base>`, committed or not, plus every member that depends on them |

`--affected` without a base compares with the merge base of the upstream branch on a
workstation; in CI (`CI` set) the base must be named, because a CI checkout often has no upstream
branch. `velt run` and `velt dev` take one member (`-p app`) and continue in its directory.

Members run in dependency order, one `velt` process per member, at most half the cores' worth at
once (each member's build already compiles on every core). A member starts once the selected
members it depends on have finished, so it takes their code from the build cache instead of
compiling it at the same time. The workspace is installed once before them, so no member writes
the lock. Output is grouped per member and printed when the member finishes; a summary lists
every member's result, and the command fails with the first failure's exit code. All members run
even after a failure, so CI sees every failure in one run; `--fail-fast` starts no more. When the
selection is one member, the command runs in the same process, in the member's directory.
`velt build` of several members skips libraries (members without a program): the members that
use them build them.

One process per member, rather than one process for all, keeps every command's output and
working directory its own; the processes already share what matters, the build cache and the
build stamps on disk.

### Kept test results

A test file's program that did not change since its tests passed, and that reaches nothing
outside itself, passes again without running (`crates/veltc/src/commands/test/cache.rs`). The
build stamp records what a program reaches: the runtime functions its entry can call after
unreachable functions are removed that touch the file system, network, clock, environment, child
processes, random numbers or standard input, and native packages' functions. For a test harness
this is per test file, walked from the function that registers the file's tests, leaving out the
runner and the other files.

`<harness>.test-results.json` keeps, under a key over the harness's bytes, `--filter`/`--exact`
and the leak check, the reports of the files whose tests all passed. The runner prints a kept
report with `(cached)` after the file's name, and a file that reaches outside with
`(ran: not cacheable, it uses …)`. `--no-cache` (or `VELT_BUILD_STAMP=off`) runs everything. This
is Turborepo's task cache, keyed by what the compiler knows the program does instead of by globs
a user declares. Caching is on by default and printed visibly, because a test that reaches the
database, network or clock is never kept.

For `velt test` on several members, the parent checks each member's stamp first: a member whose
last run passed with every result kept, and of which nothing changed, passes without starting a
`velt` of its own.

## Compilation

The workspace is loaded and checked in `crates/vpm/src/workspace/` (`Workspace::load`, member
discovery, the `"workspace"` and cycle checks in `check.rs`); the file is read by
`vpm::manifest::read::read_workspace`. `Workspace::effective_manifests` gives each member's
manifest as resolution and builds use it, with `"workspace"` replaced and the workspace's `velt`
and `registry` filled in. `vpm::install::workspace` installs every member into the one lock.

The workspace adds no cache of its own; it feeds the two that exist:

1. **The build stamp** (`crates/veltc/src/driver/stamp.rs`): a build whose inputs did not change
   compiles nothing. A member's outputs go to `<member>/target/velt/` wherever `velt` runs from,
   so `velt test` at the root and in the member hit the same stamp, and members with the same
   entry name never overwrite each other's. The stamp records the workspace file, the members'
   manifests, the root's lock, and whether a workspace file exists above the package, so changing
   a shared requirement rebuilds.
2. **The machine's build cache**: members that share a library, std and generic instances get
   their code from the cache after the first member compiles it.

## Runtime cost

None: the compiled program is the same as with path dependencies. The cost that matters is the
tools': an unchanged three-member workspace runs `velt test --workspace` in 0.06 s against 2.1 s
cold (debug `velt`, macOS), because the parent answers from the member stamps and starts no
member process.

## Diagnostics

```text
velt.workspace.velt:4:5: error: `apps/*` matches no package (no directory it names has a package.velt)
packages/server/package.velt:6:11: error: `core` is a member of this workspace; write `core: "workspace"`
packages/server/package.velt:3:3: error: `velt` is set by the workspace (velt.workspace.velt:5); remove it here
packages/server/package.velt:7:11: error: `jsno: "workspace"`, but no member and no workspace dependency is named `jsno` (did you mean `json`?)
packages/app/package.velt:5:11: error: members depend on each other in a cycle: app → core → app
packages/core/velt.lock.json: a member of a workspace has no lockfile of its own; the workspace's is the velt.lock.json beside velt.workspace.velt (delete this one)
```

## Limitations

- A member's `velt` and `registry` always come from the workspace; members cannot use different
  toolchains or registries.
- A remote cache shared between machines (CI) is not part of the design.

## Not yet implemented

- **Publishing members.** `velt publish -p <glob>` / `--workspace` publishes members in
  dependency order. The archive's `package.velt` gets `"workspace"` replaced by a requirement:
  the member's version (`"0.4.0"`, which accepts compatible versions) or the workspace's
  requirement, and the workspace's `velt` and `registry` written in, so the published package
  builds on its own. A member whose version is already published is skipped with a note, so
  `velt publish --workspace` can be rerun after a partial failure. Publishing a package with a
  `"workspace"` dependency is an error until then.
- **Adding dependencies to members.** `velt add -p <member>` adds to a member's manifest, and
  `velt add --workspace` to the workspace's `dependencies`.
- **Editor support for the workspace file.** The language server treats `velt.workspace.velt`
  like `package.velt`: the reader's diagnostics, completion of keys and of member names in
  `"workspace"`, and the members a glob matches on hover.
- **Benchmarks against pnpm and Turborepo** in `bench/`: a generated workspace of 100 members
  (each depending on 3 members and 5 registry packages from a local `velt registry serve`)
  against the same shape in pnpm with Turborepo, for cold, warm and no-op installs and for
  `test --workspace` cold, with one leaf changed and with nothing changed. Targets: a no-op
  `velt install` under 50 ms and `velt test --workspace` with nothing changed under 100 ms.
