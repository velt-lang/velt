Findings so far (stopped for the feature freeze, no PR). macOS arm64, Homebrew clang 23.1.4, instructions retired (`/usr/bin/time -l`), X25519 per operation = (1001 iterations − 1) / 1000 of RFC 7748's chain, std from `tls-velt-chacha` (#2398). Repro programs: chain alone (`small`); chain plus an unrelated string/JSON setup function (`setup`); chain plus an HMAC-SHA256 loop, so the program uses SHA instructions (`sha`); the chain as a `velt test --release` test (`test`).

**Two separate causes.**

| X25519 M instr/op | main | main + #2418 | main + #2418, budget unlimited (`VELT_INLINE_BUDGET`) |
|---|---:|---:|---:|
| small | 0.524 | 0.527 | 0.393 |
| setup | 0.438 | 0.440 | 0.393 |
| sha | **0.991** | 0.550 | 0.395 |
| test | 0.742 | 0.741 | 0.401 |

1. **#2418 (helpers' target features) accounts for the out-of-line `velt.umulh` and `velt.simd.*`.** When a program uses SHA/PMULL, every function gets `"target-features"="+sha2"`, and clang 23 refuses to inline the attribute-less `alwaysinline` helpers into them. In the rfc8448 test binary from `tls-velt-client` there were 1808 calls to `velt.*` helpers in 169 functions, including 106 in `x25519.ladder` and 115 in `state256Arm`. Giving the helpers the same features left 0. For X25519 in `sha` this is 0.99 → 0.55 M/op. Apple clang 21 inlines them, which is why I first could not reproduce it through a `VELT_CLANG` wrapper. #2418 also covers `velt.umulh`, because it gives every attribute-less helper `+neon`, so I dropped my own fix for this.
2. **The budget (#2011) accounts for the rest, about 0.53 → 0.39 M/op (−25 %) in every program:**
   - In `small` the budget is too small for the crypto code itself. It is 5,025 and the crypto alone spends 4,718. `ladder` gets 2 of its `mul`s and 2 of its `square`s inlined. `setup` is *faster* (0.44) only because its bigger program brings a bigger budget, which the setup code did not spend: 4 `mul`s and 4 `square`s get in.
   - In `test` the test runner's generated JSON plan readers (`readJson_LA` …) spend 25.7 K of the 31.9 K budget on loop sites before the crypto is reached. Everything in `field25519` then stays a call, including `mulAdd` (9–19 statements, just above `TRIVIAL_COST`).

**Ordering by static loop depth doesn't separate these.** I computed each function's depth: the most loops the call sites on a path from a root are in. The JSON readers are depth 4 (one loop over keys per nesting level of the plan type), but `Fe25519.mul` is depth 2. A prototype that spends the budget in depth tiers (deepest first, one bottom-up pass per tier) plus a free path for leaves ≤ 24 statements made things worse: `small` 0.60 M/op, `test` 0.62, json/navigate +59 % (2.08 G → 3.32 G). Deferring the non-loop small inlines to the last tier leaves them unfunded. It is committed locally only, on branch `inline-budget` (591f07b59, not pushed, target dir deleted). The free path for tiny leaves on its own wasn't measured separately.

**What unlimited budget costs** (release, IR = velt's LLVM IR bytes, CPU = whole build, loaded machine):

| program | charged (budget → unlimited) | IR bytes | build CPU |
|---|---:|---:|---:|
| json/navigate | 16.6 K → 30.3 K | 3.64 M → 4.04 M | 1.85 → 2.10 s |
| json/parse_value | 14.9 K → 20.5 K | 3.20 M → 3.31 M | 1.64 → 1.73 s |
| json/parse_union | 10.5 K → 35.7 K | 2.15 M → 3.98 M | 1.60 → 2.34 s |
| stdport/brotli_decode | 9.8 K → 27.7 K | 2.29 M → 4.06 M | 1.41 → 2.09 s |
| stdport/url_parse | 8.9 K → 19.9 K | 1.39 M → 2.09 M | 1.06 → 1.21 s |
| nbody | 119 → 119 | unchanged | unchanged |

Almost every benchmark spends its whole budget, so any reordering changes them all. Removing the budget costs up to +85 % IR. Directions still open: a free path for tiny leaves alone (it would fix `mulAdd`-like helpers whatever the budget); slack scaled for small programs, since `small` is starved by its own size; benefit per statement (GCC-style badness) rather than depth; or keeping setup code out of the budget at the source (the test runner's plan parsing is cold).
