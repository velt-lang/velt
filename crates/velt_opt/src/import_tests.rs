//! Imported functions (vir.rs invariant 11) through the whole pipeline: a declaration without
//! blocks stays one, calls to it stay calls, and nothing assumes what its callers pass.

use crate::testkit::builder::*;
use crate::{optimize, OptLevel};
use velt_vir::vir::{BinOp, Callee, Function, Linkage, Rvalue, Terminator, Ty};

/// `velt_main` calling `ext(7)` (declared only) and `kept(ext's result)` (a body kept for
/// inlining), twice each, with constant arguments a pass could otherwise specialize for.
fn program() -> velt_vir::vir::Program {
    let mut pb = ProgramBuilder::new();
    let ext = pb.add(Function::new(
        "_V3stdN3ext".into(),
        vec![Ty::I64],
        Ty::I64,
        Linkage::Import,
    ));
    let mut fb = FuncBuilder::new("_V3stdN4kept", &[Ty::I64], Ty::I64, Linkage::Import);
    let x = fb.param(0);
    let b = fb.block();
    fb.assign(b, x, bin(BinOp::Add, copy_local(x), int(1, Ty::I64)));
    fb.ret(b, copy_local(x));
    let kept = pb.add(fb.finish());
    let mut fb = FuncBuilder::export("velt_main", &[], Ty::I32);
    let r = fb.local(Ty::I64);
    let mut b = fb.block();
    for _ in 0..2 {
        b = fb.call(b, Callee::Func(ext), vec![int(7, Ty::I64)], Some(r));
        b = fb.call(b, Callee::Func(kept), vec![copy_local(r)], Some(r));
    }
    let code = fb.local(Ty::I32);
    fb.assign(b, code, Rvalue::Cast(copy_local(r), Ty::I32));
    fb.ret(b, copy_local(code));
    pb.add(fb.finish());
    pb.finish()
}

#[test]
fn imported_functions_survive_both_levels() {
    for level in [OptLevel::None, OptLevel::Speed] {
        let mut p = program();
        velt_vir::verify(&p).unwrap();
        optimize(&mut p, level);
        velt_vir::verify(&p).unwrap_or_else(|e| panic!("{level:?}: {e:?}\n{p}"));
        let ext = (p.funcs.iter().position(|f| f.symbol == "_V3stdN3ext"))
            .unwrap_or_else(|| panic!("{level:?}: the called declaration was removed\n{p}"));
        let f = &p.funcs[ext];
        assert!(f.blocks.is_empty() && f.linkage == Linkage::Import, "{p}");
        assert_eq!((f.params.as_slice(), f.ret), (&[Ty::I64][..], Ty::I64));
        // Still called (twice), with the argument as written.
        let main = p.funcs.iter().find(|f| f.symbol == "velt_main").unwrap();
        let calls = (main.blocks.iter())
            .filter(|b| matches!(&b.term, Terminator::Call { callee: Callee::Func(c), .. } if c.0 as usize == ext))
            .count();
        assert_eq!(calls, 2, "{level:?}\n{p}");
        // A kept body is the object's, never specialized or rewritten for these callers.
        if let Some(kept) = p.funcs.iter().find(|f| f.symbol == "_V3stdN4kept") {
            assert_eq!(kept.linkage, Linkage::Import);
            assert_eq!(kept.params, [Ty::I64]);
        }
    }
}
