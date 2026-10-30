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
import { u32x4 } from "velt/simd";

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
    let main = "import { scaled } from \"velt/zz_gated\";\nimport { u32x4 } from \"velt/simd\";\n\nfunction main() {\n  console.log(scaled(u32x4.splat(3)).lane(2));\n}\n";
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
    "import { probe } from \"velt/zz_gated\";\n\nfunction main() {\n  console.log(probe());\n}\n";

#[test]
fn a_call_needs_the_check_of_its_target() {
    let probe = r#"
import { u32x4 } from "velt/simd";

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
import { u32x4 } from "velt/simd";

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
import { u32x4 } from "velt/simd";

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
            "`__intrinsic_target` takes the target as a string literal: one of sha-arm, sha-x86, clmul-x86, clmul-arm",
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
    let main = "import { gated } from \"velt/zz_gated\";\nimport { u32x4 } from \"velt/simd\";\n\nfunction main() {\n  if (u32x4.hasShaArm()) {\n    console.log(gated());\n  }\n  console.log(gated());\n  const f = gated;\n}\n";
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
    let fake = "import { gated } from \"velt/zz_gated\";\n\nclass u32x4 {\n  static hasShaArm(): bool {\n    return true;\n  }\n}\n\nfunction main() {\n  if (u32x4.hasShaArm()) {\n    console.log(gated(1));\n  }\n}\n";
    check_fails(
        ONE_GATED,
        fake,
        &["`gated` runs only where the CPU has `sha-arm`, so it must be called behind that check"],
    );
    // velt/simd's `u32x4` imported under another name is.
    let dir = test_dir::TestDir::new();
    let alias = "import { gated } from \"velt/zz_gated\";\nimport { u32x4 as V } from \"velt/simd\";\n\nfunction main() {\n  console.log(V.hasShaArm() ? gated(1) : 2);\n  if (V.hasShaArm()) {\n    console.log(gated(1));\n  } else {\n    console.log(2);\n  }\n}\n";
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
    let main = "import { gated } from \"velt/zz_gated\";\nimport { u32x4 } from \"velt/simd\";\n\nfunction main() {\n  if (u32x4.hasShaArm()) {\n    console.log(gated.call(undefined, 1), gated.apply(undefined, [2]));\n  }\n  console.log(gated.call(undefined, 3));\n  console.log(gated.apply(undefined, [4]));\n}\n";
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

/// Each SHA method of an extension twice in a function gated on it, and once in ordinary code.
const SHA_PROBE: &str = r#"
import { u32x4 } from "velt/simd";

export function roundsArm(a: u32x4, n: i64): u32x4 {
  __intrinsic_target("sha-arm");
  let x = a;
  for (let i = 0; i < n; i++) {
    x = x.sha256h(a, x).sha256h(x, a).sha256h2(a, x).sha256h2(x, a);
    x = x.sha256su0(a).sha256su0(x).sha256su1(a, x).sha256su1(x, a);
    x = x.sha1c(1, a).sha1c(2, x).sha1p(3, a).sha1p(4, x).sha1m(5, a).sha1m(6, x);
    x = x.sha1su0(a, x).sha1su0(x, a).sha1su1(a).sha1su1(x);
  }
  return x;
}

export function roundsX86(a: u32x4, n: i64): u32x4 {
  __intrinsic_target("sha-x86");
  let x = a;
  for (let i = 0; i < n; i++) {
    x = x.sha256rnds2(a, x).sha256rnds2(x, a).sha256msg1(a).sha256msg1(x);
    x = x.sha256msg2(a).sha256msg2(x).sha1rnds4(a, 0).sha1rnds4(x, 3);
    x = x.sha1nexte(a).sha1nexte(x).sha1msg1(a).sha1msg1(x).sha1msg2(a).sha1msg2(x);
  }
  return x;
}

export function plain(a: u32x4): u32x4 {
  let x = a.sha256h(a, a).sha256h2(a, a).sha256su0(a).sha256su1(a, a);
  x = x.sha1c(1, a).sha1p(2, a).sha1m(3, a).sha1su0(a, a).sha1su1(a);
  x = x.sha256rnds2(a, a).sha256msg1(a).sha256msg2(a).sha1rnds4(a, 1);
  return x.sha1nexte(a).sha1msg1(a).sha1msg2(a);
}
"#;

const SHA_MAIN: &str = "import { roundsArm, roundsX86, plain } from \"velt/zz_gated\";\nimport { u32x4 } from \"velt/simd\";\n\nfunction main() {\n  const a = u32x4.splat(1);\n  if (u32x4.hasShaArm()) {\n    console.log(roundsArm(a, 3).lane(0));\n  }\n  if (u32x4.hasShaX86()) {\n    console.log(roundsX86(a, 3).lane(0));\n  }\n  console.log(plain(a).lane(0));\n}\n";

/// The LLVM functions of `ir` by name: `define … @name(…) … { body }`.
fn llvm_functions(ir: &str) -> Vec<(String, &str)> {
    ir.split("\ndefine ")
        .skip(1)
        .filter_map(|f| Some((callees_in(f.lines().next()?).into_iter().next()?, f)))
        .collect()
}

/// The `@name`s and `@"name"`s `text` refers to (without the quotes).
fn callees_in(text: &str) -> Vec<String> {
    text.split('@')
        .skip(1)
        .map(|rest| match rest.strip_prefix('"') {
            Some(quoted) => quoted.split('"').next().unwrap_or_default().to_string(),
            None => rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || "$._-".contains(*c))
                .collect(),
        })
        .collect()
}

