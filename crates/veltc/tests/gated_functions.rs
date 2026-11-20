//! Gated functions (#1330; the decorator `@targetFeature` of velt/lang/intrinsics, re-exported by
//! velt/compiler, docs/internals/design/package-machine-code.md §2): every call sits behind the
//! check of the function's CPU extension or in a function gated on the same target; a gated
//! function is never a value; a closure inside one does not inherit its target; methods are not
//! gated. Most cases gate a std function: std is copied to a temp directory with a
//! probe module added, and `velt check` / `velt run` use it through `$VELT_STD` (as
//! std_simd_import.rs does); the last ones gate a program's own function through velt/compiler.

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

/// A std with `probe.velt` added, and `main.velt` beside it; runs `velt <args> main.velt`.
fn velt(dir: &test_dir::TestDir, probe: &str, main: &str, args: &[&str]) -> Output {
    let std: PathBuf = dir.path().join("std");
    if !std.exists() {
        copy_tree(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std"),
            &std,
        );
    }
    // A module of its own (a folder's index.velt): user code imports only modules.
    std::fs::create_dir_all(std.join("zz_gated")).unwrap();
    // Each probe gates its functions with the decorator, which std imports like any definition.
    let probe = format!("import {{ targetFeature }} from \"velt/lang/intrinsics\";\n{probe}");
    std::fs::write(std.join("zz_gated/index.velt"), probe).unwrap();
    let file = dir.path().join("main.velt");
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

@targetFeature("sha-arm")
function twice(v: u32x4): u32x4 {
  return v.add(v);
}

@targetFeature("sha-arm")
function four(v: u32x4): u32x4 {
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

@targetFeature("sha-x86")
function gated(): i64 {
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
            "call it inside `if (Sha.isSupported()) { … }` (the check from velt/arch/x86)",
        ],
    );
}

