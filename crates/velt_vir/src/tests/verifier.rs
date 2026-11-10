//! Verifier negative tests: hand-broken VIR must be rejected with a precise message.

use super::lower_ok;
use crate::vir::{self, *};

fn base_program() -> vir::Program {
    lower_ok(&super::goldens::hello())
}

fn expect_err(p: &vir::Program, needle: &str) {
    match crate::verify(p) {
        Ok(()) => panic!("expected verify error containing {needle:?}"),
        Err(errs) => assert!(errs.iter().any(|e| e.contains(needle)), "{errs:?}"),
    }
}

#[test]
fn verify_rejects_use_before_assign() {
    let mut p = base_program();
    let f = &mut p.funcs[0];
    f.blocks[0].stmts.remove(2); // drop `_2 = &_1`; the call then reads an unassigned _2
    expect_err(&p, "may be used before it is assigned");
}

#[test]
fn verify_rejects_bad_jump_and_agg_param() {
    let mut p = base_program();
    p.funcs[0].blocks[0].term = Terminator::Goto(BlockId(99));
    expect_err(&p, "unknown block");

    let mut p = base_program();
    p.funcs[0].params.push(Ty::Agg(STR_AGG));
    p.funcs[0]
        .locals
        .insert(0, LocalDecl::new(Ty::Agg(STR_AGG), None));
    expect_err(&p, "non-scalar");
}

#[test]
fn verify_rejects_binary_type_mismatch_and_missing_main() {
    let mut p = base_program();
    p.funcs[0].locals.push(LocalDecl::new(Ty::I64, None));
    let l = Local(p.funcs[0].locals.len() as u32 - 1);
    p.funcs[0].blocks[0].stmts.push(Stmt::Assign(
        Place::local(l),
        Rvalue::Binary(
            BinOp::Add,
            Operand::Const(Const::Int(1), Ty::I64),
            Operand::Const(Const::Int(1), Ty::I32),
        ),
    ));
    expect_err(&p, "operand types differ");

    let mut p = base_program();
    p.funcs.pop();
    expect_err(&p, "velt_main");
}

#[test]
fn verify_rejects_conditional_init() {
    // bb0: branch c, bb1, bb2; bb1: x = 1; goto bb2; bb2: return x
    let mut p = base_program();
    p.funcs.push(Function {
        locals: vec![
            LocalDecl::new(Ty::Bool, None),
            LocalDecl::new(Ty::I64, Some("x".into())),
        ],
        blocks: vec![
            BasicBlock {
                stmts: vec![],
                term: Terminator::Branch {
                    cond: Operand::Copy(Place::local(Local(0))),
                    then: BlockId(1),
                    els: BlockId(2),
                },
            },
            BasicBlock {
                stmts: vec![Stmt::Assign(
                    Place::local(Local(1)),
                    Rvalue::Use(Operand::Const(Const::Int(1), Ty::I64)),
                )],
                term: Terminator::Goto(BlockId(2)),
            },
            BasicBlock {
                stmts: vec![],
                term: Terminator::Return(Operand::Copy(Place::local(Local(1)))),
            },
        ],
        ..Function::new("t".into(), vec![Ty::Bool], Ty::I64, Linkage::Internal)
    });
    expect_err(&p, "_1 x may be used before it is assigned");
}

#[test]
fn verify_rejects_misplaced_param_attrs() {
    let noalias = ParamAttrs {
        noalias: true,
        ..ParamAttrs::default()
    };
    let mut p = super::lower_src(super::m2_arrays::ARRAYS);
    let fi = p
        .funcs
        .iter()
        .position(|f| f.params.first() == Some(&Ty::Ptr))
        .expect("a function with a pointer param");
    let n = p.funcs[fi].params.len();
    p.funcs[fi].param_attrs = vec![noalias; n + 1];
    expect_err(&p, "param_attrs has");

    let fi = p
        .funcs
        .iter()
        .position(|f| f.params.first().is_some_and(|t| *t != Ty::Ptr))
        .expect("a function with a scalar param");
    let n = p.funcs[fi].params.len();
    p.funcs[fi].param_attrs = vec![noalias; n];
    expect_err(&p, "has pointer attributes");
}

