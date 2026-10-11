Rerun of #182's comparison on a quiet Linux VM, compiler at `15cf3ee3`.

- Azure Standard_D8as_v5 (AMD EPYC, 8 vCPUs); every benchmark under `taskset -c 0-3` (4 vCPUs, the
  shape of #182's first measurement). Velt `--release` (LLVM); @sigx/actors `6a94345`, production
  build, Node 24.
- `run.txt`: `bench/cpu.sh inproc` and `http` (REPS=2) with `VELT_SCHEDULER=stealing` and
  `per-core`, `bench/cpu.sh ir`, then `bench/inproc.sh` (2 s per level) with each scheduler.
- `velt4` / `velt` = 4 shards (one per vCPU), `velt1` = one shard on one thread, `node` = sigx's
  `host.dispatch` (in process) or its HTTP server, `bare` = bare `node:http`.
