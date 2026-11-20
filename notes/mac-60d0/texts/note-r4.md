#3202 round 4, await_chain +3.56% (x86): CAUSE FOUND, and it is not the spin or any std function on the await path. It is the codegen-unit split again, in a form @inline can't fix. Needs your decision (compiler, outside my stack).

Evidence (Mac, cross-compiled `--release --target x86_64-unknown-linux-gnu`, the same compiler for every side; objects disassembled and diffed):
- Unit counts of await_chain: std at e5f430999 (base) = 1 unit; the stack = 3 units (2 library units + the root module's own unit, units/mod.rs plan_apart). The program is ~31.3k VIR statements with the base std on Linux, 98% of the 2 x MIN_UNIT_WEIGHT = 32,000 bound; the first spin commit (+~170 statements) is simply the one that crosses it on Linux, hence your bisect. On macOS the same row is on the other side until later, and arm64 shows -0.08%.
- In ONE unit, `_V4main$poll` (the await loop is inlined there) is byte-identical for base, 8186bce99 and the head (418 instructions each). So nothing the stack adds changes the loop.
- In the 3-unit build, main$poll sits ALONE in the root unit: same calls, same inlining (every hook on the await path is in it), 421 instructions; the difference is register allocation (one more move in the loop = your +10.0M for 10M awaits), because its callees are now external symbols instead of functions of the same module.
- Base forced to 3 units with VELT_CODEGEN_UNITS=3 (plan, not plan_apart: main$poll shares a unit with 55 functions): main$poll is byte-identical to the 1-unit one. So it is the root-alone placement, not 'several units' as such.
Quick confirmation on the builder: await_chain with VELT_CODEGEN_UNITS=1 on both sides should read 0.00%.

Consequences:
- No change in std brings this row back except shrinking std below 32k statements for this program, which the next std PR undoes (the base is at 98% of the bound; #3175, 9c and 9d all add hook code).
- Moving park's spin behind a @noInline helper cannot help: park is already @noInline, never inlined into the await path, and main$poll is identical before and after in one unit.
- This is the fifth row the same bound has moved (race_select, timers_fire_phases, spawn_many on 9c PR A, await_deep, now await_chain). units/mod.rs says 'Every benchmark program is far below (MIN_UNIT_WEIGHT)': that stopped being true when the executor moved to std; an async hello is ~31k statements now.

Options (yours/owner's; I recommend 1):
1. Compiler, its own small PR (velt_codegen_llvm/src/units): a program gets one unit until it is clearly large, e.g. below 4 x MIN_UNIT_WEIGHT (64k statements; the split saves nothing measurable on a 33k program: #3173's own compile numbers were flat). It does move the bound rather than remove it, but it moves it from 'exactly where every async program sits' to twice the size of std's executor, so std PRs stop tripping it. A regression test: an async hello-sized program is one unit.
2. Compiler: keep the bound, but don't put the root module alone when the program is small (use plan's placement below some size). Same effect for this row (the forced 3-unit build is identical), smaller change in behaviour, keeps the cache benefit for large programs.
3. Accept the row as a units artefact for #3202 (you said no exceptions; listing it for completeness).

Everything else in round 4 is clean per your note (await_deep 0.00, other async rows -2.6..+0.4, gather 20/20 seq + 20/20 at load 42 and 5/5 failing with gather-back off, bounds 12/12). Meanwhile I move nothing to the PR branch until you decide; if you choose 1 or 2 I can write that PR (compiler crate, not my area: say so), and I'll take the wall-clock xwake row into the PR text when you have it.
