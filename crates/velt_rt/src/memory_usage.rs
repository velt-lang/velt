//! `process.memoryUsage()` (std/prelude/process.vlt): the resident set size the OS reports for
//! the process, and the memory the allocator has committed for the heap.
//!
//! The release runtime does not count live bytes (that would cost every allocation an atomic
//! update), so the heap figure is mimalloc's committed-memory counter: what it took from the OS
//! and committed, including freed blocks it keeps for reuse. On Windows mimalloc reports the
//! process's private committed bytes instead. Built without mimalloc, it is the RSS.

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

#[cfg(feature = "mimalloc")]
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

#[cfg(not(feature = "mimalloc"))]
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

/// Windows: the working set from `GetProcessMemoryInfo` (kernel32's `K32` export, declared here
/// so the runtime needs no extra windows-sys features).
#[cfg(windows)]
fn rss() -> Option<usize> {
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
    (ok != 0).then_some(c.working_set_size)
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
    #[test]
    fn rss_grows_when_memory_is_touched() {
        let mut seen = vec![];
        for _ in 0..5 {
            let before = velt_rt_memory_rss();
            let block = std::hint::black_box(vec![1u8; 64 << 20]);
            let after = velt_rt_memory_rss();
            drop(block);
            if after > before {
                return;
            }
            seen.push((before, after));
        }
        panic!("rss never grew: {seen:?}");
    }
}
