//! `--cpu`: the CPU generated code is for (docs/tooling/platforms.md "CPU"), shared with the LLVM
//! backend so both read a name the same way:
//! - `baseline` (alias `x86-64`): the target's first generation (SSE2 on x86_64, armv8-a);
//! - `x86-64-v2`, `x86-64-v3`, `x86-64-v4`: the x86-64 psABI levels; `x86-64-v2` is the default
//!   on x86_64 ([`DEFAULT_X86_CPU`]);
//! - `native`: the machine the compiler runs on (only when it builds for its own architecture);
//! - any other name is a CPU name clang knows for the target (`znver4`, `skylake`, `neoverse-n1`;
//!   the list is `cpu_names.rs`): clang gets it as is, Cranelift uses its preset of that name
//!   where it has one (x86) and the baseline otherwise.
//!
//! Code for a CPU above the baseline checks at start-up that the CPU has what it was built for
//! ([`required_features`] for Cranelift's code, the LLVM backend's own for clang's;
//! `velt_rt_check_cpu` in rt_abi.md "CPU check").

use cranelift_codegen::isa::{self, Builder as IsaBuilder};
use cranelift_codegen::settings::{self, Configurable};
use target_lexicon::{Architecture, Triple};

use crate::cpu_names;

/// The x86_64 CPU code is for unless `--cpu` (or `VELT_CPU`, or the package's `build.cpu`) names
/// another: x86-64-v2 (SSE4.2, POPCNT, SSSE3; every x86_64 CPU since about 2009, and what
/// Windows 11 24H2 and RHEL 9 require).
pub const DEFAULT_X86_CPU: &str = "x86-64-v2";

/// The x86 features the start-up check knows, by bit of its mask: the name `velt_rt_check_cpu`
/// reports and the Cranelift ISA flag (`None`: Cranelift does not use the feature). The order is
/// the runtime's (crates/velt_rt/src/cpu.rs), part of rt_abi.md "CPU check".
pub const X86_FEATURES: [(&str, Option<&str>); 21] = [
    ("SSE3", Some("has_sse3")),
    ("SSSE3", Some("has_ssse3")),
    ("SSE4.1", Some("has_sse41")),
    ("SSE4.2", Some("has_sse42")),
    ("POPCNT", Some("has_popcnt")),
    ("CMPXCHG16B", Some("has_cmpxchg16b")),
    ("AVX", Some("has_avx")),
    ("AVX2", Some("has_avx2")),
    ("BMI1", Some("has_bmi1")),
    ("BMI2", Some("has_bmi2")),
    ("FMA", Some("has_fma")),
    ("LZCNT", Some("has_lzcnt")),
    ("MOVBE", None),
    ("F16C", None),
    ("AVX512F", Some("has_avx512f")),
    ("AVX512BW", None),
    ("AVX512CD", None),
    ("AVX512DQ", Some("has_avx512dq")),
    ("AVX512VL", Some("has_avx512vl")),
    ("AVX512VBMI", Some("has_avx512vbmi")),
    ("AVX512BITALG", Some("has_avx512bitalg")),
];

/// The macro clang defines (`clang -dM -E -march=<cpu>`) for each feature of [`X86_FEATURES`], in
/// its order: how the LLVM backend learns what a named CPU lets clang use.
pub const X86_FEATURE_MACROS: [&str; 21] = [
    "__SSE3__",
    "__SSSE3__",
    "__SSE4_1__",
    "__SSE4_2__",
    "__POPCNT__",
    "__GCC_HAVE_SYNC_COMPARE_AND_SWAP_16",
    "__AVX__",
    "__AVX2__",
    "__BMI__",
    "__BMI2__",
    "__FMA__",
    "__LZCNT__",
    "__MOVBE__",
    "__F16C__",
    "__AVX512F__",
    "__AVX512BW__",
    "__AVX512CD__",
    "__AVX512DQ__",
    "__AVX512VL__",
    "__AVX512VBMI__",
    "__AVX512BITALG__",
];

/// The features of [`X86_FEATURES`] whose macro `defines` (clang's `-dM -E` output) defines.
pub fn x86_features_from_macros(defines: &str) -> u32 {
    let defined: Vec<&str> = defines
        .lines()
        .filter_map(|l| l.strip_prefix("#define ")?.split_whitespace().next())
        .collect();
    X86_FEATURE_MACROS
        .iter()
        .enumerate()
        .filter(|(_, m)| defined.contains(m))
        .fold(0, |mask, (bit, _)| mask | 1 << bit)
}

