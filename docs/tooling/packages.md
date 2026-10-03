# Packages and registries

The package manager is built into `velt`. A package is a directory with a
[`package.vlt`](manifest.md); dependencies come from a registry or a local path, and `velt.lock.json`
pins them.

| Command | What it does |
|---|---|
| `velt add <pkg>[@<req>]` | add a registry dependency to `package.vlt` (latest version without a requirement) and install it; comments in `package.vlt` are kept |
| `velt add <pkg> --path <dir>` | add a local package |
| `velt install [--locked]` | resolve and fetch dependencies, write `velt.lock.json`; `--locked` fails if the lock would change |
| `velt update` | resolve again, ignoring `velt.lock.json` |
| `velt publish` | publish the current package to its registry (with its prebuilt native libraries) |
| `velt native build [--target <t>]` | build the package's native library (package authors; needs Rust) |
| `velt manifest [--json]` | check `package.vlt`, or print it as JSON for other tools |
| `velt search <text>` | find packages whose name contains the text |
| `velt yank <pkg>@<version> [--undo]` | withdraw a published version: lockfiles that pin it keep working, new requirements skip it |
| `velt owner list\|add\|remove <pkg> [<user>]` | who may publish a package on a registry server |

```sh
velt new textkit --template lib     # a library with doc comments and tests
cd textkit && velt test && velt publish

cd ../app
velt add textkit                    # the latest version, into package.vlt and velt.lock.json
```

```ts ignore
import { slugify } from "textkit";
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
  clients the `https://` URL. `velt` sends `VELT_REGISTRY_TOKEN` only to an `https://` registry
  or to `http://` on this machine (`localhost`, `127.0.0.0/8`, `[::1]`), and refuses a write to
  any other registry while the variable is set.
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
export VELT_REGISTRY_TOKEN=<the token>           # on alice's machine
velt publish                                     # alice publishes `textkit` and owns it
velt owner add textkit bob                       # bob may publish it too
velt yank textkit@1.2.0                          # withdraw a broken version
```

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
