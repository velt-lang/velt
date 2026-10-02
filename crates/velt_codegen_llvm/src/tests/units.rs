//! Codegen units: the split, cross-unit linkage, imports and per-unit statics.

use super::builder::*;
use velt_vir::vir::Ty::*;
use velt_vir::vir::*;

use crate::module::emit_unit;
use crate::target::normalize;
use crate::units::{plan, unit_count, Unit};

/// `a` (tiny), `b` (calls `a`), `c` (calls `b`; returns a vtable static holding `a`'s address)
/// and `velt_main` (calls `c` and `a`, reads the vtable): 2 + 62 + 63 + 65 weight.
fn program() -> Program {
    let mut pb = ProgramBuilder::new();
    let filler = |fb: &mut FuncBuilder, b: BlockId, n: usize| {
        let x = fb.local(I64);
        for i in 0..n {
            fb.assign(b, x, Rvalue::Use(int(i as i128, I64)));
        }
    };
    let a = {
        let mut fb = FuncBuilder::internal("a", &[I64], I64);
        let b = fb.block();
        let r = fb.local(I64);
        fb.assign(b, r, bin(BinOp::Add, copy_local(fb.param(0)), int(1, I64)));
        fb.ret(b, copy_local(r));
        pb.add(fb.finish())
    };
    let b_fn = {
        let mut fb = FuncBuilder::internal("b", &[], I64);
        let b = fb.block();
        filler(&mut fb, b, 59);
        let r = fb.local(I64);
        let b = fb.call(b, Callee::Func(a), vec![int(1, I64)], Some(r));
        fb.ret(b, copy_local(r));
        pb.add(fb.finish())
    };
    let vtable = pb.stat_with(&[0; 8], 8, vec![(0, Const::Func(a))]);
    let c = {
        let mut fb = FuncBuilder::internal("c", &[], Ptr);
        let b = fb.block();
        filler(&mut fb, b, 60);
        let r = fb.local(I64);
        let b = fb.call(b, Callee::Func(b_fn), vec![], Some(r));
        fb.ret(b, Operand::Const(Const::Static(vtable), Ptr));
        pb.add(fb.finish())
    };
    let mut fb = FuncBuilder::export("velt_main", &[], I32);
    let b = fb.block();
    filler(&mut fb, b, 58);
    let (p, r, v) = (fb.local(Ptr), fb.local(I64), fb.local(Ptr));
    let b = fb.call(b, Callee::Func(c), vec![], Some(p));
    let b = fb.call(b, Callee::Func(a), vec![int(2, I64)], Some(r));
    fb.assign(
        b,
        v,
        Rvalue::Use(Operand::Const(Const::Static(vtable), Ptr)),
    );
    fb.ret(b, int(0, I32));
    pb.add(fb.finish());
    pb.finish()
}

#[test]
fn split_into_contiguous_balanced_units() {
    let p = program();
    let plan = plan(&p, 3);
    let defines: Vec<&[usize]> = plan.units.iter().map(|u| u.defines.as_slice()).collect();
    assert_eq!(defines, [&[0, 1][..], &[2], &[3]]);
    // `a` is called from main's unit and sits in c's vtable; `b` is called from c's unit; `c`
    // from main's. main is exported anyway.
    assert_eq!(plan.shared.funcs, [true, true, true, false]);
    let c = &plan.units[1];
    assert_eq!(
        (c.imports.as_slice(), c.declares.as_slice()),
        (&[][..], &[0, 1][..])
    );
    // The vtable is defined once, by the first unit using it, and shared with main's unit.
    assert_eq!(
        (c.statics.as_slice(), c.static_imports.as_slice()),
        (&[0][..], &[][..])
    );
    assert_eq!(plan.shared.statics, [true]);
    // main's unit imports tiny `a` and the vtable, and declares `c` (too large to import).
    let main = &plan.units[2];
    assert_eq!(
        (main.imports.as_slice(), main.declares.as_slice()),
        (&[0][..], &[2][..])
    );
    assert_eq!(
        (main.statics.as_slice(), main.static_imports.as_slice()),
        (&[][..], &[0][..])
    );
    assert!(plan.units[0].declares.is_empty() && plan.units[0].statics.is_empty());
}

