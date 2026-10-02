# Packages and registries

The package manager is built into `velt`. A package is a directory with a
[`package.vlt`](manifest.md); dependencies come from a registry or a local path, and `velt.lock`
pins them.

| Command | What it does |
|---|---|
| `velt add <pkg>[@<req>]` | add a registry dependency to `package.vlt` (latest version without a requirement) and install it; comments in `package.vlt` are kept |
| `velt add <pkg> --path <dir>` | add a local package |
| `velt install [--locked]` | resolve and fetch dependencies, write `velt.lock`; `--locked` fails if the lock would change |
| `velt update` | resolve again, ignoring `velt.lock` |
| `velt publish` | publish the current package to its registry (with its prebuilt native libraries) |
| `velt native build [--target <t>]` | build the package's native library (package authors; needs Rust) |
| `velt manifest [--json]` | check `package.vlt`, or print it as JSON for other tools |

```sh
velt new textkit --template lib     # a library with doc comments and tests
cd textkit && velt test && velt publish

cd ../app
velt add textkit                    # the latest version, into package.vlt and velt.lock
```

```ts ignore
import { slugify } from "textkit";
```

## Registries

- **Local** (the default): a directory, `$VELT_REGISTRY` or `~/.velt/registry/<name>/<version>/`.
  Downloads are cached in `~/.velt/cache`.
- **HTTP**: `registry: "https://…"` in `package.vlt`, or `VELT_REGISTRY` set to an
  `http(s)://` URL. `velt registry serve [--dir <d>] [--port <n>] [--host <addr>]` serves a
  registry directory (default `127.0.0.1:8091`); uploads need
  `Authorization: Bearer $VELT_REGISTRY_TOKEN` when the server has that variable set.
- Package archives contain `package.vlt`, `src/**` and the sources of a `native` crate. Their checksum is the content hash that
  `velt.lock` records, and every download is verified against it before it enters the cache.

The HTTP protocol: `GET <url>/api/v1/<name>/index` returns the package's `index.toml`;
`GET <url>/api/v1/<name>/<version>` returns an archive; `PUT` to the same path uploads one, with
an `X-Velt-Checksum: sha256:…` header. `https://` goes through the system `curl`; `http://` is
built in.

## Packages with native code

A package can include a Rust crate whose library its Velt code calls: database drivers, codecs,
bindings to C libraries. **Using** such a package needs only `velt`: the author publishes a
prebuilt library for each target, `velt add`/`velt install` download the one for your machine,
verify it against the checksum in `velt.lock`, and say which packages run native code:

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
