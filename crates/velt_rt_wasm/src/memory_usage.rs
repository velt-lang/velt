//! `process.memoryUsage()` on WebAssembly: there is no OS process to ask, so both figures are
//! the size of the module's linear memory (it only grows; freed blocks stay in it).

/// The linear memory size in bytes (0 off wasm, where this crate is only unit-tested).
fn linear_memory() -> i64 {
    #[cfg(target_arch = "wasm32")]
    {
        // In i64: 65536 pages of 64 KiB (4 GiB) overflow a 32-bit usize.
        core::arch::wasm32::memory_size(0) as i64 * 65536
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        0
    }
}

/// See velt_rt's `memory_usage`: the linear memory size.
#[no_mangle]
pub extern "C" fn velt_rt_memory_rss() -> i64 {
    linear_memory()
}

/// See velt_rt's `memory_usage`: the linear memory size.
#[no_mangle]
pub extern "C" fn velt_rt_memory_heap() -> i64 {
    linear_memory()
}
