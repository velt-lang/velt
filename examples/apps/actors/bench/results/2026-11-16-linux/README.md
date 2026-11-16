Rerun of #182's comparison after the switch to the executor in Velt (#2649), compiler at
`0e3cc202`, against the October baseline `15cf3ee3` (the run in `2026-10-11-linux-azure`) rebuilt
on the same machine and run interleaved with it.

- A quiet 4-vCPU Linux VM (Intel Xeon @ 2.1 GHz), a different machine from 2026-10-11's Azure
  EPYC: compare `main` with `oct` within these files, not with the earlier tables. Velt
  `--release` (LLVM); @sigx/actors `6a94345`, production build, Node 22.23. oha shares the 4 vCPUs.
- In every file, `main` is `0e3cc202` and `oct` is `15cf3ee3`; `velt4` / `velt` = 4 shards,
  `velt1` = one shard (`VELT_THREADS=1` in process), `node` = sigx, `bare` = bare `node:http`.
- `cpu-inproc-http-ir.txt`: `bench/cpu.sh inproc`, `http` and `ir`, REPS=1 per build, twice,
  interleaved (`http` measured `node` and `bare` once per pass).
- `cpu-inproc-1M.txt`: `bench/cpu.sh inproc` with N1=20000, N2=1020000, three passes interleaved.
- `inproc-throughput.txt`: `bench/inproc.sh` with DURATION=2000, once per build.
- `pinning-4-shards.txt`: `actors calls --shards 4`, Δ CPU between 20k and 1.02M calls, for
  `main`, `pinA` (shards pinned to workers), `pinB` (shards and callers pinned) and `oct`;
  `cores` is CPU time over wall time. `pinning.diff` is the change behind pinA and pinB.

Summary (lower of the runs):

- Instructions per call: 11.2k–13.6k on main, 9.1k–11.0k on `15cf3ee3` (+12–25%).
- One thread, CPU per call: 1.40–2.67 µs on main, 1.32–1.92 µs before, 0.42–0.53 µs for sigx.
- 4 shards: 1.6–4.7 µs per call (8.0–8.9 µs at c=1 before), but on about one core: fan-out
  over 1000 keys at c=64/512 runs 340k/289k calls/s against 797k/775k before. The HTTP server
  also runs on one thread: 48k req/s at c=64 against 127k, at 21 µs CPU per request (19 before;
  sigx 162).
- Pinned across workers, a call costs 24–26 µs at c=1 (8.3–8.8 µs with tokio) and 6.2–6.5 µs at
  c=64/512 (4.6–4.8 µs).
- A warm call on one thread allocates 36 times (21 at `15cf3ee3`). About 20 of them come from
  `Router.send`: the envelope's `JsonValue` args are deep-copied, index maps included, when
  they cross to the shard's task.
