#3252 placement A/B: measured, and option 2 is NOT 'the PR' by your test: it keeps the run-time but not the build time. Your call between three imperfect choices; numbers below. Heavy slot released.

One release velt with a temporary switch (not committed) between the placements, same std and runtime; bench/async/race_select.velt (the 32k-64k class; 3 units on main); macOS arm64, min of 5. 'cold' = build cache off. 'edit' = build cache on and a constant of main changed before each build: the rebuild after an edit of the entry file, which is what the root's own unit exists for. 'run' = instructions retired, 1 worker.

| placement | units | cold wall | cold CPU | edit wall | units from cache on an edit | run |
| main today: bound 32k, root alone | 3 | 0.63 s | 1.15 s | 0.29 s | 2 of 3 | 417.1M |
| #3252: one unit below 64k | 1 | 0.92 s (+46%) | 1.05 s | 0.85 s (2.9x) | 0 of 1 | 408.3M (-2.1%) |
| option 2: bound 32k, plan(count), root shares a unit | 2 | 0.75 s (+19%) | 1.10 s | 0.70 s (2.4x) | 1 of 2 | 408.0M (-2.2%) |
| option 2 with count+1 units | 3 | 0.76 s (+21%) | 1.54 s | 0.74 s (2.6x) | 2 of 3 | 407.2M (-2.4%) |
(A second pass at higher machine load gave the same ratios: 0.96 / 1.34 / 1.17 s cold, 0.43 / 1.28 / 1.00 s edit.)

Reading:
- Run-time: any placement where main's code is not alone gives the one-unit numbers (timers_fire -1.8% and -1.3%, chan_wake +0.6% and 0.0% for one-unit and plan). So yes, the root-alone unit is what costs the 2%.
- Cold build: plan is better than one unit (+19% against +46%) but not 0.87 = main's.
- Edit rebuild: both lose most of what plan_apart buys. With plan, the unit main shares holds a large part of std, so an edit recompiles it: 0.29 s -> 0.70 s. One unit: 0.85 s. This is the number that hurts the edit loop of a release build, and it was not in my first report.
- I also tried to fix the cause instead of the placement (root alone, as today, but every small callee of the root module's functions imported into its unit whatever the caller's weight, and the small callees of the imported bodies): run 417.06M, no change. So the 2% is not small callees left out; it is larger std functions (Promise.race's path) that one unit inlines or specializes, plus what LLVM does with same-module internal functions. Not a cheap fix.

So the choices:
A. Keep main's placement (fastest builds, cold and edit) and accept that small async programs run ~2% slower than they could and that rows move when std grows across 32k: then #3202's await_chain needs the exception you don't want, and so will the next std PR.
B. #3252 (one unit below 64k): stable rows, -2% run; cold +0.3 s and edit rebuild 0.29 -> 0.85 s for programs between 32k and 64k statements (small async programs; from 64k on nothing changes, and debug builds are Cranelift, untouched).
C. Option 2: same run-time; cold +0.12 s, edit 0.29 -> 0.70 s; but plan's placement moves library functions between units when the program's code changes (that is why plan_apart was written), so its cache hits on real edits can be worse than this one-constant edit shows.
My recommendation: B. C buys 0.15 s over B on both build numbers at the price of a placement that is unstable under edits, and neither keeps the 0.29 s. The honest framing for the owner is run-time stability of small async programs against ~0.3-0.5 s per release build of them; if build time wins, it is A with a standing exception for rows that cross the bound.

#3252 stays as pushed (B) until you decide; the experiment code is reverted.
