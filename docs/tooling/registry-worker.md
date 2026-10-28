# A registry on Cloudflare Workers

`packages/registry` is a Velt package registry written in Velt that runs as a
[Cloudflare Worker](webassembly.md#on-cloudflare-workers). It speaks the protocol `velt` speaks
to any registry, so `velt publish`, `velt add`, `velt yank`, `velt owner`, `velt search` and
`velt login` work with it unchanged, and it keeps what `velt registry serve` keeps: package
archives in an R2 bucket, and versions, users and owners in a D1 database. Cloudflare's free tier
covers a small registry; R2 charges nothing for downloads.

## Deploying

```sh
cd packages/registry
velt build --release --host workers                     # target/velt/: the Worker
npx wrangler r2 bucket create velt-registry
npx wrangler d1 create velt-registry                    # put the id it prints in wrangler.toml
npx wrangler d1 migrations apply velt-registry --remote
npx wrangler secret put ADMIN_TOKEN                     # a long random string
npx wrangler deploy
```

`wrangler dev` runs it locally instead, with a local bucket and database (apply the migrations
with `--local`, and put `ADMIN_TOKEN=…` in `.dev.vars`).

## Users

Every write needs a user's token (`Authorization: Bearer <token>`, which `velt login` stores),
and for an existing package one of its owners: the first user to publish a package owns it, and
owners add others with `velt owner add`. Users sign up with GitHub, or an administrator makes
them.

### Signing in with GitHub

Opening `https://<worker>/login` in a browser signs in with GitHub. The first time, it makes a
registry user named after the GitHub login (lowercase; `gh-` before a login that starts with a
digit); the page then shows a registry token, once, for `velt login https://<worker>`. Signing in
again shows a new token, and the previous one stops working. The user stays linked to the GitHub
account's numeric id, so renaming the account on GitHub keeps the registry user. A login whose
name an administrator already gave to another user is refused.

To turn it on, register an OAuth app on GitHub (Settings → Developer settings → OAuth Apps)
with the callback URL `https://<worker>/login/callback`, set `GITHUB_CLIENT_ID` under `[vars]`
in `wrangler.toml`, and store the app's secret:

```sh
npx wrangler secret put GITHUB_CLIENT_SECRET
```

Without them, `/login` says that sign-in is not set up. The app asks for no scopes: the registry
only reads who signed in.

### Users by the administrator

The admin API, with the `ADMIN_TOKEN` secret:

```sh
curl -X POST -H "Authorization: Bearer $ADMIN_TOKEN" https://<worker>/api/v1/_admin/users/alice
# prints alice's token, once; on alice's machine:
velt login https://<worker>
```

| Admin request | Does |
|---|---|
| `POST /api/v1/_admin/users/<user>` | make a user; the answer is the token |
| `POST /api/v1/_admin/users/<user>/token` | replace a lost or leaked token |
| `DELETE /api/v1/_admin/users/<user>` | remove a user and their ownerships |
| `PUT` / `DELETE /api/v1/_admin/owners/<package>/<user>` | add or remove an owner (an administrator may remove the last one) |

`REGISTRY_OPEN = "true"` in `wrangler.toml` makes the registry open, as `velt registry serve`
is without users: anyone may publish, and packages have no owners. Use it only on a private
network.

## What it checks

As `velt registry serve` does: the archive's form and safe paths, its content hash against the
checksum `velt publish` sends, a `package.vlt` naming the package and version of the URL (its
description and keywords within the manifest's limits, every dependency with a version), names
that differ only in `-` and `_` from a published one, and that a version, or a native library of
one, is never replaced. Native libraries are checked against their `native.json` (package,
version and target, the files it names, the export prefix).

**Not yet** ([#1325](https://github.com/velt-lang/velt/issues/1325)): downloads go through the
Worker rather than straight from R2 and the cache; `velt login` doesn't open the browser itself
(the token is copied from the page); and `velt registry user` and `velt registry owner` work
only on a registry directory.
