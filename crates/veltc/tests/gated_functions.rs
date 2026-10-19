//! Gated functions (#1330, `__intrinsic_target`, std only): every call sits behind the check of
//! the function's CPU extension or in a function gated on the same target; a gated function is
//! never a value; a closure inside one does not inherit its target. Gating is std-only, so a
//! golden file cannot declare one: std is copied to a temp directory with a probe module added,
//! and `velt check` / `velt run` use it through `$VELT_STD` (as std_simd_import.rs does).

use std::path::{Path, PathBuf};
use std::process::Output;

mod no_window;
mod test_dir;

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            copy_tree(&path, &to.join(entry.file_name()));
        } else {
            std::fs::copy(&path, to.join(entry.file_name())).unwrap();
        }
    }
}

/// A std with `probe.vlt` added, and `main.vlt` beside it; runs `velt <args> main.vlt`.
fn velt(dir: &test_dir::TestDir, probe: &str, main: &str, args: &[&str]) -> Output {
    let std: PathBuf = dir.path().join("std");
    if !std.exists() {
        copy_tree(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std"),
            &std,
        );
    }
    std::fs::write(std.join("zz_gated.vlt"), probe).unwrap();
    let file = dir.path().join("main.vlt");
    std::fs::write(&file, main).unwrap();
    let mut all: Vec<&str> = args.to_vec();
    all.push(file.to_str().unwrap());
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(&all)
        .env("VELT_STD", &std)
        .current_dir(dir.path())
        .output()
        .unwrap()
}

/// Gated functions used as they must be: a guarded call, a call from a function gated on the same
/// target, and the method that checks the target.
const GOOD: &str = r#"
import { u32x4 } from "velt:simd";

function twice(v: u32x4): u32x4 {
  __intrinsic_target("sha-arm");
  return v.add(v);
}

function four(v: u32x4): u32x4 {
  __intrinsic_target("sha-arm");
  return twice(twice(v));
}

export function scaled(v: u32x4): u32x4 {
  if (u32x4.hasShaArm()) {
    return four(v);
  }
  return v.add(v).add(v).add(v);
}
"#;

#[test]
fn guarded_and_same_target_calls_compile_and_run() {
    let dir = test_dir::TestDir::new();
    let main = "import { scaled } from \"velt:zz_gated\";\nimport { u32x4 } from \"velt:simd\";\n\nfunction main() {\n  console.log(scaled(u32x4.splat(3)).lane(2));\n}\n";
    for release in [false, true] {
        let mut args = vec!["run"];
        if release {
            args.push("--release");
        }
        let out = velt(&dir, GOOD, main, &args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{stderr}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "12",
            "{stderr}"
        );
    }
}

