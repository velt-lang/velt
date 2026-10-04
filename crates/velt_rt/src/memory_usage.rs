//! `process.memoryUsage()` (std/prelude/process.vlt): the resident set size the OS reports for
//! the process, and the memory the allocator has committed for the heap.
//!
//! The release runtime does not count live bytes (that would cost every allocation an atomic
//! update), so the heap figure is mimalloc's committed-memory counter: what it took from the OS
//! and committed, including freed blocks it keeps for reuse. On Windows it is the process's
//! private committed bytes (what mimalloc reports there too), read directly from kernel32:
//! mimalloc's `mi_process_info` loads `psapi.dll` on its first call, and loading a library while
//! the runtime's worker threads start can hang on the loader lock. Built without mimalloc, it is
//! the RSS.

/// Resident set size in bytes (0 if the OS does not say).
#[no_mangle]
pub extern "C" fn velt_rt_memory_rss() -> i64 {
    rss().map_or(0, |b| b as i64)
}

/// Bytes committed for the heap (see the module docs).
#[no_mangle]
pub extern "C" fn velt_rt_memory_heap() -> i64 {
    heap_committed().map_or_else(|| velt_rt_memory_rss(), |b| b as i64)
}

#[cfg(all(feature = "mimalloc", not(windows)))]
fn heap_committed() -> Option<usize> {
    extern "C" {
        fn mi_process_info(
            elapsed_msecs: *mut usize,
            user_msecs: *mut usize,
            system_msecs: *mut usize,
            current_rss: *mut usize,
            peak_rss: *mut usize,
            current_commit: *mut usize,
            peak_commit: *mut usize,
            page_faults: *mut usize,
        );
    }
    let mut commit = 0usize;
    let null = std::ptr::null_mut();
    // SAFETY: mimalloc writes only the non-null outputs.
    unsafe { mi_process_info(null, null, null, null, null, &mut commit, null, null) };
    (commit > 0).then_some(commit)
}

#[cfg(windows)]
fn heap_committed() -> Option<usize> {
    counters().map(|c| c.pagefile_usage).filter(|&b| b > 0)
}

#[cfg(not(any(feature = "mimalloc", windows)))]
fn heap_committed() -> Option<usize> {
    None
}

/// Linux: the second field of `/proc/self/statm` (resident pages).
#[cfg(target_os = "linux")]
fn rss() -> Option<usize> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: usize = statm.split_whitespace().nth(1)?.parse().ok()?;
    // SAFETY: a plain query.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    usize::try_from(page).ok().map(|p| pages * p)
}

/// macOS: `task_info(MACH_TASK_BASIC_INFO)`.
#[cfg(target_os = "macos")]
fn rss() -> Option<usize> {
    #[allow(deprecated)] // libc points to the mach2 crate; these bindings are still correct
    // SAFETY: `info` is a properly sized out buffer for this flavor; `count` says its size.
    unsafe {
        let mut info: libc::mach_task_basic_info = std::mem::zeroed();
        let mut count = libc::MACH_TASK_BASIC_INFO_COUNT;
        let r = libc::task_info(
            libc::mach_task_self(),
            libc::MACH_TASK_BASIC_INFO,
            &mut info as *mut libc::mach_task_basic_info as libc::task_info_t,
            &mut count,
        );
        (r == libc::KERN_SUCCESS).then_some(info.resident_size as usize)
    }
}

/// Windows: the working set from `GetProcessMemoryInfo`.
#[cfg(windows)]
fn rss() -> Option<usize> {
    counters().map(|c| c.working_set_size)
}

/// Windows `PROCESS_MEMORY_COUNTERS`.
#[cfg(windows)]
#[repr(C)]
#[derive(Default)]
struct ProcessMemoryCounters {
    cb: u32,
    page_fault_count: u32,
    peak_working_set_size: usize,
    working_set_size: usize,
    quota_peak_paged_pool_usage: usize,
    quota_paged_pool_usage: usize,
    quota_peak_non_paged_pool_usage: usize,
    quota_non_paged_pool_usage: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
}

/// The process's memory counters (kernel32's `K32GetProcessMemoryInfo`, declared here so the
/// runtime needs no extra windows-sys features and loads no library).
#[cfg(windows)]
fn counters() -> Option<ProcessMemoryCounters> {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> isize;
        fn K32GetProcessMemoryInfo(
            process: isize,
            counters: *mut ProcessMemoryCounters,
            cb: u32,
        ) -> i32;
    }
    let mut c = ProcessMemoryCounters {
        cb: std::mem::size_of::<ProcessMemoryCounters>() as u32,
        ..Default::default()
    };
    // SAFETY: `c` is a correctly sized PROCESS_MEMORY_COUNTERS; the pseudo-handle needs no close.
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) };
    (ok != 0).then_some(c)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn rss() -> Option<usize> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_positive_sizes() {
        assert!(velt_rt_memory_rss() > 0);
        assert!(velt_rt_memory_heap() > 0);
    }

    /// Other tests of this binary allocate and free concurrently, so one reading can miss the
    /// growth (another test returned memory meanwhile); one of several attempts must see it.
    /// The block comes from the system allocator, which maps a block this large fresh: the
    /// global allocator may hand back pages that other tests just freed and that are still
    /// resident, so touching them would not grow the RSS.
    #[test]
    fn rss_grows_when_memory_is_touched() {
        use std::alloc::{GlobalAlloc, Layout, System};
        let layout = Layout::from_size_align(64 << 20, 4096).unwrap();
        let mut seen = vec![];
        for _ in 0..5 {
            let before = velt_rt_memory_rss();
            // SAFETY: a non-zero layout; every byte is written before the block is freed.
            let after = unsafe {
                let block = System.alloc(layout);
                assert!(!block.is_null());
                std::ptr::write_bytes(block, 1, layout.size());
                std::hint::black_box(block);
                let after = velt_rt_memory_rss();
                System.dealloc(block, layout);
                after
            };
            if after > before {
                return;
            }
            seen.push((before, after));
        }
        panic!("rss never grew: {seen:?}");
    }
}
