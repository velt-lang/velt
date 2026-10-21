//! The CPU extensions compiled Velt code asks about (rt_abi.md "SIMD", #1330): one byte per
//! extension, 1 when this CPU has it, set by `init` before any Velt code runs and never changed
//! after, so generated code reads it as invariant. `VELT_SIMD_DISABLE=sha` leaves them 0, which
//! makes `velt:simd` emulate the instructions and std use its portable code (to test those paths
//! on a CPU that has the extension).

use std::sync::atomic::AtomicU8;

/// The ARMv8 SHA-1/SHA-256 instructions (aarch64 `sha2`).
#[no_mangle]
pub static velt_cpu_sha_arm: AtomicU8 = AtomicU8::new(0);

/// The x86 SHA extensions, with the SSSE3 and SSE4.1 the code using them may also use.
#[no_mangle]
pub static velt_cpu_sha_x86: AtomicU8 = AtomicU8::new(0);

/// Whether `VELT_SIMD_DISABLE` (a comma-separated list) names `ext`.
fn disabled(ext: &str) -> bool {
    std::env::var("VELT_SIMD_DISABLE").is_ok_and(|v| v.split(',').any(|e| e.trim() == ext))
}

/// Detect the extensions and set the flags.
pub fn init() {
    if disabled("sha") {
        return;
    }
    #[cfg(target_arch = "aarch64")]
    if std::arch::is_aarch64_feature_detected!("sha2") {
        velt_cpu_sha_arm.store(1, std::sync::atomic::Ordering::Relaxed);
    }
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("sha")
        && std::arch::is_x86_feature_detected!("ssse3")
        && std::arch::is_x86_feature_detected!("sse4.1")
    {
        velt_cpu_sha_x86.store(1, std::sync::atomic::Ordering::Relaxed);
    }
}