/// `name` and every function it calls, transitively, in `functions`.
fn reachable<'a>(functions: &'a [(String, &'a str)], name: &str) -> Vec<(&'a str, &'a str)> {
    let mut seen: Vec<(&str, &str)> = Vec::new();
    let mut pending = vec![name.to_string()];
    while let Some(n) = pending.pop() {
        let Some((n, body)) = functions.iter().find(|(f, _)| *f == n) else {
            continue;
        };
        if seen.iter().any(|(s, _)| s == n) {
            continue;
        }
        seen.push((n, body));
        pending.extend(body.lines().skip(1).flat_map(callees_in));
    }
    seen
}

#[test]
fn a_gated_function_knows_its_extension_and_stays_out_of_line() {
    // In a function gated on an extension, its check is true in every SHA method it calls,
    // however often each is called and whether ordinary code calls it too: neither the gated
    // function nor anything it calls tests the CPU flag or keeps the emulation. Ordinary code
    // keeps the check, and its guarded call into the gated function stays a call.
    let dir = test_dir::TestDir::new();
    for (triple, gated, ext) in [
        ("aarch64-unknown-linux-gnu", "N9roundsArm", "arm"),
        ("x86_64-unknown-linux-gnu", "N9roundsX86", "x86"),
    ] {
        let args = ["build", "--release", "--emit", "llvm", "--target", triple];
        let out = velt(&dir, SHA_PROBE, SHA_MAIN, &args);
        let ir = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{triple}: {stderr}");
        let functions = llvm_functions(&ir);
        let name = |part: &str| {
            functions
                .iter()
                .find(|(f, _)| f.contains("zz__gated") && f.contains(part))
                .map(|(f, _)| f.clone())
                .unwrap_or_else(|| panic!("{triple}: no {part}"))
        };
        let check = format!("@velt.simd.has_sha_{ext}()");
        let gated = name(gated);
        assert!(
            functions
                .iter()
                .any(|(f, body)| *f == gated && body.contains("\"target-features\"")),
            "{triple}: {gated} has its extension's features"
        );
        for (f, body) in reachable(&functions, &gated) {
            assert!(
                !body.contains(&check) && !body.contains("velt_cpu_sha"),
                "{triple}: {f}, reached from {gated}, tests the CPU flag:\n{body}"
            );
        }
        // `plain` may be inlined into `main` (one call site): the check is somewhere `main`
        // reaches, and not in the gated functions (above), so ordinary code keeps it.
        let main = functions
            .iter()
            .find(|(f, _)| f.contains("velt_main"))
            .map(|(f, _)| f.clone())
            .expect("the caller");
        assert!(
            reachable(&functions, &main)
                .iter()
                .any(|(_, body)| body.contains(&check)),
            "{triple}: ordinary code keeps the check"
        );
        let caller = functions
            .iter()
            .find(|(f, _)| f.contains("velt_main"))
            .map(|(_, body)| *body)
            .expect("the caller");
        assert!(
            caller
                .lines()
                .any(|l| callees_in(l).contains(&gated) && l.ends_with(" noinline")),
            "{triple}: {caller}"
        );
    }
}

/// Each carry-less multiplication in a function gated on its extension, per architecture.
const CLMUL_PROBE: &str = r#"
import { u64x2 } from "velt/simd";

export function foldX86(a: u64x2, n: i64): u64x2 {
  __intrinsic_target("clmul-x86");
  let x = a;
  for (let i = 0; i < n; i++) {
    x = x.clmulLow(a).xor(x.clmulHigh(a)).xor(x.clmulLowHigh(a)).xor(x.clmulHighLow(a));
  }
  return x;
}

export function foldArm(a: u64x2, n: i64): u64x2 {
  __intrinsic_target("clmul-arm");
  let x = a;
  for (let i = 0; i < n; i++) {
    x = x.clmulLow(a).xor(x.clmulHigh(a)).xor(x.clmulLowHigh(a)).xor(x.clmulHighLow(a));
  }
  return x;
}
"#;

const CLMUL_MAIN: &str = "import { foldArm, foldX86 } from \"velt/zz_gated\";\nimport { u64x2 } from \"velt/simd\";\n\nfunction main() {\n  const a = u64x2.splat(3);\n  if (u64x2.hasClmulX86()) {\n    console.log(foldX86(a, 3).lane(0));\n  }\n  if (u64x2.hasClmulArm()) {\n    console.log(foldArm(a, 3).lane(0));\n  }\n}\n";

#[test]
fn clmul_gated_functions_are_the_bare_instruction() {
    // In a function gated on `clmul-x86` (`clmul-arm`), the methods are PCLMULQDQ (PMULL) with
    // no test of the CPU flag and no emulation left.
    let dir = test_dir::TestDir::new();
    for (triple, gated, ext, instruction) in [
        (
            "x86_64-unknown-linux-gnu",
            "N7foldX86",
            "x86",
            "@llvm.x86.pclmulqdq",
        ),
        (
            "aarch64-unknown-linux-gnu",
            "N7foldArm",
            "arm",
            "@llvm.aarch64.neon.pmull64",
        ),
    ] {
        let args = ["build", "--release", "--emit", "llvm", "--target", triple];
        let out = velt(&dir, CLMUL_PROBE, CLMUL_MAIN, &args);
        let ir = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{triple}: {stderr}");
        let functions = llvm_functions(&ir);
        let gated = functions
            .iter()
            .find(|(f, _)| f.contains("zz__gated") && f.contains(gated))
            .map(|(f, _)| f.clone())
            .unwrap_or_else(|| panic!("{triple}: no {gated}"));
        let reached = reachable(&functions, &gated);
        let check = format!("@velt.simd.has_clmul_{ext}()");
        for (f, body) in &reached {
            assert!(
                !body.contains(&check) && !body.contains("velt_cpu_clmul"),
                "{triple}: {f}, reached from {gated}, tests the CPU flag:\n{body}"
            );
        }
        assert!(
            reached.iter().any(|(_, body)| body.contains(instruction)),
            "{triple}: {gated} uses {instruction}"
        );
    }
}
