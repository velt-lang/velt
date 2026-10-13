# chatbench: a realistic WebSocket app in Velt, Go, Bun and Node

A chat server with token auth (HMAC-SHA256), rooms, JSON messages, an SQLite `INSERT` per
message and a history query per join, written once per platform to one contract
([SPEC.md](SPEC.md)) and checked by one smoke test (`smoke.py`, 56 checks; all four pass).

```
velt/server.vlt   std/http + std/websocket + std/sqlite, all cores, one process
go/main.go        net/http + gorilla/websocket + mattn/go-sqlite3, all cores
bun/server.ts     Bun.serve websockets + bun:sqlite, pub/sub for rooms, one process
node/server.js    node:http + ws + better-sqlite3, one process
load/             chat-load (Rust): join, steady, idle
vm/               bench_chat.py (pinned, /proc metrics) and build.sh for a Linux VM
tls/              test-only CA and server certificate for wss:// (make.sh regenerates them)
```

## Running

```sh
python3 smoke.py 8080                          # against ws://127.0.0.1:8080 (or: smoke.py ws://127.0.0.1:8080)
load/target/release/chat-load steady ws://127.0.0.1:8080 1000 50 500 5
```

**With TLS (`wss://`)**: start any server with `TLS_CERT` and `TLS_KEY`, and give the clients a
`wss://` base (or `smoke.py --tls <port>`) and the CA in `CA`:

```sh
TLS_CERT=tls/cert.pem TLS_KEY=tls/key.pem go/chat-go 8443      # likewise the Velt binary,
                                                               # node node/server.js, bun bun/server.ts
CA=tls/ca.pem python3 smoke.py wss://127.0.0.1:8443
CA=tls/ca.pem load/target/release/chat-load steady wss://127.0.0.1:8443 1000 50 500 5
```

The certificates in `tls/` are for `127.0.0.1` and `localhost` and are **test-only keys**
(committed on purpose, never trust `tls/ca.pem` outside this benchmark); `tls/make.sh`
regenerates them with openssl.

**On a VM**: `vm/up.sh` provisions an Azure VM and builds everything; `vm/bench_chat.py [suite ...]`
runs the suites there (`join`, `steady`, `ramp`, `percore`, `idle`; servers `velt`,
`velt-per-core`, `go`, `bun`, `node`). The results below are from 2026-10-10: *shards* there is the
prototype of the per-core scheduler, run today as `velt-per-core` (`VELT_SCHEDULER=per-core`).

## Results: Azure D48as_v5, 2026-10-10

