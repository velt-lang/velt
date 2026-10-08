# shop-api

A JSON-heavy REST service: a product catalogue with variants, prices, attributes and ratings, and
orders priced against it. Its JSON work covers filtering, sorting and paging, typed request
decoding, bulk imports, stock-taking orders with all-or-nothing validation, and aggregate
statistics. `node/server.mjs` is the same service in idiomatic Node (`node:http`, no
dependencies). Both are seeded from the same generator, and every response is identical byte
for byte (`parity.vlt` checks 61 of them, reads and writes).

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
| `parity.vlt` | 61 requests against both servers (via `fetch`), compared byte for byte |
| `bench.sh` | parity, then each scenario against each server with `oha` (raw results: `bench-results/`) |
| `inprocess.vlt` / `node/inprocess.mjs` | per-request cost on one core, without HTTP |
| `tests/` | `velt test`: the API through `route`, and concurrent orders against the real server |
| `demo.vlt` | a scripted session (golden: `demo.out`) |

## Performance

**Basis.** The Velt server uses every core (the default runtime), while Node runs its JavaScript
on one thread. Several ratios below depend on that, and the next section shows where Velt's
extra cores don't help. All numbers come from scripts in this directory. `bench.sh` needs
`oha`, `node`, `curl` and `jq`.

`RESULTS=bench-results ./bench.sh 5 64` gives 5 s per scenario, 64 keep-alive connections and a
fresh server per run. It was run on an Apple M-series laptop (4 performance + 6 efficiency
cores), quiet (load average about 1.3), with velt `ff4da4b5`, Node 24.11.1 and oha 1.16.0. The
raw oha output is in `bench-results/`.

| Scenario | Velt req/s | Node req/s | Velt p50 / p99 ms | Node p50 / p99 ms | Velt / Node RSS MB |
|---|---|---|---|---|---|
| product by id (0.8 KB) | 208,187 | 102,480 | 0.29 / 0.66 | 0.56 / 1.22 | 39 / 154 |
| list 100, sorted by price (80 KB) | 3,590 | 1,109 | 17.9 / 32.9 | 56.4 / 115.3 | 60 / 155 |
| search + filter, 20 | 9,907 | 9,697 | 6.4 / 12.3 | 6.1 / 12.2 | 42 / 162 |
| orders page of 50 | 9,464 | 7,363 | 6.7 / 12.6 | 8.4 / 16.9 | 43 / 159 |
| stats over 20k orders | 317 | 210 | 205 / 409 | 164 / 3,128 | 51 / 274 |
| create order | 186,333 | 85,536 | 0.33 / 0.72 | 0.67 / 1.53 | 812 / 498 |
| bulk 200: decode + reject (60 KB body) | 6,030 | 921 | 10.4 / 22.8 | 68.9 / 178.3 | 55 / 325 |

- **Bulk row:** only the first bulk request creates anything. Its SKUs exist afterwards, so
  every later request decodes 200 products and rejects each one with a conflict. The row
  measures decoding and validation, not creation.
- **"create order" memory:** Velt ends with more RSS because it stored over twice as many orders
  in the same 5 s (931k vs 428k).

**Per request on one core,** with no HTTP: `velt run --release inprocess.vlt` and `node node/inprocess.mjs` time `route` on the same seeded
shop. Same machine, same quiet period:

| In-process, per request | Velt | Node |
|---|---|---|
| seed 5000 products + 20000 orders | 25.3 ms | 32.1 ms |
| `stats` | 3.15 ms | 4.46 ms |
| list 100, sorted by price | 0.25 ms | 0.83 ms |
| list 100 + serialize | 0.082 ms | 0.124 ms |
| product by id | 0.8 µs | 1.1 µs |

**Checking the parity check:** a Node server seeded with one order fewer
(`SHOP_ORDERS=19999 PORT=8081 node node/server.mjs`) makes `parity.vlt` report more than 10
differences.

**Numbers above 2^53:** `JSON.parse<i64>` reads integers exactly, while Node's numbers round
(`9007199254740993` becomes `…992`). They agree on `2.0`, `2e0` and out-of-range values. Both
servers therefore bound the integers they echo back (price amounts, customer ids), and
`parity.vlt` covers those cases.

## What limits it (#705)

Every request takes the store's one `Mutex`, so work that touches the store runs on one core at
a time. Inside the lock that includes routing, decoding the request body and `JSON.stringify`
of the response (`src/server.vlt`). Only HTTP parsing and writing run on every core.

A sort or an aggregation can't spread out, and Node is single-threaded anyway, so this is where
the two are closest: `search + filter` (level) and `stats`. In `stats`, Velt's p50 is higher than
Node's because 64 connections queue for the lock.

A read-mostly service wants readers that don't block each other: a read-write lock, or reading
a `shared` value without a `Mutex` (#36, designed in #708). With neither available today, the
idiomatic fix is to do less inside the lock. `listProducts` does that by computing each
product's lowest price once before sorting, rather than on every comparison.
