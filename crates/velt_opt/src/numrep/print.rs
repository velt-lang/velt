//! Numbers printed as integers. `velt_rt_strbuf_push_f64(b, x)` (a template's `${x}`) and
//! `velt_rt_write_f64(s, x)` (`console.log(x)`) of a whole `x` below 2^53 print its integer
//! digits, so they become the integer formatters (`…_i64`) of `x as i64`: no conversion to
//! `f64` and no test in the runtime once `x` is an integer. `console.log` prints `-0` as `-0`,
//! so there `x` must not be `-0` either (`String(-0)` is `0`, which the integer prints).

use velt_vir::vir::{Callee, ExternFn, ExternId, Function, Local, LocalDecl, Operand, Place};
use velt_vir::vir::{Rvalue, Stmt, Terminator, Ty};

use super::fact::{Fact, TWO_53};
use super::Env;
use crate::srclocs::push_stmt;

/// (float formatter, integer formatter, whether the float one prints `-0` as `-0`).
const PAIRS: [(&str, &str, bool, Ty); 2] = [
    (
        "velt_rt_strbuf_push_f64",
        "velt_rt_strbuf_push_i64",
        false,
        Ty::Ptr,
    ),
    ("velt_rt_write_f64", "velt_rt_write_i64", true, Ty::U32),
];

/// Declares the integer formatter of each float formatter the program calls.
pub(crate) fn declare(externs: &mut Vec<ExternFn>) {
    for (float, int, _, first) in PAIRS {
        let used = externs.iter().any(|e| e.symbol == float);
        if used && !externs.iter().any(|e| e.symbol == int) {
            externs.push(ExternFn {
                symbol: int.to_string(),
                params: vec![first, Ty::I64],
                ret: Ty::Unit,
                noreturn: false,
            });
        }
    }
}

/// The formatters of `externs`: (float one, integer one, prints `-0`).
pub(super) fn pairs(externs: &[ExternFn]) -> Vec<(ExternId, ExternId, bool)> {
    let id = |s: &str| {
        externs
            .iter()
            .position(|e| e.symbol == s)
            .map(|i| ExternId(i as u32))
    };
    PAIRS
        .iter()
        .filter_map(|(f, i, neg_zero, _)| Some((id(f)?, id(i)?, *neg_zero)))
        .collect()
}

/// Block `bi` ends in a float formatter call whose number `x` (with fact `f`) prints as an
/// integer: call the integer formatter with `x as i64` instead. Returns whether it did.
pub(super) fn as_integer(env: &Env, func: &mut Function, bi: usize, f: Fact) -> bool {
    let Terminator::Call {
        callee: Callee::Extern(id),
        args,
        ..
    } = &func.blocks[bi].term
    else {
        return false;
    };
    let Some(&(_, int, prints_neg_zero)) = env.formatters.iter().find(|p| p.0 == *id) else {
        return false;
    };
    let whole = f.integral && !f.nan && f.lo > -TWO_53 && f.hi < TWO_53;
    if !whole || (prints_neg_zero && f.neg_zero) || args.len() != 2 {
        return false;
    }
    let x = args[1].clone();
    func.locals.push(LocalDecl {
        ty: Ty::I64,
        name: None,
    });
    let t = Local(func.locals.len() as u32 - 1);
    let at = func.term_loc(bi);
    push_stmt(
        func,
        bi,
        Stmt::Assign(Place::local(t), Rvalue::Cast(x, Ty::I64)),
        at,
    );
    if let Terminator::Call { callee, args, .. } = &mut func.blocks[bi].term {
        *callee = Callee::Extern(int);
        args[1] = Operand::Copy(Place::local(t));
    }
    true
}
