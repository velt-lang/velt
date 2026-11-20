**Allocator step A, a correction to my plan before any code.** The plan said to give the `mi_*` declarations `allockind`. velt_codegen_llvm deliberately doesn't (runtime.rs, `Allocator`): with `allockind` LLVM deletes an allocation whose block is never read, together with the hook's null check, so a request larger than memory would no longer stop the program (`tests/golden/lang/array_from_past_memory`, `array_alloc_past_memory`). The `mi_*` declarations carry the memory effects and a `noalias` result only. So that part of the plan is wrong.

**What keeps today's code in the hot loops** is that the caller sees an opaque call carrying allocator attributes (`velt_rt_alloc`/`velt_rt_realloc`'s `allockind`, `allocsize`, `allocalign`, family), not inlined checks. So step A keeps exactly that call and changes only what is behind it:
- **The hooks.** An allocation of a run-time size goes to a new std hook `heapAllocSized` (`@noInline`: the zero-size, length-limit and out-of-memory checks, then `mi_malloc`/`mi_malloc_aligned`), and growth goes to the existing `heapRealloc` (`@noInline`). velt_opt's `heap_calls` rewrites both, no longer only constant sizes.
- **Codegen** gives those two hook *definitions* the attributes it gives `velt_rt_alloc`/`velt_rt_realloc` today: the same `allockind`, `allocsize`, `allocalign`, family and return attributes, recognised by their std hook symbols as it recognises the runtime's. Callers see the same allocator call as before, so LLVM compiles the loops as it does now; the deletion behaviour is that of today's runtime calls, which the past-memory goldens already pass with.
- **rt_abi.md "Memory"** documents which hook definitions carry which attributes, and that the `mi_*` declarations carry no `allockind`, and why.
- **The conditions stay**, adjusted:
  - **Perf:** the array-growing rows on arm64 (VM) and x86 (your builder), with hashmap and closures before and after.
  - **Debug, JIT and wasm:** a test that debug and wasm builds still call `velt_rt_alloc`/`velt_rt_realloc`.
  - **Break check:** the attributes taken off the two hook definitions should bring the hashmap/closures regression back.
  - **The slow paths in release:** a test that hits zero size, the length limit's `RangeError` text, and out-of-memory through the existing past-memory goldens.

Same scope otherwise (one PR, no Rust deleted yet). I start on this unless you object.
