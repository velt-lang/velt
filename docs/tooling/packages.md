# Packages and registries

The package manager is built into `velt`. A package is a directory with a
[`package.vlt`](manifest.md); dependencies come from a registry or a local path, and `velt.lock.json`
pins them.

| Command | What it does |
|---|---|
| `velt add <pkg>[@<req>]` | add a registry dependency to `package.vlt` (latest version without a requirement) and install it; comments in `package.vlt` are kept |
| `velt add <pkg> --path <dir>` | add a local package |
| `velt install [--locked]` | resolve and fetch dependencies, write `velt.lock.json`; `--locked` fails if the lock would change |
| `velt update` | resolve again, ignoring `velt.lock.json`, with the newest published velt the package's `velt` field accepts, installed first (recorded in the lock) |
| `velt publish` | publish the current package to its registry (with its prebuilt native libraries) |
| `velt native build [--target <t>]` | build the package's native library (package authors; needs Rust) |
| `velt manifest [--json]` | check `package.vlt`, or print it as JSON for other tools |
| `velt search <text>` | find packages by name, keywords and description (`--json` for scripts) |
| `velt yank <pkg>@<version> [--undo]` | withdraw a published version: lockfiles that pin it keep working, new requirements skip it |
| `velt owner list\|add\|remove <pkg> [<user>]` | who may publish a package on a registry server |
| `velt login <url>` / `velt logout <url>` | store or forget your token for a registry server |

```sh
velt new textkit --template lib     # a library with doc comments and tests
cd textkit && velt test && velt publish

cd ../app
velt add textkit                    # the latest version, into package.vlt and velt.lock.json
```

```ts ignore
import { slugify } from "textkit";
```

## Git dependencies

A package can come from a git repository, at a branch, a tag or a commit, without a registry:

```ts ignore
dependencies: {
  // sigx's Velt half, from the main branch of its monorepo.
  sigx: { git: "https://github.com/signalxjs/core", branch: "main", path: "packages/velt/velt" },
  json: { git: "https://github.com/acme/json", tag: "v1.2.0" },
  http: { git: "git@github.com:acme/http.git", rev: "3f2c1a9e0b7d4c1a8e5f6b2d9c0a1e3f4b5c6d7e" },
}
```

- `git` is the repository: `https://…`, `ssh://…`, `git@host:path` or `file://…`, without
  credentials in it (they would end up in `velt.lock.json`): git supplies them from your
  credential helper or SSH keys. `branch`, `tag`
  or `rev` (a full commit hash) say which commit, at most one of them; without one, the
  repository's default branch. `path` is the package's directory in the repository, for a
  repository that holds several packages. A `version` beside them is a requirement the package
  must meet.
