# `velt dev`: save → first response

`cargo test --release -p veltc --test reload -- --ignored --nocapture`
(`bench_save_to_first_response` in `crates/veltc/tests/reload.rs`):
- start `velt dev` on `tests/reload/hello_server/1`;
- 10 times: rewrite `main.vlt` with a new body string, then poll `GET /` every 1 ms until the
  new body arrives;
- reported: the time from the write to that response (median, min, max).

It includes the watcher's 30 ms settle delay, the whole front end, JIT or link, stopping the old
version and starting the new one.

| machine | JIT host (default) | `--exe` |
|---|---|---|
| Apple M4, macOS 26.6 (load ≈ 4–8 from other jobs) | **182 ms** (169–200) | 842 ms (831–857) |
| Linux aarch64, OrbStack VM on the same M4 (host loaded, load ≈ 20) | 312 ms (254–351) | 620 ms (423–1200) |
| Windows 11 x86_64, i9-12900HK (other agents' builds running, CPU ≈ 30–45 %) | **506 ms** (487–541) | 1623 ms (1425–2028) |

## Phase 3: hot swap vs restart (Windows 11 x86_64, i9-12900HK, 2026-10-01)

Same benchmark, now in three runs. The JIT-restart run also edits `main` (the `sleep` constant)
on every save, which forces a restart.

| run | median | min–max |
|---|---|---|
| JIT, body edit → **hot swap** (state kept) | **63.9 ms** | 54.9–68.7 |
| JIT, body + `main` edit → restart (`restarted (main changed …)`) | 378.1 ms | 335.5–443.3 |
| `--exe`, body edit → restart | 2614 ms | 2287–3369 |

- A hot swap has no process to start and no program to stop. What is left is the 30 ms settle
  delay, the front end in the running host, the diff, and JIT-compiling only the changed
  functions (3 for this edit).
- A JIT restart is slower than phase 2's plain reload: the old host builds the version first to
  decide, then the new host builds it again. Overlapping the two is a known follow-up.
- The machine was shared with other agents' builds, so treat these numbers as indicative.

## Phase 3 on Apple silicon (Apple M4, macOS 26.6, 2026-10-01)

The same three runs on arm64 macOS (`stream/platform-perf`, the merge of phase 3 with the
compile-speed stream). Another session was compiling on the machine (load average 9–17).

| run | median | min–max |
|---|---|---|
| JIT, body edit → **hot swap** (state kept) | **56.7 ms** | 53.4–68.0 |
| JIT, body + `main` edit → restart | 90.9 ms | 84.1–98.2 |
| `--exe`, body edit → restart | 419.1 ms | 371.8–427.4 |

- The swap is about the same as on Windows (63.9 ms): the 30 ms settle delay plus front end,
  diff and JIT of the changed functions.
- A JIT restart costs much less here than on Windows (378 ms); process start-up on macOS was
  ~25 ms in the phase 2 breakdown below, against ~150 ms on Windows.
- `--exe` is 2× faster than phase 2's 842 ms, thanks to the compile-speed stream (debug builds
  link the shared runtime in milliseconds). Most of what is left is macOS's first-launch check
  of each new executable.
- The aarch64 trampolines, slot loads and icache flush worked without changes. The phase 3
  tests (`cargo test -p velt_codegen_cl hot_swap`, all reload goldens in both modes) pass on
  macOS arm64, Debian 12 arm64 and Alpine arm64 (static musl `velt`).

## Where the time goes (Windows, JIT, release `velt dev -v`, 3 reloads)

| step | ms |
|---|---|
| settle after the last write | 30 |
| host front end: parse 11–13, **sema 248–276**, lower/verify/opt 1.2 | 265–295 |
| JIT (Cranelift, debug settings, unwind registration included) | 4.5–6.2 |
| start the `velt.exe` host process, stop the old version (`stop` over the pipe, drain), `go` | ~150 |
| `velt dev: reloaded in` (change → new version started) | 464–504 |

- Sema is again most of it, and slower here than on the M4 (~265 vs ~141 ms).
- Starting the host (the already-scanned `velt.exe`), stopping the old version and `go` take
  ~150 ms, more than on macOS (~25 ms): Windows process creation is slower.
- `--exe` builds in ~500 ms (sema ~275, `link.exe` ~200), but every version is a **new
  executable**, and Windows (Defender's on-access scan) takes ~1.4 s to start a freshly linked
  one the first time (measured: a hello world built by `velt build` starts in 1370–1500 ms the
  first time, 20–85 ms after that). That is the Windows counterpart of macOS's first-launch
  check, and why the JIT host is 3× faster here. Excluding the target directory from real-time
  scanning should remove it (not measured: it needs admin rights).
- No connection was refused in any run (the reload goldens' probe checks it in both modes).

## Where the time goes (macOS, JIT, one reload with `velt dev -v`)

| step | ms |
|---|---|
| settle after the last write | 30 |
| host front end: parse 3.6, **sema 141**, lower/verify/opt 0.4 | 146 |
| JIT (Cranelift, debug settings) | 2.2 |
| spawn the host, stop the old version (SIGTERM drain), `go`, first accept | ~25 |
| `velt dev: reloaded in` (change → new version started) | 204 |

- **Sema dominates.** For this program it took 2.4 ms at 5702c53 and 108–141 ms at c4064de, after
  the mutation-inference merge. With the old sema time, the same loop would take about 60 ms.
- The host compiles while the old version still serves, so this front-end time adds latency but
  no downtime.
- `--exe` also pays for linking (~50 ms) and, on macOS, for the first-launch check of every new
  executable (200–600 ms, much more under load). The Developer Tools setting in
  `docs/tooling/platforms.md` removes that check. I have not measured it with the setting enabled, because
  that needs admin rights and a System Settings change on this machine.
- For comparison, `velt run examples/http_hello.vlt` to the first response took ~240 ms in the
  design doc's measurements (before the sema regression).
