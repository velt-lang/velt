//! The CPU's feature word (rt_abi.md "CPU check", #3222 step 3): data only. std's start-up hook
//! (std/lang/hooks/cpu.velt) detects the CPU's features and stores the word here through
//! [`velt_rt_cpu_word_set`] before any other Velt code runs; generated code reads `velt_cpu_word`
//! as invariant (`velt_simd_cpu_word`), so the read leaves loops. Its bits are velt_common's
//! `cpu_features` raw bits: x86's in the low half, arm64's from bit 32. 0 until the hook runs, and
//! for good under Cranelift and on WebAssembly, where nothing reads it.
//!
//! It lives in the runtime rather than in std so that holding it costs a program nothing: a std
//! static would be an `Atomic`, which brings velt/sync's reclaimer into every program.

use std::sync::atomic::{AtomicU64, Ordering};

/// The feature word.
#[no_mangle]
#[allow(non_upper_case_globals)]
pub static velt_cpu_word: AtomicU64 = AtomicU64::new(0);

/// Stores the feature word: std's start-up hook, once, before any other Velt code runs.
#[no_mangle]
pub extern "C" fn velt_rt_cpu_word_set(word: u64) {
    velt_cpu_word.store(word, Ordering::Relaxed);
}

/// CPUID's register `reg` (0 to 3: EAX, EBX, ECX, EDX) for `leaf` and `subleaf`, for std's
/// detection (std/lang/cpu/detect_x86.velt): Cranelift, which builds debug programs for `--cpu`'s
/// features too, has no instruction for it. 0 off x86-64.
#[no_mangle]
pub extern "C" fn velt_rt_cpuid(leaf: u32, subleaf: u32, reg: u32) -> u32 {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: every x86-64 CPU has CPUID.
        #[allow(unused_unsafe)]
        let r = unsafe { std::arch::x86_64::__cpuid_count(leaf, subleaf) };
        [r.eax, r.ebx, r.ecx, r.edx][(reg & 3) as usize]
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = (leaf, subleaf, reg);
        0
    }
}

/// XGETBV of `xcr`: which register state the operating system saves (XCR0). Call it only where
/// CPUID says OSXSAVE (leaf 1, ECX bit 27), as std does. 0 off x86-64.
#[no_mangle]
pub extern "C" fn velt_rt_xgetbv(xcr: u32) -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        #[target_feature(enable = "xsave")]
        unsafe fn xgetbv(xcr: u32) -> u64 {
            // SAFETY: the caller checked OSXSAVE, so XGETBV exists and the OS enabled it.
            unsafe { std::arch::x86_64::_xgetbv(xcr) }
        }
        // SAFETY: the caller checked OSXSAVE (above).
        unsafe { xgetbv(xcr) }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = xcr;
        0
    }
}
