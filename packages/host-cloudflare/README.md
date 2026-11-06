# host-cloudflare

Runs Velt programs on [Cloudflare Workers](https://developers.cloudflare.com/workers/). Add it to
a package's dependencies and build with:

```sh
velt build --release --host host-cloudflare
```

The build writes `target/velt/<name>-workers/`: the program (`program.wasm`), the runtime's glue,
the host modules of the packages it uses, this package's `worker.mjs` (the Worker's entry) and a
`wrangler.toml` when there is none, so an edited one is kept. `npx wrangler deploy` there deploys
it.

The program serves requests with an accept loop over `velt/host/worker` (packages/http-server's
JS-host variant, chosen by the built-in condition `js`, does this for `serve`). Its `#name`
imports may also choose a variant for this host alone with its condition, `workers`.
Host modules see each request's `env` and `ctx` as `host.env` and `host.ctx`.

This package is all Velt knows about Cloudflare: its `package.velt`'s `host` gives the target, the
condition, the entry (`src/worker.mjs`) and the files to scaffold (`src/wrangler.toml`). Another
platform is another package like it, with no change to the compiler
(docs/internals/contracts/workers.md in the Velt repository).
