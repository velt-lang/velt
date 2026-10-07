//! `Math.floor`, `Math.ceil`, `Math.trunc` and `Math.round` (`function/rounding.rs`), JIT-compiled
//! for the baseline ISA of the host (on x86-64 without SSE4.1: the conversion sequence) and for
//! the host's own features (`roundsd` / `frint*`), against Rust's `f64::floor` & co. and the
//! runtime's `Math.round`, bit for bit, over edge values. The runtime functions are mapped to a
//! marker, so a call left in the code shows as a mismatch (#535).

use cranelift_jit::{JITBuilder, JITModule};
use velt_vir::vir::{Callee, Program, Terminator, Ty};

use super::{copy_local, FuncBuilder, ProgramBuilder};

const MARKER: f64 = 12345.0;

extern "C" fn marker(_x: f64) -> f64 {
    MARKER
}

/// `Math.round`, as the runtime computes it (`velt_rt_math_round`).
fn js_round(x: f64) -> f64 {
    if !x.is_finite() {
        return x;
    }
    let f = x.floor();
    let r = if x - f >= 0.5 { f + 1.0 } else { f };
    if r == 0.0 && x.is_sign_negative() {
        -0.0
    } else {
        r
    }
}

type Reference = fn(f64) -> f64;

const FUNCTIONS: [(&str, Reference); 4] = [
    ("velt_rt_math_floor", f64::floor),
    ("velt_rt_math_ceil", f64::ceil),
    ("velt_rt_math_trunc", f64::trunc),
    ("velt_rt_math_round", js_round),
];

/// One function `x -> symbol(x)` per entry of `FUNCTIONS`, in order.
fn program() -> Program {
    let (mut pb, _) = ProgramBuilder::new();
    for (symbol, _) in FUNCTIONS {
        let ext = pb.ext(symbol, &[Ty::F64], Ty::F64, false);
        let mut fb = FuncBuilder::internal(&format!("entry_{symbol}"), &[Ty::F64], Ty::F64);
        let r = fb.local(Ty::F64);
        let b = fb.block();
        let arg = vec![copy_local(fb.param(0))];
        let b = fb.call(
            b,
            Callee::Extern(ext),
            arg,
            Some(velt_vir::vir::Place::local(r)),
        );
        fb.term(b, Terminator::Return(copy_local(r)));
        pb.add(fb.finish());
    }
    pb.p
}

/// Edge values: zeros, halves, the 2^31/2^32/2^52/2^53/2^62/2^63 boundaries, the extremes,
/// infinities, NaN, and pseudo-random doubles (any bit pattern, and multiples of 1/8).
fn values() -> Vec<f64> {
    let mut xs = vec![
        0.0,
        -0.0,
        0.5,
        -0.5,
        0.49999999999999994,
        -0.49999999999999994,
        1.5,
        -1.5,
        2.5,
        -2.5,
        2.7,
        -2.7,
        1e-300,
        -1e-300,
        5e-324,
        -5e-324,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
        -f64::NAN,
        f64::MAX,
        f64::MIN,
        1e19,
        -1e19,
    ];
    for e in [31, 32, 52, 53, 62, 63] {
        let p = 2f64.powi(e);
        for d in [-1.5, -1.0, -0.5, -0.25, 0.0, 0.25, 0.5, 1.0, 1.5] {
            xs.extend([p + d, -(p + d)]);
        }
        let below = f64::from_bits(p.to_bits() - 1);
        xs.extend([below, -below]);
    }
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    for _ in 0..20_000 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        xs.extend([f64::from_bits(seed), (seed % 2000) as f64 / 8.0 - 125.0]);
    }
    xs
}

/// The mismatches of the program compiled for `target` (`""`: the host with its features).
fn mismatches(target: &str, optimize: bool) -> Vec<String> {
    let isa = crate::isa::make_isa(target, optimize, true).expect("ISA");
    let mut jb = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
    for (name, p) in super::jit::stub_symbols() {
        jb.symbol(name, p);
    }
    for (symbol, _) in FUNCTIONS {
        jb.symbol(symbol, marker as *const u8);
    }
    let mut module = JITModule::new(jb);
    let program = program();
    let built = crate::module::build_module(&mut module, &program, &crate::module::Naming::Program)
        .expect("build");
    module.finalize_definitions().expect("finalize");
    let mut bad = vec![];
    for (i, (symbol, reference)) in FUNCTIONS.iter().enumerate() {
        let code = module.get_finalized_function(built.funcs[i].expect("defined"));
        let f: extern "C" fn(f64) -> f64 = unsafe { std::mem::transmute(code) };
        for x in values() {
            let (got, want) = (f(x), reference(x));
            let same = (got.is_nan() && want.is_nan()) || got.to_bits() == want.to_bits();
            if !same {
                bad.push(format!("{symbol}({x:e}) = {got:e}, want {want:e}"));
            }
        }
    }
    unsafe { module.free_memory() };
    bad
}

#[test]
fn rounding_matches_rust_bit_for_bit() {
    for target in [crate::host_triple(), String::new()] {
        for optimize in [false, true] {
            let bad = mismatches(&target, optimize);
            assert!(
                bad.is_empty(),
                "target `{target}`, optimize={optimize}: {} mismatches:\n{}",
                bad.len(),
                bad[..bad.len().min(20)].join("\n")
            );
        }
    }
}
