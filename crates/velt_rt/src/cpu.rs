//! The CPU extensions compiled Velt code asks about (rt_abi.md "SIMD", #1330): one byte per
//! extension, 1 when this CPU has it, set by `init` before any Velt code runs and never changed
//! after, so generated code reads it as invariant. `VELT_SIMD_DISABLE=sha` (or `clmul`, or
//! `sha,clmul`) leaves those 0, which makes `velt/simd` emulate the instructions and std use its
//! portable code (to test those paths on a CPU that has the extension).

use std::sync::atomic::AtomicU8;

/// The ARMv8 SHA-1/SHA-256 instructions (aarch64 `sha2`).
#[no_mangle]
pub static velt_cpu_sha_arm: AtomicU8 = AtomicU8::new(0);

/// The x86 SHA extensions, with the SSSE3 and SSE4.1 the code using them may also use.
#[no_mangle]
pub static velt_cpu_sha_x86: AtomicU8 = AtomicU8::new(0);

/// The x86 carry-less multiplication (PCLMULQDQ), with the SSSE3 and SSE4.1 the code using it
/// may also use.
#[no_mangle]
pub static velt_cpu_clmul_x86: AtomicU8 = AtomicU8::new(0);

/// The ARMv8 64-bit polynomial multiplication (PMULL).
#[no_mangle]
pub static velt_cpu_clmul_arm: AtomicU8 = AtomicU8::new(0);

/// Whether `VELT_SIMD_DISABLE` (a comma-separated list) names `ext`.
fn disabled(ext: &str) -> bool {
    std::env::var("VELT_SIMD_DISABLE").is_ok_and(|v| v.split(',').any(|e| e.trim() == ext))
}

