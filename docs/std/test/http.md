# velt/test/http

`import "velt/test/http"`. Matchers for a `Response`, for tests of HTTP servers and clients
([Testing](../../book/testing.md)): the import adds them to `expect(response)`.

- `toHaveStatus(status)`: the response's status is `status`.
- `toHaveHeader(name, value?)`: the response has the header `name` (in any case); with `value`,
  with that value. With `.not`: that it doesn't.

```ts
import { expect, test } from "velt/test";
import "velt/test/http";

test("not modified", () => {
  const res = new Response(null, { status: 304, headers: { etag: "\"v1\"" } });
  expect(res).toHaveStatus(304);
  expect(res).toHaveHeader("etag", "\"v1\"");
  expect(res).not.toHaveHeader("content-type");
});
```
