//! The allocator wrappers of a release build (rt_abi.md "Memory", #2196): functions `velt_opt`'s
//! `heap_calls` adds, with `velt_rt_alloc`'s and `velt_rt_realloc`'s signatures and contracts,
//! whose bodies call std's hooks (std/lang/hooks/heap.velt). An allocation of a run-time size and
//! array growth call them; backends give them the runtime functions' allocator attributes.

/// The wrapper of `heapAlloc`: `(size: u64, align: u64) -> ptr`, as `velt_rt_alloc`.
pub const ALLOC_WRAPPER: &str = "velt.heap.alloc";

/// The wrapper of `heapRealloc`: `(p: ptr, old_size: u64, align: u64, new_size: u64) -> ptr`, as
/// `velt_rt_realloc`.
pub const REALLOC_WRAPPER: &str = "velt.heap.realloc";