/// Detect the extensions and set the flags.
pub fn init() {
    if !disabled("clmul") {
        init_clmul();
    }
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

/// Detect the carry-less multiplications.
fn init_clmul() {
    #[cfg(target_arch = "aarch64")]
    if std::arch::is_aarch64_feature_detected!("pmull") {
        velt_cpu_clmul_arm.store(1, std::sync::atomic::Ordering::Relaxed);
    }
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("pclmulqdq")
        && std::arch::is_x86_feature_detected!("ssse3")
        && std::arch::is_x86_feature_detected!("sse4.1")
    {
        velt_cpu_clmul_x86.store(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// The x86 features `velt_rt_check_cpu` checks, by bit of its mask (rt_abi.md "CPU check"; the
/// order of `velt_codegen_cl::X86_FEATURES`). `VELT_CPU_DISABLE` names them in lowercase.
const X86_FEATURES: [&str; 21] = [
    "SSE3",
    "SSSE3",
    "SSE4.1",
    "SSE4.2",
    "POPCNT",
    "CMPXCHG16B",
    "AVX",
    "AVX2",
    "BMI1",
    "BMI2",
    "FMA",
    "LZCNT",
    "MOVBE",
    "F16C",
    "AVX512F",
    "AVX512BW",
    "AVX512CD",
    "AVX512DQ",
    "AVX512VL",
    "AVX512VBMI",
    "AVX512BITALG",
];

/// The psABI levels' masks, to name the level a program was built for.
const X86_LEVELS: [(&str, u32); 3] = [
    ("x86-64-v2", 0x3f),
    ("x86-64-v3", 0x3fff),
    ("x86-64-v4", 0x7ffff),
];

/// The features of [`X86_FEATURES`] this CPU has.
fn x86_features() -> u32 {
    #[cfg(target_arch = "x86_64")]
    {
        let has: [bool; 21] = [
            std::arch::is_x86_feature_detected!("sse3"),
            std::arch::is_x86_feature_detected!("ssse3"),
            std::arch::is_x86_feature_detected!("sse4.1"),
            std::arch::is_x86_feature_detected!("sse4.2"),
            std::arch::is_x86_feature_detected!("popcnt"),
            std::arch::is_x86_feature_detected!("cmpxchg16b"),
            std::arch::is_x86_feature_detected!("avx"),
            std::arch::is_x86_feature_detected!("avx2"),
            std::arch::is_x86_feature_detected!("bmi1"),
            std::arch::is_x86_feature_detected!("bmi2"),
            std::arch::is_x86_feature_detected!("fma"),
            std::arch::is_x86_feature_detected!("lzcnt"),
            std::arch::is_x86_feature_detected!("movbe"),
            std::arch::is_x86_feature_detected!("f16c"),
            std::arch::is_x86_feature_detected!("avx512f"),
            std::arch::is_x86_feature_detected!("avx512bw"),
            std::arch::is_x86_feature_detected!("avx512cd"),
            std::arch::is_x86_feature_detected!("avx512dq"),
            std::arch::is_x86_feature_detected!("avx512vl"),
            std::arch::is_x86_feature_detected!("avx512vbmi"),
            std::arch::is_x86_feature_detected!("avx512bitalg"),
        ];
        has.iter()
            .enumerate()
            .fold(0, |mask, (bit, &on)| mask | (on as u32) << bit)
    }
    #[cfg(not(target_arch = "x86_64"))]
    0
}

/// `have` without the features `disable` (`VELT_CPU_DISABLE`, a comma-separated list such as
/// `popcnt,avx2`) names: how the check is tested on a CPU that has them.
fn without_disabled(have: u32, disable: &str) -> u32 {
    let named = |name: &str| {
        disable
            .split(',')
            .any(|d| d.trim().eq_ignore_ascii_case(name))
    };
    X86_FEATURES
        .iter()
        .enumerate()
        .filter(|(_, name)| named(name))
        .fold(have, |have, (bit, _)| have & !(1 << bit))
}

/// The error for a CPU that has `have` running code that needs `required`, naming the highest
/// level the CPU has to rebuild for; `None` when it has everything.
fn missing_message(required: u32, have: u32) -> Option<String> {
    let missing = required & !have;
    if missing == 0 {
        return None;
    }
    let names = |mask: u32| {
        let list: Vec<&str> = (0..X86_FEATURES.len())
            .filter(|b| mask & 1 << b != 0)
            .map(|b| X86_FEATURES[b])
            .collect();
        list.join(", ")
    };
    let built_for = match X86_LEVELS.iter().find(|(_, mask)| *mask == required) {
        Some((level, _)) => format!("{level} ({})", names(required)),
        None => format!("an x86-64 CPU with {}", names(required)),
    };
    let fits = X86_LEVELS.iter().rev().find(|(_, mask)| mask & !have == 0);
    Some(format!(
        "this program was built for {built_for}; this CPU lacks {}; rebuild it with \
         `--cpu {}`",
        names(missing),
        fits.map_or("baseline", |(level, _)| level)
    ))
}

/// Exit with an error unless this CPU has the features `required` (a mask over
/// [`X86_FEATURES`]): called first thing in `velt_main` by programs built for more than the
/// baseline (rt_abi.md "CPU check"), so an older CPU gets a message instead of an illegal
/// instruction. Runs after the runtime's start-up, which is baseline code.
#[no_mangle]
pub extern "C" fn velt_rt_check_cpu(required: u32) {
    let have = match std::env::var("VELT_CPU_DISABLE") {
        Ok(disable) => without_disabled(x86_features(), &disable),
        Err(_) => x86_features(),
    };
    if let Some(msg) = missing_message(required, have) {
        crate::io::flush_stdout();
        eprintln!("error: {msg}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_feature_is_named_with_the_level() {
        let v2 = X86_LEVELS[0].1;
        assert_eq!(missing_message(v2, v2), None);
        assert_eq!(missing_message(0, 0), None);
        let no_popcnt = without_disabled(v2, "POPCNT");
        assert_eq!(no_popcnt, v2 & !(1 << 4));
        assert_eq!(
            missing_message(v2, no_popcnt).as_deref(),
            Some(
                "this program was built for x86-64-v2 (SSE3, SSSE3, SSE4.1, SSE4.2, POPCNT, \
                 CMPXCHG16B); this CPU lacks POPCNT; rebuild it with `--cpu baseline`"
            )
        );
        let msg = missing_message(0b1_0001, 0).unwrap();
        assert!(msg.starts_with("this program was built for an x86-64 CPU with SSE3, POPCNT;"));
        assert!(msg.contains("lacks SSE3, POPCNT;"));
        assert_eq!(without_disabled(v2, " sse4.2 ,avx2"), v2 & !(1 << 3));
        // A CPU with a lower level is told to build for it.
        let msg = missing_message(X86_LEVELS[1].1, v2).unwrap();
        assert!(msg.ends_with("lacks AVX, AVX2, BMI1, BMI2, FMA, LZCNT, MOVBE, F16C; rebuild it with `--cpu x86-64-v2`"), "{msg}");
    }

    #[test]
    fn levels_match_the_feature_list() {
        let ends = ["CMPXCHG16B", "F16C", "AVX512VL"];
        for ((_, mask), last) in X86_LEVELS.iter().zip(ends) {
            assert_eq!(
                mask.count_ones() as usize,
                X86_FEATURES.iter().position(|f| *f == last).unwrap() + 1
            );
        }
    }
}
