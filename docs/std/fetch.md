# fetch

`fetch`, `Request`, `Response` and `Headers` are global, as in Node: no import. They follow the
WHATWG Fetch standard, which TypeScript's `lib.dom.d.ts` types, and run on hyper with pooled
connections (HTTP/1.1 keep-alive, and HTTP/2 when an `https://` server offers it), rustls for
HTTPS, and redirects followed as the standard says. `velt:fetch` exports the same names and
the option types (`RequestInit`, `RequestRedirect`, `ResponseInit`, `HeadersInit`, `BodyInit`)
and `BodyStream`, the type of `res.body`.

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

- `IoError` (from [`velt:io`](io.md)) when the request fails: `code` is `"ECONNREFUSED"`,
  `"ETIMEDOUT"` (connecting takes more than 10 s), `"ECONNRESET"`, … and the message starts with
  `fetch failed`, as Node's does. An invalid URL, header name or value, a GET or HEAD request
  with a body, and a scheme other than `http:` / `https:` fail with `"EINVAL"` / `"ENOTSUP"`
  before anything is sent. A redirect loop (more than 20) or a redirect with `redirect: "error"`
  fails with `fetch failed: …`.
- The signal's `AbortError` (from [`velt:task`](task.md)) when `init.signal` is aborted: a
  `TimeoutError`, which extends `AbortError`, for `AbortSignal.timeout(ms)`. Aborting cancels
  the request in the runtime (it is dropped and its connection closed), also while the body is
  received.

`RequestInit` has the standard's fields that make sense outside a browser:

| Field | Type | |
|---|---|---|
| `method` | `string` | default `"GET"`; `get`, `post`, `put`, `delete`, `head` and `options` are uppercased |
| `headers` | `Headers \| Record<string, string>` | an object literal works: `{ "content-type": "text/csv" }` |
| `body` | `string \| u8[] \| URLSearchParams` | a string sets `content-type: text/plain;charset=UTF-8` and form fields `application/x-www-form-urlencoded;charset=UTF-8`, unless one is set |
| `redirect` | `"follow" \| "error" \| "manual"` | default `"follow"` (at most 20); `"manual"` returns the 3xx response |
| `signal` | `AbortSignal` | cancels the request |
| `ca` | `string` | Velt: extra trusted CA certificates (PEM) for `https://`, e.g. a private or test CA |

Requests send `accept: */*`, `user-agent: velt` and, like Node, `accept-encoding: gzip,
deflate` (`br, gzip, deflate` over HTTPS) unless you set them. Following a redirect
works as in browsers and Node: a 303 (and a 301 or 302 after a POST) becomes a GET without a
body, and `authorization`, `cookie` and a `host` you set are not sent to another origin.

HTTPS trusts Mozilla's root certificates (compiled in, so the result does not depend on the
machine) plus `ca`. An untrusted certificate fails with `IoError`.

## `Response`

- `status: i64`, `ok: bool` (200–299), `statusText` (the server's reason phrase, else the
  standard one), `headers: Headers`, `url` (the final URL, after redirects), `redirected`,
  `type` (`"basic"` for a fetched response, `"default"` for one you made, `"error"`),
  `bodyUsed`.
- Reading the body: `await res.text()` (invalid UTF-8 becomes U+FFFD and a leading byte order
  mark is dropped, as in JS),
  `await res.json<T>()`, `await res.bytes(): u8[]` and `await res.arrayBuffer(): u8[]` (JS
  returns an `ArrayBuffer`). A body is read once: a second read throws `IoError`
  (`Body is unusable: Body has already been read`); a response without a body can be read any
  number of times, as in JS. A large body is received into one buffer
  sized from `content-length`, so `bytes()` copies it once.
- `json<T>()` decodes the body as `T` and checks it (`JsonError` names what doesn't match),
  so there is no separate schema step. Velt has no `any`: name the type (`res.json<User[]>()`)
  or give the variable one (`const users: User[] = await res.json()`); an untyped
  `res.json()` is a compile error that says so.
- `res.body` is the body as it arrives: `for await (const chunk of res.body)` yields `u8[]`
  chunks (JS: a `ReadableStream` of `Uint8Array`s), so a large download need not be held in
  memory: each chunk is at most 64 KiB, however much a compressed body expands. It is a
  `BodyStream | null`: null for a status without a body (101, 103, 204, 205, 304), for a
  `HEAD` request's response and for a response made without one. Reading it uses the body up,
  as `text()` does.
- A body the server compressed (`content-encoding: gzip`, `deflate` or `br`, or several of
  them, such as `deflate, gzip`: up to five) is decoded as it arrives, whichever way you read
  it; the headers stay as received. A list naming another coding leaves the body as sent, and
  a response without a body (a `HEAD` request's, a 204 or 304) has nothing to decode; an empty
  body is `""` in any coding, as in Node. A body
  that does not decode fails the read with `IoError` (`fetch failed: invalid compressed body:
  …`).
- The status text, URL and headers stay readable after the body was read; they are copied out
  of the runtime the first time you read them, so a response whose headers you never look at
  costs nothing for them. `headers` is read-only, as in JS (a getter): change the `Headers` it
  returns, not the property. Dropping a response whose body was not read closes its
  connection.
- `new Response(body?: string | u8[] | URLSearchParams | null, init?: ResponseInit { status?;
  statusText?; headers? })`, `Response.json(data, init?)` (`content-type: application/json`),
  `Response.error()` and `Response.redirect(url, status = 302)` build responses, e.g. for
  tests.

## `Request`

`new Request(input: string | URL | Request, init?: RequestInit)` holds what `fetch` takes:
`url`, `method`, `headers`, `redirect`, `signal` (`AbortSignal | null`: JS gives every request
a signal), `bodyUsed`, and the body readers `text()`, `json<T>()`, `bytes()`, `arrayBuffer()`.
`fetch(request)` sends it (its body counts as read afterwards); `fetch(request, init)`
overrides fields of it.

## `Headers`

Case-insensitive, in insertion order, with repeated names: `new Headers(init?: Headers |
Record<string, string>)`, `append(name, value)`, `set`, `delete`, `get(name): string | null`
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

## Timeouts and cancellation

```ts
import { AbortError, TimeoutError } from "velt:task";

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
too; their error classes `AbortError` and `TimeoutError` come from [`velt:task`](task.md).

## Differences from Node

- Errors are typed: an `IoError` with a `code` where Node throws `TypeError: fetch failed`
  with a `cause`, and the signal's `AbortError` / `TimeoutError` where Node throws a
  `DOMException` (or the abort reason).
- `res.json()` needs the type of the data (above).
- Bodies are `u8[]` (Velt has no `ArrayBuffer`, `Blob` or `FormData`), and a `Request`
  without a signal has `signal == null`.
- `res.body` is an async iterable of `u8[]`, not a `ReadableStream` (no `getReader()`,
  `pipeTo()`).
- Mistakes JS reports with a catchable `RangeError` or `TypeError` stop the program instead,
  like an index out of bounds: `new Response(body, { status })` outside 200–599,
  `Response.redirect(url, status)` with a status other than 301, 302, 303, 307 or 308, and
  changing a fetched response's (immutable) headers.
- A compressed body cut off before its stream ends fails the read with `IoError`; Node returns
  the part that decoded. Silently truncated data is a bug source Velt does not copy.
- WebAssembly programs have no network: `fetch` rejects with `IoError` `ENOTSUP`.
- **Planned**: `clone()`, and header pairs as an array of `[name, value]` tuples.

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
