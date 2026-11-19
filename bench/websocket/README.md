# WebSocket benchmark: Velt vs Node vs Bun

The same tiny WebSocket server several times, driven by one Rust load generator:

```
velt/server.velt   http-server + velt/net/websocket (the codec in Velt; multi-threaded)
velt-raw/          the same server written directly on velt/net, handshake and codec included
node/server.mjs    node:http + ws 8 (the usual Node library; single-threaded)
bun/server.ts      Bun.serve websockets (uWebSockets; single-threaded), server.publish for /bcast
rust/              ws-bench-rust: MODE=raw (plain tokio-tungstenite) or MODE=hyper (hyper
                   upgrade, mutex-guarded halves); READ_BUF sets the read buffer
load/              ws-load: tokio + tokio-tungstenite load generator (THREADS=4 by default)
bench.py           starts each server fresh per scenario, samples RSS + CPU time, runs ws-load
vm/                build.sh and bench_vm.py: the many-core runs on an Azure VM
autobahn/          the Autobahn TestSuite configurations for the server and client codecs
```

Every server serves `/echo` (each message back to its sender) and `/bcast` (each message to every
connection on `/bcast`, the sender included), takes the port as `argv[1]` and listens on
127.0.0.1. Compression is off everywhere.

## Running

```sh
cargo build --release -p veltc -p velt_rt
VELT_STD=std target/release/velt build --release bench/websocket/velt/server.velt \
  -o bench/websocket/.work/velt-server
VELT_STD=std target/release/velt build --release bench/websocket/velt-raw/server.velt \
  -o bench/websocket/.work/velt-raw-server          # only for the velt-raw servers
(cd bench/websocket/node && npm ci)
(cd bench/websocket/load && cargo build --release)
(cd bench/websocket/rust && cargo build --release)
bench/websocket/bench.py                 # velt node bun rust-raw rust-hyper; QUICK=1: 3 s runs
SCENARIOS=echo-100,conns bench/websocket/bench.py velt bun
```

Results go to `results/<timestamp>.jsonl`, one line per server and scenario.

## Scenarios

| scenario | what ws-load does |
|---|---|
| echo-N | N connections, each in a closed loop: send 32 bytes, wait for the echo. Round trips/s, latency |
| echo-100-4KiB | the same with 4 KiB messages |
| bcast-1000x8 | 1000 connections on /bcast, 8 of them send in a closed loop (next message when their own comes back); deliveries/s counts every message every connection receives |
| bcast-5000x2 | 5000 connections, 2 senders |
| conns | opens up to 15 000 idle connections, waits, checks each one still echoes; KB/conn = (RSS with every connection open − idle RSS) / connections |

CPU is the server's CPU time over the run (10 s), so 10 s = one core fully busy.

## Results: many cores (Azure D48as_v5)

`vm/build.sh` and `vm/bench_vm.py` on an Azure Standard_D48as_v5 (AMD EPYC 7763, 24 cores + SMT,
Ubuntu 24.04, kernel 6.17), 2026-10-10, commit not recorded. The server runs on one vCPU per
physical core, cores 0..7, and the load generator on the other 16 cores (`taskset`), so they
never share a core. Raw rows: `vm/results-20261010-d48as_v5.jsonl` and, for `velt`,
`vm/results-20261010-codec-d48as_v5.jsonl`.

These runs predate the `http-server` package and the executor in Velt: `velt` is the server on
the runtime's HTTP server with the WebSocket codec of the time (on the velt-raw design), and the
two velt-raw rows ran on the runtime's two schedulers (tokio's work stealing, and a per-core
prototype). Those schedulers are gone: `vm/bench_vm.py`'s `velt-raw-steal` and
`velt-raw-per-core` still set `VELT_SCHEDULER`, which nothing reads any more, so both now run the
executor in Velt.

**One message in flight per connection (echo, 1000 connections), round trips/s:**

| server | 1 core | 2 | 4 | 8 | µs CPU/msg |
|---|---:|---:|---:|---:|---:|
| velt-raw, work stealing | 88k | 177k | 354k | 580k | 11.3–12.2 |
| velt-raw, per-core | 89k | 181k | 365k | **632k** | 11.0–11.9 |
| velt | 84k | 172k | 340k | 546k | 11.9–12.8 |
| rust-raw (tokio-tungstenite) | 98k | 200k | 401k | 603k | 10.0–11.0 |
| Bun | 103k | | | | 9.8 |
| Node | 65k | | | | 15.7 |

