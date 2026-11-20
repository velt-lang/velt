A release build splits a program into codegen units by its size: until now from 32 000 VIR statements on (2 × `MIN_UNIT_WEIGHT`). When that bound was set, the largest benchmark had about 4 000 statements. Since the executor is std code, every program with a task carries about 31 000 statements of it, so the bound sat exactly where small async programs are: each std change moved some of them across it, and five bench/async rows regressed that way in #3097's pull requests (race_select, timers_fire_phases, spawn_many, await_deep, await_chain) with no change to the code they run.

What a split costs such a program: its entry file's code gets a unit of its own (`plan_apart`), so its callees are other modules' symbols. For bench/async/await_chain.velt on x86-64 the await loop, inlined into `main`'s poll function, is byte-identical with any std in one unit, and has one more instruction per await (3.6 %) once the program is three units; forced into three units by `VELT_CODEGEN_UNITS=3`, where `main` shares a unit, it is byte-identical again.

A program is now one unit below 64 000 statements (`ONE_UNIT_BELOW`, 4 × `MIN_UNIT_WEIGHT`), twice the size of std's executor, and four units from there on, as before. Nothing changes for programs of 64 000 statements or more.

## Measurements

macOS arm64, release `velt`, build cache off, min of 5; base is this branch with the bound put back.

Compile (`velt build --release`):

| program | units | wall | CPU (user+sys) | instructions | size |
| --- | --- | --- | --- | --- | --- |
| bench/async/race_select.velt | 3 → 1 | 0.87 → 1.27 s | 1.58 → 1.45 s | −0.7 % | −0.15 % |
| examples/apps/notes-cli | 2 → 1 | 1.45 → 1.86 s | 2.25 → 2.09 s | −0.6 % | −0.09 % |
| examples/apps/actors | 5 → 5 | 6.31 → 6.31 s | 17.47 → 17.39 s | −0.2 % | same |
| tests/golden/m1/hello.velt | 1 → 1 | 0.24 → 0.24 s | 0.29 → 0.29 s | −0.1 % | same |

So a program between 32 000 and 64 000 statements builds about 0.4 s later in wall time (one clang process instead of two or three in parallel) and takes less CPU; larger programs are untouched (seek, log-pipeline, redis-clone and todo-api also stay at 5 units).

Runs (instructions retired, min of 3, 1 and 4 workers): 18 of the 28 bench/async rows were three units on main and are one now. race_select −2.1 / −2.3 %, timers_fire −1.8 / −2.1 %, timers_fire_phases −1.6 / −1.8 %, promise_identity_loop −1.7 / −1.7 %, timers_due_stored −0.4 / −1.0 %; the others within ±0.6 %. timer_await and timer_await_top wait on the clock and vary by ±8 % between runs of one binary on this machine, so they are not in the list.

## Tests

- `unit_count_from_size_unless_requested` and `large_programs_get_bounded_units` (velt_codegen_llvm) state the new bound.
- `a_small_program_with_tasks_is_one_codegen_unit` (veltc, llvm_goldens.rs) builds bench/async/race_select.velt and finds no second unit object; with the old bound it fails (`race_select.cgu1.o`, `race_select.cgu2.o`).

docs/internals/contracts/cli.md states the bound and is updated.

Part of #3097
