**Self-check** (CONTRIBUTING.md "Before opening a pull request"):
- **Node:** n/a (module naming in the loader).
- **Docs:** none; nothing user-visible changes, except that the ICE is gone.
- **Regression tests:** velt_loader `spelling_tests.rs`. The link case fails with the retry removed (names `std/../std/x/…`). The Windows case covers both mixed spellings. On Windows, main's loader hit the ICE with mixed spellings, and this branch passes (coordinator's runs on #244).
- **No unwrap on user input, no sleeps:** `canonicalize` failures fall back to the plain name (`file_key`).
- **Benchmarks, size:** n/a. The retry runs only when a file's plain path leaves its origin's root, which is rare. `locate.rs` and the test file are under the limits.

**Gate** (`scripts/check.sh` on macOS arm64, at c5ba4d044): 2766 tests, 2765 passed. The one failure, `cpu_levels::half_dot_products_on_this_cpu_print_the_golden`, has nothing to do with the loader: this Mac's Homebrew clang 23 with `-mcpu=native` can't select `llvm.aarch64.neon.bfdot` ("Cannot select: intrinsic"). The rest of the gate passed.

**Since the gate (de7a9191a):** test-only changes, checked with precheck and `cargo test -p velt_loader spelling` (2/2 here; the Windows case compiles only on Windows):
- the reverse Windows spelling;
- the std test removed, since it couldn't bite.

**Item 4 of #244 (the ICE):** sema's construct demand didn't miss the hook. `sched_hooks` demanded the module, which was loaded, under the second name, so `prelude_fn`'s lookup by canonical name found nothing and `filter_map` dropped it without a word. Making a missing demanded hook a diagnostic at that point would be a separate change in velt_sema. I left it out of this PR; say if you want it filed.
