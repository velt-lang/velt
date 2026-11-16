//! `**` on floats (std/lang/math/pow.velt, #1676) runs one of several copies of glibc's `pow`: on
//! x86-64 the one compiled for FMA where the CPU has it (`hasFmaX86()`,
//! std/lang/math/fma_check.velt), else the one calling C's `fma`; Cranelift's in debug builds. The golden `lang/math_pow_node`
//! checks Node's bits on the path its machine takes; this runs the release build on the others.

use std::path::{Path, PathBuf};

mod no_window;
mod test_dir;

/// The golden program and Node's output for it.
fn golden() -> (PathBuf, String) {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden/lang");
    let want = std::fs::read_to_string(base.join("math_pow_node.out")).unwrap();
    (base.join("math_pow_node.velt"), want)
}

/// `velt build --release` of `source` into `dir`; the executable.
fn build(dir: &Path, source: &Path) -> PathBuf {
    let exe = dir.join(format!("pow{}", std::env::consts::EXE_SUFFIX));
    let out = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "--release", "-o"])
        .arg(&exe)
        .arg(source)
        .env_remove("VELT_CPU")
        .current_dir(dir)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "velt build: {stderr}");
    exe
}

/// What `command` prints, which must succeed.
fn stdout(mut command: std::process::Command, what: &str) -> String {
    let out = command.output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{what}: {stderr}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn every_release_path_gives_nodes_bits() {
    let (source, want) = golden();
    let dir = test_dir::TestDir::new();
    let exe = build(dir.path(), &source);
    for disable in [None, Some("clmul"), Some("sha,clmul")] {
        let mut command = no_window::command(&exe);
        command.env_remove("VELT_SIMD_DISABLE");
        if let Some(list) = disable {
            command.env("VELT_SIMD_DISABLE", list);
        }
        let got = stdout(command, &format!("{disable:?}"));
        assert!(
            got == want,
            "VELT_SIMD_DISABLE={disable:?}: not Node's output"
        );
    }
}

/// CPUs with and without FMA, which a machine with it can only show under an emulator: without it
/// the check must be false, or the first `vfmadd` is an illegal instruction. Runs where QEMU is
/// installed.
#[test]
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn cpus_with_and_without_fma_under_qemu() {
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
    let (source, want) = golden();
    let dir = test_dir::TestDir::new();
    let exe = build(dir.path(), &source);
    for model in [
        "Nehalem",
        "SandyBridge",
        "Haswell",
        "Haswell,-fma",
        "Haswell,-avx",
    ] {
        let mut command = no_window::command(qemu);
        command
            .args(["-cpu", model])
            .arg(&exe)
            .env_remove("VELT_SIMD_DISABLE");
        let got = stdout(command, model);
        assert!(got == want, "-cpu {model}: not Node's output");
    }
}
