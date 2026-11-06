//! velt/simd's AES checks (#2401) against the CPU. The golden `std/simd_aes` proves that every path
//! gives the same bits, which by design can't tell the paths apart; this looks at the checks.

use std::path::{Path, PathBuf};

mod no_window;
mod test_dir;

const PROGRAM: &str = "import { u8x16 } from \"velt/simd\";\n\nfunction main() {\n  console.log(u8x16.hasAes(), u8x16.hasAesX86(), u8x16.hasAesArm());\n}\n";

/// What the checks print on a CPU with (`x86`, `arm`).
fn printed(x86: bool, arm: bool) -> String {
    format!("{} {x86} {arm}", x86 || arm)
}

const NONE: &str = "false false false";

/// This CPU's (`hasAesX86`, `hasAesArm`), from the Rust standard library's detection.
fn cpu() -> (bool, bool) {
    #[cfg(target_arch = "x86_64")]
    return (
        std::arch::is_x86_feature_detected!("aes")
            && std::arch::is_x86_feature_detected!("pclmulqdq")
            && std::arch::is_x86_feature_detected!("ssse3")
            && std::arch::is_x86_feature_detected!("sse4.1"),
        false,
    );
    #[cfg(target_arch = "aarch64")]
    return (false, std::arch::is_aarch64_feature_detected!("pmull"));
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    return (false, false);
}

/// `velt build <args>` of the program, in `dir`; the executable.
fn build(dir: &Path, name: &str, args: &[&str]) -> PathBuf {
    let source = dir.join("main.velt");
    std::fs::write(&source, PROGRAM).unwrap();
    let exe = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    let out = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .arg("build")
        .args(args)
        .arg("-o")
        .arg(&exe)
        .arg(&source)
        .env_remove("VELT_CPU")
        .current_dir(dir)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "velt build {args:?}: {stderr}");
    exe
}

/// What `exe` prints with `VELT_SIMD_DISABLE` set to `disable`.
fn run(exe: &Path, disable: Option<&str>) -> String {
    let mut command = no_window::command(exe);
    command.env_remove("VELT_SIMD_DISABLE");
    if let Some(list) = disable {
        command.env("VELT_SIMD_DISABLE", list);
    }
    let out = command.output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{disable:?}: {stderr}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn release_checks_are_the_cpu_unless_disabled() {
    let dir = test_dir::TestDir::new();
    let exe = build(dir.path(), "release", &["--release"]);
    let (x86, arm) = cpu();
    let detected = printed(x86, arm);
    assert_eq!(run(&exe, None), detected, "plain");
    for other in ["sha", "", "aesx", "xaes", "aes-x86"] {
        assert_eq!(run(&exe, Some(other)), detected, "{other:?}");
    }
    for off in ["aes", "clmul", "sha,aes", " aes ", "clmul,sha"] {
        assert_eq!(run(&exe, Some(off)), NONE, "{off:?}");
    }
}

#[test]
fn debug_builds_never_pass_the_checks() {
    let dir = test_dir::TestDir::new();
    let exe = build(dir.path(), "debug", &[]);
    assert_eq!(run(&exe, None), NONE);
    assert_eq!(run(&exe, Some("aes")), NONE);
}

/// `--cpu native` makes the carry-less check constant where the CPU has it, so `clmul` no longer
/// reaches the AES checks; `aes` still does, because they are read at run time.
#[test]
#[cfg(target_arch = "x86_64")]
fn native_builds_ignore_only_clmul() {
    let (x86, _) = cpu();
    if !x86 {
        return;
    }
    let dir = test_dir::TestDir::new();
    let exe = build(dir.path(), "native", &["--release", "--cpu", "native"]);
    let detected = printed(true, false);
    assert_eq!(run(&exe, None), detected);
    assert_eq!(run(&exe, Some("clmul")), detected);
    assert_eq!(run(&exe, Some("aes")), NONE);
}

/// The CPUs a machine with AES-NI can only show under an emulator: without the instructions the
/// check must be false, or the first round is an illegal instruction. Runs where QEMU is installed.
#[test]
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn cpus_without_aes_under_qemu() {
    let found = ["qemu-x86_64-static", "qemu-x86_64"].into_iter().find(|q| {
        std::process::Command::new(q)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    });
    let Some(qemu) = found else {
        eprintln!("skipped: no qemu-x86_64");
        return;
    };
    let dir = test_dir::TestDir::new();
    let exe = build(dir.path(), "release", &["--release"]);
    for (model, want) in [
        ("Westmere", printed(true, false)),
        ("Westmere,-aes", NONE.to_string()),
        ("Westmere,-pclmulqdq", NONE.to_string()),
        ("Nehalem", NONE.to_string()),
    ] {
        let out = no_window::command(qemu)
            .args(["-cpu", model])
            .arg(&exe)
            .env_remove("VELT_SIMD_DISABLE")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{model}: {stderr}");
        assert_eq!(stdout.trim(), want, "{model}");
    }
}
