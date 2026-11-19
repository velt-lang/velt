//! The target features of a program that uses a CPU extension (#1330): every function and helper
//! states some on arm64, and an ordinary function has each extension the program uses.

use super::builder::*;
use velt_vir::vir::Ty::*;
use velt_vir::vir::*;

/// `velt_main` running `op` on two zero vectors, then `velt_simd_u64x2_xor` on the result,
/// returning lane 0.
fn sample(ops: &[&str]) -> Program {
    let mut pb = ProgramBuilder::new();
    let ops: Vec<ExternId> = ops
        .iter()
        .map(|op| pb.ext(op, &[V128, V128], V128, false))
        .collect();
    let xor = pb.ext("velt_simd_u64x2_xor", &[V128, V128], V128, false);
    let lane = pb.ext("velt_simd_u64x2_lane", &[V128, U64], U64, false);
    let mut fb = FuncBuilder::export("velt_main", &[], I32);
    let (v, r, w) = (fb.local(V128), fb.local(U64), fb.local(I32));
    let zero = Operand::Const(Const::Int(0), V128);
    let mut b = fb.block();
    fb.assign(b, v, Rvalue::Use(zero.clone()));
    for op in ops {
        b = fb.call(
            b,
            Callee::Extern(op),
            vec![copy_local(v), zero.clone()],
            Some(v),
        );
    }
    let b = fb.call(b, Callee::Extern(xor), vec![copy_local(v), zero], Some(v));
    let b = fb.call(
        b,
        Callee::Extern(lane),
        vec![copy_local(v), int(0, U64)],
        Some(r),
    );
    fb.assign(b, w, Rvalue::Cast(copy_local(r), I32));
    fb.ret(b, copy_local(w));
    pb.add(fb.finish());
    pb.finish()
}

fn ir(p: &Program, triple: &str) -> String {
    let target = crate::target::normalize(triple).unwrap();
    crate::checked_ir(p, &target, false).unwrap()
}

fn line<'a>(ir: &'a str, needle: &str) -> &'a str {
    ir.lines()
        .find(|l| l.starts_with("define") && l.contains(needle))
        .unwrap_or_else(|| panic!("no definition of {needle} in\n{ir}"))
}

#[test]
fn arm64_helpers_state_neon_when_functions_state_features() {
    // `velt_main` uses PMULL, so it has `+aes`. LLVM 23 inlines an `alwaysinline` helper into it
    // only if the helper states features too ("conflicting target features" otherwise, which
    // left every SIMD operation of such a program a call): every helper does (NEON, the arm64
    // baseline), and the operation's own helper its extension.
    let arm = ir(
        &sample(&["velt_simd_u64x2_pmull_0"]),
        "aarch64-unknown-linux-gnu",
    );
    let main = line(&arm, "@\"velt_main\"(");
    assert!(main.contains("\"target-features\"=\"+aes\""), "{main}");
    for helper in ["@velt.simd.u64x2_xor(", "@velt.simd.u64x2_lane("] {
        let def = line(&arm, helper);
        assert!(def.contains("\"target-features\"=\"+neon\""), "{def}");
    }
    let pmull = line(&arm, "@velt.simd.u64x2_pmull_0(");
    assert!(pmull.contains("\"target-features\"=\"+aes\""), "{pmull}");
    // Without an extension no function states features, and neither does a helper.
    let plain = ir(&sample(&[]), "aarch64-unknown-linux-gnu");
    assert!(!line(&plain, "@velt.simd.u64x2_xor(").contains("target-features"));
    // x86 inlines a helper without features into any function (LLVM checks a subset there).
    let x86 = ir(
        &sample(&["velt_simd_u64x2_pclmulqdq_0"]),
        "x86_64-unknown-linux-gnu",
    );
    assert!(!line(&x86, "@velt.simd.u64x2_xor(").contains("target-features"));
}

#[test]
fn a_function_has_every_extension_the_program_uses() {
    // SHA and PCLMULQDQ in one x86 program: an ordinary function gets both features, so both
    // operations' helpers inline into it (with only the first, the other stayed a call).
    let p = sample(&["velt_simd_u32x4_sha256msg1", "velt_simd_u64x2_pclmulqdq_0"]);
    let x86 = ir(&p, "x86_64-unknown-linux-gnu");
    let main = line(&x86, "@\"velt_main\"(");
    assert!(
        main.contains("\"target-features\"=\"+pclmul,+sha\""),
        "{main}"
    );
}

/// Start-up code (`Function::baseline`, std's start-up check of the CPU) under `--cpu x86-64-v4`:
/// it and the helpers its module defines are for clang's `x86-64` with its own features, so
/// nothing in it uses AVX-512 before the check (#3293 review); an ordinary function keeps the
/// build's CPU, and the helpers still inline into it (a subset of its features).
#[test]
fn start_up_code_and_its_helpers_are_for_the_baseline_cpu() {
    const BASE: &str =
        "\"target-cpu\"=\"x86-64\" \"target-features\"=\"+cx8,+fxsr,+mmx,+sse,+sse2,+x87\"";
    let mut p = sample(&[]);
    p.funcs[0].baseline = true;
    let v4 = crate::target::normalize("x86_64-unknown-linux-gnu")
        .unwrap()
        .with_cpu(Some("x86-64-v4"))
        .unwrap();
    let text = crate::checked_ir(&p, &v4, false).unwrap();
    assert!(line(&text, "@\"velt_main\"(").contains(BASE), "{text}");
    assert!(
        line(&text, "@velt.simd.u64x2_lane(").contains(BASE),
        "{text}"
    );
    // Without start-up code, nothing changes.
    p.funcs[0].baseline = false;
    let text = crate::checked_ir(&p, &v4, false).unwrap();
    assert!(!text.contains("\"target-cpu\""), "{text}");
    // On arm64 without `--cpu` the default CPU is the baseline: nothing to state.
    p.funcs[0].baseline = true;
    let arm = ir(&p, "aarch64-unknown-linux-gnu");
    assert!(!arm.contains("\"target-cpu\""), "{arm}");
}
