# WebSocket benchmark: Velt vs Node vs Bun

The same tiny WebSocket server three times, driven by one Rust load generator:

```
velt/server.vlt   std/http + std/websocket (Velt's own codec in the runtime; multi-threaded)
velt-raw/         the same server written in Velt on std/net, codec included
node/server.mjs   node:http + ws 8 (the usual Node library; single-threaded)
bun/server.ts     Bun.serve websockets (uWebSockets; single-threaded), server.publish for /bcast
rust/             ws-bench-rust: MODE=raw (plain tokio-tungstenite) or MODE=hyper (Velt's
                  runtime path: hyper upgrade, mutex-guarded halves); READ_BUF sets the buffer
load/             ws-load: tokio + tokio-tungstenite load generator (THREADS=4 by default)
bench.py          starts each server fresh per scenario, samples RSS + CPU time, runs ws-load
```

Every server serves `/echo` (each message back to its sender) and `/bcast` (each message to every
connection on `/bcast`, the sender included), takes the port as `argv[1]` and listens on
127.0.0.1. Compression is off everywhere.

**Status (2026-10-13):** the results below are from 2026-10-10 and led to std/websocket's own
codec (#976), topics (#985), faster `u8[]` and `TcpStream.read` (#977), the per-core scheduler
(#992, #993) and dropping tungstenite (#1040). Rows labelled *std/websocket (tungstenite)* or
`-8k` measure the old implementation; *shards* is the prototype of the per-core scheduler, run
today as `VELT_SCHEDULER=per-core` (`velt-raw-per-core` in `vm/bench_vm.py`). The plan and its
outcome are in [PLAN.md](PLAN.md); the work continues in #1023.

## Running

```sh
cargo build --release -p veltc -p velt_rt
VELT_STD=std target/release/velt build --release bench/websocket/velt/server.vlt \
  -o bench/websocket/.work/velt-server
(cd bench/websocket/node && npm ci)
(cd bench/websocket/load && cargo build --release)
(cd bench/websocket/rust && cargo build --release)
bench/websocket/bench.py                 # all servers, ~7 min; QUICK=1 for 3 s runs
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

## Results

Apple M4 (10 cores), macOS, Node 24.11.1, Bun 1.4.2, Velt `e0d33d7f` release build (LLVM). The
rows of the M4 runs in this section and the next two were not kept (`results/` is ignored); the
Azure runs below have their raw rows in `vm/`.
Server and load generator share the machine; another workload was running at times (load
average 3 to 6 during this run), so treat differences under ~15 % as noise.

| scenario | | Velt | Node | Bun |
|---|---|---:|---:|---:|
| echo-1 | round trips/s | 50 700 | 48 000 | 49 200 |
| | p99 | 0.04 ms | 0.05 ms | 0.05 ms |
| echo-100 | round trips/s | 157 000 | 167 000 | **188 000** |
| | CPU | 26.7 s | 10.0 s | 9.8 s |
| | peak RSS | **23 MB** | 104 MB | 34 MB |
| echo-1000 | round trips/s | 114 000 | 128 000 | **176 000** |
| | p99 | 21 ms | 15 ms | **8 ms** |
| | peak RSS | 142 MB | 151 MB | **34 MB** |
| echo-100-4KiB | round trips/s | 145 000 | 93 000 | **159 000** |
| | peak RSS | **28 MB** | 118 MB | 53 MB |
| bcast-1000x8 | deliveries/s | 273 000 | 249 000 | **544 000** |
| | peak RSS | 152 MB | 106 MB | **26 MB** |
| bcast-5000x2 | deliveries/s | 134 000 | 195 000 | **246 000** ¹ |
| | peak RSS | 660 MB | 128 MB | **35 MB** |
| conns | idle RSS | **8 MB** | 74 MB | 19 MB |
| | memory per connection | 131 KB ² | 6.5 KB | **2.4 KB** |
| | 15 000 connections open | 1.6 GB ³ | 240 MB | **54 MB** |

¹ Bun used 6.7 CPU-seconds here: the load generator (5000 receiving connections) was the limit.
² 10.7 KB with an 8 KiB tungstenite read buffer, see below.
³ Extrapolated from 12 444 connections at 131 KB each. That run hit the client's ephemeral-port
limit, because ports from the previous scenario were still in TIME_WAIT (bench.py now waits first).

### Takeaways

- **Bun wins** almost everything: uWebSockets on one core does the most work per CPU-second, and
  `server.publish` (one encoded frame, corked writes to every subscriber) is 2× anyone's broadcast.
- **Velt** has the smallest process (8 MB idle) and keeps up on throughput: level with Node on
  32-byte echo, ahead of Node on 4 KiB messages and broadcast. It gets there by using 2 to 3 cores
  where Node and Bun use one, so per CPU-second it is the least efficient of the three.
- **Memory per connection is Velt's big loss**, 131 KB against Node's 6.5 KB. The runtime
  creates tungstenite connections with the default config, and tungstenite zero-fills its whole
  128 KiB read buffer on the first read (`FrameCodec::read_in`), so every connection keeps
  128 KiB resident, idle or not. With `WebSocketConfig::default().read_buffer_size(8 * 1024)` in
  `velt_rt/src/ws` (accept and connect), Velt measured **10.7 KB per connection** (15 000
  connections in 169 MB; peak 25 MB instead of 142 MB at echo-1000, 72 MB instead of 660 MB at
  bcast-5000x2). Throughput was within noise in an A/B run.
- Node's ~74 MB baseline is V8 plus the `ws` library; its throughput is good but its p99 tails
  are the widest at 100 connections.

## Investigating: Velt against Rust on the same stack

`bench.py velt velt-8k velt-1t rust-hyper rust-hyper-8k rust-hyper-1t rust-raw rust-raw-8k node bun`
(`-8k`: 8 KiB tungstenite read buffer, Velt's then built against a patched runtime; the variant
is gone with tungstenite; `-1t`: one worker thread). Throughput is noisy here (another workload
shared the machine), CPU per message less so.

| | Velt | Rust (hyper path) | Rust (raw) | Node | Bun |
|---|---:|---:|---:|---:|---:|
| 15 000 idle connections | 1 927 MB | 1 911 MB | 1 913 MB | 167 MB | 54 MB |
| … with an 8 KiB read buffer | 165 MB | 146 MB | 148 MB | | |
| memory per connection | 131 KB → 10.7 KB | 130 KB → 9.8 KB | 130 KB → 9.9 KB | 6.3 KB | 2.3 KB |
| echo-1000 round trips per CPU-second, all cores | 44 000 | 48 000 | 53 000 | 126 000 | 160 000 |
| … one worker thread | 93 000 | 141 000 | | | |
| echo-100-4KiB per CPU-second, one worker thread | 132 000 | 116 000 | | 77 000 | 154 000 |
| bcast-1000x8 deliveries per CPU-second, one worker thread | 266 000 | 253 000 | 86 000 ⁴ | 228 000 | 2 140 000 |

⁴ all cores; a writer task per connection.

**Velt costs about what Rust on the same libraries costs.** The gaps to Node and Bun come from
the stack (tokio + tungstenite as the runtime uses them), not from compiled Velt code. Profiles
(`samply`, echo-1000; velt code, leaf futures and the registry are each under 2 % of CPU):

1. **Memory: tungstenite's 128 KiB default read buffer.** `FrameCodec::read_in` resizes the buffer
   to its full capacity (zero-filled) before every read, so all 128 KiB stay resident per
   connection. Fix: `WebSocketConfig::read_buffer_size(8 * 1024)` in `velt_rt/src/ws` (accept and
   connect): 131 → 10.7 KB per connection.
2. **CPU: the same zero-fill, on every message.** With one worker, `__bzero` is **21 %** of
   Velt's CPU (zeroing up to 128 KiB per incoming frame). The 8 KiB buffer cuts it 16×.
3. **CPU: tokio's multi-threaded scheduler.** At similar throughput, 10 workers use ~2.4× the CPU
   of one (`__psynch_cvwait`/`cvsignal`: workers parked and woken for every I/O event). Rust has
   the same cost. Per-message work is tiny, so waking threads dominates.
4. **Broadcast: one syscall per message per recipient.** Every `send` is feed + flush (one
   `sendto`), copies the text and boxes a future. Bun corks: the messages a socket receives in
   one loop iteration go out in one write, so it delivers ~8× more per CPU-second. Coalescing
   writes (flush once when no more sends are queued) and sharing one encoded frame across
   recipients would close much of that.
5. What is left per message, single-threaded, is `sendto` + `recvfrom` (64 % of CPU): the floor
   for one message in flight per connection, the same for every server.

## velt-raw: a WebSocket server written in Velt

`velt-raw/server.vlt` does the HTTP/1.1 handshake and RFC 6455 framing itself, on `std/net`
(no tungstenite). Broadcast encodes each frame once and queues it for every member; each
connection's writer task sends everything queued in one write (`Channel.tryReceive`), which is how
uWebSockets corks. It needed three std/runtime changes that every Velt program gets:

- `u8[].slice` / `concat` as one memcpy (they were element-by-element: 20 % of CPU on 4 KiB echo),
  `utf8Valid` through Rust's validator (14 %), and `u8[].xorMask` for unmasking (16 %);
- `TcpStream.read` allocates its buffer once data is ready, so idle connections hold none.

One worker thread (`VELT_THREADS=1`) against Bun; quiet machine except where noted:

| scenario | velt-raw | Bun | |
|---|---:|---:|---|
| echo-1, round trips per CPU-second | 154 000 | 104 000 | **Velt +48 %** |
| echo-100, round trips/s | 219 000 | 224 000 | tie |
| echo-1000, round trips/s | 201 000 | 198 000 | tie |
| echo-100-4KiB, round trips/s ⁵ | 167 000 | 182 000 | Bun +9 % |
| bcast-1000x8, deliveries/s | 4 790 000 | 1 970 000 | **Velt 2.4×** |
| idle process | 6.7 MB | 19 MB | **Velt** |
| memory per connection | 2.7 KB | 2.1 KB | Bun |
| 15 000 connections open | 46 MB | 51 MB | **Velt** |

⁵ the second run, with a busier machine.

With every core (the default), velt-raw does *worse* than with one: tokio's work-stealing
scheduler wakes parked workers for every I/O event, and the room's lock and the writer tasks
bounce between threads. The default worker count is the next thing to look at for I/O-bound
servers.

## Many cores: Azure D48as_v5 (2026-10-10)

`vm/build.sh` and `vm/bench_vm.py` on an Azure Standard_D48as_v5 (AMD EPYC 7763, 24 cores + SMT,
Ubuntu 24.04, kernel 6.17). The server runs on one vCPU per physical core, cores 0..7, and the load
generator on the other 16 cores (`taskset`), so they never share a core. One EPYC vCPU is about
half an M4 performance core, so compare scaling and ratios. Raw rows:
`vm/results-20261010-d48as_v5.jsonl`; the rows for std/websocket's own codec (#976), from a second
run on the same VM type, are in `vm/results-20261010-codec-d48as_v5.jsonl`.

**One message in flight per connection (echo, 1000 connections), round trips/s:**

| server | 1 core | 2 | 4 | 8 | µs CPU/msg |
|---|---:|---:|---:|---:|---:|
| velt-raw, work stealing | 88k | 177k | 354k | 580k | 11.3–12.2 |
| velt-raw, shards | 89k | 181k | 365k | **632k** | 11.0–11.9 |
| rust-raw (tokio-tungstenite) | 98k | 200k | 401k | 603k | 10.0–11.0 |
| velt, std/websocket (tungstenite, replaced) | 54k | 102k | 192k | 328k | 18.6–24.4 |
| **velt, std/websocket (own codec, #976)** | 84k | 172k | 340k | 546k | 11.9–12.8 |
| Bun | 103k | | | | 9.8 |
| Node | 65k | | | | 15.7 |

**64 messages in flight per connection (400 connections), messages/s:**

| server | 1 core | 2 | 4 | 8 | µs CPU/msg |
|---|---:|---:|---:|---:|---:|
| velt-raw, work stealing | 3.01M | 6.09M | 12.16M | 23.44M | 0.33 |
| velt-raw, shards | **3.06M** | 6.18M | 12.63M | **24.78M** | 0.32 |
| rust-raw | 0.14M | 0.29M | 0.58M | 1.21M | 6.6–7.3 |
| velt, std/websocket (tungstenite, replaced) | 0.13M | 0.26M | 0.52M | 1.08M | 7.5–8.1 |
| **velt, std/websocket (own codec, #976)** | 2.30M | 4.24M | 8.76M | 16.79M | 0.44–0.47 |
| Bun | 2.61M | | | | 0.39 |
| Node | 0.10M | | | | 10.4 |

**Broadcast (1000 connections, 8 senders), deliveries/s:**

| server | 1 core | 2 | 4 | 8 |
|---|---:|---:|---:|---:|
| velt-raw, work stealing | **1.96M** | 2.83M | 3.80M | **4.65M** |
| velt-raw, shards | 0.92M | 3.03M | 3.87M | 3.98M |
| velt, std/websocket (tungstenite, replaced) | 0.13M | 0.23M | 0.15M | 0.16M |
| **velt, std/websocket (own codec, #976)** | **2.50M** | **3.44M** | **4.03M** | 4.54M |
| Bun | 1.10M | | | |
| Node | 0.11M | | | |

**Fixed load, 100k messages/s offered, server µs CPU per message:**

| threads | velt-raw steal | velt-raw shards | rust-raw |
|---|---:|---:|---:|
| 1 | 11.2 | 11.2 | 9.9 |
| 2 | 12.2 | 11.5 | 10.8 |
| 4 | 14.0 | 13.0 | 12.6 |
| 8 | 16.4 | 16.3 | 14.7 |

Bun 10.1, Node 15.6 (Node reaches only 66k/s).

**20 000 idle connections:** velt with tungstenite (replaced) 2 572 MB (131 KB each), with its
8 KiB read buffer 223 MB (10.9 KB), velt with the own codec 2.58 KB each (codec run), velt-raw 71 MB (3.07 KB), rust-raw 201 MB (10.1 KB), Node 190 MB (6.5 KB), Bun
48 MB (1.63 KB).

What this says:

- **Velt scales with cores**: 7.1× on 8 cores at one message in flight, 8.1× pipelined, and the
  CPU per message stays flat while the cores are busy. The "more threads is slower" of the M4
  machine above came from client and server sharing four performance cores, not from Velt.
- **velt-raw beats Bun on one core** where a connection has more than one message waiting (3.06M
  vs 2.61M) and is within 13 % of it otherwise; on 8 cores it does 6× (echo) to 9.5× (pipelined)
  Bun's throughput.
- **Shards vs work stealing**: shards are 6–9 % faster on 8 cores and have lower tail latency, but
  half the broadcast throughput on one core. Reproduced in a later run (rows not kept), it is a latency/throughput
  trade-off, not a bug: work stealing runs woken writers later, so 96 messages go out per `sendto`
  against 36 on a shard (p99 59 against 12 ms). Yielding in the writer before each write was tried
  and rejected (20–50 % slower).
- **Broadcast scales only 2.4× on 8 cores**: every broadcast locks the room and wakes writer tasks
  on other threads. Since then std/websocket encodes a broadcast once per publish (topics,
  #985); #708's `Atom` for member lists is still open.
- **Idle wake-ups**: with more threads than the load needs, CPU per message rises by up to 45 %
  (11 → 16 µs), in Rust as in Velt: workers sleep and are woken per event.
- **std/websocket** with tungstenite needed 1.7× velt-raw's CPU per message, did not batch
  writes, and held 131 KB per connection. Its own codec (#976), on the velt-raw design, replaced
  it: on 8 cores echo 546k/s, pipelined 16.8M/s, broadcast 4.5M/s (faster than velt-raw's room),
  and 2.58 KB per idle connection.