#[test]
fn huge_callers_import_nothing() {
    let mut p = program();
    // `b` and main grow past the import limit; main still calls tiny `a`, now only declared.
    for f in [1, 3] {
        let func = &mut p.funcs[f];
        let x = Local(func.locals.len() as u32);
        func.locals.push(LocalDecl {
            ty: I64,
            name: None,
        });
        for i in 0..5_000 {
            func.blocks[0]
                .stmts
                .push(Stmt::Assign(Place::local(x), Rvalue::Use(int(i, I64))));
        }
    }
    let plan = plan(&p, 3);
    let defines: Vec<&[usize]> = plan.units.iter().map(|u| u.defines.as_slice()).collect();
    assert_eq!(defines, [&[0, 1][..], &[2, 3]]);
    let second = &plan.units[1];
    assert!(second.imports.is_empty());
    assert_eq!(second.declares, [0, 1]);
}

#[test]
fn one_unit_unless_requested() {
    let p = program();
    assert_eq!(unit_count(&p, None), 1);
    assert_eq!(unit_count(&p, Some(0)), 1);
    assert_eq!(unit_count(&p, Some(3)), 3);
    // Never more units than functions; uneven weights can leave fewer.
    assert_eq!(unit_count(&p, Some(64)), 4);
    assert_eq!(plan(&p, 64).units.len(), 3);
}

#[test]
fn unit_modules_link_across_units() {
    let p = program();
    let plan = plan(&p, 3);
    let target = normalize("x86_64-unknown-linux-gnu").unwrap();
    let ir: Vec<String> = plan
        .units
        .iter()
        .map(|u| emit_unit(&p, &target, true, u, &plan.shared).unwrap())
        .collect();
    let has = |i: usize, needle: &str| {
        assert!(
            ir[i].contains(needle),
            "unit {i}: missing `{needle}` in\n{}",
            ir[i]
        );
    };
    has(0, "define hidden i64 @\"a\"(");
    has(0, "define hidden i64 @\"b\"(");
    has(1, "declare hidden i64 @\"a\"(i64)");
    has(1, "declare hidden i64 @\"b\"()");
    has(1, "define hidden ptr @\"c\"(");
    has(
        1,
        "@.s0 = hidden unnamed_addr constant <{ ptr }> <{ ptr @\"a\" }>",
    );
    has(2, "define available_externally hidden i64 @\"a\"(");
    has(2, "declare hidden ptr @\"c\"()");
    has(2, "define dso_local i32 @\"velt_main\"(");
    has(
        2,
        "@.s0 = available_externally hidden unnamed_addr constant <{ ptr }> <{ ptr @\"a\" }>",
    );
    assert!(
        !ir[0].contains("@.s0 ="),
        "the first unit does not use the vtable"
    );

    // One unit with everything is the whole-program module.
    let whole = Unit {
        defines: (0..p.funcs.len()).collect(),
        statics: (0..p.statics.len()).collect(),
        ..Unit::default()
    };
    let ir = emit_unit(&p, &target, true, &whole, &Default::default()).unwrap();
    assert!(ir.contains("define internal i64 @\"a\"("));
    assert!(ir.contains("@.s0 = private unnamed_addr constant"));
    assert!(!ir.contains("hidden") && !ir.contains("declare i64"));
}

/// An exported function used from another unit is declared `dso_local` (a direct call, no GOT)
/// but not `hidden`: the shared runtime of debug builds finds exported functions by name.
#[test]
fn exported_functions_are_declared_dso_local() {
    let mut p = program();
    p.funcs[2].linkage = Linkage::Export;
    p.funcs[2].symbol = "velt_exported".into();
    let plan = plan(&p, 3);
    let target = normalize("x86_64-unknown-linux-gnu").unwrap();
    let main = emit_unit(&p, &target, true, &plan.units[2], &plan.shared).unwrap();
    assert!(
        main.contains("declare dso_local ptr @\"velt_exported\"()"),
        "{main}"
    );
    let first = emit_unit(&p, &target, true, &plan.units[1], &plan.shared).unwrap();
    assert!(
        first.contains("define dso_local ptr @\"velt_exported\"("),
        "{first}"
    );
    let all: String = plan
        .units
        .iter()
        .map(|u| emit_unit(&p, &target, true, u, &plan.shared).unwrap())
        .collect();
    for symbol in ["velt_exported", "velt_main"] {
        assert!(
            !all.contains(&format!("hidden ptr @\"{symbol}\"("))
                && !all.contains(&format!("hidden i32 @\"{symbol}\"(")),
            "{symbol} must keep default visibility"
        );
    }
}
