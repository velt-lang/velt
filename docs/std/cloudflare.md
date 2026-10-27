# velt:cloudflare

`import { R2Bucket, D1Database, KVNamespace, DurableObjectNamespace, Cache, CloudflareError } from
"velt:cloudflare"`. The bindings of a Cloudflare Worker: a program built with
[`velt build --host workers`](../tooling/webassembly.md#on-cloudflare-workers) reaches the R2
buckets, D1 databases, KV namespaces and Durable Objects its `wrangler.toml` binds, and the
Worker's cache. Calling any of it in a build for another host is a build error.

```ts
import { serve } from "velt:http";
import { D1Database, R2Bucket } from "velt:cloudflare";

struct Release {
  version: string;
  hash: string;
}

async function main() {
  const db = D1Database.binding("DB");
  const blobs = R2Bucket.binding("BLOBS");
  await serve({ port: 8080 }, async (req: Request): Promise<Response> => {
    const name = new URL(req.url).pathname.slice(1);
    const latest = await db
      .prepare("SELECT version, hash FROM releases WHERE name = ? ORDER BY published DESC")
      .bind(name)
      .first<Release>();
    if (latest == null) {
      return new Response("no such package", { status: 404 });
    }
    const archive = await blobs.get(`blobs/sha256/${latest.hash}`);
    return archive == null ? new Response("gone", { status: 410 }) : new Response(archive.bytes());
  });
}
```

```toml
# wrangler.toml
[[d1_databases]]
binding = "DB"
database_name = "registry"
database_id = "…"

[[r2_buckets]]
binding = "BLOBS"
bucket_name = "registry-blobs"
```

- **Bindings** are looked up by name: `R2Bucket.binding("BLOBS")`, `D1Database.binding("DB")`,
  `KVNamespace.binding("KV")`, `DurableObjectNamespace.binding("COUNTER")`. A name the Worker
  doesn't bind throws `CloudflareError`. Look them up once, in `main` or a handler (a module
  constant initialized by a call would look up again at every use).
- **Errors**: a failed operation throws `CloudflareError` with Cloudflare's message, and so does
  a result that doesn't fit the type asked for (a D1 row without a field of `T`).
- **R2**: `get(key): R2Object | null` (with its body: `bytes()`, `text()`, read once; `meta` and
  `key`, `size`), `head(key): R2Meta | null`, `put(key, value: string | u8[], { onlyIfAbsent?,
  contentType? }): R2Meta | null` (null when `onlyIfAbsent` and the key exists), `delete(key)`,
  `list({ prefix?, cursor?, limit? }): R2Listing` (`objects`, `truncated`, `cursor`). `R2Meta`
  has `key`, `size`, `etag`, `httpEtag`, `uploaded` (milliseconds) and `contentType`.
- **D1**: `prepare(sql).bind(...values)` with `?` placeholders (`string`, `number`, `bool` or
  `null`), then `first<T>(): T | null`, `all<T>(): T[]` or `run(): { changes, lastRowId }`.
  Rows decode with `JSON.parse<T>` (columns by name; rename with `AS`). `exec(sql)` runs a
  script, `batch(statements)` runs statements in one transaction.
- **KV**: `get(key): string | null`, `put(key, value, { expirationTtl? })`, `delete(key)`,
  `list(prefix?): string[]`.
- **Durable Objects**: `DurableObjectNamespace.binding(name).get(objectName)` is a stub for the
  object of that name (`idFromName`); `stub.fetch(input, init)` sends it a request like the
  global `fetch`. The objects themselves are JavaScript classes for now.
- **Cache**: `Cache.delete(url): bool` removes a cached response (false if there was none).

Operations are asynchronous: each is one call to the Worker's binding, and the handler's task
waits for it while other requests run.