/// `t(c: bool) -> i64` with local `_1 x: i64` and the given blocks, added to a valid program.
fn with_test_fn(blocks: Vec<BasicBlock>) -> vir::Program {
    let mut p = base_program();
    let local = |ty, name: Option<&str>| LocalDecl::new(ty, name.map(Into::into));
    p.funcs.push(Function {
        locals: vec![local(Ty::Bool, None), local(Ty::I64, Some("x"))],
        blocks,
        ..Function::new("t".into(), vec![Ty::Bool], Ty::I64, Linkage::Internal)
    });
    p
}

fn block(stmts: Vec<Stmt>, term: Terminator) -> BasicBlock {
    BasicBlock { stmts, term }
}

fn set_x(v: i128) -> Stmt {
    Stmt::Assign(
        Place::local(Local(1)),
        Rvalue::Use(Operand::Const(Const::Int(v), Ty::I64)),
    )
}

fn branch(then: u32, els: u32) -> Terminator {
    Terminator::Branch {
        cond: Operand::Copy(Place::local(Local(0))),
        then: BlockId(then),
        els: BlockId(els),
    }
}

fn ret_x() -> Terminator {
    Terminator::Return(Operand::Copy(Place::local(Local(1))))
}

#[test]
fn verify_accepts_init_on_every_path_and_ignores_unreachable_reads() {
    // bb0: branch bb1, bb2; bb1: x = 1; goto bb3; bb2: x = 2; goto bb3; bb3: return x;
    // bb4 (unreachable): return x
    let p = with_test_fn(vec![
        block(vec![], branch(1, 2)),
        block(vec![set_x(1)], Terminator::Goto(BlockId(3))),
        block(vec![set_x(2)], Terminator::Goto(BlockId(3))),
        block(vec![], ret_x()),
        block(vec![], ret_x()),
    ]);
    assert_eq!(crate::verify(&p), Ok(()));
}

#[test]
fn verify_checks_loop_carried_reads() {
    // bb0: x = 0; goto bb1; bb1: branch bb2, bb3; bb2: x = x + 1 (read first); goto bb1;
    // bb3: return x — fine: x is assigned before the loop.
    let incr = Stmt::Assign(
        Place::local(Local(1)),
        Rvalue::Binary(
            BinOp::Add,
            Operand::Copy(Place::local(Local(1))),
            Operand::Const(Const::Int(1), Ty::I64),
        ),
    );
    let looped = |entry: Vec<Stmt>| {
        with_test_fn(vec![
            block(entry, Terminator::Goto(BlockId(1))),
            block(vec![], branch(2, 3)),
            block(vec![incr.clone()], Terminator::Goto(BlockId(1))),
            block(vec![], ret_x()),
        ])
    };
    assert_eq!(crate::verify(&looped(vec![set_x(0)])), Ok(()));
    // Without the assignment before the loop, the first iteration reads x unassigned (bb2)
    // and so does the exit (bb3), although the loop body assigns it.
    let errs = crate::verify(&looped(vec![])).expect_err("x is read unassigned");
    let at = |bb: &str| {
        errs.iter()
            .any(|e| e.contains(&format!("{bb}: local _1 x may be used")))
    };
    assert!(at("bb2") && at("bb3"), "{errs:?}");
}

#[test]
fn verify_rejects_dangling_debug_descriptions() {
    let described = |ty: DebugTyId| LocalDebug {
        decl: SrcLoc {
            file: 0,
            line: 1,
            col: 1,
        },
        end: None,
        ty,
        by_ref: false,
        param: false,
    };
    let mut p = base_program();
    p.files.push("main.velt".into());
    p.funcs[0].locals[0].debug = Some(described(DebugTyId(3)));
    expect_err(&p, "unknown debug type #3");

    let mut p = base_program();
    p.files.push("main.velt".into());
    p.debug_types.push(DebugTy {
        name: "Point".into(),
        kind: DebugKind::Struct {
            agg: STR_AGG,
            fields: vec![DebugField {
                name: "x".into(),
                index: 7,
                ty: DebugTyId(0),
            }],
        },
    });
    expect_err(&p, "bad field `x` of agg#0");
}

