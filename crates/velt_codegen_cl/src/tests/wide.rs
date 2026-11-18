//! The 256-bit vector operations (`function/simd_wide.rs`), JIT-compiled for the host and the
//! baseline ISA, optimized and not, against the same operation in Rust lane by lane. No runtime
//! defines `velt_simd_f32x8_*`, so a call left in the code would fail to resolve: a debug build
//! computes each operation inline, on the two halves. The result may be written over an
//! operand.

use cranelift_jit::{JITBuilder, JITModule};
use velt_vir::simd::SimdCall;
use velt_vir::vir::{Callee, Program, Terminator, Ty};

use super::{copy_local, int, FuncBuilder, ProgramBuilder};

const OPS: [&str; 4] = ["add", "mul", "mul_add", "sqrt"];

/// One function per entry of `OPS` that passes its pointers on to the operation.
fn program() -> Program {
    let (mut pb, _) = ProgramBuilder::new();
    for op in OPS {
        let symbol = format!("velt_simd_f32x8_{op}");
        let (params, ret) = SimdCall::of_symbol(&symbol).expect("wide op").signature();
        assert_eq!(ret, Ty::Unit);
        let ext = pb.ext(&symbol, &params, ret, false);
        let mut fb = FuncBuilder::internal(&format!("entry_{op}"), &params, Ty::U64);
        let b = fb.block();
        let args = (0..params.len() as u32).map(|i| copy_local(fb.param(i)));
        let next = fb.call(b, Callee::Extern(ext), args.collect(), None);
        fb.term(next, Terminator::Return(int(0, Ty::U64)));
        pb.add(fb.finish());
    }
    pb.p
}

type Wide = [f32; 8];
type Two = extern "C" fn(*const Wide, *mut Wide) -> u64;
type Three = extern "C" fn(*const Wide, *const Wide, *mut Wide) -> u64;
type Four = extern "C" fn(*const Wide, *const Wide, *const Wide, *mut Wide) -> u64;

fn bits(v: Wide) -> [u32; 8] {
    v.map(f32::to_bits)
}

fn mismatches(target: &str, optimize: bool) -> Vec<String> {
    let isa = crate::isa::make_isa(target, None, optimize, true).expect("ISA");
    let mut jb = JITBuilder::with_isa(isa.clone(), cranelift_module::default_libcall_names());
    for (name, p) in super::jit::stub_symbols() {
        jb.symbol(name, p);
    }
    let mut module = JITModule::new(jb);
    let program = program();
    let naming = crate::module::Naming::Program;
    let built = crate::module::build_module(&mut module, &*isa, &program, &naming).expect("build");
    module.finalize_definitions().expect("finalize");
    let code = |k: usize| module.get_finalized_function(built.funcs[k].expect("defined"));
    let a: Wide = [1.5, -2.0, 3.25, 0.0, -0.0, 1e30, 7.0, 0.1];
    let b: Wide = [4.0, 0.5, -1.0, 9.0, 2.0, 1e30, -7.0, 0.2];
    let c: Wide = [0.25, 1.0, 1.0, -9.0, 0.0, -1e30, 49.0, 0.3];
    let lanes = |f: &dyn Fn(usize) -> f32| -> Wide { std::array::from_fn(f) };
    let mut bad = vec![];
    let mut check = |what: &str, got: Wide, want: Wide| {
        if bits(got) != bits(want) {
            bad.push(format!("{what}: {got:?}, want {want:?}"));
        }
    };
    unsafe {
        let add = std::mem::transmute::<*const u8, Three>(code(0));
        let mul = std::mem::transmute::<*const u8, Three>(code(1));
        let mul_add = std::mem::transmute::<*const u8, Four>(code(2));
        let sqrt = std::mem::transmute::<*const u8, Two>(code(3));
        let mut out = [0f32; 8];
        add(&a, &b, &mut out);
        check("add", out, lanes(&|k| a[k] + b[k]));
        mul(&a, &b, &mut out);
        check("mul", out, lanes(&|k| a[k] * b[k]));
        mul_add(&a, &b, &c, &mut out);
        check("mul_add", out, lanes(&|k| a[k].mul_add(b[k], c[k])));
        sqrt(&b, &mut out);
        check("sqrt", out, lanes(&|k| b[k].sqrt()));
        // The result over an operand: both halves of every operand are read first.
        let mut x = a;
        let p: *mut Wide = &mut x;
        add(p, p, p);
        check("add over its operands", x, lanes(&|k| a[k] + a[k]));
        let mut x = c;
        let p: *mut Wide = &mut x;
        mul_add(&a, &b, p, p);
        check(
            "mul_add over its addend",
            x,
            lanes(&|k| a[k].mul_add(b[k], c[k])),
        );
    }
    bad
}

#[test]
fn wide_operations_are_inline_on_both_halves() {
    for optimize in [false, true] {
        assert_eq!(mismatches("", optimize), Vec::<String>::new());
    }
}
