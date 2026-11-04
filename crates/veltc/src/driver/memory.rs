//! The compiler's memory at the end of each stage, for `velt build --timings`: the resident set
//! and its peak so far (the peak as mimalloc, the compiler's allocator through `velt_rt_host`,
//! reports it, and on Windows the OS). A stage that keeps an earlier stage's data alive shows as a resident set that does not
//! go down after it.

use std::sync::{Arc, Mutex};

/// The order in which the stages ended and their data was freed: what bounds the compiler's
/// memory is which data is still alive in which stage (each stage's end is its name, each freed
/// value the name [`Noted`] was given).
#[derive(Clone, Debug, Default)]
pub struct Lifetimes(Arc<Mutex<Vec<&'static str>>>);

impl Lifetimes {
    pub(crate) fn push(&self, event: &'static str) {
        if let Ok(mut events) = self.0.lock() {
            events.push(event);
        }
    }

    /// The events so far, in order.
    pub fn events(&self) -> Vec<&'static str> {
        self.0.lock().map(|e| e.clone()).unwrap_or_default()
    }
}

/// A stage's output that notes in [`Lifetimes`] when it is freed.
pub(crate) struct Noted<T> {
    value: T,
    freed: &'static str,
    log: Lifetimes,
}

impl<T> Noted<T> {
    pub(crate) fn new(value: T, freed: &'static str, log: &Lifetimes) -> Self {
        let log = log.clone();
        Noted { value, freed, log }
    }
}

impl<T> std::ops::Deref for Noted<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}

impl<T> std::ops::DerefMut for Noted<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.value
    }
}

impl<T> Drop for Noted<T> {
    fn drop(&mut self) {
        self.log.push(self.freed);
    }
}

/// The end of `stage`'s line in `--timings`: its memory, if it was read.
pub(crate) fn line_end(memory: &[(&'static str, Reading)], stage: &str) -> String {
    match memory.iter().find(|(n, _)| *n == stage) {
        Some((_, r)) => format!(
            "  ({})
",
            render(*r)
        ),
        None => "
"
        .to_string(),
    }
}

/// One reading: `(resident, peak resident)` in bytes, `None` where the OS does not say.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reading {
    pub rss: usize,
    pub peak: usize,
}

/// Windows: the peak working set from kernel32, as `velt_rt`'s `memory_usage` reads its counters:
/// `mi_process_info` loads `psapi.dll` on its first call, which can hang on the loader lock.
#[cfg(windows)]
fn os_peak() -> Option<usize> {
    #[repr(C)]
    #[derive(Default)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        rest: [usize; 7],
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> *mut std::ffi::c_void;
        fn K32GetProcessMemoryInfo(
            process: *mut std::ffi::c_void,
            counters: *mut ProcessMemoryCounters,
            cb: u32,
        ) -> i32;
    }
    let mut c = ProcessMemoryCounters::default();
    let cb = std::mem::size_of::<ProcessMemoryCounters>() as u32;
    c.cb = cb;
    // SAFETY: `c` is a properly sized out buffer and `cb` says its size.
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, cb) };
    (ok != 0).then_some(c.peak_working_set_size)
}

/// The peak resident set as mimalloc, the compiler's allocator, reports it.
#[cfg(not(windows))]
fn os_peak() -> Option<usize> {
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
    let mut peak = 0usize;
    let null = std::ptr::null_mut();
    // SAFETY: mimalloc writes only the outputs that are not null.
    unsafe { mi_process_info(null, null, null, null, &mut peak, null, null, null) };
    Some(peak)
}

/// The process's resident set and its peak now.
pub fn read() -> Option<Reading> {
    let peak = os_peak().unwrap_or(0);
    // The resident set as the OS counts it (mimalloc's current figure is its committed memory
    // on some systems).
    let rss = usize::try_from(velt_rt_host::memory_usage::velt_rt_memory_rss()).unwrap_or(0);
    (rss > 0).then_some(Reading {
        rss,
        peak: peak.max(rss),
    })
}

/// `rss 812 MB, peak 900 MB` (whole megabytes).
pub fn render(r: Reading) -> String {
    let mb = |b: usize| (b + (1 << 19)) >> 20;
    format!("rss {} MB, peak {} MB", mb(r.rss), mb(r.peak))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_resident_set_within_its_peak() {
        let r = read().expect("the OS reports the resident set");
        assert!(r.rss > 0 && r.peak >= r.rss, "{r:?}");
        assert_eq!(
            render(Reading {
                rss: 3 << 20,
                peak: (5 << 20) + (1 << 19)
            }),
            "rss 3 MB, peak 6 MB"
        );
    }
}