/// `velt check` of `probe` (and `main`) fails with each of `messages`.
fn check_fails(probe: &str, main: &str, messages: &[&str]) {
    let dir = test_dir::TestDir::new();
    let out = velt(&dir, probe, main, &["check"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    for m in messages {
        assert!(err.contains(m), "missing `{m}` in:\n{err}");
    }
}

const MAIN_PROBE: &str =
    "import { probe } from \"velt:zz_gated\";\n\nfunction main() {\n  console.log(probe());\n}\n";

#[test]
fn a_call_needs_the_check_of_its_target() {
    let probe = r#"
import { u32x4 } from "velt:simd";

function gated(): i64 {
  __intrinsic_target("sha-x86");
  return 1;
}

export function probe(): i64 {
  if (u32x4.hasShaArm()) {
    return gated();
  }
  return gated();
}
"#;
    check_fails(
        probe,
        MAIN_PROBE,
        &[
            "`gated` runs only where the CPU has `sha-x86`, so it must be called behind that check",
            "call it inside `if (u32x4.hasShaX86()) { … }`",
        ],
    );
}

#[test]
fn a_gated_function_is_not_a_value() {
    let probe = r#"
import { u32x4 } from "velt:simd";

function gated(): i64 {
  __intrinsic_target("sha-arm");
  return 1;
}

export function probe(): i64 {
  if (u32x4.hasShaArm()) {
    const f = gated;
    return f();
  }
  return 0;
}
"#;
    check_fails(
        probe,
        MAIN_PROBE,
        &["`gated` is compiled for `sha-arm` and can only be called directly"],
    );
}

#[test]
fn a_closure_does_not_inherit_the_target() {
    let probe = r#"
import { u32x4 } from "velt:simd";

function inner(): i64 {
  __intrinsic_target("sha-arm");
  return 1;
}

function outer(): i64 {
  __intrinsic_target("sha-arm");
  const f = (): i64 => inner();
  return f();
}

export function probe(): i64 {
  if (u32x4.hasShaArm()) {
    return outer();
  }
  return 0;
}
"#;
    check_fails(
        probe,
        MAIN_PROBE,
        &["`inner` runs only where the CPU has `sha-arm`, so it must be called behind that check"],
    );
}

#[test]
fn the_statement_is_first_named_and_not_in_a_method() {
    let probe = r#"
function late(): i64 {
  const x = 1;
  __intrinsic_target("sha-arm");
  return x;
}

function unknown(): i64 {
  __intrinsic_target("sha-mips");
  return 1;
}

class Box {
  get(): i64 {
    __intrinsic_target("sha-arm");
    return 1;
  }
}

export function probe(): i64 {
  return late() + unknown() + new Box().get();
}
"#;
    check_fails(
        probe,
        MAIN_PROBE,
        &[
            "`__intrinsic_target` must be the first statement of a function (not a method or closure)",
            "`__intrinsic_target` takes the target as a string literal: one of sha-arm, sha-x86",
        ],
    );
}

#[test]
fn user_code_calls_an_exported_gated_function_only_behind_the_check() {
    let probe = r#"
export function gated(): i64 {
  __intrinsic_target("sha-arm");
  return 1;
}
"#;
    let main = "import { gated } from \"velt:zz_gated\";\nimport { u32x4 } from \"velt:simd\";\n\nfunction main() {\n  if (u32x4.hasShaArm()) {\n    console.log(gated());\n  }\n  console.log(gated());\n  const f = gated;\n}\n";
    check_fails(
        probe,
        main,
        &[
            "`gated` runs only where the CPU has `sha-arm`, so it must be called behind that check",
            "`gated` is compiled for `sha-arm` and can only be called directly",
        ],
    );
}

const ONE_GATED: &str = r#"
export function gated(x: i64): i64 {
  __intrinsic_target("sha-arm");
  return x + 1;
}
"#;

#[test]
fn the_check_is_velt_simds_method_not_its_name() {
    // A user type named `u32x4` with a `hasShaArm` that returns true is not the CPU check.
    let fake = "import { gated } from \"velt:zz_gated\";\n\nclass u32x4 {\n  static hasShaArm(): bool {\n    return true;\n  }\n}\n\nfunction main() {\n  if (u32x4.hasShaArm()) {\n    console.log(gated(1));\n  }\n}\n";
    check_fails(
        ONE_GATED,
        fake,
        &["`gated` runs only where the CPU has `sha-arm`, so it must be called behind that check"],
    );
    // velt:simd's `u32x4` imported under another name is.
    let dir = test_dir::TestDir::new();
    let alias = "import { gated } from \"velt:zz_gated\";\nimport { u32x4 as V } from \"velt:simd\";\n\nfunction main() {\n  console.log(V.hasShaArm() ? gated(1) : 2);\n  if (V.hasShaArm()) {\n    console.log(gated(1));\n  } else {\n    console.log(2);\n  }\n}\n";
    let out = velt(&dir, ONE_GATED, alias, &["run"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "a conditional expression is not a guard"
    );
    assert!(
        stderr.contains("must be called behind that check"),
        "{stderr}"
    );
    let alias = alias.replace("  console.log(V.hasShaArm() ? gated(1) : 2);\n", "");
    let out = velt(&dir, ONE_GATED, &alias, &["run"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    // Debug builds (Cranelift) report the extension unavailable.
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "2", "{stderr}");
}

#[test]
fn call_and_apply_are_direct_calls() {
    let main = "import { gated } from \"velt:zz_gated\";\nimport { u32x4 } from \"velt:simd\";\n\nfunction main() {\n  if (u32x4.hasShaArm()) {\n    console.log(gated.call(undefined, 1), gated.apply(undefined, [2]));\n  }\n  console.log(gated.call(undefined, 3));\n  console.log(gated.apply(undefined, [4]));\n}\n";
    let dir = test_dir::TestDir::new();
    let out = velt(&dir, ONE_GATED, main, &["check"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    assert_eq!(
        err.matches("must be called behind that check").count(),
        2,
        "only the two calls outside the check:\n{err}"
    );
}

#[test]
fn a_gated_function_knows_its_extension_and_stays_out_of_line() {
    // In a function gated on `sha-arm`, the check of `sha-arm` in an inlined method is `1`, so
    // the emulation behind it is dead; the guarded call into it stays a call.
    let probe = r#"
import { u32x4 } from "velt:simd";

export function rounds(a: u32x4, n: i64): u32x4 {
  __intrinsic_target("sha-arm");
  let x = a;
  for (let i = 0; i < n; i++) {
    x = x.sha256h(a, x);
  }
  return x;
}
"#;
    let main = "import { rounds } from \"velt:zz_gated\";\nimport { u32x4 } from \"velt:simd\";\n\nfunction main() {\n  if (u32x4.hasShaArm()) {\n    console.log(rounds(u32x4.splat(1), 3).lane(0));\n  }\n}\n";
    let dir = test_dir::TestDir::new();
    let args = [
        "build",
        "--release",
        "--emit",
        "llvm",
        "--target",
        "aarch64-unknown-linux-gnu",
    ];
    let out = velt(&dir, probe, main, &args);
    let ir = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let gated = ir
        .split("\ndefine ")
        .find(|f| f.contains("zz__gatedN6rounds") && f.contains("\"target-features\"=\"+sha2\" {"))
        .expect("the gated function");
    assert!(!gated.contains("@velt.simd.has_sha_arm()"), "{gated}");
    assert!(gated.contains("store i8 1,"), "{gated}");
    let caller = ir
        .split("\ndefine ")
        .find(|f| f.contains("velt_main") && f.contains("zz__gatedN6rounds"))
        .expect("the caller");
    assert!(
        caller
            .lines()
            .any(|l| l.contains("zz__gatedN6rounds") && l.ends_with(" noinline")),
        "{caller}"
    );
}