#[test]
fn a_gated_function_is_not_a_value() {
    let probe = r#"
import { u32x4 } from "velt/simd";

@targetFeature("sha-arm")
function gated(): i64 {
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

@targetFeature("sha-arm")
function inner(): i64 {
  return 1;
}

@targetFeature("sha-arm")
function outer(): i64 {
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
fn the_decorator_names_a_known_target_and_a_function() {
    // An unknown target and a method are type errors; the statement form std used before the
    // decorator is an error too, never a silently ungated function (an older std change rebased
    // onto the decorator).
    let probe = r#"
@targetFeature("sha-mips")
function unknown(): i64 {
  return 1;
}

class Box {
  @targetFeature("sha-arm")
  get(): i64 {
    return 1;
  }
}

function old(): i64 {
  targetFeature("sha-arm");
  return 1;
}

export function probe(): i64 {
  return unknown() + new Box().get() + old();
}
"#;
    check_fails(
        probe,
        MAIN_PROBE,
        &[
            "3:16: error: this argument of the decorator doesn't fit its parameter's type",
            "9:3: error: the decorator `targetFeature` cannot be applied to a method",
            "16:3: error: `targetFeature` is a decorator: it can only be used as `@targetFeature`",
        ],
    );
}

#[test]
fn the_decorator_refuses_what_nothing_would_check() {
    // A function C calls: nothing checks the CPU before C calls it (a std probe, since a program
    // needs native code for `@export`).
    let probe = r#"
import { export as cExport } from "velt/ffi";

@cExport("zz_gated_probe")
@targetFeature("sha-arm")
function forC(): i64 {
  return 1;
}

export function probe(): i64 {
  return forC();
}
"#;
    check_fails(
        probe,
        MAIN_PROBE,
        &["6:1: error: `@targetFeature` can't gate a function C calls (`@export`)"],
    );
}

#[test]
fn user_code_calls_an_exported_gated_function_only_behind_the_check() {
    let probe = r#"
@targetFeature("sha-arm")
export function gated(): i64 {
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
@targetFeature("sha-arm")
export function gated(x: i64): i64 {
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

@targetFeature("sha-arm")
export function roundsArm(a: u32x4, n: i64): u32x4 {
  let x = a;
  for (let i = 0; i < n; i++) {
    x = x.sha256h(a, x).sha256h(x, a).sha256h2(a, x).sha256h2(x, a);
    x = x.sha256su0(a).sha256su0(x).sha256su1(a, x).sha256su1(x, a);
    x = x.sha1c(1, a).sha1c(2, x).sha1p(3, a).sha1p(4, x).sha1m(5, a).sha1m(6, x);
    x = x.sha1su0(a, x).sha1su0(x, a).sha1su1(a).sha1su1(x);
  }
  return x;
}

@targetFeature("sha-x86")
export function roundsX86(a: u32x4, n: i64): u32x4 {
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

@targetFeature("clmul-x86")
export function foldX86(a: u64x2, n: i64): u64x2 {
  let x = a;
  for (let i = 0; i < n; i++) {
    x = x.clmulLow(a).xor(x.clmulHigh(a)).xor(x.clmulLowHigh(a)).xor(x.clmulHighLow(a));
  }
  return x;
}

@targetFeature("clmul-arm")
export function foldArm(a: u64x2, n: i64): u64x2 {
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

/// The AES rounds and a carry-less product in a function gated on the AES target, per
/// architecture: the target covers the carry-less multiplication too (#2401).
const AES_PROBE: &str = r#"
import { u8x16 } from "velt/simd";

@targetFeature("aes-x86")
export function roundsX86(a: u8x16, k: u8x16, n: i64): u8x16 {
  let x = a;
  for (let i = 0; i < n; i++) {
    x = x.aesenc(k).aesenclast(k).aesdec(k).aesdeclast(k).aesimc().aeskeygenassist(1);
    x = x.xor(x.asU64x2().clmulLow(k.asU64x2()).asU8x16());
  }
  return x;
}

@targetFeature("aes-arm")
export function roundsArm(a: u8x16, k: u8x16, n: i64): u8x16 {
  let x = a;
  for (let i = 0; i < n; i++) {
    x = x.aese(k).aesmc().aesd(k).aesimc();
    x = x.xor(x.asU64x2().clmulHigh(k.asU64x2()).asU8x16());
  }
  return x;
}
"#;

const AES_MAIN: &str = "import { roundsArm, roundsX86 } from \"velt/zz_gated\";\nimport { u8x16 } from \"velt/simd\";\n\nfunction main() {\n  const a = u8x16.splat(3);\n  if (u8x16.hasAesX86()) {\n    console.log(roundsX86(a, a, 3).lane(0));\n  }\n  if (u8x16.hasAesArm()) {\n    console.log(roundsArm(a, a, 3).lane(0));\n  }\n}\n";

#[test]
fn aes_gated_functions_are_the_bare_instructions() {
    // In a function gated on `aes-x86` (`aes-arm`), the methods are AES-NI (ARMv8 AES) and the
    // carry-less product PCLMULQDQ (PMULL), with no check, no CPUID and no emulation left.
    let dir = test_dir::TestDir::new();
    for (triple, gated, instructions) in [
        (
            "x86_64-unknown-linux-gnu",
            "N9roundsX86",
            &[
                "@llvm.x86.aesni.aesenc(",
                "@llvm.x86.aesni.aesenclast(",
                "@llvm.x86.aesni.aesdec(",
                "@llvm.x86.aesni.aesdeclast(",
                "@llvm.x86.aesni.aesimc(",
                "@llvm.x86.aesni.aeskeygenassist(",
                "@llvm.x86.pclmulqdq(",
            ][..],
        ),
        (
            "aarch64-unknown-linux-gnu",
            "N9roundsArm",
            &[
                "@llvm.aarch64.crypto.aese(",
                "@llvm.aarch64.crypto.aesmc(",
                "@llvm.aarch64.crypto.aesd(",
                "@llvm.aarch64.crypto.aesimc(",
                "@llvm.aarch64.neon.pmull64(",
            ][..],
        ),
    ] {
        let args = ["build", "--release", "--emit", "llvm", "--target", triple];
        let out = velt(&dir, AES_PROBE, AES_MAIN, &args);
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
        for (f, body) in &reached {
            for check in [
                "@velt.simd.has_",
                "velt_cpu_",
                "@velt.simd.cpuid",
                "aes__emulation",
                "N6detect",
            ] {
                assert!(
                    !body.contains(check),
                    "{triple}: {f}, reached from {gated}, has {check}:\n{body}"
                );
            }
        }
        for instruction in instructions {
            assert!(
                reached.iter().any(|(_, body)| body.contains(instruction)),
                "{triple}: {gated} uses {instruction}"
            );
        }
    }
}

/// The carry-less target's functions, and the AES target's, per architecture.
const COVER_GATES: &str = r#"
import { u8x16, u64x2 } from "velt/simd";

@targetFeature("clmul-x86")
function clmulX86(): i64 {
  return 1;
}

@targetFeature("clmul-arm")
function clmulArm(): i64 {
  return 2;
}

@targetFeature("aes-x86")
function aesX86(): i64 {
  return 3;
}

@targetFeature("aes-arm")
function aesArm(): i64 {
  return 4;
}
"#;

#[test]
fn the_aes_target_covers_the_carry_less_one() {
    // Behind the AES check, and from a function gated on the AES target (#2401): every CPU with
    // the instructions has the carry-less multiplication of its architecture.
    let probe = format!(
        r#"{COVER_GATES}
@targetFeature("aes-x86")
function fromAesX86(): i64 {{
  return clmulX86();
}}

@targetFeature("aes-arm")
function fromAesArm(): i64 {{
  return clmulArm();
}}

export function probe(): i64 {{
  if (u8x16.hasAesX86()) {{
    return clmulX86() + fromAesX86() + aesX86();
  }}
  if (u8x16.hasAesArm()) {{
    return clmulArm() + fromAesArm() + aesArm();
  }}
  return u64x2.hasClmul() ? 1 : 0;
}}
"#
    );
    let dir = test_dir::TestDir::new();
    let out = velt(&dir, &probe, MAIN_PROBE, &["check"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
}

#[test]
fn the_carry_less_target_does_not_cover_the_aes_one() {
    // A CPU can have the carry-less multiplication without AES, and each target exists on one
    // architecture only.
    for (gate, target, check) in [
        ("aesX86", "aes-x86", "u64x2.hasClmulX86()"),
        ("aesArm", "aes-arm", "u64x2.hasClmulArm()"),
        ("clmulArm", "clmul-arm", "u8x16.hasAesX86()"),
        ("clmulX86", "clmul-x86", "u8x16.hasAesArm()"),
    ] {
        let probe = format!(
            "{COVER_GATES}\nexport function probe(): i64 {{\n  if ({check}) {{\n    return {gate}();\n  }}\n  return u8x16.hasAes() && u64x2.hasClmul() ? 1 : 0;\n}}\n"
        );
        let message = format!("`{gate}` runs only where the CPU has `{target}`");
        check_fails(&probe, MAIN_PROBE, &[&message]);
    }
    for (gate, target, from) in [
        ("aesX86", "aes-x86", "clmul-x86"),
        ("aesArm", "aes-arm", "clmul-arm"),
        ("clmulArm", "clmul-arm", "aes-x86"),
    ] {
        let probe = format!(
            "{COVER_GATES}\n@targetFeature(\"{from}\")\nfunction caller(): i64 {{\n  return {gate}();\n}}\n\nexport function probe(): i64 {{\n  return u8x16.hasAes() && u64x2.hasClmul() ? 1 : 0;\n}}\n"
        );
        let message = format!("`{gate}` runs only where the CPU has `{target}`");
        check_fails(&probe, MAIN_PROBE, &[&message]);
    }
}

/// A program's own gated function, through velt/compiler: the guarded call compiles and runs
/// (the gated copy where the CPU has carry-less multiplication in a release build, the portable
/// path in a debug one, where every check is false), with the same result either way.
const PROGRAM_GATED: &str = r#"import { targetFeature } from "velt/compiler";
import { u64x2 } from "velt/simd";

@targetFeature("clmul-x86")
function mulX86(a: u64x2, b: u64x2): u64x2 {
  return a.clmulLow(b);
}

function mulPortable(a: u64x2, b: u64x2): u64x2 {
  return a.clmulLow(b);
}

function main() {
  const a = u64x2.splat(0x1234);
  const b = u64x2.splat(0x5678);
  let c = u64x2.splat(0);
  if (u64x2.hasClmulX86()) {
    c = mulX86(a, b);
  } else {
    c = mulPortable(a, b);
  }
  console.log(c.lane(0));
}
"#;

#[test]
fn a_program_gates_its_own_function_through_velt_compiler() {
    let dir = test_dir::TestDir::new();
    let mut outs = vec![];
    for args in [&["run"][..], &["run", "--release"]] {
        let out = velt(&dir, "", PROGRAM_GATED, args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        outs.push(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    assert_eq!(outs[0], outs[1]);
    assert!(!outs[0].trim().is_empty());
    // And the program's function is compiled for its extension, out of line.
    let args = [
        "build",
        "--release",
        "--emit",
        "llvm",
        "--target",
        "x86_64-unknown-linux-gnu",
    ];
    let out = velt(&dir, "", PROGRAM_GATED, &args);
    let ir = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let functions = llvm_functions(&ir);
    let gated = (functions.iter())
        .find(|(f, _)| f.contains("mulX86"))
        .unwrap_or_else(|| panic!("no mulX86 in:\n{ir}"));
    assert!(
        gated.1.contains("\"target-features\""),
        "mulX86 has its extension's features:\n{}",
        gated.1
    );
}

/// A program gating on several of velt/arch's features (#3222 step 3): a function gated on AVX2
/// and BMI2 needs both checks, and may call one gated on AVX, which AVX2 implies.
const PROGRAM_ARCH: &str = r#"import { targetFeature } from "velt/compiler";
import { Avx2, Bmi2 } from "velt/arch/x86";

@targetFeature("avx-x86")
function base(x: i64): i64 {
  return x + 1;
}

@targetFeature("avx2-x86", "bmi2-x86")
function both(x: i64): i64 {
  return base(x) * 2;
}

function main() {
  let r: i64 = 42;
  if (Avx2.isSupported() && Bmi2.isSupported()) {
    r = both(20);
  }
  console.log(r);
}
"#;

#[test]
fn a_program_gates_on_several_features_with_velt_arch() {
    let dir = test_dir::TestDir::new();
    for args in [&["run"][..], &["run", "--release"]] {
        let out = velt(&dir, "", PROGRAM_ARCH, args);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{args:?}: {err}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "42\n", "{args:?}");
    }
    let args = [
        "build",
        "--release",
        "--emit",
        "llvm",
        "--target",
        "x86_64-unknown-linux-gnu",
    ];
    let out = velt(&dir, "", PROGRAM_ARCH, &args);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let ir = String::from_utf8_lossy(&out.stdout);
    let functions = llvm_functions(&ir);
    let both = (functions.iter())
        .find(|(f, _)| f.contains("both"))
        .unwrap_or_else(|| panic!("no `both` in:\n{ir}"));
    let define = both.1.lines().next().unwrap_or_default();
    assert!(
        define.contains("\"target-features\"=\"+avx2,+bmi2\""),
        "`both` gets both features:\n{}",
        both.1
    );
    // One check of the two is not enough, and neither is a set that doesn't cover the other's.
    let one = PROGRAM_ARCH.replace(
        "Avx2.isSupported() && Bmi2.isSupported()",
        "Avx2.isSupported()",
    );
    let out = velt(&dir, "", &one, &["check"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    for m in [
        "`both` runs only where the CPU has `avx2-x86` and `bmi2-x86`, so it must be called behind those checks",
        "call it inside `if (Avx2.isSupported() && Bmi2.isSupported()) { … }` (the checks from velt/arch/x86)",
    ] {
        assert!(err.contains(m), "missing `{m}` in:\n{err}");
    }
    let narrower = PROGRAM_ARCH.replace("\"avx2-x86\", \"bmi2-x86\"", "\"bmi2-x86\"");
    let out = velt(&dir, "", &narrower, &["check"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("`base` runs only where the CPU has `avx-x86`"),
        "{err}"
    );
}

#[test]
fn a_programs_gated_function_keeps_the_rules() {
    // Outside std the same rules hold: a call behind no check and a use as a value are errors.
    let main = r#"import { targetFeature } from "velt/compiler";
import { u64x2 } from "velt/simd";

@targetFeature("clmul-x86")
function mulX86(a: u64x2, b: u64x2): u64x2 {
  return a.clmulLow(b);
}

function main() {
  const a = u64x2.splat(3);
  console.log(mulX86(a, a).lane(0));
  const f = mulX86;
}
"#;
    let dir = test_dir::TestDir::new();
    let out = velt(&dir, "", main, &["check"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    for m in [
        "11:15: error: `mulX86` runs only where the CPU has `clmul-x86`, so it must be called behind that check",
        "12:13: error: `mulX86` is compiled for `clmul-x86` and can only be called directly",
    ] {
        assert!(err.contains(m), "missing `{m}` in:\n{err}");
    }
}

#[test]
fn a_program_can_gate_only_what_its_target_reaches() {
    // `main` runs unchecked; a nested, async or generator function is compiled into a closure or
    // a resume function the target would not reach; an overload signature calls the
    // implementation unchecked. Each is refused, with the decorator on the implementation or on
    // a signature.
    let main = r#"import { targetFeature } from "velt/compiler";

@targetFeature("clmul-x86")
async function later(): Promise<i64> {
  return 1;
}

@targetFeature("clmul-x86")
function* gen(): Generator<i64> {
  yield 1;
}

function outer(): i64 {
  @targetFeature("clmul-x86")
  function inner(): i64 {
    return 1;
  }
  return inner();
}

function capturing(k: i64): i64 {
  @targetFeature("clmul-x86")
  function inner(): i64 {
    return k;
  }
  return inner();
}

class Box {
  v: i64 = 1;
  get(): i64 {
    const v = this.v;
    @targetFeature("clmul-x86")
    function inner(): i64 {
      return v;
    }
    return inner();
  }
}

function twice(a: i64): i64;
function twice(a: f64): f64;
@targetFeature("clmul-x86")
function twice(a: i64 | f64): i64 | f64 {
  return a;
}

@targetFeature("clmul-x86")
function once(a: i64): i64;
function once(a: f64): f64;
function once(a: i64 | f64): i64 | f64 {
  return a;
}

@targetFeature("clmul-x86")
async function main() {
  console.log(outer() + capturing(1) + new Box().get() + twice(1) + once(2) + (await later()));
  for (const v of gen()) {
    console.log(v);
  }
}
"#;
    let dir = test_dir::TestDir::new();
    let out = velt(&dir, "", main, &["check"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    for m in [
        "3:1: error: `@targetFeature` can't gate an async function or a generator",
        "8:1: error: `@targetFeature` can't gate an async function or a generator",
        "14:3: error: `@targetFeature` can't gate a function declared inside another",
        "22:3: error: `inner` uses its enclosing function's locals, so it takes no decorators",
        "33:5: error: `inner` uses its enclosing function's locals, so it takes no decorators",
        "43:1: error: `@targetFeature` can't gate an overloaded function",
        "48:1: error: `@targetFeature` can't gate an overloaded function",
        "55:1: error: `@targetFeature` can't gate the program's entry, `main`",
    ] {
        assert!(err.contains(m), "missing `{m}` in:\n{err}");
    }
    assert!(
        !err.contains("not compatible with its implementation"),
        "an overload's refusal is the decorator's own error:\n{err}"
    );
}

/// velt/arch's instructions (#3222 step 4) are gated on their class's feature: a call sits behind
/// that check (or one whose feature implies it) or in a gated function; one anywhere else, the
/// method taken as a value and the extern declared outside std are errors.
#[test]
fn a_velt_arch_instruction_runs_only_behind_its_check() {
    let dir = test_dir::TestDir::new();
    let good = r#"
import { targetFeature } from "velt/compiler";
import { Avx2, Sse42 } from "velt/arch/x86";

@targetFeature("avx2-x86")
function viaAvx2(c: u64): u64 {
  return Sse42.crc32U64(c, 1);
}

function main() {
  if (Sse42.isSupported()) {
    console.log(Sse42.crc32U64(0, 1));
  }
  if (Avx2.isSupported()) {
    console.log(viaAvx2(0) + Sse42.crc32U64(0, 2));
  }
}
"#;
    let out = velt(&dir, "", good, &["check"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bad = r#"
import { Bmi2, Sse42 } from "velt/arch/x86";

declare function velt_arch_sse42_crc32_u64(crc: u64, data: u64): u64;

function main() {
  console.log(Sse42.crc32U64(0, 1));
  const f = Bmi2.parallelBitDepositU64;
}
"#;
    let out = velt(&dir, "", bad, &["check"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    for want in [
        "`velt_arch_sse42_crc32_u64` is an instruction of velt/arch: only the standard library may declare it",
        "`Sse42.crc32U64` runs only where the CPU has `sse4.2-x86`, so it must be called behind that check",
        "`Bmi2.parallelBitDepositU64` is compiled for `bmi2-x86` and can only be called directly",
    ] {
        assert!(err.contains(want), "no `{want}` in:\n{err}");
    }
}