/// The psABI levels as feature masks over [`X86_FEATURES`].
const X86_V2: u32 = 0b11_1111;
const X86_V3: u32 = X86_V2 | 0b11_1111_1100_0000;
const X86_V4: u32 = X86_V3 | 0b111_1100_0000_0000_0000;

/// The named levels `--cpu` takes on x86_64.
const X86_LEVELS: [(&str, u32); 5] = [
    ("baseline", 0),
    ("x86-64", 0),
    ("x86-64-v2", X86_V2),
    ("x86-64-v3", X86_V3),
    ("x86-64-v4", X86_V4),
];

/// Cranelift presets that are not CPUs (`sse42` is a feature set, not an `-march` value).
const NOT_CPUS: [&str; 4] = ["sse3", "ssse3", "sse41", "sse42"];

/// The architecture of `target` (empty, `native` or `host`: the host's).
fn arch(target: &str) -> Architecture {
    let target = target.trim();
    if target.is_empty() || target == "native" || target == "host" {
        return Triple::host().architecture;
    }
    target
        .parse::<Triple>()
        .map_or(Architecture::Unknown, |t| t.architecture)
}

/// The CPU names of `arch` besides `native` and `baseline` (the levels are among x86_64's).
fn names(arch: Architecture) -> &'static [&'static str] {
    match arch {
        Architecture::X86_64 => cpu_names::X86_64,
        Architecture::Aarch64(_) => cpu_names::AARCH64,
        _ => &[],
    }
}

/// The architecture other than `arch` that `cpu` names a CPU of, if it is not one of `arch`'s:
/// `x86_64` for `x86-64-v3` or `znver4` on arm64, `aarch64` for `neoverse-n1` on x86_64 (and
/// `native` when `arch` is not the host's).
fn other_arch(arch: Architecture, cpu: &str) -> Option<&'static str> {
    if cpu == "baseline" || names(arch).contains(&cpu) {
        return None;
    }
    if cpu == "native" {
        return (arch != Triple::host().architecture).then_some("this machine's");
    }
    if cpu_names::X86_64.contains(&cpu) {
        Some("x86_64")
    } else if cpu_names::AARCH64.contains(&cpu) {
        Some("aarch64")
    } else {
        None
    }
}

/// Whether `cpu` is meant for an architecture other than `target`'s: a `$VELT_CPU` or
/// `build.cpu` like that does not apply to a build for `target`.
pub fn is_for_another_arch(target: &str, cpu: &str) -> bool {
    other_arch(arch(target), cpu).is_some()
}

/// Whether Cranelift's x86 ISA has a CPU preset named `cpu`.
fn cranelift_x86_preset(cpu: &str) -> bool {
    if NOT_CPUS.contains(&cpu) || cpu.contains('_') {
        return false;
    }
    let Ok(mut builder) = isa::lookup(Triple {
        architecture: Architecture::X86_64,
        ..Triple::unknown()
    }) else {
        return false;
    };
    builder.enable(cpu).is_ok()
}

/// Whether `cpu` can build for `target`: `baseline`, `native` (only for the host's
/// architecture) or one of the target's CPU names (`cpu_names.rs`; the levels on x86_64).
pub fn check_cpu(target: &str, cpu: &str) -> Result<(), String> {
    let arch = arch(target);
    if cpu == "native" && arch != Triple::host().architecture {
        return Err(format!(
            "`--cpu native` builds for this machine's CPU, which cannot run `{target}` code"
        ));
    }
    match other_arch(arch, cpu) {
        Some(other) => {
            return Err(format!(
                "`--cpu {cpu}` is for {other} targets, not `{target}`"
            ));
        }
        None if cpu == "baseline" || cpu == "native" || names(arch).contains(&cpu) => {
            return Ok(());
        }
        None => {}
    }
    let known = names(arch);
    let examples = if arch == Architecture::X86_64 {
        "baseline, x86-64-v2, x86-64-v3, x86-64-v4, native or a CPU name such as `znver4`"
    } else {
        "baseline, native or a CPU name such as `neoverse-n1`"
    };
    let hint = closest(cpu, known)
        .map(|c| format!("; did you mean `{c}`?"))
        .unwrap_or_default();
    let target = if target.trim().is_empty() {
        Triple::host().to_string()
    } else {
        target.trim().to_string()
    };
    Err(format!(
        "unknown CPU `{cpu}` for `{target}` (expected {examples}){hint}"
    ))
}

