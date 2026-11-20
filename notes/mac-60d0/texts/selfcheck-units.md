Self-check (CONTRIBUTING "Before opening a pull request"):

1. **Node**: n/a, no golden added or changed.
2. **Docs**: not user-visible in behaviour (the same programs, the same output); the unit count is described in docs/internals/contracts/cli.md, updated here. That file is a contract: the change of the bound was decided by the coordinator on crew task #126.
3. **Tests**: `a_small_program_with_tasks_is_one_codegen_unit` fails with the old bound (`bench/async/race_select.velt was split: unit objects ["race_select.cgu1.o", "race_select.cgu2.o"] beside the first`); the two unit tests in velt_codegen_llvm state the new numbers.
4. **Panics**: n/a.
5. **Performance**: in the description (compile: wall, CPU, instructions, size; runs: every bench/async row whose unit count changed). Honest reading of the compile numbers: programs between 32 000 and 64 000 statements lose the parallel clang runs, +0.4 s wall on a 1 s build, with less CPU; nothing changes from 64 000 statements on. Measured on a loaded Mac (arm64); the x86-64 rows are the builder's to confirm, await_chain first (expected to match its one-unit count). Two timer rows (timer_await, timer_await_top) are left out because one binary's count varies by ±8 % between runs here (they wait on the clock).
6. **Timing**: the new test waits for nothing.
7. **Size**: 82 changed lines.
8. **std**: untouched.

How base and head were compared: one target directory, two `velt` binaries, the base one built with `ONE_UNIT_BELOW` put back to `2 * MIN_UNIT_WEIGHT` and the source restored after; same std, same release runtime, `VELT_BUILD_CACHE=off`, outputs removed before each build; units counted from the `<exe>.cgu<N>.o` files.

Gate, macOS arm64: `cargo xtask precheck` passed; `scripts/check.sh` passed (fmt, timing lint, build, clippy, unit + integration tests, doctests, goldens debug and bundled linker, velt fmt, std-lint; 1382 s).
