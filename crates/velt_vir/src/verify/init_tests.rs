//! The cost of the definite-assignment check: a chain of `if`s that each assign an accumulator
//! is checked in work linear in its length, counted rather than timed.

use super::*;

/// `f(x)`: `s = 0; if (x < 0) s = s + 1 else s = s - 1; ...` `n` times, then `return s`. Each
/// `if` is a condition block and two branches joining at the next condition block, so the
/// dominator tree is a path of `n` condition blocks and `s` is assigned in `2n + 1` blocks, none
/// of which dominates a later read.
fn chain(n: usize) -> Function {
    let (x, s) = (Local(0), Local(1));
    let mut locals = vec![
        LocalDecl::new(Ty::I64, Some("x".into())),
        LocalDecl::new(Ty::I64, Some("s".into())),
    ];
    let int = |v: i128| Operand::Const(Const::Int(v), Ty::I64);
    let copy = |l: Local| Operand::Copy(Place::local(l));
    let assign = |l: Local, rv: Rvalue| Stmt::Assign(Place::local(l), rv);
    let mut blocks = vec![BasicBlock {
        stmts: vec![assign(s, Rvalue::Use(int(0)))],
        term: Terminator::Goto(BlockId(1)),
    }];
    for _ in 0..n {
        let c = Local(locals.len() as u32);
        locals.push(LocalDecl::new(Ty::Bool, None));
        let at = blocks.len() as u32;
        let (then, els, join) = (BlockId(at + 1), BlockId(at + 2), BlockId(at + 3));
        blocks.push(BasicBlock {
            stmts: vec![assign(c, Rvalue::Binary(BinOp::Lt, copy(x), int(0)))],
            term: Terminator::Branch {
                cond: copy(c),
                then,
                els,
            },
        });
        for op in [BinOp::Add, BinOp::Sub] {
            blocks.push(BasicBlock {
                stmts: vec![assign(s, Rvalue::Binary(op, copy(s), int(1)))],
                term: Terminator::Goto(join),
            });
        }
    }
    blocks.push(BasicBlock {
        stmts: vec![],
        term: Terminator::Return(copy(s)),
    });
    Function {
        locals,
        blocks,
        ..Function::new("f".into(), vec![Ty::I64], Ty::I64, Linkage::Internal)
    }
}

/// The blocks the dominator climbs look at for `f`, checking that it has no errors.
fn steps(f: &Function) -> u64 {
    let p = Program::default();
    let mut errs = vec![];
    let steps = FnCheck {
        p: &p,
        f,
        errs: &mut errs,
        ctx: String::new(),
    }
    .definite_init();
    assert!(errs.is_empty(), "{errs:?}");
    steps
}

#[test]
fn a_chain_of_ifs_is_checked_in_linear_work() {
    let small = steps(&chain(500));
    let large = steps(&chain(4000));
    // Without remembering answers, each of the 2n reads of `s` climbs the whole path to the
    // entry: the square of the length.
    assert!(
        large <= small * 9,
        "8 times the length costs {large} against {small}: more than linear"
    );
}

#[test]
fn a_read_no_assignment_dominates_is_still_reported() {
    // Drop `s = 0`: every branch reads `s` before any path assigned it.
    let mut f = chain(20);
    f.blocks[0].stmts.clear();
    let p = Program::default();
    let mut errs = vec![];
    FnCheck {
        p: &p,
        f: &f,
        errs: &mut errs,
        ctx: String::new(),
    }
    .definite_init();
    assert!(
        errs.iter()
            .any(|e| e.contains("_1 s may be used before it is assigned")),
        "{errs:?}"
    );
}
