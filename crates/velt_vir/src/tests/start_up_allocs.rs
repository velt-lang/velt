//! The start-up check of the CPU (std/lang/hooks/cpu.velt) runs before `main` in every program,
//! so it allocates nothing on any target: a program's heap is its own from its first statement,
//! and tests that count frees count only the program's. On arm64 macOS each `sysctlbyname` once
//! took two heap boxes, 18 blocks before `main` (std/lang/cpu/arm_darwin.velt).

use super::{interp, std_src};
use crate::LowerOptions;

/// Hello world, for `target`'s variants of std's start-up code, run in the interpreter (which
/// answers every CPU query as a CPU with no feature).
fn hello_on(target: &str) -> interp::Outcome {
    let src = "function main() {\n  console.log(\"hi\");\n}\n";
    let hir = std_src::check_src_for(src, Some(target));
    let opts = LowerOptions {
        target: Some(target),
        ..LowerOptions::default()
    };
    let v = crate::lower_with(&hir, &opts);
    if let Err(errs) = crate::verify(&v) {
        panic!("{target}: verify failed:\n{}", errs.join("\n"));
    }
    interp::run(&v)
}

#[test]
fn the_start_up_check_allocates_nothing_on_any_target() {
    // The targets CI runs, with each arm64 detector (getauxval, sysctlbyname,
    // IsProcessorFeaturePresent) and x86-64's CPUID.
    for target in [
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "x86_64-pc-windows-msvc",
        "aarch64-pc-windows-msvc",
    ] {
        let out = hello_on(target);
        assert_eq!(out.stdout, "hi\n", "{target}: {}", out.stderr);
        assert_eq!(
            (out.allocations, out.frees),
            (0, 0),
            "{target}: hello world allocated before or around `main`"
        );
    }
}
