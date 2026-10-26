//! `--cpu` (#1613, docs/tooling/platforms.md "CPU"): which setting wins (`--cpu`, `$VELT_CPU`,
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
    std::fs::write(root.join("src/main.vlt"), PROGRAM).unwrap();
    let manifest = |build: &str| {
        let text = format!(
            "import type {{ Package }} from \"velt:package\";\n\nexport const pkg: Package = \
             {{ name: \"app\", version: \"0.1.0\"{build} }};\n"
        );
        std::fs::write(root.join("package.vlt"), text).unwrap();
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
    assert!(!checks_the_cpu(root, &["src/main.vlt"], &[]));
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
    assert!(String::from_utf8_lossy(&o.stderr).contains("invalid CPU"));
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
    std::fs::write(root.join("main.vlt"), PROGRAM).unwrap();
    let no_popcnt = [("VELT_CPU_DISABLE", "popcnt")];
    for cpu in ["x86-64-v2", "native"] {
        let o = velt(root, &["run", "--cpu", cpu, "main.vlt"], &no_popcnt);
        let err = String::from_utf8_lossy(&o.stderr);
        assert_eq!(o.status.code(), Some(1), "{cpu}: {err}");
        assert!(o.stdout.is_empty(), "{cpu}: main ran");
        assert!(err.contains("this CPU lacks POPCNT"), "{cpu}: {err}");
        assert!(err.contains("`--cpu baseline`"), "{cpu}: {err}");
    }
    let o = velt(root, &["run", "--cpu", "x86-64-v2", "main.vlt"], &[]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = velt(root, &["run", "--cpu", "baseline", "main.vlt"], &no_popcnt);
    assert_eq!(String::from_utf8_lossy(&o.stdout), "started\n");
}
