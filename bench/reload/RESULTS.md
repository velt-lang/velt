# `velt dev`: save → first response

`cargo test --release -p veltc --test reload -- --ignored --nocapture`
(`bench_save_to_first_response` in `crates/veltc/tests/reload.rs`):
- start `velt dev` on `tests/reload/hello_server/1`;
- 10 times: rewrite `main.velt` with a new body string, then poll `GET /` every 1 ms until the
  new body arrives;
- reported: the time from the write to that response (median, min–max).

It includes the watcher's 30 ms settle delay (`crates/veltc/src/dev/watch/mod.rs`), the front
end, JIT or link, and, for a restart, stopping the old version and starting the new one. Three
runs: a body edit in the JIT host (a hot swap: only the changed functions are compiled and the
state is kept), a body and `main` edit in the JIT host (a restart), and a body edit with
`--exe` (a new executable every time).

All three runs on 2026-10-01, compiler commit not recorded:

| run | Apple M4, macOS 26.6 (load 9–17) | Linux x86_64, 4 vCPUs (load ≈ 2.5) | Windows 11, i9-12900HK (shared) |
|---|---|---|---|
| JIT, body edit → **hot swap** | **56.7 ms** (53.4–68.0) | 56.1–59.8 ms | **63.9 ms** (54.9–68.7) |
| JIT, body + `main` edit → restart | 90.9 ms (84.1–98.2) | 92.8–94.8 ms | 378.1 ms (335.5–443.3) |
| `--exe`, body edit → restart | 419.1 ms (371.8–427.4) | 194.8–238.4 ms | 2614 ms (2287–3369) |

(Linux: two runs each, medians shown as a range.)

- A hot swap has no process to start and no program to stop: what is left is the settle delay,
  the front end in the running host, the diff and JIT-compiling the changed functions. The host
  compiles while the old version still serves, so this adds latency but no downtime.
- After a restart, `velt dev` starts the next host together with the next reload request
  (`SPECULATE` in `crates/veltc/src/dev/supervisor.rs`), so a restart does not run the front end
  twice in a row. Not on Windows: there a second host compiling beside the running one made hot
  swaps about 4× slower and restarts no faster.
- Starting a host and stopping the old version takes about 25 ms on macOS and about 150 ms on
  Windows, where process creation is slower.
- `--exe` pays for a link and for the first start of a new executable: macOS's first-launch check
  (200–600 ms, more under load; the Developer Tools setting in
  https://veltlang.com/docs/tooling/platforms/ removes it) and, on Windows, Defender's on-access
  scan (a freshly linked hello world starts in 1370–1500 ms the first time, 20–85 ms after that).
  Excluding the target directory from real-time scanning should remove the latter (not
  measured).
- No connection is refused during a reload (the reload goldens' probe checks it in both modes).
