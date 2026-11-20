Self-check (CONTRIBUTING "Before opening a pull request"):

1. **Node**: n/a, no golden added or changed.
2. **Docs**: no user-visible change (an internal allocation of the executor; the leak counters are a debug facility).
3. **Tests**: `starting_a_spare_counts_no_block` fails without the fix: with main's `thread_posix.velt` it panics with `blocks made with 1 and with 2 spares: left 17, right 18` (macOS); in the Linux VM the same program made 22 and 23 blocks on main's std and 20 and 20 with the fix (200 runs each, `spares=1` and `spares=2` in every run).
4. **Panics**: n/a (no path user input reaches).
5. **Performance**: not a hot path: one `calloc`/`free` instead of one counted alloc/free per thread started.
6. **Timing**: the new test orders its events by atomics and awaits, no sleeps. `cargo xtask repeat` covers Velt test files and goldens, not this Rust test, so I ran the test binary 20 at once, twice: 0 of 40 failed (each run is 20 pairs of program runs).
7. **Size**: 92 changed lines.
8. **std**: no API changed; `std-lint` passes (precheck).

How the cause was found: a temporary probe in the runtime (not in this pull request) listed every counted block with its size and thread at the exit report and again 100 ms later. The unfreed block was 144 bytes, made by `startThread` under `sched_monitor.velt` `startSpare` on the monitor's thread, and freed within 100 ms after the report. With the fix the probe shows no counted block made off the main thread in that program.

Gate, macOS arm64: `cargo xtask precheck` passed; `scripts/check.sh` passed (all selected gates: fmt, timing lint, build, clippy, unit + integration tests, doctests, goldens debug and bundled linker, VIR identity, velt fmt, std-lint; 2370 s). Linux: the VM runs above (program loop and the block counts), not a full gate. Windows: `thread_win32.velt` takes no block for a thread's start; the new test returns early there.
