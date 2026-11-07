//! `--cpu` (#1613, https://veltlang.com/docs/tooling/platforms/ "CPU"): which setting wins (`--cpu`, `$VELT_CPU`,
//! the package's `build.cpu`), the start-up check x86_64 code above the baseline makes
//! (`velt_rt_check_cpu`, seen in the object on any host), and its message on an x86_64 host, where
//! `$VELT_CPU_DISABLE` takes a feature away.

use std::path::Path;
use std::process::Output;

mod no_window;
mod test_dir;

const VELT: &str = env!("CARGO_BIN_EXE_velt");
const PROGRAM: &str = "function main() {\n  console.log(\"started\");\n}\n";

fn velt(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = no_window::command(VELT);
    cmd.args(args).current_dir(dir).env_remove("VELT_CPU");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

/// Whether the x86_64 Linux object `velt build <args>` writes calls the CPU check.
fn checks_the_cpu(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> bool {
    let obj = dir.join("out.o");
    let _ = std::fs::remove_file(&obj);
    let mut all = vec![
        "build",
        "--target",
        "x86_64-unknown-linux-gnu",
        "--emit",
        "obj",
    ];
    all.extend(["-o", obj.to_str().unwrap()]);
    all.extend(args);
    let o = velt(dir, &all, env);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let bytes = std::fs::read(&obj).unwrap();
    bytes.windows(17).any(|w| w == b"velt_rt_check_cpu")
}

#[test]
fn the_flag_wins_over_the_environment_and_the_package() {
    let dir = test_dir::TestDir::new();
    let root = dir.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/main.velt"), PROGRAM).unwrap();
    let manifest = |build: &str| {
        let text = format!(
            "import type {{ Package }} from \"velt/package\";\n\nexport const pkg: Package = \
             {{ name: \"app\", version: \"0.1.0\"{build} }};\n"
        );
        std::fs::write(root.join("package.velt"), text).unwrap();
    };
    manifest("");
    // x86-64-v2 by default.
    assert!(checks_the_cpu(root, &[], &[]));
    assert!(!checks_the_cpu(root, &["--cpu", "baseline"], &[]));
    manifest(", build: { cpu: \"baseline\" }");
    assert!(!checks_the_cpu(root, &[], &[]));
    assert!(checks_the_cpu(root, &[], &[("VELT_CPU", "x86-64-v3")]));
    let flag = checks_the_cpu(root, &["--cpu", "baseline"], &[("VELT_CPU", "x86-64-v3")]);
    assert!(!flag);
    // A single file in the package takes its setting too.
    assert!(!checks_the_cpu(root, &["src/main.velt"], &[]));
    // An x86 CPU does not apply to an arm64 build from the package, and is an error from --cpu.
    manifest(", build: { cpu: \"x86-64-v3\" }");
    let arm = [
        "build",
        "--target",
        "aarch64-unknown-linux-gnu",
        "--emit",
        "obj",
    ];
    assert!(velt(root, &arm, &[]).status.success());
    let o = velt(root, &[&arm[..], &["--cpu", "x86-64-v3"]].concat(), &[]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("is for x86_64 targets"), "{err}");
    let o = velt(root, &["build", "--cpu", "Zen 4"], &[]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("unknown CPU `Zen 4`"));
    // An arm64 CPU from the environment or the package leaves x86_64 code at x86-64-v2, checked.
    assert!(checks_the_cpu(root, &[], &[("VELT_CPU", "neoverse-n1")]));
    manifest(", build: { cpu: \"neoverse-n1\" }");
    assert!(checks_the_cpu(root, &[], &[]));
    // ... and an x86_64 one from either leaves arm64 code alone, without reaching clang.
    for release in [&[][..], &["--release"]] {
        let env = [("VELT_CPU", "znver5")];
        let o = velt(root, &[&arm[..], release].concat(), &env);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }
}

/// A CPU name no architecture has is the same error in debug and release builds, with the name
/// it is closest to; one of the other architecture's names from `--cpu` says which it is for.
#[test]
fn cpu_names_are_checked_against_the_target() {
    let dir = test_dir::TestDir::new();
    let root = dir.path();
    std::fs::write(root.join("main.velt"), PROGRAM).unwrap();
    let x86 = [
        "build",
        "--target",
        "x86_64-unknown-linux-gnu",
        "--emit",
        "obj",
    ];
    for release in [&[][..], &["--release"]] {
        let o = velt(
            root,
            &[&x86[..], release, &["--cpu", "skylkae", "main.velt"]].concat(),
            &[],
        );
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(!o.status.success(), "{err}");
        assert!(err.contains("unknown CPU `skylkae`"), "{err}");
        assert!(err.contains("did you mean `skylake`?"), "{err}");
        assert!(!err.contains("compiler bug"), "{err}");
        let o = velt(
            root,
            &[&x86[..], release, &["--cpu", "neoverse-n1", "main.velt"]].concat(),
            &[],
        );
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(
            err.contains("`--cpu neoverse-n1` is for aarch64 targets"),
            "{err}"
        );
    }
    let o = velt(root, &["build", "--cpu", "main.velt"], &[]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("not the file `main.velt`"), "{err}");
}

/// `--emit llvm` prints the IR clang compiles for the CPU: with the start-up check, rounding
/// without SSE4.1 for the baseline, and for a CPU name the features clang gives it.
#[test]
fn printed_ir_is_for_the_cpu() {
    let dir = test_dir::TestDir::new();
    let root = dir.path();
    let floor = "function main() {\n  const x = Number(process.argv.length) / 3;\n  \
                 console.log(Math.floor(x));\n}\n";
    std::fs::write(root.join("main.velt"), floor).unwrap();
    let ir = |cpu: &[&str]| {
        let args = [
            "build",
            "--target",
            "x86_64-unknown-linux-gnu",
            "--emit",
            "llvm",
        ];
        let o = velt(root, &[&args[..], cpu, &["main.velt"]].concat(), &[]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap()
    };
    let default = ir(&[]);
    assert!(
        default.contains("call void @\"velt_rt_check_cpu\"(i32 63)"),
        "x86-64-v2"
    );
    assert!(default.contains("@llvm.floor.f64"));
    let baseline = ir(&["--cpu", "baseline"]);
    assert!(!baseline.contains("velt_rt_check_cpu"));
    assert!(
        !baseline.contains("@llvm.floor.f64"),
        "no roundsd without SSE4.1"
    );
    if velt_codegen_llvm::available() {
        // Cranelift's `alderlake` has no AVX2; clang's has, and the check checks it.
        let alder = ir(&["--cpu", "alderlake"]);
        let mask = alder
            .split("call void @\"velt_rt_check_cpu\"(i32 ")
            .nth(1)
            .and_then(|r| r.split(')').next())
            .and_then(|m| m.parse::<u32>().ok())
            .expect("the check");
        const AVX2: u32 = 1 << 7;
        assert_ne!(mask & AVX2, 0, "{mask:#x}");
    }
}

/// The executable is rebuilt when only the CPU changes (#1613): built for x86-64-v2, then for
/// the baseline into the same file, it runs on a CPU without POPCNT.
#[test]
fn another_cpu_relinks_the_program() {
    if !cfg!(target_arch = "x86_64") {
        return;
    }
    let dir = test_dir::TestDir::new();
    let root = dir.path();
    std::fs::write(root.join("main.velt"), PROGRAM).unwrap();
    let exe = root.join(if cfg!(windows) { "app.exe" } else { "app" });
    let build = |cpu: &str| {
        let args = [
            "build",
            "main.velt",
            "--cpu",
            cpu,
            "-o",
            exe.to_str().unwrap(),
        ];
        let o = velt(root, &args, &[]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        std::fs::read(&exe).unwrap()
    };
    let run = || {
        no_window::command(exe.to_str().unwrap())
            .env("VELT_CPU_DISABLE", "popcnt")
            .output()
            .unwrap()
    };
    let v2 = build("x86-64-v2");
    assert_eq!(run().status.code(), Some(1));
    let baseline = build("baseline");
    assert_ne!(v2, baseline);
    let o = run();
    assert_eq!(String::from_utf8_lossy(&o.stdout), "started\n");
}

/// On an x86_64 host: a program built for x86-64-v2 on a CPU without POPCNT (as
/// `$VELT_CPU_DISABLE` makes it look) says so and exits with 1 before `main`; built for the
/// baseline it runs.
#[test]
fn a_cpu_without_a_feature_gets_a_message_not_a_crash() {
    if !cfg!(target_arch = "x86_64") {
        return;
    }
    let dir = test_dir::TestDir::new();
    let root = dir.path();
    std::fs::write(root.join("main.velt"), PROGRAM).unwrap();
    let no_popcnt = [("VELT_CPU_DISABLE", "popcnt")];
    for cpu in ["x86-64-v2", "native"] {
        let o = velt(root, &["run", "--cpu", cpu, "main.velt"], &no_popcnt);
        let err = String::from_utf8_lossy(&o.stderr);
        assert_eq!(o.status.code(), Some(1), "{cpu}: {err}");
        assert!(o.stdout.is_empty(), "{cpu}: main ran");
        assert!(err.contains("this CPU lacks POPCNT"), "{cpu}: {err}");
        assert!(err.contains("`--cpu baseline`"), "{cpu}: {err}");
    }
    let o = velt(root, &["run", "--cpu", "x86-64-v2", "main.velt"], &[]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = velt(root, &["run", "--cpu", "baseline", "main.velt"], &no_popcnt);
    assert_eq!(String::from_utf8_lossy(&o.stdout), "started\n");
}

/// The arguments that build a program for each path `f16x8`'s arithmetic has on this host,
/// Cranelift's first: the CPU's own instructions and, on arm64, the same in `f32x4` (x86's default
/// is that, so the goldens run it). `--cpu native` has FP16 on Apple's CPUs and recent Arm servers.
fn f16_paths() -> Vec<Vec<&'static str>> {
    let mut paths = vec![vec![]];
    if velt_codegen_llvm::available() {
        let llvm = ["--release", "--backend", "llvm", "--cpu"];
        paths.push([&llvm[..], &["native"]].concat());
        if cfg!(target_arch = "aarch64") {
            paths.push([&llvm[..], &["cortex-a53"]].concat());
        }
        if cfg!(target_arch = "x86_64") {
            // The default level has no F16C, and the fast goldens run debug builds only.
            paths.push([&llvm[..], &["x86-64-v2"]].concat());
        }
    }
    paths
}

/// The golden `name` built with `flags`, as a program in `root`.
fn built_golden(root: &Path, name: &str, flags: &[&str], at: usize) -> std::path::PathBuf {
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden/std");
    let source = golden.join(format!("{name}.velt"));
    let exe = root.join(format!("app{at}{}", std::env::consts::EXE_SUFFIX));
    let mut args = vec!["build"];
    args.extend(flags);
    args.extend([source.to_str().unwrap(), "-o", exe.to_str().unwrap()]);
    let o = velt(root, &args, &[]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    exe
}

/// `f16x8`'s arithmetic gives the goldens' bits on each path (`--cpu` is not a golden's mode).
#[test]
fn f16_arithmetic_on_every_path_prints_the_goldens() {
    let dir = test_dir::TestDir::new();
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden/std");
    for name in ["simd_half_arith", "simd_half_sweep"] {
        let want = std::fs::read_to_string(golden.join(format!("{name}.out"))).unwrap();
        for (at, flags) in f16_paths().iter().enumerate() {
            let exe = built_golden(dir.path(), name, flags, at);
            let o = no_window::command(exe.to_str().unwrap()).output().unwrap();
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
            assert_eq!(String::from_utf8_lossy(&o.stdout), want, "{flags:?}");
        }
    }
}

/// What `simd_half_sweep` prints for every pair of the 2^32, from FEAT_FP16's own instructions: the
/// same sweep in C with `arm_neon.h` (`clang -O2 -mcpu=apple-m1 -ffp-contract=off`), which agrees
/// with Node on the golden's sample.
const F16_EVERY_PAIR: &str = "\
add 6691 60078 14932 52373 17398 6313 45340 17661
sub 61413 9001 30597 47456 46560 48152 21932 39857
mul 3717 58994 20928 61334 8632 6273 14481 33192
div 13385 12245 24729 37292 11577 40563 57992 12845
min 26271 46567 53330 62277 3469 59280 43966 53799
max 2459 43491 17605 45940 37224 50709 35644 63782
eq 124320705 ne 3203141158 lt 3586748204 le 4158566134 gt 4176928514 ge 2841051895
sqrt 8851 3179 39004 27337 60597 50405 13888 9415
neg 21906 52337 45105 17308 34548 27764 12475 15965
abs 30586 47474 10500 59872 42431 36468 10527 12984
";

/// Every pair of patterns on each path: under a minute on LLVM's, about a quarter of an hour under
/// Cranelift.
#[test]
#[ignore = "every pair of the 2^32 on each path: about a quarter of an hour"]
fn f16_every_pair() {
    let dir = test_dir::TestDir::new();
    let root = dir.path();
    let paths = f16_paths();
    std::thread::scope(|s| {
        let runs: Vec<_> = paths
            .iter()
            .enumerate()
            .map(|(at, flags)| {
                s.spawn(move || {
                    let exe = built_golden(root, "simd_half_sweep", flags, at);
                    let o = no_window::command(exe.to_str().unwrap())
                        .arg("1")
                        .output()
                        .unwrap();
                    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
                    assert_eq!(
                        String::from_utf8_lossy(&o.stdout),
                        F16_EVERY_PAIR,
                        "{flags:?}"
                    );
                })
            })
            .collect();
        for run in runs {
            run.join().expect("a path");
        }
    });
}

/// `f16x8.dotAdd` and `bf16x8.dotAdd` give the golden's bits with `--cpu native`, where the CPU's
/// own instructions come in (on Apple arm64, `fmlal` always and `bfdot` for `dotAddFast` from the
/// M2), and `dotAddFast`'s lanes still add up to the dot product.
#[test]
fn half_dot_products_on_this_cpu_print_the_golden() {
    if !velt_codegen_llvm::available() {
        return;
    }
    let dir = test_dir::TestDir::new();
    let root = dir.path();
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden/std");
    let source = golden.join("simd_half_dot.velt");
    let exe = root.join(if cfg!(windows) { "app.exe" } else { "app" });
    let args = [
        "build",
        "--release",
        "--backend",
        "llvm",
        "--cpu",
        "native",
        source.to_str().unwrap(),
        "-o",
        exe.to_str().unwrap(),
    ];
    let o = velt(root, &args, &[]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = no_window::command(exe.to_str().unwrap()).output().unwrap();
    let want = std::fs::read_to_string(golden.join("simd_half_dot.out")).unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8_lossy(&o.stdout), want);
}
