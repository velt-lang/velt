# Design: workspaces (monorepos of Velt packages)

Status: proposed (issue #1855). The contract changes land in
[manifest.md](../contracts/manifest.md) with step 1.

## Problem

A repository with several Velt packages (an app, its libraries, a CLI sharing them) is wired by
hand today: every package has its own `velt.lock.json` and its own resolution, a library is a
`{ path: "../core" }` in each package that uses it, and the `velt` pin and `registry` are
repeated in every `package.vlt`. Two packages can lock different versions of the same
dependency without anyone noticing, and there is no way to check, test or build all of them,
or only those a change touched: CI scripts loop over directories and start `velt` once per
package.

Velt already reads pnpm, npm and Yarn workspaces, but only to find the TypeScript sources of npm
packages ([packages.md](../../tooling/packages.md#npm-packages)). Its own packages have no
workspace. The [manifest design](package-manifest.md) left the field for "when that feature
exists".

pnpm (with Turborepo for tasks) is the bar. Velt can be better than that and faster, because most of what
pnpm does is work Velt does not need:

- **No linking.** pnpm's install builds `node_modules` trees of symlinks and hard links per
  project. Velt imports resolve through the package graph straight to the content-addressed
  download cache (`~/.velt/cache`) or to a member's directory, so an install is a resolution
  plus downloads of what is missing, and a no-op install reads two files.
- **One process, one shared code cache.** pnpm and Turborepo start a tool per package. Velt
  can run every member's build in one process and in parallel. The machine's
  [build cache](../../tooling/cli.md#build-cache) already stores compiled functions by content,
  so a library that five members import is compiled once, for all of them.
- **Strict by construction.** pnpm's strictness (no phantom dependencies) comes from its link
  layout. Velt's package graph already resolves an import only through the importing package's
  own `dependencies` (`PackageGraph::dependency_root`).

## Proposal

### The workspace file

`velt.workspace.vlt` at the repository root, data only and read like `package.vlt` (no code
runs; the same reader, errors with locations, completions and hovers in the editor):

```ts ignore
import type { Workspace } from "velt:package";

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
  a `package.vlt`. `*` matches one path segment, `**` any number (not entering symbolic links,
  as the npm workspace walk does); a `!` prefix excludes (`"!packages/legacy"`). A glob that
  matches no package is an error, so a typo does not silently drop a member. A `package.vlt`
  in the workspace root itself is a member when `"."` is listed.
- `velt`, `registry`: as in `package.vlt`, for every member. In a member of a workspace they are
  an error that points at the workspace file: one toolchain and one registry per resolution
  is what makes one lock possible.
- `dependencies`: requirements (the same forms as a package's `dependencies`) that members
  refer to with `"workspace"`. A key that names a member is an error.

The separate file, rather than a `workspace` field in a root `package.vlt` (Cargo's way), keeps
`package.vlt` meaning "a package" with its `name` and `version` required, and lets the
root be just a directory, as `pnpm-workspace.yaml` does.

### Members depend on each other with `"workspace"`

```ts ignore
// packages/server/package.vlt
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
is an error listing the members. It is not a semver requirement, so it cannot be mistaken for
one. A member listed with a version (`core: "0.1"`) or a `path` to another member is an error
with a fix (`core: "workspace"`): one way to say it. A member keeps its own requirements for
anything else; they join the one resolution below.

`"workspace"` outside a workspace is an error that says so.

### One resolution, one lock

The workspace root holds the only `velt.lock.json`. `velt install` anywhere in the workspace
resolves every member's dependencies together, and the resolver keeps one version per package
name, as it does today for one package. Two members whose requirements no version satisfies both
is an error with both chains (`server 0.4.0 → json 1.2`, `cli 0.1.0 → json 2.0`), not two
copies.

The lock lists members as `"source": "workspace+packages/core"` entries with their direct
dependencies, and every other package as today. The format stays version 1: an older velt
reading it is never chosen, since the lock's `velt` names the toolchain that wrote it. A member
that still has a `velt.lock.json` of its own gets an error telling to delete it, or to move it to
the root when the workspace has no lockfile yet, which keeps its versions.

A package outside every `members` glob, below a workspace file, stays on its own; `velt install`
there notes that the workspace does not list it.

### Running commands on several members

`velt check`, `velt test`, `velt build`, `velt fmt` and `velt doc` take a member selection:

| Selection | Members |
|---|---|
| (none, in a member) | that member, as today |
| (none, at the root, no root member) | all of them |
| `--workspace` | all of them |
| `-p <glob>` (repeatable) | members whose name matches (`-p 'server*'`) |
| `--affected [<base>]` | members with a file changed since `<base>` (default: the merge base with the upstream branch), plus every member that depends on them |

`velt run` and `velt dev` take one member (`-p app`); at the root without one, they list the
runnable members.

The members run in dependency order, as one `velt` per member, at most half the cores' worth at
once (each member's build already compiles on every core). A member starts once the selected
members it depends on finished, so it takes their code from the machine's build cache instead of
compiling it at the same time. The workspace is installed once before them, so none writes the
lock. Output is grouped per member and printed when the member finishes; a summary lists every
member's result, and the command fails when one did, with the first failure's exit code. All
members run even after a failure (CI wants every failure in one run); `--fail-fast` starts no
more. One selected member runs in the same process, in its directory. `velt build` skips
libraries when it builds several members: the members that use them build them.

*Revised in step 2:* this note first proposed one process for all members (one installed graph,
one std). The commands print as they work and read the current directory, so that would mean
capturing every command's output and passing a directory to all of them; the processes share
what matters already (the build cache and the stamps are on disk), and the frontend work that a
single process could share is #1612's. `--affected` without a base takes the upstream merge base
on a workstation and is an error in CI (`CI` set): a CI checkout often has no upstream branch.

### The caches workspaces rely on

The workspace adds no cache of its own. Velt has two, and a workspace must feed both correctly:

1. **The build stamp** (`driver/stamp.rs`, #1612 step 0): a build whose inputs did not change
   since the last build of the same output compiles nothing. Two things must change for
   workspaces:
   - **Outputs per member.** `velt build`/`velt run` put the program at `target/velt/<entry
     stem>` relative to the current directory today. From a workspace root, every member with a
     `src/main.vlt` would write `target/velt/main` and overwrite each other's stamp, so nothing
     would ever be up to date. A member's outputs go to `<member>/target/velt/`, wherever `velt`
     runs from, so `velt test` at the root and in the member hit the same stamp. (`velt test`
     already writes to `<package>/target/velt/test`.)
   - **The workspace's files are inputs.** `note_project` records each package's `package.vlt`
     and lockfile. It must also record `velt.workspace.vlt` and the root's `velt.lock.json`, and
     that no workspace file appeared above a package that had none, so changing a shared
     requirement rebuilds.
2. **The machine's build cache** (`build_cache.rs`): compiled functions by content, shared by
   every build on the machine. Members that share a library, std and generic instances get its
   code from the cache after the first member compiles it, with no change. The parallel builds of
   one `velt test --workspace` share the process's one `Cache` handle; it is already used from
   every core by a debug build's codegen and locks its files across processes.

**Test results** (step 3, implemented). A rebuilt-nothing test still runs. When the harness's
program is the same, its last outcome stands in for running it, but only for a test file whose
tests the compiler can show reach nothing outside the program (the #1856 review's rule). The
build stamp records what the program reaches (`outside`): the runtime functions its entry can
call (`velt_opt::used_externs`, after unreachable functions are gone) that touch the file system,
network, clock, environment, child processes, random numbers or standard input, and native
packages' functions. For a harness this is per test file: walked from the function that
registers the file's tests, leaving out the runner (`velt:test`'s `__run`) and the other files.
`<harness>.test-results.json` keeps, under a key over the harness's bytes and
`--filter`/`--exact`, the reports of the files whose tests all passed. The runner prints a kept
report as `(cached)`, and a file that reaches outside as `(ran: not cacheable, it uses …)`.
`--no-cache` (or `VELT_BUILD_STAMP=off`) runs everything. This is Turborepo's task cache, but
keyed by what the compiler knows the program does instead of by globs a user declares.

Planned, not part of this design: a remote cache (CI sharing entries) and sharing the frontend's
parsed and checked std between members of one process, which #1612 already tracks.

### Publishing

`velt publish -p <glob>` / `--workspace` publishes members in dependency order. The archive's
`package.vlt` gets `"workspace"` replaced by a requirement: the member's version (`"0.4.0"`,
which accepts compatible versions) or the workspace's requirement. The workspace's `velt` and
`registry` are written into it too, so the published package builds on its own. A member whose
version is already published is skipped with a note, which makes `velt publish --workspace` safe
to rerun after a partial failure.

### Editors

The language server finds `velt.workspace.vlt` above a package and loads one graph for the
workspace: go to definition crosses members, and the workspace file gets the same completions,
hovers and errors as `package.vlt` (member names in `"workspace"`, glob matches on hover).

## Costs and measurements

There is no runtime cost: the compiled program is the same as with path dependencies. The
promise is speed of the tools, and these numbers come with the steps that make them:

- A generated workspace (100 members, each depending on 3 members and 5 registry packages, from
  a local `velt registry serve`) against the same shape in pnpm (local verdaccio) with Turborepo:
  cold and warm install, a no-op install, `test --workspace` cold, with one leaf changed, and with
  nothing changed.
- Targets: a no-op `velt install` under 50 ms; `velt test --workspace` with nothing changed under
  100 ms (stamps and test stamps only, no parsing). With one `velt` per member (step 2), process
  start alone exceeds that at 50 members, so step 3 checks the build and test stamps in the
  parent and starts a member's `velt` only when one misses.
- Build time and IR size for the members' builds as one process per member.

## Diagnostics

```text
velt.workspace.vlt:4:5: error: `apps/*` matches no package (no directory below `apps` has a package.vlt)
packages/server/package.vlt:6:11: error: `core` is a member of this workspace; write `core: "workspace"`
packages/server/package.vlt:3:3: error: `velt` is set by the workspace (velt.workspace.vlt:5); remove it here
packages/server/package.vlt:7:11: error: `jsno: "workspace"`, but no member and no workspace dependency is named `jsno` (did you mean `json`?)
packages/app/package.vlt:5:11: error: members depend on each other in a cycle: app → core → app
error: `json` has no version that every member accepts:
  server 0.4.0 requires json 1.2
  cli 0.1.0 requires json 2.0
packages/core/velt.lock.json: error: a member of a workspace has no lock of its own; the workspace's is velt.lock.json (delete this one)
```

## Steps

1. The workspace file (reader, schema, `velt:package` `Workspace`), member discovery, `"workspace"`
   dependencies, one resolution and one lock; `velt install` and builds of one member inside a
   workspace. Outputs per member, and the workspace's files in the build stamp.
2. Member selection (`--workspace`, `-p`, `--affected`) for `check`, `test`, `build`, `fmt`,
   `doc`, run in dependency order and in parallel, one `velt` per member.
3. Test results kept for test files that reach nothing outside the program, and a member stamp
   the parent checks before starting a member's `velt` (done: an unchanged three-member workspace
   tests in 0.06 s against 2.1 s cold, debug `velt` on macOS).
4. `velt publish` of members; `velt add -p <member>` and `velt add --workspace` (into the
   workspace's `dependencies`).
5. The language server's workspace graph and the workspace file's editor support.
6. The benchmarks above, against pnpm and Turborepo, in `bench/`.

Steps 1 and 2 make a working monorepo; each step updates
[packages.md](../../tooling/packages.md), [manifest.md](../../tooling/manifest.md) and
[cli.md](../../tooling/cli.md).

## Open questions

- Should test results be cached by default, or opt in per member? Tests that talk to a database
  or the network can pass once and fail later; the stamp only knows the program and its
  environment. The proposal caches by default and prints `cached` visibly; `--no-cache` and the
  per-member `test: { cache: false }` field (not proposed yet) are the ways out.
