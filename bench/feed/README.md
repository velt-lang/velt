# bench/feed: the "$12 server challenge" API

The feed API of <https://github.com/arjaythedev/twelve-dollar-server-challenge> (five endpoints
on a fixed SQLite schema, HS256 auth, byte-exact JSON) as a Velt program, to compare the CPU a
request costs with other implementations of the same API. The challenge scores how many users
one vCPU serves, so CPU per request is the number that decides it.

- `velt/server.vlt`: the server on `std/http` + `std/sqlite`, written the way an application
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