/// The name of `known` closest to `cpu`, if close enough to be a typo of it.
fn closest<'a>(cpu: &str, known: &[&'a str]) -> Option<&'a str> {
    let distance = |a: &str, b: &str| {
        let b: Vec<char> = b.chars().collect();
        let mut row: Vec<usize> = (0..=b.len()).collect();
        for (i, ca) in a.chars().enumerate() {
            let mut prev = row[0];
            row[0] = i + 1;
            for (j, cb) in b.iter().enumerate() {
                let cur = row[j + 1];
                row[j + 1] = (prev + usize::from(ca != *cb)).min(row[j] + 1).min(cur + 1);
                prev = cur;
            }
        }
        row[b.len()]
    };
    let limit = (cpu.chars().count() / 3).max(1);
    known
        .iter()
        .map(|k| (distance(cpu, k), *k))
        .filter(|(d, _)| *d <= limit)
        .min_by_key(|(d, _)| *d)
        .map(|(_, k)| k)
}

/// The x86 features code for `cpu` on `target` needs whoever compiles it, as a mask over
/// [`X86_FEATURES`] (`None`: the default): 0 off x86_64 and for the baseline, the level's for a
/// level, the host's for `native`; `None` for a CPU name, whose features depend on the backend.
pub fn level_features(target: &str, cpu: Option<&str>) -> Result<Option<u32>, String> {
    if let Some(cpu) = cpu {
        check_cpu(target, cpu)?;
    }
    if arch(target) != Architecture::X86_64 {
        return Ok(Some(0));
    }
    let cpu = cpu.unwrap_or(DEFAULT_X86_CPU);
    if let Some((_, mask)) = X86_LEVELS.iter().find(|(n, _)| *n == cpu) {
        return Ok(Some(*mask));
    }
    Ok((cpu == "native").then(host_x86_features))
}

/// The x86 features Cranelift's code for `cpu` on `target` needs, as a mask over
/// [`X86_FEATURES`] (`None`: the default): [`level_features`], or for a CPU name what
/// Cranelift's preset of that name enables (nothing for a CPU it has no preset for: such code is
/// for the baseline, and the start-up check checks nothing).
pub fn required_features(target: &str, cpu: Option<&str>) -> Result<u32, String> {
    if let Some(mask) = level_features(target, cpu)? {
        return Ok(mask);
    }
    let cpu = cpu.unwrap_or(DEFAULT_X86_CPU);
    if !cranelift_x86_preset(cpu) {
        return Ok(0);
    }
    let mut builder = isa::lookup(Triple {
        architecture: Architecture::X86_64,
        ..Triple::unknown()
    })
    .map_err(|e| format!("codegen: {e}"))?;
    builder
        .enable(cpu)
        .map_err(|e| format!("codegen: CPU `{cpu}`: {e}"))?;
    let isa = builder
        .finish(settings::Flags::new(settings::builder()))
        .map_err(|e| format!("codegen: CPU `{cpu}`: {e}"))?;
    let flags = isa.isa_flags();
    let enabled: Vec<&str> = flags
        .iter()
        .filter(|f| f.as_bool() == Some(true))
        .map(|f| f.name)
        .collect();
    Ok(X86_FEATURES
        .iter()
        .enumerate()
        .filter(|(_, (_, flag))| flag.is_some_and(|f| enabled.contains(&f)))
        .fold(0, |mask, (bit, _)| mask | 1 << bit))
}

