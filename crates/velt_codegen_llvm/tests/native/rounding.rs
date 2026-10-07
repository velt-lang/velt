//! `Math.floor`, `Math.ceil`, `Math.trunc` and `Math.round` compiled for the host (`clang -O3`)
//! against Rust's `f64::floor` & co. and the runtime's `Math.round`, bit for bit, over edge
//! values: zeros of both signs, halves, the 2^52 and 2^53 boundaries, `i64` limits, the
//! largest doubles, infinities and NaN. The runtime functions are defined to return a marker,
//! so a call left in the code shows as a mismatch (#535).

use std::fmt::Write;

use crate::common::builder::*;
use crate::harness;
use velt_vir::vir::Ty::*;
use velt_vir::vir::{Callee, Program, Terminator};

const FUNCTIONS: [(&str, &str); 4] = [
    ("velt_rt_math_floor", "x.floor()"),
    ("velt_rt_math_ceil", "x.ceil()"),
    ("velt_rt_math_trunc", "x.trunc()"),
    ("velt_rt_math_round", "js_round(x)"),
];

/// `velt_main` and, per function, an exported `entry_<symbol>(x) = symbol(x)`.
fn program() -> Program {
    let mut pb = ProgramBuilder::new();
    for (symbol, _) in FUNCTIONS {
        let ext = pb.ext(symbol, &[F64], F64, false);
        let mut fb = FuncBuilder::export(&format!("entry_{symbol}"), &[F64], F64);
        let r = fb.local(F64);
        let b = fb.block();
        let arg = vec![copy_local(fb.param(0))];
        let b = fb.call(b, Callee::Extern(ext), arg, Some(r));
        fb.term(b, Terminator::Return(copy_local(r)));
        pb.add(fb.finish());
    }
    let mut fb = FuncBuilder::export("velt_main", &[], I32);
    let b = fb.block();
    fb.ret(b, int(0, I32));
    pb.add(fb.finish());
    pb.finish()
}

const HARNESS: &str = r#"
const MARKER: f64 = 12345.0;
fn js_round(x: f64) -> f64 {
    if !x.is_finite() { return x; }
    let f = x.floor();
    let r = if x - f >= 0.5 { f + 1.0 } else { f };
    if r == 0.0 && x.is_sign_negative() { -0.0 } else { r }
}
fn same(a: f64, b: f64) -> bool { (a.is_nan() && b.is_nan()) || a.to_bits() == b.to_bits() }
fn values() -> Vec<f64> {
    let mut xs = vec![0.0, -0.0, 0.5, -0.5, 0.49999999999999994, -0.49999999999999994, 1.0,
        -1.0, 1.5, -1.5, 2.5, -2.5, 2.7, -2.7, 1e-300, -1e-300, 5e-324, -5e-324,
        f64::INFINITY, f64::NEG_INFINITY, f64::NAN, -f64::NAN, f64::MAX, f64::MIN,
        9223372036854775807.0, -9223372036854775808.0, 1e19, -1e19, 1e300, -1e300];
    for e in [52, 53, 31, 32, 62, 63] {
        let p = 2f64.powi(e);
        for d in [-1.5, -1.0, -0.5, -0.25, 0.0, 0.25, 0.5, 1.0, 1.5] {
            xs.push(p + d);
            xs.push(-(p + d));
        }
        xs.push(f64::from_bits(p.to_bits() - 1));
        xs.push(-f64::from_bits(p.to_bits() - 1));
    }
    let mut seed = 0x9E3779B97F4A7C15u64;
    for _ in 0..20000 {
        seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17;
        let x = f64::from_bits(seed);
        xs.push(x);
        xs.push((seed % 2000) as f64 / 8.0 - 125.0);
    }
    xs
}
"#;

fn harness_source() -> String {
    let mut s = String::from(HARNESS);
    for (symbol, _) in FUNCTIONS {
        let _ = writeln!(
            s,
            "#[no_mangle] pub extern \"C\" fn {symbol}(_x: f64) -> f64 {{ MARKER }}"
        );
    }
    s.push_str("extern \"C\" {\n");
    for (symbol, _) in FUNCTIONS {
        let _ = writeln!(s, "    fn entry_{symbol}(x: f64) -> f64;");
    }
    s.push_str("}\nfn main() {\n    let mut bad = 0;\n    for x in values() {\n");
    for (symbol, reference) in FUNCTIONS {
        let _ = writeln!(
            s,
            "        let (got, want) = (unsafe {{ entry_{symbol}(x) }}, {reference});
        if !same(got, want) {{ bad += 1; println!(\"{symbol}({{x:e}}) = {{got:e}}, want {{want:e}}\"); }}"
        );
    }
    s.push_str("    }\n    println!(\"mismatches {bad}\");\n}\n");
    s
}

#[test]
fn rounding_matches_rust_bit_for_bit() {
    let Some((clang, rustc)) = harness::tools() else {
        return;
    };
    let dir = std::env::temp_dir().join(format!("velt_llvm_rounding_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let ir = velt_codegen_llvm::emit_ir(&program(), "").expect("emit_ir");
    let ll = dir.join("rounding.ll");
    std::fs::write(&ll, ir).expect("write .ll");
    let objects = harness::compile_all(&clang, &[ll]);
    let source = dir.join("harness.rs");
    std::fs::write(&source, harness_source()).expect("write harness");
    let output = harness::link_and_run(&rustc, &dir, &source, &objects);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(output.ends_with("mismatches 0\n"), "{output}");
}
