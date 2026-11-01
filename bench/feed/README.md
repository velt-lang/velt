# bench/feed: the "$12 server challenge" API

The feed API of <https://github.com/arjaythedev/twelve-dollar-server-challenge> (five endpoints
on a fixed SQLite schema, HS256 auth, byte-exact JSON) as a Velt program, to compare the CPU a
request costs with other implementations of the same API. The challenge scores how many users
one vCPU serves, so CPU per request is the number that decides it.

- `velt/server.vlt`: the server on `std/http` + the `sqlite` package, written the way an application
  would be. It passes the challenge's `test/test.sh` (42 checks).
- `measure.py`: runs each server with one worker on a fresh copy of the seed database, loads it
  with wrk per endpoint and for the load test's mix, and reports server CPU per request.

```sh
git clone https://github.com/arjaythedev/twelve-dollar-server-challenge challenge
bash challenge/seed/make-seed.sh                     # feed.db and tokens.json; needs node, sqlite3
velt build --release bench/feed/velt/server.vlt -o feed-velt
bench/feed/measure.py --seed challenge/seed rust=<a Rust submission's binary> velt=./feed-velt
```

The Rust bar used so far is the challenge's `rust-axum-SvetlinZarev` submission (axum, rusqlite,
per-thread read connections, a writer thread with group commit).

## First results (2026-10-13)

Apple M4, macOS, wrk on the same machine, one server worker, best of 5 rounds; the machine was
busy with other work, so only server CPU per request is worth reading, and only to about ±10%.
`velt 0.2.1` is the released toolchain with one connection and no mapping; `velt #1060` is this
server on the runtime of #1060 (SQLite rows without the JSON text round trip, #33).

| test | Rust axum | velt 0.2.1 | velt #1060 |
|---|---:|---:|---:|
| `GET /feed` | 23–25 µs | 29–32 µs (1.27–1.35×) | 25–26 µs (1.03–1.09×) |
| `GET /posts/:id` | 7–12 µs | 10–16 µs (1.3–1.4×) | 8–13 µs (1.08–1.12×) |

The write tests and the mix swung too much between runs to quote (the mix came out at
0.74–0.86× Rust's CPU for Velt in every run, creating a post at about 2× Rust's: the Rust
server commits writes in groups on a thread of its own). Run them again on a quiet Linux machine.