/// The features of the x86 CPU the compiler runs on (0 elsewhere).
fn host_x86_features() -> u32 {
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

/// Configure `builder` for `cpu` (`None`: the default; checked by the caller): the host's flags
/// for `native`, the level's features for a psABI level, Cranelift's preset for another x86 CPU
/// it knows; nothing (the baseline) for anything else.
pub(crate) fn configure(builder: &mut IsaBuilder, cpu: Option<&str>) -> Result<(), String> {
    if cpu == Some("native") {
        return cranelift_native::infer_native_flags(builder)
            .map_err(|e| format!("codegen: host is not supported: {e}"));
    }
    if builder.triple().architecture != Architecture::X86_64 {
        return Ok(());
    }
    let cpu = cpu.unwrap_or(DEFAULT_X86_CPU);
    let mask = match X86_LEVELS.iter().find(|(n, _)| *n == cpu) {
        Some((_, mask)) => *mask,
        None if cranelift_x86_preset(cpu) => {
            return builder
                .enable(cpu)
                .map_err(|e| format!("codegen: CPU `{cpu}`: {e}"))
        }
        None => 0,
    };
    for (bit, (_, flag)) in X86_FEATURES.iter().enumerate() {
        if let Some(flag) = flag.filter(|_| mask & 1 << bit != 0) {
            builder
                .enable(flag)
                .map_err(|e| format!("codegen: cannot enable `{flag}`: {e}"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINUX: &str = "x86_64-unknown-linux-gnu";

    fn names(mask: u32) -> Vec<&'static str> {
        (0..X86_FEATURES.len())
            .filter(|b| mask & 1 << b != 0)
            .map(|b| X86_FEATURES[b].0)
            .collect()
    }

    #[test]
    fn levels_are_the_psabi_levels() {
        let v2 = ["SSE3", "SSSE3", "SSE4.1", "SSE4.2", "POPCNT", "CMPXCHG16B"];
        assert_eq!(names(X86_V2), v2);
        let v3 = [
            "AVX", "AVX2", "BMI1", "BMI2", "FMA", "LZCNT", "MOVBE", "F16C",
        ];
        assert_eq!(names(X86_V3 & !X86_V2), v3);
        let v4 = ["AVX512F", "AVX512BW", "AVX512CD", "AVX512DQ", "AVX512VL"];
        assert_eq!(names(X86_V4 & !X86_V3), v4);
    }

    #[test]
    fn the_runtime_checks_the_same_features() {
        let rt = include_str!("../../velt_rt/src/cpu.rs");
        let start = rt
            .find("const X86_FEATURES")
            .expect("the runtime's feature list");
        let list = &rt[start..start + rt[start..].find("];").expect("its end")];
        let names: Vec<&str> = list.split('"').skip(1).step_by(2).collect();
        let ours: Vec<&str> = X86_FEATURES.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ours);
        for (level, mask) in [
            ("x86-64-v2", X86_V2),
            ("x86-64-v3", X86_V3),
            ("x86-64-v4", X86_V4),
        ] {
            assert!(rt.contains(&format!("(\"{level}\", {mask:#x})")), "{level}");
        }
    }

    #[test]
    fn required_features_per_cpu() {
        let req = |cpu| required_features(LINUX, cpu).unwrap();
        assert_eq!(req(None), X86_V2, "x86-64-v2 by default");
        assert_eq!(req(Some("baseline")), 0);
        assert_eq!(req(Some("x86-64")), 0);
        assert_eq!(req(Some("x86-64-v4")), X86_V4);
        // A CPU Cranelift has a preset for: what the preset enables.
        assert_eq!(names(req(Some("nehalem"))), names(X86_V2));
        assert!(names(req(Some("skylake-avx512"))).contains(&"AVX512VL"));
        // One clang knows and Cranelift has no preset for: baseline code, checked for nothing.
        assert_eq!(req(Some("znver5")), 0);
        assert_eq!(required_features("aarch64-apple-darwin", None).unwrap(), 0);
        assert_eq!(
            required_features("aarch64-unknown-linux-gnu", Some("neoverse-n1")).unwrap(),
            0
        );
    }

    #[test]
    fn cranelift_flags_follow_the_cpu() {
        let enabled = |cpu| {
            let isa = crate::isa::make_isa(LINUX, cpu, false, false).unwrap();
            let flags = isa.isa_flags();
            flags
                .iter()
                .filter(|f| f.as_bool() == Some(true))
                .map(|f| f.name.to_string())
                .collect::<Vec<_>>()
        };
        let default = enabled(None);
        assert!(default.contains(&"has_popcnt".into()) && default.contains(&"has_sse42".into()));
        assert!(!default.contains(&"has_avx2".into()));
        assert!(!enabled(Some("baseline")).contains(&"has_popcnt".into()));
        assert!(enabled(Some("x86-64-v3")).contains(&"has_avx2".into()));
        assert!(enabled(Some("znver4")).contains(&"has_avx512vbmi".into()));
        assert!(!enabled(Some("znver5")).contains(&"has_sse3".into()));
        let arm = crate::isa::make_isa(
            "aarch64-unknown-linux-gnu",
            Some("neoverse-n1"),
            false,
            false,
        );
        assert!(arm.is_ok());
        assert!(
            crate::isa::make_isa("aarch64-apple-darwin", Some("x86-64-v2"), false, false).is_err()
        );
    }

    #[test]
    fn names_are_checked_against_the_target() {
        let arm = "aarch64-unknown-linux-gnu";
        assert!(check_cpu(LINUX, "znver4").is_ok());
        assert!(
            check_cpu(LINUX, "znver5").is_ok(),
            "known to clang, not to Cranelift"
        );
        assert!(check_cpu(arm, "neoverse-n1").is_ok());
        assert!(check_cpu(arm, "baseline").is_ok());
        assert!(check_cpu(arm, "x86-64-v2")
            .unwrap_err()
            .contains("x86_64 targets"));
        assert!(check_cpu(arm, "znver5")
            .unwrap_err()
            .contains("for x86_64 targets"));
        let n1 = check_cpu(LINUX, "neoverse-n1").unwrap_err();
        assert_eq!(
            n1,
            format!("`--cpu neoverse-n1` is for aarch64 targets, not `{LINUX}`")
        );
        // A typo is an error, with the name it is closest to.
        let typo = check_cpu(LINUX, "skylkae").unwrap_err();
        assert!(
            typo.starts_with("unknown CPU `skylkae` for `x86_64-unknown-linux-gnu`"),
            "{typo}"
        );
        assert!(typo.ends_with("did you mean `skylake`?"), "{typo}");
        assert!(check_cpu(LINUX, "Skylake")
            .unwrap_err()
            .contains("`skylake`"));
        // 32-bit CPUs, Cranelift's feature sets and a file name are not x86_64 CPUs.
        for bad in [
            "",
            "i686",
            "pentium4",
            "sse42",
            "has_avx2",
            "a b",
            "x86-64-v2;",
            "a.vlt",
        ] {
            assert!(
                check_cpu(LINUX, bad)
                    .unwrap_err()
                    .starts_with("unknown CPU"),
                "{bad}"
            );
        }
        let foreign = match Triple::host().architecture {
            Architecture::X86_64 => arm,
            _ => LINUX,
        };
        assert!(check_cpu(foreign, "native")
            .unwrap_err()
            .contains("this machine"));
        assert!(check_cpu("", "native").is_ok());
        assert!(is_for_another_arch(arm, "x86-64-v3") && is_for_another_arch(arm, "arrowlake"));
        assert!(
            is_for_another_arch(LINUX, "neoverse-n1") && is_for_another_arch(LINUX, "apple-m4")
        );
        assert!(!is_for_another_arch(LINUX, "baseline") && !is_for_another_arch(LINUX, "znver4"));
        assert!(!is_for_another_arch(LINUX, "skylkae"), "unknown everywhere");
        assert!(is_for_another_arch(foreign, "native"));
    }

    #[test]
    fn features_from_clangs_macros() {
        let defines = "#define __SSE3__ 1\n#define __AVX2__ 1\n#define __AVX2X__ 1\n\
                       #define __GCC_HAVE_SYNC_COMPARE_AND_SWAP_16 1\n#define __x86_64__ 1\n";
        assert_eq!(
            names(x86_features_from_macros(defines)),
            ["SSE3", "CMPXCHG16B", "AVX2"]
        );
        assert_eq!(X86_FEATURE_MACROS.len(), X86_FEATURES.len());
    }

    #[test]
    fn level_features_leave_cpu_names_to_the_backend() {
        assert_eq!(level_features(LINUX, None).unwrap(), Some(X86_V2));
        assert_eq!(
            level_features(LINUX, Some("x86-64-v3")).unwrap(),
            Some(X86_V3)
        );
        assert_eq!(level_features(LINUX, Some("alderlake")).unwrap(), None);
        assert_eq!(
            level_features("aarch64-apple-darwin", Some("apple-m4")).unwrap(),
            Some(0)
        );
        assert!(level_features(LINUX, Some("skylkae")).is_err());
    }
}
