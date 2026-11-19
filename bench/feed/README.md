# bench/feed: the "$12 server challenge" API

The feed API of <https://github.com/arjaythedev/twelve-dollar-server-challenge> (five endpoints
on a fixed SQLite schema, HS256 auth, byte-exact JSON) as a Velt program, to compare the CPU a
request costs with other implementations of the same API. The challenge scores how many users
one vCPU serves, so CPU per request is the number that decides it.

- `velt/server.velt`: the server on the `http-server` and `sqlite` packages, written the way an
  application would be. It passes the challenge's `test/test.sh`.
- `measure.py`: runs each server with one worker on a fresh copy of the seed database, loads it
  with wrk per endpoint and for the load test's mix, and reports server CPU per request.

```sh
git clone https://github.com/arjaythedev/twelve-dollar-server-challenge challenge
bash challenge/seed/make-seed.sh                     # feed.db and tokens.json; needs node, sqlite3
velt build --release bench/feed/velt/server.velt -o feed-velt
bench/feed/measure.py --seed challenge/seed rust=<a Rust submission's binary> velt=./feed-velt
```

The Rust bar used so far is the challenge's `rust-axum-SvetlinZarev` submission (axum, rusqlite,
per-thread read connections, a writer thread with group commit).

## Results

Apple M4, macOS, wrk on the same machine, one server worker, best of 5 rounds, 2026-10-13, on the
runtime of #1060 (merged as 802ba2b83: SQLite rows read without a JSON text round trip). The Velt
server then ran on the runtime's HTTP server, not the `http-server` package. The machine was
busy, so only server CPU per request is worth reading, and only to about ±10%.

| test | Rust axum | Velt |
|---|---:|---:|
| `GET /feed` | 23–25 µs | 25–26 µs (1.03–1.09×) |
| `GET /posts/:id` | 7–12 µs | 8–13 µs (1.08–1.12×) |

The write tests and the mix swung too much between runs to quote; creating a post cost about 2×
Rust's CPU, because the Rust server commits writes in groups on a thread of its own.
