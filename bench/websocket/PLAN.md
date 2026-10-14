# WebSocket and multi-core performance: plan

Written 2026-10-10 while the work below was in progress; kept as the record of why each step was
taken. Numbers: README.md (Mac and Azure D48as_v5) and `../chat/README.md`. Each step is measured
with `bench.py` (Mac) and `vm/run.sh` (many cores) before and after. The work continues in #1023.

## Outcome (2026-10-13)

| step | landed as | effect |
|---|---|---|
| `u8[].slice`/`concat` as memcpy, `u8[].xorMask`, `utf8Valid` in Rust; `TcpStream.read` allocates once data is ready | #977 | 4 KiB echo in Velt code 2× faster; idle connections hold no read buffer |
| std/websocket's own codec (option 1A below; supersedes the 8 KiB tungstenite buffer) | #976, then #1040 dropped tungstenite | 5000 idle connections 662 → 19.8 MB; pipelined 5×, broadcast 7× |
| topics (`subscribe`, `publish`; 2 below) | #985 | a frame encoded once per publish |
| one runtime per core, listener accepted on every worker (`VELT_SCHED=shards` in this file, `VELT_SCHEDULER=per-core` today; 4 below) | #992, #993, then #211's later steps | 6–9 % more than work stealing on 8 cores, lower p99; stays opt-in |
| background SQLite checkpoints | parked, #1022 | not a uniform win on a local SSD (round 4) |
| benchmarks: this one, `../chat`, Rust baselines, velt-raw (a WebSocket server in Velt) | this directory | velt-raw: Bun on one core, 6–9.5× Bun on 8 |

## The plan as written (2026-10-10)

### 1. std/websocket at velt-raw speed (largest user-visible win)

Today std/websocket (tungstenite) costs 1.7× velt-raw's CPU per message, writes once per message
(22× slower pipelined) and holds 11 KB per connection even with the 8 KiB fix. Two ways:

- **A. A lean codec in the runtime, no API change.** Replace tungstenite in `velt_rt/src/ws` with
  our own RFC 6455 codec (the velt-raw design in Rust): no fixed buffers, allocate after
  readiness, 8-byte unmasking, UTF-8 validation. `send` resolves once the frame is queued (as in
  Bun and Node, with a high-water mark for backpressure) and a writer per socket sends everything
  queued in one write, so *every* program gets corking, also a plain `receive`/`send` echo loop.
  For plain TCP the upgraded connection is downcast to the raw `TcpStream` (no hyper layer).
- **B. std/websocket written in Velt on std/net** (velt-raw moved into std). Best dogfooding, but
  `WebSocket` is a Copy handle today; a reader buffer in Velt needs a class or shared state, which
  changes the API, and the runtime must hand over the upgraded connection as a stream.

Recommendation: **A** now (same API, every user gets it), and keep velt-raw as the proof that Velt
code reaches this speed. (Decided: A, #976.)

**A was prototyped** (codec, docs, runtime tests; goldens and the chat example passed). std/websocket, unchanged programs, 1 thread on the M4 machine
of README.md (rows not kept):

| | tungstenite (then) | 8 KiB fix | own codec | velt-raw |
|---|---:|---:|---:|---:|
| echo, 1000 clients | 163k/s | 216k/s | 217k/s | 213k/s |
| pipelined (16) | 480k/s | 506k/s | **2.48M/s** | 3.14M/s |
| broadcast 1000×8 | 316k/s | 321k/s | **2.14M/s** | 5.38M/s |
| 5000 idle connections | 662 MB | 61 MB | **19.8 MB** | 20.2 MB |

The codec supersedes the 8 KiB buffer fix. With a send that encodes at the call when the queue
has room, and the bench server broadcasting with a plain loop of `await s.send(text)` instead of
`Promise.all` over 1000 sends, std/websocket broadcast reaches **5.1–5.7M deliveries/s**, as fast
as velt-raw (5.0M). `Promise.all` was the gap: each child (an async function awaiting a runtime
send) costs a boxed state, a scheduler node and a promise slot, about 300 ns, more than the queued
send itself; `bench/async/fanout_all` shows the same per-child cost is at Node/Rust parity, so
this is idiom, not a bug. The std/websocket docs should show the loop for fan-out.

### 2. Broadcast that scales

Single-thread broadcast is solved by queued sends (above). Across cores, velt-raw broadcast
scales 2.4× on 8 cores (echo: 7×): every broadcast locks the room and wakes writer tasks on other
threads; std/websocket with queued sends wakes one writer per socket on its shard.

- **A topic API in std/websocket** (`subscribe(topic)`, `publish(topic, msg)`, like Bun's
  `server.publish`): the runtime keeps subscribers per shard, encodes a frame once, posts one
  message per shard, and each shard writes to its own subscribers (corked). No language change.
  (Landed: #985.)
- **#708 `Atom<T>`** for rooms users build themselves: lock-free reads of the member list. Follows
  #708's approved order (#36 → `Atom` → `RwLock`); the velt-raw room becomes one of its benchmarks.

### 3. Fewer idle wake-ups

With more threads than the load needs, CPU per message rises from 11 to 16 µs (Rust too): each
worker sleeps in epoll/kqueue and is woken for nearly every event. The fix (poll the I/O driver
for a few microseconds before sleeping, as Seastar does) is not possible through tokio's API. This
is where a Velt executor for the shards mode (on mio, keeping tokio's I/O types for hyper and the
database drivers) would start: the `VeltFut` ABI keeps compiled code independent of it.

### 4. The default scheduler

Shards win on throughput and latency at 8 cores; one VM run showed half the broadcast throughput
on a single shard (later reproduced on the VM and explained as a latency/throughput trade-off: README.md,
"Shards vs work stealing"). Decide after 1–2,
whether `VELT_SCHED=shards` becomes the default. (So far: it landed as the opt-in per-core
scheduler, #992, chosen automatically for a program that places a task, #1032.)

### 5. Benchmark infrastructure

- `vm/run.sh`: one command, VM deleted on exit.
- Faster VM runs: `gh workflow run release.yml --ref <branch>` builds Linux toolchain archives as
  artifacts; the VM installs one instead of building Velt (needs the branch pushed).
- Per-connection memory: velt-raw 3.07 KB vs Bun 1.63 KB. Measured part: the connection task's
  compiled state is 888 bytes (stored in the 1024-byte size class; a broadcast writer's 288 bytes
  also take 1024). The compiler already overlaps locals and child states that are never live
  together (velt_vir spill.rs), so shrinking it means looking at what stays live across the
  read loop's awaits. Cheaper first: a 512-byte size class in `with_state_store!` (the writer
  would drop from 1024 to 512, a velt-raw connection stays at 1024), the pending read's leaf box,
  the stream object's UTF-8 decoder (only `readString` needs it).

## After the plan

The chat benchmark's later rounds (`../chat/README.md`, rounds 3 and 4: background checkpoints,
topics) and the pinned HTTP run (`../web/README.md`) were measured against this plan. The next
steps are tracked in #1023.
