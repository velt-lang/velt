**The allocator (the checklist's "generated code calls mimalloc through `declare c`"): where it stands and a plan.** Design note before code; nobody has an open PR on it (searched open PRs and issues for mimalloc/allocator/heap hooks).

**Done (#3092, 2026-11-17).** A native release build's allocations of a constant size (objects, closure environments, counted boxes) and every free call std's hooks (std/lang/hooks/heap.velt: `mi_malloc`/`mi_free` through `declare c`, zero-size and out-of-memory checks in Velt), inlined. velt_opt's `heap_calls` swaps the runtime's calls for the hooks as its last pass; veltc's `driver/heap_hooks.rs` decides which builds get them.

**Still the runtime's** (`crates/velt_rt/src/mem.rs`: `velt_rt_alloc`, `velt_rt_realloc`, `velt_rt_free`; rt_abi.md "Memory"):
1. **Release builds:** an array buffer of a run-time length, and array growth (`velt_rt_realloc`, which also holds the array length limit check). `heap_calls` leaves them because inlining the hooks there changed how LLVM compiles the loops that fill or push (a peeled first iteration and two more induction variables: bench/hashmap +4.7%, bench/closures +4.1%). The runtime's calls don't do that: they are opaque, and codegen marks them as allocators (`"alloc-family"="velt_rt_alloc"`, allockind, allocsize; velt_codegen_llvm runtime.rs).
2. **Debug builds, and release builds linking a debug runtime:** the debug allocator (`VELT_RT_DEBUG_ALLOC`, `VELT_RC_STATS`) gives every block a header and counts it. It wraps Rust's global allocator, which the runtime's own Rust uses too, so generated code must allocate through it.
3. **The JIT (`velt dev`) and WebAssembly**, whose runtimes keep their own allocators.

**Plan, in this order:**
- **Step A (release, item 1):** the hooks for run-time sizes and for growth, without the loop regression.
  - codegen gives the `mi_*` declarations the same allocator attributes it gives `velt_rt_alloc` today (`allockind("alloc,uninitialized")`/`("realloc")`/`("free")`, `allocsize`, an alloc-family of their own), so LLVM still sees an allocator in the loop.
  - The hooks' slow paths (zero size, the array length limit, out of memory) move out of line (`@noInline`, cold), so what inlines at each site is a compare, the `mi_*` call and a null check.
  - **Measure:** the bench rows that grow arrays (hashmap, closures, array push, string builder and json rows), by instructions in the VM against main, ±3%. If a row still moves, profile it.
  - **Deletes nothing yet:** release binaries stop calling `velt_rt_realloc`, and the runtime keeps the three functions for items 2 and 3.
- **Step B (item 2, the debug allocator in std):** the header and the counters as std code over `mi_*` (or over the C library's `malloc` in a runtime without mimalloc), and generated code calls it in debug builds too. It can't go first: while velt_rt's Rust exists it allocates through Rust's global allocator, and a block must be freeable by either side. So step B waits for the runtime Rust that allocates (`str`, `bytes` and the rest) to be gone, or makes std's debug allocator the global one through a `#[global_allocator]` that calls std. That is new Rust, which #2196 rules out, so it waits.
- **Step C (item 3):** the JIT's host allocator and WebAssembly's (wasi-libc `malloc` through `declare c`) follow the same hooks once step B exists.
- **The three externs go after B and C.** Until then rt_abi.md "Memory" stays as it is.

My proposal: do step A now, as one PR (codegen attributes, hooks for the run-time-size and growth paths, `heap_calls` lifting its restriction, the VM bench table), and note B and C on this issue as waiting for the runtime's own allocating Rust to go. Corrections welcome before I start.
