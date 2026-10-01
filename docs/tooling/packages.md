# Packages and registries

The package manager is built into `velt`. A package is a directory with a
[`velt.toml`](manifest.md); dependencies come from a registry or a local path, and `velt.lock`
pins them.

| Command | What it does |
|---|---|
| `velt add <pkg>[@<req>]` | add a registry dependency to `velt.toml` (latest version without a requirement) and install it; formatting of `velt.toml` is preserved |
| `velt add <pkg> --path <dir>` | add a local package |
| `velt install [--locked]` | resolve and fetch dependencies, write `velt.lock`; `--locked` fails if the lock would change |
| `velt update` | resolve again, ignoring `velt.lock` |
| `velt publish` | publish the current package to its registry |

```sh
velt new textkit --template lib     # a library with doc comments and tests
cd textkit && velt test && velt publish

cd ../app
velt add textkit                    # the latest version, into velt.toml and velt.lock
```

```ts ignore
import { slugify } from "textkit";
```

## Registries

- **Local** (the default): a directory, `$VELT_REGISTRY` or `~/.velt/registry/<name>/<version>/`.
  Downloads are cached in `~/.velt/cache`.
- **HTTP**: `registry = "https://…"` at the top of `velt.toml`, or `VELT_REGISTRY` set to an
  `http(s)://` URL. `velt registry serve [--dir <d>] [--port <n>] [--host <addr>]` serves a
  registry directory (default `127.0.0.1:8091`); uploads need
  `Authorization: Bearer $VELT_REGISTRY_TOKEN` when the server has that variable set.
- Package archives contain `velt.toml` and `src/**`. Their checksum is the content hash that
  `velt.lock` records, and every download is verified against it before it enters the cache.

The HTTP protocol: `GET <url>/api/v1/<name>/index` returns the package's `index.toml`;
`GET <url>/api/v1/<name>/<version>` returns an archive; `PUT` to the same path uploads one, with
an `X-Velt-Checksum: sha256:…` header. `https://` goes through the system `curl`; `http://` is
built in.

**Not yet available**: a public package registry. Until there is one, share packages through a
registry you serve yourself, or as path dependencies.
