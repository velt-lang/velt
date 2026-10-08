# shop-api

A JSON-heavy REST service: a product catalogue with variants, prices, attributes and ratings, and
orders priced against it. Its JSON work covers filtering, sorting and paging, typed request
decoding, bulk imports, stock-taking orders with all-or-nothing validation, and aggregate
statistics. `node/server.mjs` is the same service in idiomatic Node (`node:http`, no
dependencies). Both are seeded from the same generator, and every response is identical byte
for byte (`parity.vlt` checks 58 of them, reads and writes).

```sh
velt run                                   # http://127.0.0.1:8080, 5000 products, 20000 orders
curl 'localhost:8080/products?category=coffee&sort=-price&limit=5'
curl -X POST localhost:8080/orders -d '{"customer":{"id":1,"name":"Ada","email":"ada@example.com"},
  "items":[{"sku":"P1-1","quantity":2}],"shipping":{"line1":"1 Main St","city":"Oslo","postalCode":"0150","country":"NO"}}'
velt test                                  # tests/: validation, money, stock, paging
velt run demo.vlt                          # a scripted session (golden: demo.out)
./bench.sh [seconds] [connections]         # parity check, then Velt vs Node under load (oha)
```

`PORT`, `SHOP_PRODUCTS` and `SHOP_ORDERS` change the port and the seeded sizes.

| Route | |
|---|---|
| `GET /products?category&brand&tag&q&minPrice&maxPrice&inStock&sort&page&limit` | filtered, sorted (`id`, `price`, `-price`, `rating`, `name`, `newest`), paged (≤ 100) |
| `GET /products/:id`, `PATCH /products/:id` | one product; a partial update (validated before any change) |
| `POST /products`, `POST /products/bulk` | create; up to 1000 at once, each valid one created, the rest reported by index |
| `GET /orders?status&customer&page&limit`, `GET /orders/:id` | newest first |
| `POST /orders` | prices the lines from the catalogue, takes the stock (all or nothing): 10% off from 500.00, 25% VAT, free shipping from 300.00 |
| `POST /orders/:id/status` | pending → paid → shipped; pending or paid → cancelled (returns the stock) |
| `GET /stats` | totals, orders by status, revenue per category, top 10 products |

Errors are `{"error":{"code","message"}}`: 400 `invalid` / `bad_json`, 404 `not_found`, 405
`method_not_allowed`, 409 `conflict`. Money is in cents (`i64`); request bodies are decoded with
`JSON.parse<T>` into the types in `src/model.vlt`, which reports the path of the first mismatch
(`expected i64 at $.variants[0].stock`).

| File | What |
|---|---|
| `src/model.vlt` | the documents, request bodies and `ApiError` |
| `src/seed.vlt` | the deterministic generator (a 32-bit LCG that node mirrors with `Math.imul`), order pricing |
| `src/store.vlt` | `Store`: products with id and SKU indexes, orders, listing, validation, stock, stats |
| `src/api.vlt` | `route(store, method, path, query, body)` → `{ status, body }` (no sockets) |
| `src/server.vlt` | `std/http`: the store behind one `shared(new Mutex(…))`, each request routed while holding it |
| `node/server.mjs` | the same service on `node:http`, with decoders as strict as `JSON.parse<T>` |
| `parity.vlt` | 58 requests against both servers (via `fetch`), compared byte for byte |
| `bench.sh` | parity, then each scenario against each server with `oha` |
| `tests/api.test.vlt` / `demo.vlt` | `velt test`; the golden session |

## Performance

`./bench.sh 5 64` (5 s per scenario, 64 connections, keep-alive; a fresh server per run) on an
Apple M-series laptop (4 performance + 6 efficiency cores, macOS, Node 24), with other work
running, so treat the ratios rather than the absolute numbers as the result:

| Scenario | Velt req/s | Node req/s | Velt p50 / p99 ms | Node p50 / p99 ms | Velt / Node RSS MB |
|---|---|---|---|---|---|
| product by id (0.8 KB) | 158,984 | 22,663 | 0.33 / 1.6 | 2.1 / 12.9 | 39 / 154 |
| list 100, sorted by price (80 KB) | 651 | 217 | 96 / 210 | 154 / 2,791 | 58 / 153 |
| search + filter, 20 | 3,286 | 1,735 | 19 / 46 | 33 / 92 | 41 / 158 |
| orders page of 50 | 2,742 | 1,477 | 23 / 48 | 39 / 94 | 43 / 162 |
| stats over 20k orders | 97 | 65 | 688 / 1,356 | 256 / 4,035 | 51 / 241 |
| create order | 74,406 | 17,524 | 0.56 / 8.4 | 2.9 / 14.5 | 342 / 266 |
| bulk import 200 (60 KB body) | 2,058 | 246 | 30 / 70 | 156 / 2,729 | 57 / 289 |

Per request on one core (in-process, no HTTP), Velt is ahead everywhere: 0.27 ms vs 0.85 ms
for the sorted listing, 0.09 vs 0.13 ms to list and serialize 100 products, 4.1 vs 4.8 ms for
`stats`, and 18 vs 35 ms to seed. Under load the gap widens where HTTP handling, which runs
on every core outside the store lock, is a large part of the work, and narrows where nearly all
of it happens inside the lock
(`stats`, whose p50 is higher than Node's because 64 connections queue for the lock). "create
order" ends with more RSS in Velt because it stored 4x as many orders in the same 5 s.

## What limits it (#705)

Every request takes the store's one `Mutex`, so work that touches the store runs on one core
at a time: Velt parses HTTP and writes responses on every core, but a sort or an aggregation
can't spread out. Node is single-threaded anyway, so this is where the two are closest
(`stats`). A read-mostly service wants readers that don't block each other (a read-write
lock, or reading a `shared` value without a `Mutex`, #36); with neither available today, the
idiomatic fix is to do less inside the lock, as `listProducts` does by computing each product's
lowest price once before sorting rather than on every comparison.
