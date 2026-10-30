# fetch

`fetch`, `Request`, `Response` and `Headers` are global, as in Node: no import. They follow the
WHATWG Fetch standard, which TypeScript's `lib.dom.d.ts` types. The client is written in Velt
on [`velt/net`](net.md) (`std/fetch/client/`): HTTP/1.1 with keep-alive connections pooled per
origin and shared by every task (HTTP/2 is not used, as in Node's `fetch`), TLS from the runtime
for HTTPS, bodies decoded by [`velt/compress`](compress.md), and redirects followed as the
standard says. A pooled connection idle for more than a second is checked before it is reused,
and one idle for a minute is closed. A request sent on a *reused* pooled
connection that the server had closed (it fails before any response) is sent once more, on
another connection, when its method is idempotent (`GET`, `HEAD`, `OPTIONS`, `TRACE`, `PUT`,
`DELETE`), as Go's client does; `POST` and `PATCH` are never sent twice, and neither is a request
on a fresh connection (that server got the request and failed). Node fails such a request. A [`velt/http`](http.md) server's handlers
take and return the same `Request` and `Response`, as in Deno and Bun. The option types of their
signatures (`RequestInit`, `RequestRedirect`, `ResponseInit`, `HeadersInit`, `BodyInit`) are
global too, as in TypeScript, so an init object can be built before the call:
`const init: RequestInit = { method: "POST" }; if (body != null) { init.body = body; }`.
`velt/net/http` exports the same names and [`BodyStream`](#bodystream), a Velt body stream of
`u8[]` chunks.

```ts
type User = { id: number; name: string };

async function users(base: string): Promise<User[]> {
  const res = await fetch(`${base}/users`, { headers: { accept: "application/json" } });
  if (!res.ok) {
    throw new Error(`GET /users: ${res.status} ${res.statusText}`);
  }
  return await res.json<User[]>();
}
```

## `fetch(input, init?)`

`fetch(input: string | URL | Request, init: RequestInit = {}): Promise<Response, IoError |
AbortError>` sends a request and resolves once the response's status and headers have arrived;
the body is received when you read it. An HTTP error status (404, 500) is a response, not an
error: check `res.ok`. `fetch` rejects when there is no response:

- `IoError` (from [`velt/io`](io.md)) when the request fails: `code` is `"ECONNREFUSED"`,
  `"ETIMEDOUT"` (connecting takes more than 10 s), `"ECONNRESET"`, … and the message starts with
  `fetch failed`, as Node's does. An invalid URL, header name or value, a GET or HEAD request
  with a body, and a scheme other than `http:` / `https:` fail with `"EINVAL"` / `"ENOTSUP"`
  before anything is sent. A redirect loop (more than 20) or a redirect with `redirect: "error"`
  fails with `fetch failed: …`.
- The signal's `AbortError` (from [`velt/task`](task.md)) when `init.signal` is aborted: a
  `TimeoutError`, which extends `AbortError`, for `AbortSignal.timeout(ms)`. Aborting stops
  the request where it waits (connecting, sending, waiting for the response or receiving its
  body) and closes its connection.

`RequestInit` has the standard's fields that make sense outside a browser:

| Field | Type | |
|---|---|---|
| `method` | `string` | default `"GET"`; `get`, `post`, `put`, `delete`, `head` and `options` are uppercased |
| `headers` | `Headers \| Record<string, string>` | an object literal works: `{ "content-type": "text/csv" }` |
| `body` | `string \| Uint8Array \| ArrayBuffer \| DataView \| u8[] \| URLSearchParams \| ReadableStream<Uint8Array>` (any typed array) | a string sets `content-type: text/plain;charset=UTF-8` and form fields `application/x-www-form-urlencoded;charset=UTF-8`, unless one is set |
| `redirect` | `"follow" \| "error" \| "manual"` | default `"follow"` (at most 20); `"manual"` returns the 3xx response |
| `signal` | `AbortSignal` | cancels the request |
| `ca` | `string` | Velt: extra trusted CA certificates (PEM) for `https://`, e.g. a private or test CA |

Requests send `accept: */*`, `user-agent: velt` and, like Node, `accept-encoding: gzip,
deflate` (`br, gzip, deflate` over HTTPS, chosen again for each redirect hop) unless you set
them. Following a redirect
works as in browsers and Node: only 301, 302, 303, 307 and 308 are followed (a 300 or 304 is
returned as it is), a 303 (and a 301 or 302 after a POST) becomes a GET without a
body, and `authorization`, `cookie` and a `host` you set are not sent to another origin.

The body's framing is the client's own, as in Node: `content-length` is written from the body
(one you set must agree with it, else `IoError` "EINVAL" `Request body length does not match
content-length header`), and setting `transfer-encoding` fails with `IoError` "EINVAL"
(`invalid transfer-encoding header`), so a header cannot make the server read another body than
the one sent. Responses are read as strictly as Node reads them: a status below 100, a header
name that is not a token, a control character in a header value or the reason phrase, both
`transfer-encoding` and `content-length`, or a chunked trailer larger than a head may be (400
KiB) fail with `IoError` "EPROTO". A connection taken from the pool is checked first, so a
request does not go out on one whose close has arrived. A request that may not be sent twice (a
POST) takes only one idle for at least 5 ms, by when a close the server sent shortly after its
last response (at once, on its next turn or a few milliseconds later) has usually arrived; it
gets a new connection otherwise. This narrows the race every HTTP/1.1 client has, but does not
rule it out: a server that closes an idle connection later than that, or one slowed by load, can
still close it just as a POST goes out, and the POST then fails with `IoError` "ECONNRESET"
(`other side closed`) after it reached the server. `fetch` never sends a POST again, as Node does
not (Node waits two event-loop turns before it reuses a connection, and has the same residual
race): retry a POST yourself only if it is idempotent for your server.

HTTPS trusts Mozilla's root certificates (compiled in, so the result does not depend on the
machine) plus `ca`. An untrusted certificate fails with `IoError`.

## `Response`

- `status: number`, `ok: bool` (200–299), `statusText` (the server's reason phrase, else the
  standard one), `headers: Headers`, `url` (the final URL, after redirects), `redirected`,
  `type` (`"basic"` for a fetched response, `"default"` for one you made, `"error"`),
  `bodyUsed`.
- Reading the body: `await res.text()` (invalid UTF-8 becomes U+FFFD and a leading byte order
  mark is dropped, as in JS),
  `await res.json<T>()`, `await res.bytes(): Uint8Array` and `await res.arrayBuffer():
  ArrayBuffer` ([typed arrays](typed_array.md)). A body is read once: a second read throws `IoError`
  (`Body is unusable: Body has already been read`); a response without a body can be read any
  number of times, as in JS. A large body is received into one buffer
  sized from `content-length`, so `bytes()` copies it once.
- `json<T>()` decodes the body as `T` and checks it (`JsonError` names what doesn't match),
  so there is no separate schema step. Velt has no `any`: name the type (`res.json<User[]>()`)
  or give the variable one (`const users: User[] = await res.json()`); an untyped
  `res.json()` is a compile error that says so.
- `res.body` is the body as it arrives, a [`ReadableStream`](streams.md) of `Uint8Array`s:
  `res.body.getReader()` or `for await (const chunk of res.body)`, so a large download need not
  be held in memory: each chunk is at most 64 KiB, however much a compressed body expands. It is
  `null` for a status without a body (101, 103, 204, 205, 304), for a `HEAD` request's response
  and for a response made without one. Reading it uses the body up, as `text()` does; getting
  it does not. A [`ReadableStream`](streams.md) of `Uint8Array` chunks is a body too, as in
  Node: `new Response(ReadableStream.from(gen()))` (a chunk of another type fails the read
  with `IoError` "EINVAL", `Received non-Uint8Array chunk`), and so is a
  [`BodyStream`](#bodystream).
- A body the server compressed (`content-encoding: gzip`, `deflate` or `br`, or several of
  them, such as `deflate, gzip`: up to five) is decoded as it arrives, whichever way you read
  it; the headers stay as received. A list naming another coding leaves the body as sent, and
  a response without a body (a `HEAD` request's, a 204 or 304) has nothing to decode; an empty
  body is `""` in any coding, as in Node. A body
  that does not decode fails the read with `IoError` (`fetch failed: invalid compressed body:
  …`).
- The status text, URL and headers stay readable after the body was read; the headers are
  kept as received and made into strings the first time you read them, so a response whose
  headers you never look at costs nothing for them. A small body that arrives with the head
  gives its connection back to the pool at once, whether or not you read it. `headers` is read-only, as in JS (a getter): change the `Headers` it
  returns, not the property. Dropping a response whose body was not read closes its
  connection.
- `new Response(body?: string | Uint8Array | ArrayBuffer | DataView | u8[] | URLSearchParams |
  BodyStream | null, init?:
  ResponseInit { status?; statusText?; headers? })`, `Response.json(data, init?)`
  (`content-type: application/json`), `Response.error()` and `Response.redirect(url, status =
  302)` build responses: what a [server](http.md#the-response) handler returns, or a stand-in
  in tests. A string body implies `content-type: text/plain;charset=UTF-8` and form fields
  `application/x-www-form-urlencoded;charset=UTF-8`, unless `headers` set one; a constructed
  response makes no `Headers` until you read `headers`. A [`Blob`](#blob-file-and-formdata)
  body implies its `type`, and a `FormData` body `multipart/form-data; boundary=…`.
- `await res.blob()` and `await res.formData()` read the body as a
  [`Blob`](#blob-file-and-formdata) (its type the `content-type`) or a `FormData` (a
  `multipart/form-data` or `application/x-www-form-urlencoded` body; any other fails with
  `IoError` "EINVAL").
- `res.clone()` is a copy whose body can be read on its own, also at the same time as the
  original's: a streamed or fetched body is teed, each chunk going to both, and a chunk one of
  them has not read waits in memory for it, as in Node. Cloning a response whose body was read
  throws `IoError` "EINVAL" (`Response.clone: Body has already been consumed.`).

## `Request`

`new Request(input: string | URL | Request, init?: RequestInit)` holds what `fetch` takes:
`url`, `method`, `headers`, `redirect`, `signal` (the `init.signal`, else one that is never
aborted, as in JS), `bodyUsed`, the body readers `text()`, `json<T>()`, `bytes()`,
`arrayBuffer()`, `blob()`, `formData()` and `body` (a [`ReadableStream`](streams.md) of
`Uint8Array`s, `null` without a body), and `clone()` (as `Response.clone()`; a received
request's body is teed too). `fetch(request)` sends it
(its body counts as read afterwards); `fetch(request, init)` overrides fields of it. A body that is
a stream is read whole before the request is sent (**Planned**: streamed uploads).

A [server](http.md#the-request) hands its handler a `Request` for what it received: its `url`
is absolute, its headers are immutable, and its body is received while it is read.

## `Blob`, `File` and `FormData`

Global, as in Node. `new Blob(parts?: (string | Uint8Array | ArrayBuffer | DataView | u8[] |
Blob)[], { type? })` holds the bytes of its parts (text as UTF-8) with a media type (lowercased;
`""` if it is not printable ASCII): `size`, `type`, `slice(start?, end?, contentType?)`
(negative indices count from the end), and the readers `text()`, `bytes(): Uint8Array`,
`arrayBuffer()` and `stream()` (a [`ReadableStream`](streams.md) of `Uint8Array`s). `new
File(parts, name, { type?, lastModified? })` is a `Blob` with a `name` and a `lastModified`
time (milliseconds; default now).

`FormData` holds named fields, text or files: `append(name, value: string | Blob, filename?)`
(a blob is held as a `File` named `filename`, its own name, or `"blob"`), `set`, `get(name):
string | File | null`, `getAll`, `has`, `delete`, `forEach`, `keys()`, `values()`, `entries()`
and `for...of`. As a body it is sent as `multipart/form-data`, the way undici writes it (line
breaks in text values as CRLF, a file without a type as `application/octet-stream`).

```ts
async function upload(url: string, report: string): Promise<number> {
  const form = new FormData();
  form.append("title", "Weekly report");
  form.append("file", new Blob([report], { type: "text/csv" }), "report.csv");
  const res = await fetch(url, { method: "POST", body: form });
  return res.status;
}
```

## `BodyStream`

Velt's body stream of `u8[]` chunks (a [`ReadableStream`](streams.md) of `Uint8Array`s is a
body too, as in JS): `BodyStream.from(chunks: AsyncIterable<u8[], E>)` makes one
from an async iterable, such as an async generator, for `new Response(stream)`: a server sends
each chunk as it is made, and `for await (const chunk of stream)` reads it. A stream is read once. An error the iterable throws reaches the reader as
an `IoError` (`"EIO"`) carrying its text.

```ts
import { BodyStream } from "velt/net/http";
import { utf8Encode } from "velt/encoding";

async function* lines(): AsyncGenerator<u8[]> {
  yield utf8Encode("one ");
  yield utf8Encode("two");
}

async function main() {
  const res = new Response(BodyStream.from(lines()));
  console.log(await res.text()); // one two
}
```

## `Headers`

Case-insensitive, in insertion order, with repeated names: `new Headers(init?: Headers |
Record<string, string> | [string, string][])` (pairs keep repeated names:
`new Headers([["accept", "text/html"], ["accept", "application/json"]])`), `append(name, value)`, `set`, `delete`, `get(name): string | null`
(repeated values joined with `", "`), `has`, `getSetCookie(): string[]`, `forEach((value,
name) => …)`, and `entries()`, `keys()`, `values()` and `for...of`, which yield the names
lowercased and sorted, with repeats joined (each `set-cookie` separately), as in JS. Values have
surrounding HTTP whitespace (tab, LF, CR, space) removed. Unlike JS, an invalid name or value is not rejected when it is
added: `fetch` rejects it with `IoError` `"EINVAL"`.

```ts
const h = new Headers({ "Content-Type": "text/plain" });
h.append("Set-Cookie", "a=1");
h.append("set-cookie", "b=2");
for (const [name, value] of h) {
  console.log(name, value); // content-type text/plain, then set-cookie a=1, set-cookie b=2
}
```

`console.log` prints a `Response`, `Request`, `Headers`, `AbortSignal` or `AbortController` the
way Node does, with its public state (never the runtime handle behind it):

```ts
const res = new Response("hi", { status: 201 });
console.log(res.headers); // Headers { 'content-type': 'text/plain;charset=UTF-8' }
console.log(res.status, new Headers({ a: "1" })); // 201 Headers { a: '1' }
```

## Timeouts and cancellation

```ts
import { AbortError, TimeoutError } from "velt/task";

async function status(url: string): Promise<string> {
  try {
    const res = await fetch(url, { signal: AbortSignal.timeout(2000) });
    return `${res.status}`;
  } catch (e) {
    if (e instanceof TimeoutError) {
      return "timed out";
    }
    if (e instanceof AbortError) {
      return "cancelled";
    }
    return `failed: ${e.message}`; // IoError
  }
}
```

`AbortController`, `AbortSignal` (with `AbortSignal.timeout` and `AbortSignal.any`) are global
too; their error classes `AbortError` and `TimeoutError` come from [`velt/task`](task.md).

## Differences from Node

- Errors are typed: an `IoError` with a `code` where Node throws `TypeError: fetch failed`
  with a `cause`, and the signal's `AbortError` / `TimeoutError` where Node throws a
  `DOMException` (or the abort reason).
- `res.json()` needs the type of the data (above).
- A `Request` given a signal has that signal as its `signal`; Node gives it one that follows
  it.
- `console.log` shows a `File` as a `Blob` (`File { size, type }`), without its name and time.
- `res.body` and `req.body` have no `pipeTo()`, `pipeThrough()`, `tee()` or BYOB reader
  ([streams](streams.md)).
- Mistakes JS reports with a catchable `RangeError` or `TypeError` stop the program instead,
  like an index out of bounds: `new Response(body, { status })` outside 200–599,
  a body with a status that has none (204, 205, 304), `Response.redirect(url, status)` with a
  status other than 301, 302, 303, 307 or 308, and changing the (immutable) headers of a
  fetched response or a received request.
- A compressed body cut off before its stream ends fails the read with `IoError`; Node returns
  the part that decoded. Silently truncated data is a bug source Velt does not copy.
- Response heads are accepted where Node is stricter in two ways: lines may end with a bare LF,
  and a head may be up to 400 KiB (Node: 16 KiB).
- WebAssembly programs have no network: `fetch` rejects with `IoError` `ENOTSUP`.
- `console.log` shows header names in lowercase, where Node keeps the case a name was first
  added with (`Headers { 'content-type': … }` for `"Content-Type"`). A printed `Request` shows
  Node's values for the options Velt does not have (`mode: 'cors'`, `cache: 'default'`, …), and
  its `url` as it was given (Node normalizes it: `http://a.com/` for `http://a.com`).

```ts
async function download(url: string): Promise<number> {
  const res = await fetch(url);
  let size = 0;
  const body = res.body;
  if (body != null) {
    for await (const chunk of body) {
      size += chunk.length; // or write it to a file
    }
  }
  return size;
}
```
