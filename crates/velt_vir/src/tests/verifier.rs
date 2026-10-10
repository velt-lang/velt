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
    let mut p = lower_ok(&super::m2_arrays::arrays());
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
        ty,
        by_ref: false,
        param: false,
    };
    let mut p = base_program();
    p.files.push("main.vlt".into());
    p.funcs[0].locals[0].debug = Some(described(DebugTyId(3)));
    expect_err(&p, "unknown debug type #3");

    let mut p = base_program();
    p.files.push("main.vlt".into());
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
