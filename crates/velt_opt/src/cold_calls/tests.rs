//! Unit tests for merging the calls that never return.

use super::*;
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use velt_vir::vir::{BinOp, Linkage, Local};

/// `check(i, n)`: two bounds checks, `i < n` and `i + 1 < n`, each reporting through `oob(len,
/// index, at)` with its own location `at`, then `i`. Returns the program and `check`'s id.
fn two_checks(oob_returns: bool) -> (Program, usize) {
    let mut pb = ProgramBuilder::new();
    let panic = pb.ext("velt_rt_panic", &[Ty::I64], Ty::Unit, true);
    let mut fb = FuncBuilder::internal("oob", &[Ty::I64, Ty::I64, Ty::Ptr], Ty::Unit);
    let b = fb.block();
    match oob_returns {
        true => fb.ret(b, Operand::Const(velt_vir::vir::Const::Unit, Ty::Unit)),
        false => {
            let n = fb.call(
                b,
                Callee::Extern(panic),
                vec![copy_local(fb.param(1))],
                None,
            );
            fb.term(n, Terminator::Unreachable);
        }
    }
    let oob = pb.add(fb.finish());
    let (at1, at2) = (pb.stat(b"a.vlt:1:1", 1), pb.stat(b"a.vlt:2:1", 1));
    let mut fb = FuncBuilder::new("check", &[Ty::I64, Ty::I64], Ty::I64, Linkage::Export);
    let (i, n) = (fb.param(0), fb.param(1));
    let (c, j) = (fb.local(Ty::Bool), fb.local(Ty::I64));
    let (b0, b1, b2, b3, fail1, fail2) = (
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
    );
    fb.assign(b0, c, bin(BinOp::Lt, copy_local(i), copy_local(n)));
    fb.branch(b0, c, b1, fail1);
    fb.assign(b1, j, bin(BinOp::Add, copy_local(i), int(1, Ty::I64)));
    fb.assign(b1, c, bin(BinOp::Lt, copy_local(j), copy_local(n)));
    fb.branch(b1, c, b2, fail2);
    fb.goto(b2, b3);
    fb.ret(b3, copy_local(i));
    let static_ptr = |s| Operand::Const(velt_vir::vir::Const::Static(s), Ty::Ptr);
    for (fail, idx, at) in [(fail1, i, at1), (fail2, j, at2)] {
        let args = vec![copy_local(n), copy_local(idx), static_ptr(at)];
        let next = fb.call(fail, Callee::Func(oob), args, None);
        fb.term(next, Terminator::Unreachable);
    }
    let check = pb.add(fb.finish());
    (pb.finish(), check.0 as usize)
}

fn calls(f: &Function) -> Vec<&[Operand]> {
    (f.blocks.iter())
        .filter_map(|b| match &b.term {
            Terminator::Call { args, .. } => Some(args.as_slice()),
            _ => None,
        })
        .collect()
}

#[test]
fn calls_that_never_return_become_one() {
    let (mut p, check) = two_checks(false);
    let no_return = NoReturn::find(&p);
    assert!(run(&no_return, &mut p.funcs[check]));
    crate::simplify_cfg::run(&mut p.funcs[check]);
    assert_valid(&p);
    let f = &p.funcs[check];
    let calls = calls(f);
    assert_eq!(calls.len(), 1, "one call of `oob` is left");
    // `n` is the same at both sites and stays as it is; the index and location are locals.
    assert_eq!(calls[0][0], copy_local(Local(1)));
    assert!(matches!(&calls[0][1], Operand::Copy(p) if p.local.0 >= 4));
    assert!(matches!(&calls[0][2], Operand::Copy(p) if p.local.0 >= 4));
}

#[test]
fn a_callee_that_returns_keeps_its_calls() {
    let (mut p, check) = two_checks(true);
    let no_return = NoReturn::find(&p);
    assert!(!run(&no_return, &mut p.funcs[check]));
    assert_eq!(calls(&p.funcs[check]).len(), 2);
}
