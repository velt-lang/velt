//! Imported functions (vir.rs invariant 11) through the whole pipeline: a declaration without
//! blocks stays one, calls to it stay calls, and nothing assumes what its callers pass.

use crate::testkit::builder::*;
use crate::{optimize, OptLevel};
use velt_vir::vir::{
    BinOp, Callee, Const, Function, Linkage, LocalDecl, Operand, Place, Rvalue, Terminator, Ty,
};

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

/// `velt_main` builds a closure `c = { code: f, env: 0 }`, hands `&c` to the declaration
/// `ext(ptr)`, then calls through `c.code`. `ext` may have replaced the code pointer: the call
/// stays indirect (no constant-field fact survives the call), and nothing is specialized for the
/// known closure (a declaration has no body to clone).
#[test]
fn a_declaration_may_write_through_what_it_is_given() {
    let mut pb = ProgramBuilder::new();
    let closure = pb.agg("closure", 16, 8, &[(Ty::Ptr, 0), (Ty::Ptr, 8)]);
    let ext = pb.add(Function::new(
        "_V3stdN3ext".into(),
        vec![Ty::Ptr],
        Ty::Unit,
        Linkage::Import,
    ));
    let mut fb = FuncBuilder::new("_V1f", &[Ty::Ptr], Ty::I64, Linkage::Internal);
    let b = fb.block();
    fb.ret(b, int(1, Ty::I64));
    let f = pb.add(fb.finish());
    let mut fb = FuncBuilder::export("velt_main", &[], Ty::I32);
    let c = fb.local(Ty::Agg(closure));
    let p = fb.local(Ty::Ptr);
    let code = fb.local(Ty::Ptr);
    let r = fb.local(Ty::I64);
    let out = fb.local(Ty::I32);
    let mut b = fb.block();
    let fields = vec![Operand::Const(Const::Func(f), Ty::Ptr), int(0, Ty::Ptr)];
    fb.assign(b, c, Rvalue::Aggregate(closure, fields));
    fb.assign(b, p, Rvalue::AddrOf(Place::local(c)));
    b = fb.call(b, Callee::Func(ext), vec![copy_local(p)], None);
    fb.assign(b, code, Rvalue::Use(copy_place(field(c, 0))));
    let through = Callee::Ptr {
        target: copy_local(code),
        params: vec![Ty::Ptr],
        ret: Ty::I64,
    };
    b = fb.call(b, through, vec![int(0, Ty::Ptr)], Some(r));
    fb.assign(b, out, Rvalue::Cast(copy_local(r), Ty::I32));
    fb.ret(b, copy_local(out));
    pb.add(fb.finish());
    let mut p = pb.finish();
    velt_vir::verify(&p).unwrap();
    optimize(&mut p, OptLevel::Speed);
    velt_vir::verify(&p).unwrap_or_else(|e| panic!("{e:?}\n{p}"));
    assert!(!p.funcs.iter().any(|f| f.symbol.contains("$spec")), "{p}");
    let main = p.funcs.iter().find(|f| f.symbol == "velt_main").unwrap();
    let indirect = (main.blocks.iter())
        .filter(|b| {
            matches!(
                &b.term,
                Terminator::Call {
                    callee: Callee::Ptr { .. },
                    ..
                }
            )
        })
        .count();
    assert_eq!(indirect, 1, "the code pointer was assumed unchanged:\n{p}");
}

/// Debug builds inline the compiler's int32 helpers by name; a declaration of one stays a call.
#[test]
fn a_declared_helper_is_not_inlined() {
    let mut pb = ProgramBuilder::new();
    let helper = pb.add(Function::new(
        velt_vir::mangle::mangle("std/prelude/math::__mulToInt32"),
        vec![Ty::F64, Ty::F64],
        Ty::I32,
        Linkage::Import,
    ));
    let mut fb = FuncBuilder::export("velt_main", &[], Ty::I32);
    let r = fb.local(Ty::I32);
    let mut b = fb.block();
    let args = vec![float(2.0, Ty::F64), float(3.0, Ty::F64)];
    b = fb.call(b, Callee::Func(helper), args, Some(r));
    fb.ret(b, copy_local(r));
    pb.add(fb.finish());
    let mut p = pb.finish();
    optimize(&mut p, OptLevel::None);
    velt_vir::verify(&p).unwrap_or_else(|e| panic!("{e:?}\n{p}"));
    let main = p.funcs.iter().find(|f| f.symbol == "velt_main").unwrap();
    assert_eq!(count_calls(main), 1, "{p}");
}