/// An atomic operation's extern: its symbol must be canonical, it returns, and no code takes
/// its address (no function exists behind it).
#[test]
fn verify_rejects_misused_atomic_externs() {
    let atomic = |symbol: &str, noreturn| ExternFn {
        symbol: symbol.into(),
        params: vec![Ty::Ptr],
        ret: Ty::U8,
        noreturn,
        host: None,
        c_decl: None,
        variadic: false,
    };
    let mut p = base_program();
    p.externs.push(atomic("velt_atomic_load_08_acq", false));
    expect_err(&p, "velt_atomic_load_08_acq: not an atomic operation");

    let mut p = base_program();
    p.externs.push(atomic("velt_atomic_load_8_acq", true));
    expect_err(&p, "velt_atomic_load_8_acq: an atomic operation returns");

    let mut p = with_test_fn(vec![block(vec![set_x(0)], ret_x())]);
    let ext = ExternId(p.externs.len() as u32);
    p.externs.push(atomic("velt_atomic_load_8_acq", false));
    let f = p.funcs.last_mut().expect("test fn");
    f.locals.push(LocalDecl::new(Ty::Ptr, None));
    f.blocks[0].stmts.push(Stmt::Assign(
        Place::local(Local(2)),
        Rvalue::Use(Operand::Const(Const::Extern(ext), Ty::Ptr)),
    ));
    expect_err(&p, "the address of `velt_atomic_load_8_acq` is taken");
}

/// Invariant 11: only an imported function may have no blocks; it prints as a declaration.
#[test]
fn only_imported_functions_have_no_blocks() {
    let mut p = base_program();
    let decl = Function::new(
        "_V3stdN4twice".into(),
        vec![Ty::I64],
        Ty::I64,
        Linkage::Import,
    );
    p.funcs.push(decl);
    crate::verify(&p).unwrap();
    assert!(
        p.to_string().contains(&format!(
            "fn#{} import _V3stdN4twice(i64) -> i64\n",
            p.funcs.len() - 1
        )),
        "{p}"
    );
    p.funcs.last_mut().unwrap().linkage = Linkage::Internal;
    expect_err(&p, "has no blocks");
}

/// Code compiled on its own to link into a program (the std cache) declares the entry rather
/// than defining it (invariant 5); any other linkage, or none, is still an error.
#[test]
fn the_entry_may_be_declared() {
    let mut p = base_program();
    let main = p
        .funcs
        .iter()
        .position(|f| f.symbol == "velt_main")
        .unwrap();
    p.funcs[main] = Function::new("velt_main".into(), vec![], Ty::I32, Linkage::Import);
    crate::verify(&p).unwrap();
    p.funcs[main].linkage = Linkage::Internal;
    p.funcs[main].blocks.push(BasicBlock {
        stmts: vec![],
        term: Terminator::Return(Operand::Const(Const::Int(0), Ty::I32)),
    });
    expect_err(&p, "velt_main must be");
    p.funcs.remove(main);
    expect_err(&p, "missing exported entry");
}

/// The program with one more extern, `symbol(params) -> ret`.
fn with_extern(symbol: &str, params: Vec<Ty>, ret: Ty) -> vir::Program {
    let mut p = base_program();
    p.externs.push(ExternFn {
        symbol: symbol.into(),
        params,
        ret,
        noreturn: false,
        host: None,
        c_decl: None,
        variadic: false,
    });
    p
}

#[test]
fn verify_rejects_math_externs_with_another_signature() {
    // `velt_rt_math_clz64` is `(u64) -> u64` (`velt_vir::math::MathOp::Clz64`).
    let ok = with_extern("velt_rt_math_clz64", vec![Ty::U64], Ty::U64);
    assert_eq!(crate::verify(&ok), Ok(()));
    let wrong_param = with_extern("velt_rt_math_clz64", vec![Ty::I64], Ty::U64);
    expect_err(&wrong_param, "signature differs from the math operation's");
    let wrong_ret = with_extern("velt_rt_pow_f64", vec![Ty::F64, Ty::F64], Ty::I64);
    expect_err(&wrong_ret, "signature differs from the math operation's");
    let mut noreturn = with_extern("velt_rt_math_sqrt", vec![Ty::F64], Ty::F64);
    noreturn.externs.last_mut().expect("the extern").noreturn = true;
    expect_err(&noreturn, "signature differs from the math operation's");
}

/// `_n = math.sqrt(x)` appended to `main`'s first block, with `sqrt`'s extern declared or not.
fn with_sqrt(x: Operand, declare: bool) -> vir::Program {
    let mut p = base_program();
    if declare {
        p.externs.push(ExternFn {
            symbol: "velt_rt_math_sqrt".into(),
            params: vec![Ty::F64],
            ret: Ty::F64,
            noreturn: false,
            host: None,
            c_decl: None,
            variadic: false,
        });
    }
    let f = &mut p.funcs[0];
    f.locals.push(LocalDecl::new(Ty::F64, None));
    let l = Local(f.locals.len() as u32 - 1);
    let op = crate::math::MathOp::Sqrt;
    f.blocks[0].stmts.push(Stmt::Assign(
        Place::local(l),
        Rvalue::Intrinsic(Intrinsic::Math(op), vec![x]),
    ));
    if let Some(locs) = f.locs.get_mut(0) {
        let at = locs.len().saturating_sub(1);
        locs.insert(at, None);
    }
    p
}