**64 messages in flight per connection (400 connections), messages/s:**

| server | 1 core | 2 | 4 | 8 | µs CPU/msg |
|---|---:|---:|---:|---:|---:|
| velt-raw, work stealing | 3.01M | 6.09M | 12.16M | 23.44M | 0.33 |
| velt-raw, per-core | **3.06M** | 6.18M | 12.63M | **24.78M** | 0.32 |
| velt | 2.30M | 4.24M | 8.76M | 16.79M | 0.44–0.47 |
| rust-raw | 0.14M | 0.29M | 0.58M | 1.21M | 6.6–7.3 |
| Bun | 2.61M | | | | 0.39 |
| Node | 0.10M | | | | 10.4 |

**Broadcast (1000 connections, 8 senders), deliveries/s:**

| server | 1 core | 2 | 4 | 8 |
|---|---:|---:|---:|---:|
| velt-raw, work stealing | 1.96M | 2.83M | 3.80M | **4.65M** |
| velt-raw, per-core | 0.92M | 3.03M | 3.87M | 3.98M |
| velt | **2.50M** | **3.44M** | **4.03M** | 4.54M |
| Bun | 1.10M | | | |
| Node | 0.11M | | | |

**Fixed load, 100k messages/s offered, server µs CPU per message:**

| threads | velt-raw, work stealing | velt-raw, per-core | rust-raw |
|---|---:|---:|---:|
| 1 | 11.2 | 11.2 | 9.9 |
| 2 | 12.2 | 11.5 | 10.8 |
| 4 | 14.0 | 13.0 | 12.6 |
| 8 | 16.4 | 16.3 | 14.7 |

Bun 10.1, Node 15.6 (Node reaches only 66k/s).

**20 000 idle connections:** velt 2.58 KB each, velt-raw 71 MB (3.07 KB), rust-raw 201 MB
(10.1 KB), Node 190 MB (6.5 KB), Bun 48 MB (1.63 KB).

What this says:

- **Velt scales with cores**: about 7× on 8 cores at one message in flight and 8× pipelined, with
  the CPU per message flat while the cores are busy.
- **velt-raw beats Bun on one core** where a connection has more than one message waiting (3.06M
  vs 2.61M) and is within 13% of it otherwise; on 8 cores it does 6× (echo) to 9.5× (pipelined)
  Bun's throughput.
- **Per-core vs work stealing**: per-core was 6–9% faster on 8 cores with lower tail latency, but
  had half the broadcast throughput on one core: work stealing runs woken writers later, so 96
  messages go out per `sendto` against 36 (p99 59 against 12 ms).
- **Broadcast scales only 2.4× on 8 cores** in velt-raw: every broadcast locks the room and wakes
  writer tasks on other threads. Topics (`subscribe`, `publish` in `velt/net/websocket`) encode a
  frame once per publish.
- **Idle wake-ups**: with more threads than the load needs, CPU per message rises by up to 45%
  (11 → 16 µs), in Rust as in Velt: workers sleep and are woken per event.

## Lessons

- **Cork writes.** A writer per connection that sends everything queued in one write
  (`Channel.tryReceive` in velt-raw) is how uWebSockets gets its broadcast numbers; one syscall
  per message per recipient costs about 8× the CPU per delivery.
- **Fan out with a loop, not `Promise.all`.** Broadcasting with `await s.send(text)` in a plain
  loop reaches 5.1–5.7M deliveries/s on one thread; `Promise.all` over 1000 sends pays a boxed
  state, a scheduler node and a promise slot per child (about 300 ns), more than a queued send.
- **Allocate read buffers when data is ready.** A connection that holds no read buffer while idle
  costs about 2.6 KB; a fixed 128 KiB buffer that is zero-filled on the first read (tungstenite's
  default) kept 131 KB per connection resident and spent 21% of CPU in `bzero`.
- **Memory per connection** is still above Bun's (2.6–3.1 KB against 1.6 KB): the connection
  task's compiled state (888 bytes in velt-raw, stored in a 1024-byte size class) and the pending
  read's leaf box are the places to look.
