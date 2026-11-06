//! A CPU check this build answers false is known in lowering (#2456), so what only its branch
//! reaches is never built: a gated function for another architecture, and under Cranelift every
//! gated function. Every shape of call is also run, since a wrong drop would be a link error or an
//! ICE. Gating is std-only, so std is copied with a probe module added (as gated_functions.rs).

use std::path::{Path, PathBuf};
use std::process::Output;

mod no_window;
mod test_dir;

const X86: &str = "x86_64-unknown-linux-gnu";
const ARM: &str = "aarch64-unknown-linux-gnu";
const WASM: &str = "wasm32-wasip1";

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

/// `velt <args> main.velt`, with a std holding `PROBE` as `velt/zz_gated`.
fn velt(dir: &test_dir::TestDir, main: &str, args: &[&str]) -> Output {
    let std: PathBuf = dir.path().join("std");
    if !std.exists() {
        copy_tree(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std"),
            &std,
        );
        std::fs::create_dir_all(std.join("zz_gated")).unwrap();
        std::fs::write(std.join("zz_gated/index.velt"), PROBE).unwrap();
    }
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

/// Each shape returns the same number whichever path it takes. The ordinary functions hold their
/// name as text, found in the code whether or not they were inlined.
const PROBE: &str = r#"
import { targetFeature } from "velt/lang/intrinsics";
import { u8x16, u32x4, u64x2 } from "velt/simd";

let ticks: i64 = 0;

function belowArmIf(): i64 {
  if (ticks < 0) {
    console.log("belowArmIf");
  }
  return 1;
}
function armIf(): i64 { targetFeature("sha-arm"); return belowArmIf(); }
function x86If(): i64 { targetFeature("sha-x86"); return 1; }
export function shapeIf(): i64 {
  if (u32x4.hasShaArm()) {
    return armIf();
  }
  if (u32x4.hasShaX86()) {
    return x86If();
  }
  return 1;
}

function armElse(): i64 { targetFeature("sha-arm"); return 2; }
function x86Else(): i64 { targetFeature("sha-x86"); return 2; }
function portableElse(): i64 {
  if (ticks < 0) {
    console.log("portableElse");
  }
  return 2;
}
export function shapeElse(): i64 {
  if (u32x4.hasShaArm()) {
    return armElse();
  } else if (u32x4.hasShaX86()) {
    return x86Else();
  } else {
    return portableElse();
  }
}

function armNested(): i64 { targetFeature("clmul-arm"); return 3; }
function x86Nested(): i64 { targetFeature("clmul-x86"); return 3; }
export function shapeNested(): i64 {
  if (u64x2.hasClmul()) {
    if (u64x2.hasClmulX86()) {
      return x86Nested();
    }
    if (u64x2.hasClmulArm()) {
      return armNested();
    }
  }
  return 3;
}

function armAes(): i64 { targetFeature("aes-arm"); return 4; }
function x86Aes(): i64 { targetFeature("aes-x86"); return 4; }
export function shapeAes(): i64 {
  if (u8x16.hasAesArm()) {
    return armAes();
  }
  if (u8x16.hasAesX86()) {
    return x86Aes();
  }
  return 4;
}

function sharedBelow(n: i64): i64 {
  if (ticks < 0) {
    console.log("sharedBelow");
  }
  return n + 5;
}
function armShared(): i64 { targetFeature("sha-arm"); return sharedBelow(0); }
function x86Shared(): i64 { targetFeature("sha-x86"); return sharedBelow(0); }
export function shapeShared(): i64 {
  if (u32x4.hasShaArm()) {
    return armShared();
  }
  if (u32x4.hasShaX86()) {
    return x86Shared();
  }
  return sharedBelow(0);
}

function afterNot(): i64 {
  if (ticks < 0) {
    console.log("afterNot");
  }
  return 6;
}
function behindWrapper(): i64 {
  if (ticks < 0) {
    console.log("behindWrapper");
  }
  return 7;
}
function behindAnd(): bool {
  if (ticks < 0) {
    console.log("behindAnd");
  }
  return true;
}
function behindTernary(): i64 {
  if (ticks < 0) {
    console.log("behindTernary");
  }
  return 8;
}
function wrapsArm(): bool { return u32x4.hasShaArm(); }
function wrapsWrapper(): bool { return !(!wrapsArm()); }

export function shapeNot(): i64 {
  if (!u32x4.hasShaArm()) {
    return 6;
  }
  return afterNot();
}
export function shapeWrapper(): i64 {
  if (wrapsWrapper()) {
    return behindWrapper();
  }
  return 7;
}
export function shapeAnd(): bool {
  return !wrapsArm() || behindAnd();
}
export function shapeTernary(): i64 {
  return wrapsArm() ? behindTernary() : 8;
}
export function shapeAndValue(): bool {
  return wrapsArm() && behindAnd();
}

// Not checks: whatever else they do still happens.
function tick(): bool { ticks = ticks + 1; return true; }
function tickThenCheck(): bool { return tick() && wrapsArm(); }
function tickFirst(): bool { tick(); return wrapsArm(); }
export function shapeEffects(): i64 {
  if (tickThenCheck()) {
    ticks = ticks + 100;
  }
  if (tickFirst()) {
    ticks = ticks + 100;
  }
  return ticks;
}

// A cycle of functions without arguments is no check, and looking at it ends.
function cycleA(): bool { return cycleB(); }
function cycleB(): bool { return cycleA(); }
export function shapeCycle(never: bool): i64 {
  if (never && cycleA()) {
    return 0;
  }
  return 9;
}
"#;

const MAIN: &str = r#"
import { shapeIf, shapeElse, shapeNested, shapeAes, shapeShared, shapeNot, shapeWrapper, shapeAnd, shapeTernary, shapeAndValue, shapeEffects, shapeCycle } from "velt/zz_gated";

function main() {
  console.log(shapeIf(), shapeElse(), shapeNested(), shapeAes(), shapeShared());
  console.log(shapeNot(), shapeWrapper(), shapeTernary(), shapeCycle(false));
  console.log(shapeAnd());
  shapeAndValue();
  console.log(shapeEffects() % 100);
}
"#;

const ARM_FNS: [&str; 5] = ["armIf", "armElse", "armNested", "armAes", "armShared"];
const X86_FNS: [&str; 5] = ["x86If", "x86Else", "x86Nested", "x86Aes", "x86Shared"];
/// Ordinary code behind a check that is false on x86-64 and under Cranelift.
const BEHIND_ARM: [&str; 5] = [
    "belowArmIf",
    "afterNot",
    "behindWrapper",
    "behindTernary",
    "behindAnd",
];

/// The code `velt build <args>` prints.
fn emitted(dir: &test_dir::TestDir, main: &str, args: &[&str]) -> String {
    let mut all = vec!["build"];
    all.extend_from_slice(args);
    let out = velt(dir, main, &all);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{args:?}: {stderr}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn expect(what: &[&str], names: &[&str], here: bool, code: &str) {
    for name in names {
        assert_eq!(
            code.contains(name),
            here,
            "{what:?}: `{name}` should {}be in:\n{code}",
            if here { "" } else { "not " }
        );
    }
}

#[test]
fn the_other_architectures_functions_are_gone_in_release() {
    let dir = test_dir::TestDir::new();
    for (triple, gone, kept) in [(X86, ARM_FNS, X86_FNS), (ARM, X86_FNS, ARM_FNS)] {
        let args = ["--release", "--emit", "llvm", "--target", triple];
        let ir = emitted(&dir, MAIN, &args);
        expect(&args, &gone, false, &ir);
        expect(&args, &kept, true, &ir);
    }
    let args = ["--release", "--emit", "llvm", "--target", X86];
    expect(&args, &BEHIND_ARM, false, &emitted(&dir, MAIN, &args));
}

#[test]
fn nothing_gated_is_left_on_webassembly() {
    let dir = test_dir::TestDir::new();
    let args = ["--release", "--emit", "llvm", "--target", WASM];
    let ir = emitted(&dir, MAIN, &args);
    expect(&args, &ARM_FNS, false, &ir);
    expect(&args, &X86_FNS, false, &ir);
}

#[test]
fn nothing_gated_is_left_under_cranelift() {
    let dir = test_dir::TestDir::new();
    let debug = ["--emit", "vir"];
    let debug_arm = ["--emit", "vir", "--target", ARM];
    let release = ["--release", "--backend", "cranelift", "--emit", "vir"];
    for args in [&debug[..], &debug_arm[..], &release[..]] {
        let vir = emitted(&dir, MAIN, args);
        expect(args, &ARM_FNS, false, &vir);
        expect(args, &X86_FNS, false, &vir);
        expect(args, &BEHIND_ARM, false, &vir);
        // Also called from code that stays: still there, and so is the path after the checks.
        expect(args, &["sharedBelow", "portableElse"], true, &vir);
    }
}

#[test]
fn the_llvm_backend_without_release_keeps_its_own_architectures_functions() {
    let dir = test_dir::TestDir::new();
    let args = ["--emit", "llvm", "--target", X86];
    let ir = emitted(&dir, MAIN, &args);
    expect(&args, &ARM_FNS, false, &ir);
    expect(&args, &X86_FNS, true, &ir);
}

#[test]
fn every_shape_runs_with_every_backend() {
    let dir = test_dir::TestDir::new();
    let release_cl = ["run", "--release", "--backend", "cranelift"];
    for args in [&["run"][..], &["run", "--release"][..], &release_cl[..]] {
        let out = velt(&dir, MAIN, args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{args:?}: {stderr}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "1 2 3 4 5\n6 7 8 9\ntrue\n2\n",
            "{args:?}: {stderr}"
        );
    }
}

const SHA: &str = r#"
import { sha256Hex, sha1 } from "velt/crypto";

function main() {
  console.log(sha256Hex("abc"));
  console.log(sha1([97, 98, 99]).length);
}
"#;

const CRC: &str = r#"
import { gzip } from "velt/compress";

function main() {
  console.log(gzip([97, 98, 99]).length);
}
"#;

const AES: &str = r#"
import { u8x16 } from "velt/simd";

function main() {
  const s = u8x16.splat(1);
  console.log(s.aesenc(s).lane(0), s.aese(s).lane(0));
}
"#;

#[test]
fn std_keeps_only_the_gated_code_of_the_target() {
    let dir = test_dir::TestDir::new();
    let sha_arm = ["sha256Arm", "sha1Arm"];
    let sha_x86 = ["sha256X86", "sha1X86"];
    let x86 = ["--release", "--emit", "llvm", "--target", X86];
    let ir = emitted(&dir, SHA, &x86);
    expect(&x86, &sha_arm, false, &ir);
    expect(&x86, &sha_x86, true, &ir);
    let arm = ["--release", "--emit", "llvm", "--target", ARM];
    let ir = emitted(&dir, SHA, &arm);
    expect(&arm, &sha_arm, true, &ir);
    expect(&arm, &sha_x86, false, &ir);
    let debug = ["--emit", "vir"];
    let vir = emitted(&dir, SHA, &debug);
    expect(&debug, &sha_arm, false, &vir);
    expect(&debug, &sha_x86, false, &vir);

    // A `velt/simd` method drops its other architecture's path: only the emulation's and this
    // architecture's instructions are left.
    let ir = emitted(&dir, AES, &x86);
    expect(&x86, &["aesenc"], true, &ir);
    expect(&x86, &["crypto.aese", "crypto.aesmc"], false, &ir);
    let vir = emitted(&dir, AES, &debug);
    expect(
        &debug,
        &["velt_simd_cpuid", "velt_simd_u8x16_aesenc"],
        false,
        &vir,
    );

    // CRC-32 has a check nested in a check.
    let fold = ["foldX86"];
    expect(&x86, &fold, true, &emitted(&dir, CRC, &x86));
    expect(&arm, &fold, false, &emitted(&dir, CRC, &arm));
    expect(&debug, &fold, false, &emitted(&dir, CRC, &debug));
}