#[test]
fn verify_checks_intrinsics() {
    let two = Operand::Const(Const::Float(2.0), Ty::F64);
    assert_eq!(crate::verify(&with_sqrt(two.clone(), true)), Ok(()));
    expect_err(&with_sqrt(two, false), "without its runtime function");
    let int = Operand::Const(Const::Int(2), Ty::I64);
    expect_err(
        &with_sqrt(int, true),
        "intrinsic math.sqrt takes [F64], got [I64]",
    );
}

#[test]
fn verify_rejects_a_direct_call_of_a_math_operation() {
    // t: x = velt_rt_pow_i64(2, 10) as a call: math operations are statements.
    let call = |p: &vir::Program| {
        let pow = ExternId(p.externs.len() as u32 - 1);
        block(
            vec![],
            Terminator::Call {
                callee: Callee::Extern(pow),
                args: vec![
                    Operand::Const(Const::Int(2), Ty::I64),
                    Operand::Const(Const::Int(10), Ty::I64),
                ],
                dest: Some(Place::local(Local(1))),
                next: BlockId(1),
            },
        )
    };
    let mut p = with_test_fn(vec![]);
    p.externs.push(ExternFn {
        symbol: "velt_rt_pow_i64".into(),
        params: vec![Ty::I64, Ty::I64],
        ret: Ty::I64,
        noreturn: false,
        host: None,
        c_decl: None,
        variadic: false,
    });
    let t = p.funcs.len() - 1;
    p.funcs[t].blocks = vec![call(&p), block(vec![], ret_x())];
    expect_err(
        &p,
        "a call of `velt_rt_pow_i64`: math operations are statements",
    );
    // The same operation as a statement is accepted.
    let pow = Intrinsic::Math(crate::math::MathOp::PowI64);
    let args = vec![
        Operand::Const(Const::Int(2), Ty::I64),
        Operand::Const(Const::Int(10), Ty::I64),
    ];
    let stmt = Stmt::Assign(Place::local(Local(1)), Rvalue::Intrinsic(pow, args));
    p.funcs[t].blocks = vec![block(vec![stmt], ret_x())];
    assert_eq!(crate::verify(&p), Ok(()));
}

/// A variadic extern's call passes at least its fixed arguments, the further ones promoted (no
/// type narrower than `I32`, no `F32`); any other extern's passes exactly its parameters.
#[test]
fn variadic_calls_pass_promoted_further_arguments() {
    let call = |variadic: bool, extra: Operand| {
        let mut p = with_test_fn(vec![]);
        let ext = ExternId(p.externs.len() as u32);
        p.externs.push(ExternFn {
            symbol: "fcntl".into(),
            params: vec![Ty::I32, Ty::I32],
            ret: Ty::I32,
            noreturn: false,
            host: None,
            c_decl: None,
            variadic,
        });
        let f = p.funcs.last_mut().expect("test fn");
        f.locals.push(LocalDecl::new(Ty::I32, None));
        let i32_ = |v| Operand::Const(Const::Int(v), Ty::I32);
        f.blocks = vec![
            block(
                vec![set_x(0)],
                Terminator::Call {
                    callee: Callee::Extern(ext),
                    args: vec![i32_(3), i32_(4), extra],
                    dest: Some(Place::local(Local(2))),
                    next: BlockId(1),
                },
            ),
            block(vec![], ret_x()),
        ];
        p
    };
    crate::verify(&call(true, Operand::Const(Const::Int(1), Ty::I32))).unwrap();
    crate::verify(&call(true, Operand::Const(Const::Float(1.0), Ty::F64))).unwrap();
    expect_err(
        &call(true, Operand::Const(Const::Int(1), Ty::I8)),
        "variadic call arg 2: I8 is not a promoted C type",
    );
    expect_err(
        &call(true, Operand::Const(Const::Float(1.0), Ty::F32)),
        "variadic call arg 2: F32 is not a promoted C type",
    );
    expect_err(
        &call(false, Operand::Const(Const::Int(1), Ty::I32)),
        "call passes 3 args, callee takes 2",
    );
}
