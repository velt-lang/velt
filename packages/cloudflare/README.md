# cloudflare

Cloudflare's bindings for Velt programs on [Workers](https://developers.cloudflare.com/workers/):
[R2](https://developers.cloudflare.com/r2/), [D1](https://developers.cloudflare.com/d1/),
[KV](https://developers.cloudflare.com/kv/), the
[Cache API](https://developers.cloudflare.com/workers/runtime-apis/cache/) and
[Durable Object](https://developers.cloudflare.com/durable-objects/) stubs, shaped like the Workers
API. Use it with [host-cloudflare](../host-cloudflare/README.md):

```sh
velt add cloudflare host-cloudflare
velt build --release --host host-cloudflare
```

A binding is named as in the Worker's env (`binding = "BUCKET"` in `wrangler.toml`):

```ts ignore
import { caches, d1Database, durableObjectNamespace, kvNamespace, r2Bucket } from "cloudflare";

const bucket = r2Bucket("BUCKET");
const db = d1Database("DB");
```

Making a wrapper looks nothing up; each call finds the binding in the env of the request it
runs in. Async methods reject with an `IoError`: code `ENOENT` when the env has no binding of
that name and kind, else the name of what Cloudflare threw (D1's errors are `Error`, with D1's
message). The functions call the package's host module (`src/cloudflare.mjs`), so they work in
`--host` builds only: on a native target a call of one is a compile error.

## R2

```ts ignore
const put = await bucket.put("notes/a.txt", "hello", {
  httpMetadata: { contentType: "text/plain" },
  customMetadata: { by: "velt" },
});
// Create-only: null (nothing written) when the key exists.
await bucket.put("notes/a.txt", "again", { onlyIf: { etagDoesNotMatch: "*" } });
// Compare-and-swap: only over the version read.
await bucket.put("notes/a.txt", "new", { onlyIf: { etagMatches: put?.etag } });

const obj = await bucket.get("notes/a.txt"); // R2ObjectBody | null
if (obj != null) {
  const text = await obj.text(); // or bytes(), json<T>(), or the stream `obj.body`
}
const meta = await bucket.head("notes/a.txt"); // R2Object | null: key, size, etag, uploaded, …
const page = await bucket.list({ prefix: "notes/", limit: 100 }); // cursor, truncated, delimiter
await bucket.delete(["notes/a.txt", "notes/b.txt"]);
```

`put` takes a `string` (stored as UTF-8) or a `u8[]`. Read a body while handling the request
that got the object: Workers tie it to that request.

## D1

```ts ignore
struct User {
  id: i64;
  name: string;
  avatar: u8[] | null;
}

const { meta } = await db.prepare("INSERT INTO users (name, avatar) VALUES (?, ?)")
  .bind("Ada", null)
  .run(); // meta.changes, meta.last_row_id
const users = await db.prepare("SELECT id, name, avatar FROM users").all<User>(); // .results
const ada = await db.prepare("SELECT id, name, avatar FROM users WHERE id = ?").bind(1).first<User>();
const results = await db.batch([stmt1, stmt2]); // one transaction; results[i].results<T>()
await db.exec("CREATE TABLE IF NOT EXISTS users (id INTEGER PRIMARY KEY, name TEXT, avatar BLOB)");
```

`bind` takes `string`, `number`, `bool`, `null` and `u8[]` (a BLOB), for the statement's `?` in
order. Rows become typed values as `velt/db` reads them for every Velt database driver: each
column fills the field of its name (rename with `AS`); a row that does not fit throws
`RowError`. D1 gives numbers as JavaScript numbers, which integer fields take when they are
integers exactly (up to 2^53); D1 has no booleans, so read 0/1 into an integer field. A BLOB is
a `u8[]`.

Parameters and rows cross between Velt and JavaScript as one binary frame per call
(`src/frame.vlt`), not as JSON text, in which a BLOB (an array of bytes) could not be told from
an array.

## KV

```ts ignore
const kv = kvNamespace("KV");
await kv.put("greeting", "hej", { expirationTtl: 3600 });
const text = await kv.get("greeting"); // string | null; getBytes(key) for u8[] | null
const page = await kv.list({ prefix: "g" }); // keys, list_complete, cursor
await kv.delete("greeting");
```

## The cache

```ts ignore
const cache = caches.default; // or await caches.open("name")
const hit = await cache.match(request); // Response | null; a URL is a GET of it
await cache.put(request, response.clone());
await cache.delete(request);
```

Requests and responses are std's `Request` and `Response`; bodies cross whole.

## Durable Objects

```ts ignore
const rooms = durableObjectNamespace("ROOMS");
const stub = rooms.get(rooms.idFromName("room-1"));
const res = await stub.fetch("https://room/messages", { method: "POST", body: "hi" });
```

Ids are `idFromName(name)`, `idFromString(hex)` (the id's `toString()`) and `newUniqueId()`;
`idFromString` throws `IoError` for a string that is not an id of the namespace. A stub sends
std `Request`s to the object's `fetch` handler and gives a `Response` (bodies whole; a response
of a status below 200, such as a WebSocket upgrade, is an `IoError` "ENOTSUP"). Durable Objects
written in Velt are not supported yet.

## Not yet

R2's ranged `get`, multipart uploads and `get` with `onlyIf`; streamed request bodies for `put`;
KV's `getWithMetadata` and metadata; D1's `raw()` and sessions; WebSockets to Durable Objects.
