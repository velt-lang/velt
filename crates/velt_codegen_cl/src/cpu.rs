//! `--cpu`: the CPU generated code is for (docs/tooling/platforms.md "CPU"), shared with the LLVM
//! backend so both read a name the same way:
//! - `baseline` (alias `x86-64`): the target's first generation (SSE2 on x86_64, armv8-a);
//! - `x86-64-v2`, `x86-64-v3`, `x86-64-v4`: the x86-64 psABI levels; `x86-64-v2` is the default
//!   on x86_64 ([`DEFAULT_X86_CPU`]);
//! - `native`: the machine the compiler runs on (only when it builds for its own architecture);
//! - any other name is an LLVM CPU name (`znver4`, `skylake`, `neoverse-n1`): clang gets it as is,
//!   Cranelift uses its preset of that name where it has one (x86) and the baseline otherwise.
//!
//! Code for a CPU above the baseline checks at start-up that the CPU has what it was built for
//! ([`required_features`], `velt_rt_check_cpu` in rt_abi.md "CPU check").

use cranelift_codegen::isa::{self, Builder as IsaBuilder};
use cranelift_codegen::settings::{self, Configurable};
use target_lexicon::{Architecture, Triple};

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

/// Whether `cpu` names an x86 CPU or level (`x86-64-v3`, `skylake`), which no other
/// architecture can build for.
pub fn is_x86_cpu(cpu: &str) -> bool {
    cpu != "baseline" && (X86_LEVELS.iter().any(|(n, _)| *n == cpu) || cranelift_x86_preset(cpu))
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

/// Whether `cpu` can build for `target`: a CPU name made of lowercase letters, digits, `.` and
/// `-`; `native` only for the host's architecture, an x86 CPU only for x86_64.
pub fn check_cpu(target: &str, cpu: &str) -> Result<(), String> {
    let well_formed = !cpu.is_empty()
        && cpu
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.');
    if !well_formed {
        return Err(format!(
            "invalid CPU `{cpu}` (expected baseline, x86-64-v2, x86-64-v3, x86-64-v4, native or \
             an LLVM CPU name such as `znver4`)"
        ));
    }
    let arch = arch(target);
    if cpu == "native" && arch != Triple::host().architecture {
        return Err(format!(
            "`--cpu native` builds for this machine's CPU, which cannot run `{target}` code"
        ));
    }
    if arch != Architecture::X86_64 && is_x86_cpu(cpu) {
        return Err(format!(
            "`--cpu {cpu}` is for x86_64 targets, not `{target}`"
        ));
    }
    Ok(())
}

/// The x86 features code for `cpu` on `target` needs, as a mask over [`X86_FEATURES`] (`None`:
/// the default): 0 off x86_64, for the baseline and for a CPU name Cranelift does not know (the
/// start-up check then checks nothing).
pub fn required_features(target: &str, cpu: Option<&str>) -> Result<u32, String> {
    if arch(target) != Architecture::X86_64 {
        return Ok(0);
    }
    let cpu = cpu.unwrap_or(DEFAULT_X86_CPU);
    check_cpu(target, cpu)?;
    if let Some((_, mask)) = X86_LEVELS.iter().find(|(n, _)| *n == cpu) {
        return Ok(*mask);
    }
    if cpu == "native" {
        return Ok(host_x86_features());
    }
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
        // One it does not know is checked for nothing.
        assert_eq!(req(Some("some-future-cpu")), 0);
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
        assert!(!enabled(Some("some-future-cpu")).contains(&"has_sse3".into()));
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
        assert!(check_cpu(arm, "neoverse-n1").is_ok());
        assert!(check_cpu(arm, "baseline").is_ok());
        assert!(check_cpu(arm, "x86-64-v2")
            .unwrap_err()
            .contains("x86_64 targets"));
        assert!(check_cpu(arm, "skylake")
            .unwrap_err()
            .contains("x86_64 targets"));
        for bad in ["", "Skylake", "has_avx2", "a b", "x86-64-v2;"] {
            assert!(
                check_cpu(LINUX, bad).unwrap_err().contains("invalid CPU"),
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
        assert!(is_x86_cpu("x86-64-v3") && is_x86_cpu("znver3") && !is_x86_cpu("baseline"));
        assert!(!is_x86_cpu("sse42") && !is_x86_cpu("neoverse-n1"));
    }
}