- **npm packages too.** A target without a `package.vlt` but with a `package.json` is an npm
  package, compiled from its TypeScript sources ([npm packages](#npm-packages)), as
  `npm install git+…` takes it; a workspace's root (`pnpm-workspace.yaml`, or `workspaces` in its
  `package.json`) provides every package of the workspace besides its own. As in npm, a
  workspace package without a `name` is named after its directory, and two packages of one name
  are an error; so is a `package.json` that can't be read or doesn't parse, naming the file.
  Several dependencies can come from one repository (`sigx` and `@sigx/runtime` from its root);
  two that provide the same package from different repositories or commits are an error. The dependency's name is one of those packages, and its `version`, if
  written, is checked against that package's. So a framework can be tested straight from its
  repository, with no `paths` alias and nothing installed into `node_modules`:

  ```ts ignore
  dependencies: {
    // Every package of vuejs/core's workspace: `vue`, `@vue/runtime-core`, `@vue/server-renderer`, …
    vue: { git: "https://github.com/vuejs/core", branch: "main" },
    // One package of a repository.
    "@sigx/reactivity": { git: "https://github.com/signalxjs/core", path: "packages/reactivity" },
  }
  ```

  The packages' imports of each other resolve inside the checkout; an import of an npm package
  the repository doesn't hold (a published dependency, such as Vue's compiler's `@babel/parser`)
  needs that package from `node_modules` or another git dependency. A package written in
  JavaScript without TypeScript sources (React's are Flow) can't be compiled yet.
- **Locked.** The first install resolves the branch or tag to a commit and records it in
  `velt.lock.json` with a checksum of the checkout
  (`"source": "git+https://github.com/signalxjs/core?branch=main&path=packages/velt/velt#<commit>"`).
  Builds use that commit without the network; `velt update` moves to the branch's newest commit,
  and `--locked` fails rather than move it.
- **Fetched once.** `velt` runs the `git` command, so your credentials and SSH keys work for
  private repositories, and fetches just the commit, into `~/.velt/cache/git/`: each commit is
  checked out once per machine and never changed, and every use is checked against the lock's
  checksum. The checkout ignores your git configuration (`core.autocrlf`, filters), so it, and
  the checksum, are the same on every machine, and the variables a git hook sets (`GIT_DIR`)
  never redirect it.
- **Packages of the same repository.** A `path` dependency of a package from git is another
  package of the same commit, locked with it; a `path` that leads out of the repository is an
  error, and so is a commit with a symbolic link that points out of it (the lock pins the
  repository's files, not what such a link reaches). On Windows, a repository's symbolic links
  are checked out as plain files holding their target, as git does where it can't make links,
  so the checkout hashes the same as on Linux and macOS. Links in a git dependency are not
  followed on Windows: a package that imports a file or a directory through one builds on Linux
  and macOS but not on Windows, and `velt` warns about each link in such a package there. A package from git can't be a workspace member: its
  `"workspace"` dependencies are errors. A registry requirement on the same name (another
  package's `json: "1.2"`) is met by the git package when its version matches, as by a `path`
  package.
- **The whole repository is there.** The checksum covers every file of the commit, not only the
  package's, because a package may import its repository's other packages: the TypeScript
  packages of a pnpm, npm or Yarn workspace in the checkout compile from their sources
  ([npm packages](#npm-packages)), with nothing installed into `node_modules`.
- **Nothing runs.** Installing never runs code from the repository. A package with native code is
  built from source with cargo only when you set `VELT_NATIVE_FROM_SOURCE=1`, since that runs its
  build scripts.
- **Publishing.** A registry package is built from the registry alone, so `velt publish` needs a
  `version` beside `git`, which becomes the published requirement.

## Workspaces

A repository with several Velt packages (an app, the libraries it shares with a CLI) is a
workspace: a `velt.workspace.vlt` at its root lists the member packages, which then share one
`velt.lock.json`, one `velt` version and one registry, and depend on each other by name.

```ts ignore
// velt.workspace.vlt
import type { Workspace } from "velt:package";

export const workspace: Workspace = {
  members: ["packages/*", "apps/*"],
  velt: "0.2",                     // every member builds with this velt
  dependencies: { json: "1.2" },   // requirements members share
};
```

```ts ignore
// apps/server/package.vlt
export const pkg: Package = {
  name: "server",
  version: "0.4.0",
  dependencies: {
    core: "workspace",   // the member named `core`
    json: "workspace",   // the workspace's requirement for `json`
    log: "0.2",          // a requirement of this member's own
  },
};
```

- **Members** are the directories with a `package.vlt` that the `members` globs list: `*` matches
  one directory name, `**` any number of them, a `!` in front excludes (`"!packages/old"`), and
  `"."` is a `package.vlt` beside the workspace file. A glob that lists no package is an error, so
  a typo never drops a member silently. Two members can't have the same name.
- **`"workspace"`** names the member of that name, else the workspace's `dependencies` entry. A
  member is always a `"workspace"` dependency: `core: "0.1"` or `core: { path: "../core" }` for a
  member is an error that says to write `core: "workspace"`. Outside a workspace, `"workspace"`
  is an error.
- **One resolution, one lock.** `velt install`, `velt build` and the other commands resolve every
  member's dependencies together, wherever in the workspace they run, into the `velt.lock.json`
  beside the workspace file (members are listed as `"source": "workspace+packages/core"`). A
  package name has one version in the whole workspace: two members whose requirements no version
  meets is an error that names both. A member also stands in for a registry package of its name,
  for the registry packages that depend on that name too, so a fork of `json` kept as a member
  is the `json` every package in the workspace builds with. A member has no lockfile of its own.
  `velt install` at the root installs every member. Members can't depend on each other in a
  cycle: `members depend on each other in a cycle: a → core → b → a`.
- **`velt` and `registry`** are the workspace's; a member that sets either is an error. The velt
  launcher runs the version the workspace's lock records, as it does for a package.
- **Builds.** A member's build sees only the packages it depends on, directly or not (its imports
  resolve through its own `dependencies`, as always), and its programs go to its own
  `target/velt/`, from the root or from the member. Every member's build stamp includes the
  workspace file, what its globs list and the lockfile, and the machine's build cache shares
  the code of a library every member uses: a second member compiles little more than its own
  code.
- **Commands on several members.** At the workspace's root (outside its packages), `velt check`,
  `velt test`, `velt build`, `velt fmt` and `velt doc` run on every member; anywhere, they take a
  selection:

  | Selection | Members |
  |---|---|
  | `--workspace` | all of them |
  | `-p <glob>` (repeatable) | those whose name matches (`-p 'server*'`); a scoped name matches whole or without its scope (`-p 'server*'` and `-p '@org/*'` both select `@org/server`) |
  | `--affected [<base>]` | those with a file changed since `<base>` (committed or not), and every member that uses them; without a base, the merge base with the upstream branch (in CI, name it: `--affected origin/main`). A change to `velt.workspace.vlt` or `velt.lock.json` affects all |

  Members run in dependency order, several at once, each in its own `velt`; each member's output
  is printed in one piece when it finishes, and a summary names the members that failed. A failed
  member doesn't stop the others; `--fail-fast` starts no more. `--watch`, `-o` and `--json`
  need one member; with one, a relative `-o` is a path from where you ran `velt`. With
  `velt test`, a member whose last run passed with every test file's result kept (none reaches
  outside the program: [kept results](../book/testing.md#results-that-are-kept)) and of which
  nothing changed isn't started at all: it is listed as `Unchanged core (packages/core): 3
  passed`, so a workspace with nothing changed is tested in about the time it takes to start
  `velt` once. `velt build`
  skips libraries (the programs that use them build them), and `velt run -p <member>` and
  `velt dev -p <member>` run one member's program from anywhere in the workspace; its arguments
  follow `--` (`velt run -p server -- --port 8080`).

  ```sh
  velt test                       # at the root: every member
  velt check --affected           # what this branch touched, and what uses it
  velt build -p 'server*' -p cli  # some members, by name
  velt run -p server              # one member's program
  ```
- A package below the workspace that no glob lists stays on its own, with its own lockfile and
  its own `velt` (the launcher reads the globs too); `velt install` there says so.

## npm packages

Velt compiles TypeScript packages from npm, and the packages of pnpm, npm and Yarn workspaces,
from their sources, without a `paths` alias for each import. A bare import that no `paths`
alias and no entry of `dependencies` claims (`@sigx/reactivity`, `@sigx/reactivity/internals`,
`sigx`) names an npm package:

1. **Finding it**, as Node does: `node_modules/<name>` in the importing file's directory or one
   above it (at its real path when it is a symlink, as Node loads it, so pnpm's
   `node_modules/.pnpm` layout finds the package's own dependencies), then the package the importing
   file belongs to when it has that name, then the packages of the workspace around it
   (`packages:` globs in `pnpm-workspace.yaml`, or `workspaces` in the root `package.json`), so
   a workspace compiles before `pnpm install` has linked anything. A `**` glob does not enter
   symlinked directories, and the search lists at most 10,000 directories: past that, an
   import it did not find says which pattern to narrow.
2. **The file**, from its `package.json`: `exports` when it has one (a subpath it doesn't list
   is an error, as in Node), else `types`, `main`, then `index`. Conditions are matched in the
   order the package writes them, as in Node, with those of a server build as Vite's SSR build
   picks them: `node`, `import`, `module` and `default`. `browser`, `require`, `development`
   and `production` never match (Velt has no development or production mode), so a package that
   maps `browser` and `node` to different files is compiled from its `node` one. As in Node,
   only the first matching condition counts: with `{ "node": "./dist/server.js", "default":
   "./dist/client.js" }`, Velt compiles the source of `dist/server.js` or reports that it has
   none, never the client build. Besides it, the targets of `types` and `velt` (a package can
   point Velt at a file of its own) are tried as hints to the source.
3. **Back to the source.** The file `exports` names is usually build output
   (`./dist/index.js`, `./dist/index.d.ts`). Velt drops the extension and looks for the
   TypeScript file it was built from, as TypeScript's project references do: below `rootDir`
   where the package's `tsconfig.json` puts `outDir` (or `declarationDir`), then with `dist/`,
   `lib/`, `build/`, `out/` or `types/` replaced by `src/`, then in place (`.ts`, `.tsx`,
   `/index.ts`, `/index.tsx`). A `velt` target is tried first, then the target Node loads, then
   the `types` target, wherever each is written: with `{ "types": "./dist/index.d.ts", "node":
   "./dist/index.node.js" }`, Velt compiles `src/index.node.ts`, as Node runs its build, and
   `"types"` still finds `src/index.ts` when the JavaScript bundle has another name.
4. **The root's fallback.** When none of the targets of the package's root (`import "sigx"`,
   not a subpath such as `sigx/internals`) has a source, Velt compiles `src/index.ts` (or
   `src/index.tsx`). Packages built with Rollup need it, Vue's among them: `main` and Node's
   `default` name a CommonJS shim (`index.js`, `module.exports = require("./dist/…")`), and
   `types` a declaration file rolled up from all of `src/` and named after the package
   (`dist/runtime-dom.d.ts`), none of which says where the source is. It applies only when no
   target of the root under any condition has a source, those Velt never matches included
   (`browser`, `require`, `default` after `node`): with `{ "node": "./dist/server.js",
   "default": "./dist/client.js" }` and a `src/client.ts`, the package has a source per
   environment, and `src/index.ts` may be the client's entry, so a missing `src/server.ts` stays
   an error. A subpath is mapped only as step 3 says. A package without TypeScript sources is an error that names the file Node
   loads and says what was looked for.

The package's modules then compile like the program's own, and their `env.d.ts` constants come
from [`define`](manifest.md#define). A Velt dependency of the same name wins over an npm
package, so a Velt package named `sigx` and the npm `sigx` can live side by side.

**A package that brings the browser along.** When a package has no server condition and its
entry re-exports DOM code (sigx's `sigx` re-exports `@sigx/runtime-dom`), alias it for the
server build with `paths`, which win over npm packages:

```ts ignore
paths: {
  "sigx": "server/sigx",                     // export * from "@sigx/reactivity" and "@sigx/runtime-core"
  "sigx/internals": "server/sigx-internals", // the same two packages' "/internals"
},
```

## Registries

- **Local** (the default): a directory, `$VELT_REGISTRY` or `~/.velt/registry/<name>/<version>/`.
  Downloads are cached in `~/.velt/cache`.
- **HTTP**: `registry: "https://…"` in `package.vlt`, or `VELT_REGISTRY` set to an
  `http(s)://` URL. `velt registry serve [--dir <d>] [--port <n>] [--host <addr>]` serves a
  registry directory (default `127.0.0.1:8091`). `https://` registries are verified against
  Mozilla's root certificates, plus the PEM file in `$VELT_CA_FILE` for a private CA.
  `HTTPS_PROXY` and other proxy variables are not honored: `velt` connects to the registry
  directly.
- `velt registry serve` speaks plain HTTP, so tokens and packages cross the network unencrypted.
  Beyond localhost, put it behind a reverse proxy that terminates TLS (Caddy, nginx) and give
  clients the `https://` URL. `velt` sends a token (from `velt login` or `VELT_REGISTRY_TOKEN`)
  only to an `https://` registry or to `http://` on this machine (`localhost`, `127.0.0.0/8`,
  `[::1]`), and refuses a write that would send one anywhere else.
- A request may take 10 minutes in all. `velt` gives up on a server that stays silent for 60
  seconds, or that it can't connect to within 10 seconds.
- Package archives contain `package.vlt`, `src/**` and the sources of a `native` crate. Their checksum is the content hash that
  `velt.lock.json` records, and every download is verified against it before it enters the cache.

### Users, owners and yanking

A registry server without users is open: anyone who can reach it may publish, which suits a
laptop or a trusted network. Once it has users, every write needs a user's token, and only a
package's owners may change the package. `velt registry serve` refuses to start an open
registry while `VELT_REGISTRY_TOKEN` is set, since that variable no longer protects a server:

```sh
velt registry user add alice --dir ./registry    # prints alice's token, once
velt login https://registry.example.com          # on alice's machine: paste the token
velt publish                                     # alice publishes `textkit` and owns it
velt owner add textkit bob                       # bob may publish it too
velt yank textkit@1.2.0                          # withdraw a broken version
```

`velt login <url>` reads the token from stdin (a prompt that doesn't echo in a terminal, or a
pipe in a script) and stores it in `$VELT_HOME/credentials.json`, keyed by the registry's URL and
readable only by you (mode 0600 on Unix; on Windows an access list for its owner alone). `velt`
sends a stored token only to the registry it was stored for, so a checkout whose `package.vlt`
names another registry never receives it, and `velt login` refuses a plain `http://` registry on
another machine. `velt logout <url>` forgets the token. `VELT_REGISTRY_TOKEN`, if set,
overrides the stored tokens and is sent to whichever registry the command uses, which suits CI
jobs that publish to one registry.

`velt registry user token alice` replaces a lost or leaked token, and `velt registry user remove`
deletes a user; removing the last one opens the registry again and needs `--open`. A removed
user is also dropped from the owners of every package, so adding a user of the same name later
gives back nothing; the command names the packages left without an owner. The server
stores only a hash of each token. A yanked version stays downloadable, so a project whose
`velt.lock.json` pins it keeps building (with a warning), but `velt add`, `velt update` and new
requirements never pick it; `velt yank <pkg>@<version> --undo` brings it back.

A package published while the server was open has no owners, and nobody may change it until an
administrator, who has the registry directory, assigns one:

```sh
velt registry owner add textkit alice --dir ./registry
```

`velt registry user` and `velt registry owner` change the registry directory directly, and may run
while the server is up: they take the same lock (`<dir>/.lock`) as the server's writes. Run them
as the OS user the server runs as. The users file, `.auth/users.json`, is created with the
default permissions (0644 under a usual umask) inside `.auth/`, which is 0700 on Unix so other
users can't read the token hashes. A users file written by another OS user may be unreadable to
the server, which then answers every write with 500 until the file's owner is fixed.

A crash while a new package is being published can leave its owner recorded without a version
(a `<dir>/<name>/owners.json` and no `index.json`); the name then stays reserved for that user.
To free it, delete the `<dir>/<name>` directory.

Package and user names can't be Windows device names (`con`, `nul`, `aux`, `com1`, …), since a
package is stored in a directory named after it.

The HTTP protocol: `GET <url>/api/v1/<name>/index` returns the package's `index.json`;
`GET <url>/api/v1/<name>/<version>` returns an archive; `PUT` to the same path uploads one, with
an `X-Velt-Checksum: sha256:…` header. `GET <url>/api/v1/search?q=<text>` searches, and the
owner and yank endpoints are listed in [the contract](../internals/contracts/manifest.md).

## Packages with native code

A package can include a Rust crate whose library its Velt code calls: database drivers, codecs,
bindings to C libraries. **Using** such a package needs only `velt`: the author publishes a
prebuilt library for each target, `velt add`/`velt install` download the one for your machine,
verify it against the checksum in `velt.lock.json`, and say which packages run native code:

```text
     Adding `sqlite` 0.1.0
     Native `sqlite` 0.1.0 runs native code (prebuilt, checksum verified, x86_64-unknown-linux-gnu)
```

`velt run` and debug builds link the library as a shared library, `velt build --release` links
it statically (the executable stays self-contained; on Windows the DLL is placed next to it), and
`velt dev` loads it into the running program. If nobody published a library for your target,
`velt` builds it from the package's sources when Rust is installed and you allow it with
`VELT_NATIVE_FROM_SOURCE=1` (the build runs the package's build scripts, from its published
`Cargo.lock`), and otherwise says so:

```text
error: `sqlite 0.1.0` has no prebuilt native library for aarch64-unknown-linux-gnu (published: x86_64-unknown-linux-gnu).
Install Rust (https://rustup.rs) to build it from source, or ask the package author to publish this target.
```

Native libraries cannot be used for WebAssembly targets. Writing one: [the Book's
chapter](../book/native-packages.md); the details: [native_abi.md](../internals/contracts/native_abi.md).

**Not yet available**: a public package registry. Until there is one, share packages through a
registry you serve yourself, or as path dependencies.
