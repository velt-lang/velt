# velt:url

A WHATWG-style `URL` and `URLSearchParams` (global, as in Node; `velt:url` exports them too),
plus JS's URI encoding functions (`import { encodeURIComponent } from "velt:url"`).

- `new URL(input, base: string | null = null)`: throws `UrlError` (`Invalid URL: …`, or
  `Invalid base URL: …` when `base` does not parse).
  `URL.parse(input, base = null): URL | null`. `URL.canParse(input, base = null)`.
- Getters: `href protocol username password host hostname port pathname search hash origin`.
- Setters: everything except `origin`. Invalid values are ignored, as in JS, except `href`,
  which throws.
- `searchParams` (get) returns a **copy**. Write changes back with `u.searchParams = params`.
- `toString()` and `toJSON()` both return `href`.
- `new URLSearchParams(init = "")`: a leading `?` is skipped.
  - `size`, `get(name): string | null`, `getAll`, `has(name, value?)`
  - `append`, `set`, `delete(name, value?)`, `sort()` (stable)
  - `keys()`, `values()`, `entries()`: arrays, not iterators
  - `toString()`: form encoding (space becomes `+`)
- `encodeURIComponent`, `encodeURI`, `decodeURIComponent`, `decodeURI`: they throw
  `UrlError("URI malformed")` where JS throws `URIError`: the encoders on a lone surrogate, the
  decoders on a bad escape or invalid UTF-8.

```ts
import { URL, URLSearchParams, encodeURIComponent } from "velt:url";

function main() {
  const u = new URL("../api/items?page=2#top", "https://Example.com:443/app/v1/index.html");
  console.log(u.href); // https://example.com/app/api/items?page=2#top
  const params = u.searchParams;
  params.set("q", "red shoes");
  u.searchParams = params;
  console.log(u.href); // https://example.com/app/api/items?page=2&q=red+shoes#top
  console.log(new URLSearchParams("a=1&b=x+y").get("b"), encodeURIComponent("a&b/c"));
}
```

Notes / gaps:
- Special schemes (`http https ws wss ftp file`) get lowercased hosts, dropped default ports,
  `\` treated as `/`, dot-segment removal, and IPv4 normalization (`127.1` becomes `127.0.0.1`).
- No IDNA/punycode: a non-ASCII host is invalid.
- IPv6 literals are validated loosely and not compressed.
- `file:` URLs have no Windows drive-letter quirks.