/// The per-function passes leave a declaration alone, whatever it declares: `sroa` splits an
/// aggregate local at a body's first block, which a declaration (here one made from a function,
/// keeping its locals) doesn't have.
#[test]
fn per_function_passes_skip_declarations() {
    let mut pb = ProgramBuilder::new();
    let pair = pb.agg("pair", 16, 8, &[(Ty::I64, 0), (Ty::I64, 8)]);
    let mut ext = Function::new(
        "_V3stdN3ext".into(),
        vec![Ty::Ptr],
        Ty::I64,
        Linkage::Import,
    );
    ext.locals.push(LocalDecl::new(Ty::Agg(pair), None));
    let ext = pb.add(ext);
    let mut fb = FuncBuilder::export("velt_main", &[], Ty::I32);
    let r = fb.local(Ty::I64);
    let code = fb.local(Ty::I32);
    let mut b = fb.block();
    b = fb.call(b, Callee::Func(ext), vec![int(0, Ty::Ptr)], Some(r));
    fb.assign(b, code, Rvalue::Cast(copy_local(r), Ty::I32));
    fb.ret(b, copy_local(code));
    pb.add(fb.finish());
    let program = pb.finish();
    velt_vir::verify(&program).unwrap();
    for level in [OptLevel::None, OptLevel::Speed] {
        let mut p = program.clone();
        optimize(&mut p, level);
        velt_vir::verify(&p).unwrap_or_else(|e| panic!("{level:?}: {e:?}\n{p}"));
        let ext = p.funcs.iter().find(|f| f.symbol == "_V3stdN3ext").unwrap();
        assert!(ext.blocks.is_empty(), "{level:?}\n{p}");
    }
}

/// A declaration may call back what it is given: `g`, whose address `velt_main` hands to `ext`,
/// keeps its own code (no parameter fact from its one direct call, `g(5)`), and is kept.
#[test]
fn a_declaration_may_call_back() {
    let mut pb = ProgramBuilder::new();
    let ext = pb.add(Function::new(
        "_V3stdN3ext".into(),
        vec![Ty::Ptr],
        Ty::Unit,
        Linkage::Import,
    ));
    let mut fb = FuncBuilder::new("_V1g", &[Ty::I64], Ty::I64, Linkage::Internal);
    let x = fb.param(0);
    let b = fb.block();
    fb.assign(b, x, bin(BinOp::Mul, copy_local(x), int(3, Ty::I64)));
    fb.ret(b, copy_local(x));
    let g = pb.add(fb.finish());
    let mut fb = FuncBuilder::export("velt_main", &[], Ty::I32);
    let r = fb.local(Ty::I64);
    let out = fb.local(Ty::I32);
    let mut b = fb.block();
    b = fb.call(
        b,
        Callee::Func(ext),
        vec![Operand::Const(Const::Func(g), Ty::Ptr)],
        None,
    );
    b = fb.call(b, Callee::Func(g), vec![int(5, Ty::I64)], Some(r));
    fb.assign(b, out, Rvalue::Cast(copy_local(r), Ty::I32));
    fb.ret(b, copy_local(out));
    pb.add(fb.finish());
    let program = pb.finish();
    for level in [OptLevel::None, OptLevel::Speed] {
        let mut p = program.clone();
        optimize(&mut p, level);
        velt_vir::verify(&p).unwrap_or_else(|e| panic!("{level:?}: {e:?}\n{p}"));
        let g = (p.funcs.iter().find(|f| f.symbol == "_V1g"))
            .unwrap_or_else(|| panic!("{level:?}: the called-back function was removed\n{p}"));
        assert_eq!(g.params, [Ty::I64], "{level:?}");
        // Still multiplies its argument: not folded to `g(5)`'s 15.
        assert!(
            !g.blocks.iter().any(|b| matches!(
                &b.term,
                Terminator::Return(Operand::Const(Const::Int(15), _))
            )),
            "{level:?}: specialized for its direct caller\n{p}"
        );
    }
}
