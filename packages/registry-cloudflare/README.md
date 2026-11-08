# registry-cloudflare

The Velt package registry on [Cloudflare Workers](https://developers.cloudflare.com/workers/)
([#1740](https://github.com/velt-lang/velt/issues/1740), step 4): registry-core's `/api/v1` handling
over R2 and D1, with users signing in with GitHub. It is for staging only: #1740
lists what must change before it goes public. It takes archives up to 10 MiB (`MAX_ARCHIVE_BYTES`
in `src/main.velt`, from measurements under workerd: a publish takes about three times the archive
in memory) and refuses packages with native code, as it serves no native libraries.
It needs a paid Workers plan: the Free plan's 10 ms of CPU can't hash an archive.

It is built to cost as little as Cloudflare allows ([#1740](https://github.com/velt-lang/velt/issues/1740#issuecomment-6712612196)):
everything stays within what Workers Paid ($5 a month) and R2's free tier include. Clients'
reads are static files in R2 (archives cached for good, indexes briefly), which a public bucket
can serve without running the Worker at all once `velt` reads them there. D1 holds only small
rows, and there is no Durable Object: a write commits only from the revision it read.

| registry-core's interface | Here | Binding |
|---|---|---|
| `BlobStore` | `R2BlobStore`: the static files, archives at `blobs/sha256/<hex>` and indexes at `index/<name>.json`, with their HTTP headers | `BLOBS` |
| `MetadataDb` | `D1MetadataDb`: a row per package (its revision), per version, per owner, and a search row | `DB` |
| `AuthProvider` | `D1TokenAuth`: users and token hashes. Never open: no users, no writes | `DB` |
| (signing in) | `GitHubLogin`: `GET /login` with GitHub, a new token per sign-in | `DB` |
| `CachePurge` | `CloudflareCachePurge`: Cloudflare's purge API, when a zone is set | see below |

The package is its own host package, for the files a deployment needs: `wrangler.toml` with these
bindings, and the database's schema (`src/migrations`).

## Deploying to staging

```sh
velt build --release --host registry-cloudflare
cd target/velt/registry-cloudflare-workers    # the build scaffolds wrangler.toml and migrations/
# `velt clean` removes this directory: keep a copy of the wrangler.toml you fill in.
npx wrangler r2 bucket create velt-registry-staging
npx wrangler d1 create velt-registry-staging  # put its id in wrangler.toml's `database_id`
npx wrangler d1 migrations apply velt-registry-staging --remote
npx wrangler deploy
```

## Signing in

Users sign in at `<PUBLIC_URL>/login` with GitHub. The first sign-in makes a user named after the
GitHub login in lowercase, bound to the account's id, so the name stays when the account is
renamed. A login that can't be a user name here (one starting with a digit), or one whose name
another user holds, is refused. Each sign-in shows a new token once, for `velt login <url>`; the
registry keeps only its hash, and earlier tokens keep working. Listing and revoking tokens isn't
there yet.

A user is never deleted, so a name never goes to someone else (the database refuses to delete a
user who owns packages). To take someone's access away, delete their rows in `tokens`.

Create a GitHub OAuth app (GitHub's settings, Developer settings, OAuth Apps) with the callback
URL `<PUBLIC_URL>/login/callback`, and give the Worker its id and secret (below).

An administrator can also add a user by the hash of a token (`sha256:` and the hex SHA-256 of the
token); the token itself is never stored. The hash has no salt, so make tokens long and random
(`openssl rand -hex 32`):

```sh
HASH="sha256:$(printf %s "$TOKEN" | shasum -a 256 | cut -d' ' -f1)"
npx wrangler d1 execute velt-registry-staging --remote --command \
  "INSERT INTO users (name) VALUES ('alice'); INSERT INTO tokens (token_hash, user, created) VALUES ('$HASH', 'alice', 0)"
```

Then hold it to what every registry must do, with two users:

```sh
cd packages/registry-conformance   # in the repository
velt run src/main.velt <url> --alice <token> --bob <token>
```

## Editing the database by hand

Writes are compare-and-set on each package's `revision`. An administrator who changes a package's
rows by SQL (its `owners`, say) raises its revision in the same batch, or a write that read the
package before will put back what it read:

```sh
npx wrangler d1 execute velt-registry-staging --remote --command \
  "DELETE FROM owners WHERE package = 'lib'; INSERT INTO owners VALUES ('lib', 0, 'alice'); UPDATE packages SET revision = revision + 1 WHERE name = 'lib'"
```

## Settings

`PUBLIC_URL` and the GitHub app's two settings are needed for signing in. With a cache in front of
the registry, set the cache's too, so a write purges what it changed. Without a `CACHE_ZONE_ID`,
nothing is purged. The registry's answers don't ask to be cached yet, so nothing needs purging
until a cache rule says otherwise.

| Name | What |
|---|---|
| `PUBLIC_URL` | The registry's public URL (GitHub sends users back below it; the purged URLs are below it) |
| `GITHUB_CLIENT_ID` | The GitHub OAuth app's client id |
| `GITHUB_CLIENT_SECRET` | The app's client secret: a secret (`wrangler secret put`) |
| `CACHE_ZONE_ID` | The Cloudflare zone |
| `CACHE_PURGE_TOKEN` | An API token that may purge the zone: a secret (`wrangler secret put`) |

## Testing

`crates/veltc/tests/registry_worker.rs` runs the conformance suite against the Worker under workerd,
with workerd's local R2 and D1 (nine publishes of one package at the same time among its cases),
the upload limit, and signing in against a stub of GitHub. It hasn't run on real Cloudflare yet.
