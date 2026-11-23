//! A stack value aligned above the 16 bytes Cranelift aligns a slot to (an `@align(32)` or
//! `@align(64)` struct, #3539): its address, as C code given it would see it, is a multiple of
//! its alignment whatever the frame around it holds, and the value written there reads back.

use cranelift_jit::{JITBuilder, JITModule};
use velt_vir::vir::{AggLayout, BinOp, Proj, Rvalue, Stmt, Terminator, Ty};

use super::{copy_local, copy_place, int, place, FuncBuilder, ProgramBuilder};

/// The alignments checked, and the 8-byte locals in front of the aligned one (each shifts the
/// frame's layout by a word).
const ALIGNS: [u32; 2] = [32, 64];
const PADDING: [usize; 4] = [0, 1, 2, 3];

/// Per alignment and padding, `entry() -> u64`: `(address of x) % align * 1000 + x.0` after
/// `x.0 = 7`, for `x` a local of a struct of one `u64` aligned to `align`.
fn program() -> velt_vir::vir::Program {
    let (mut pb, _) = ProgramBuilder::new();
    for align in ALIGNS {
        let agg = pb.agg(AggLayout {
            name: format!("Aligned{align}"),
            size: align,
            align,
            fields: vec![(Ty::U64, 0)],
        });
        for pad in PADDING {
            let mut fb = FuncBuilder::internal(&format!("entry_{align}_{pad}"), &[], Ty::U64);
            let b = fb.block();
            // Words in memory before the aligned local: their addresses are taken too.
            for _ in 0..pad {
                let w = fb.local(Ty::U64);
                let pw = fb.local(Ty::Ptr);
                fb.assign(b, w, Rvalue::Use(int(1, Ty::U64)));
                fb.assign(b, pw, Rvalue::AddrOf(place(w, vec![])));
            }
            let x = fb.local(Ty::Agg(agg));
            let field = place(x, vec![Proj::Field(0)]);
            fb.push(b, Stmt::Assign(field, Rvalue::Use(int(7, Ty::U64))));
            let p = fb.local(Ty::Ptr);
            fb.assign(b, p, Rvalue::AddrOf(place(x, vec![])));
            let addr = fb.local(Ty::U64);
            fb.assign(b, addr, Rvalue::Cast(copy_local(p), Ty::U64));
            let rem = fb.local(Ty::U64);
            let mask = int(i128::from(align - 1), Ty::U64);
            fb.assign(
                b,
                rem,
                Rvalue::Binary(BinOp::BitAnd, copy_local(addr), mask),
            );
            let scaled = fb.local(Ty::U64);
            let k = int(1000, Ty::U64);
            fb.assign(b, scaled, Rvalue::Binary(BinOp::Mul, copy_local(rem), k));
            let read = copy_place(place(p, vec![Proj::Deref(Ty::Agg(agg)), Proj::Field(0)]));
            let out = fb.local(Ty::U64);
            fb.assign(b, out, Rvalue::Binary(BinOp::Add, copy_local(scaled), read));
            fb.term(b, Terminator::Return(copy_local(out)));
            pb.add(fb.finish());
        }
    }
    pb.p
}

#[test]
fn a_stack_value_aligned_above_16_bytes_sits_at_a_multiple_of_its_alignment() {
    for optimize in [false, true] {
        let isa = crate::isa::make_isa("", None, optimize, true).expect("ISA");
        let mut jb = JITBuilder::with_isa(isa.clone(), cranelift_module::default_libcall_names());
        for (name, p) in super::jit::stub_symbols() {
            jb.symbol(name, p);
        }
        let mut module = JITModule::new(jb);
        let program = program();
        let built = crate::module::build_module(
            &mut module,
            &*isa,
            &program,
            &crate::module::Naming::Program,
        )
        .expect("build");
        module.finalize_definitions().expect("finalize");
        let mut i = 0;
        for align in ALIGNS {
            for pad in PADDING {
                let code = module.get_finalized_function(built.funcs[i].expect("defined"));
                let f: extern "C" fn() -> u64 = unsafe { std::mem::transmute(code) };
                // Called from frames of two depths, so the stack pointer differs too.
                for got in [f(), nested(f)] {
                    assert_eq!(
                        got,
                        7,
                        "align {align}, {pad} words before (optimize: {optimize}): the address \
                         is {} past a multiple of {align}, the value {}",
                        got / 1000,
                        got % 1000
                    );
                }
                i += 1;
            }
        }
        unsafe { module.free_memory() };
    }
}

/// `f()` called from a frame with a word more on the stack.
#[inline(never)]
fn nested(f: extern "C" fn() -> u64) -> u64 {
    let pad = std::hint::black_box([0u8; 24]);
    f() + u64::from(pad[0])
}