AMD EPYC 7763; every server pinned to the same 8 physical cores (one vCPU each), the load
generator on the other 16. Velt with its own WebSocket codec (since merged, #976). Raw rows:
`vm/results-20261010-d48as_v5.jsonl`.

**Steady: 10 000 clients in 100 rooms, 1 000 messages/s → 100 000 deliveries/s**

| server | p50 | p99 | server CPU | memory |
|---|---:|---:|---:|---:|
| **Velt** | **0.58 ms** | 19.9 ms | 1.05 cores | 72 MB |
| Go | 0.61 ms | **19.3 ms** | 1.35 cores | 362 MB |
| Bun | 2.0 ms | 197 ms | 0.86 (one thread) | **55 MB** |
| Node | 1 562 ms | 2 663 ms | saturated (84k of 100k deliveries/s) | 179 MB |

**Ramp: the same clients, the rate doubling until p99 > 100 ms**

| server | highest rate with p99 ≤ 100 ms | deliveries/s | p99 there | first failing rate |
|---|---:|---:|---:|---|
| **Velt** (8 cores) | 4 000/s | 396k | **40 ms** | 8 000/s: p99 689 ms, still 756k/s delivered |
| Go (8 cores) | 4 000/s | 397k | 72 ms | 8 000/s: 453k/s, p99 10.6 s |
| Bun | < 1 000/s | | | 1 000/s: p99 211 ms |
| Node | < 1 000/s | | | 1 000/s: 79k/s, p99 2.4 s |
| Velt, 1 core | < 1 000/s | | | 1 000/s: p99 295 ms, all delivered |
| Go, 1 core | < 1 000/s | | | 1 000/s: 84 % delivered, p99 5.8 s |

**Join storm: 20 000 clients connect (auth + history query each)**

| | Velt | Bun | Go | Node |
|---|---:|---:|---:|---:|
| joins/s | **30 000** | 18 300 | 14 800 | 6 600 |
| peak memory | 92 MB | 72 MB | 703 MB | 281 MB |

**Idle: 50 000 connected clients:** Bun 1.58 KB per connection (102 MB), Velt 3.66 KB (192 MB),
Node 5.56 KB (339 MB), Go 34.4 KB (1.69 GB).

## Round 3: after the fixes, database on tmpfs (2026-10-10)

The same VM type, Velt with the shard-aware HTTP server (since merged, #993) and the background
checkpointer (parked, #1022), every
server's database on `/dev/shm` so the cloud disk (whose burst credits make fsync stalls vary
from run to run) is out of the measurement. Raw rows: `vm/results-20261010-r3-tmpfs.jsonl` (and
`-r3-disk.jsonl` for the same suites on the disk).

**Ramp, p99 per rate (10 000 clients, 100 rooms, 8 cores):**

| rate (deliveries/s) | 1 000 (100k) | 2 000 (200k) | 4 000 (400k) | 8 000 (800k) | 16 000 (1.6M) | 32 000 |
|---|---:|---:|---:|---:|---:|---:|
| **Velt, shards** | 1.6 ms | 2.6 ms | 17.7 ms | 35 ms | **89 ms (1.57M/s)** | collapses |
| Velt, work stealing | 1.7 ms | 3.1 ms | 17.1 ms | **34 ms** | 115 ms (1.57M/s) | |
| Go | 1.3 ms | 2.3 ms | **18.2 ms** | 6 078 ms (608k/s) | | |
| Bun | 192 ms | | | | | |
| Node | 1 465 ms (86k/s) | | | | | |
| Velt, 1 core | 174 ms | | | | | |
| Go, 1 core | 4 893 ms (89 %) | | | | | |

Velt sustains 16 000 messages/s (1.57M deliveries/s) under 100 ms: 4× Go's limit and over 16×
Bun's and Node's. At low load Velt and Go are level (p99 1.3–1.7 ms), Velt with less CPU (4.0 vs
5.1 cores at 4 000/s) and a fifth of the memory. With the shard-aware HTTP server, shards beat work
stealing at the top (p99 89 vs 115 ms at 16 000/s, 12 % less CPU). On tmpfs the background
checkpointer makes no difference (as expected): its effect has to be measured on a steady disk.

On the disk, at 1 000/s, the background checkpointer cut Velt's p99 from 21 to 7.5 ms (Go 19.9 ms,
Bun 192 ms), but later disk runs contradicted each other (burst credits): the checkpointer is parked (#1022)
until it is measured on a disk with provisioned IOPS or local NVMe.

## What it shows

- Velt holds up in an app with auth, JSON, a database and shared state: Go's latency and
  throughput with less CPU and a fifth of its memory, about 4× the sustained load of Bun and more
  than 5× Node's, the fastest joins, and graceful degradation past the limit.
- On one core Velt is at Bun's level (both deliver everything at 1 000/s with p99 over 100 ms).
- Bun stays the leanest per idle connection (1.6 KB vs Velt's 3.7 KB).

## Findings for Velt (and what was done)

1. **SQLite checkpoints stall the only connection.** At 8 000 messages/s Velt used 3.6 of 8 cores
   with p99 0.7–0.9 s; with the database on tmpfs, or with automatic checkpoints off, the same run
   used 7.4 cores at p99 36–38 ms. SQLite's automatic checkpoint runs inside the commit that
   crosses 1 000 WAL pages and syncs the database file (slow on a cloud disk), while every other
   message waits for the connection. Prototype (parked, #1022): the runtime checkpoints in
   the background (PASSIVE every 100 ms on its own connection, RESTART past 16 MiB of WAL so the
   log starts over). At 4 000/s: p50 8–10 → 3.7–4.4 ms, p99 45 → 33 ms. Past ~8 000 messages/s
   this spec is disk-bound for any server (each message is its own transaction: about 8 KB of WAL
   plus the same again at checkpoint, against a disk of ~100 MB/s).
2. **`VELT_SCHED=shards` needed a shard-aware HTTP server**: std/http accepted on one shard, so
   every session ran on it (p99 179 ms). Fixed with an accept loop per worker (#993): p99
   9.2 ms at 1 000/s, and the best high-load numbers of all (round 3).
3. **Idle memory per connection** (3.7 KB vs Bun's 1.6 KB): the HTTP connection state kept after
   the upgrade, the session task's state and the WebSocket objects are the places to look.
4. Not measured yet: TLS (`wss://`; every server and both clients support it, see
   [Running](#running)), and the same app with batched inserts.
