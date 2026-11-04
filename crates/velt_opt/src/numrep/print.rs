//! Numbers printed as integers. std's float formatters pair up with its integer ones: a call of
//! the hook `numberWrite` (`console.log(x)`), `numberText`, `numberPush`, `floatText` or
//! `floatPush` of a whole `x` below 2^53 becomes one of `intWrite`, `intText` or `intPush` of
//! `x as i64`: no conversion to `f64` and no test in the formatter once `x` is an integer.
//! `console.log` prints `-0` as `-0`, so for `numberWrite` `x` must not be `-0` either
//! (`` `${-0}` `` is `0`, which the integer prints). Each float hook calls its integer twin, so
//! a program that calls one has both. The runtime's float printers have no integer twin left:
//! the runtime prints no integer (#2197).

use velt_vir::vir::Place;
use velt_vir::vir::{Callee, FuncId, Function, Local, LocalDecl, Operand};
use velt_vir::vir::{Rvalue, Stmt, Terminator, Ty};

use super::fact::{Fact, TWO_53};
use super::Env;
use crate::srclocs::push_stmt;

/// The float formatters' hooks and their integer twins: (float one, integer one, whether the
/// float one prints `-0` as `-0`, the index of the number among the arguments), each taking the
/// number at the same place.
const HOOK_PAIRS: [(&str, &str, bool, usize); 5] = [
    (
        velt_common::std_hook!(number_write),
        velt_common::std_hook!(int_write),
        true,
        1,
    ),
    (
        velt_common::std_hook!(number_push),
        velt_common::std_hook!(int_push),
        false,
        1,
    ),
    (
        velt_common::std_hook!(number_text),
        velt_common::std_hook!(int_text),
        false,
        0,
    ),
    (
        velt_common::std_hook!(float_push),
        velt_common::std_hook!(int_push),
        false,
        1,
    ),
    (
        velt_common::std_hook!(float_text),
        velt_common::std_hook!(int_text),
        false,
        0,
    ),
];

/// The formatters of `funcs`: (float one, integer one, prints `-0`, the index of the number
/// among the arguments).
pub(super) fn pairs(funcs: &[Function]) -> Vec<(Callee, Callee, bool, usize)> {
    let mut out = vec![];
    // A hook has one function per lock mode (`_L…` after its symbol); each pairs with the
    // integer hook of its mode.
    for (float, int, neg_zero, at) in HOOK_PAIRS {
        let (fb, ib) = (
            velt_vir::mangle::mangle(float),
            velt_vir::mangle::mangle(int),
        );
        for (fi, f) in funcs.iter().enumerate() {
            let Some(rest) = f.symbol.strip_prefix(fb.as_str()) else {
                continue;
            };
            if !(rest.is_empty() || rest.starts_with("_L")) {
                continue;
            }
            let twin = format!("{ib}{rest}");
            if let Some(ii) = funcs.iter().position(|g| g.symbol == twin) {
                out.push((
                    Callee::Func(FuncId(fi as u32)),
                    Callee::Func(FuncId(ii as u32)),
                    neg_zero,
                    at,
                ));
            }
        }
    }
    out
}

/// The number a call of the float formatter `callee` prints (`None` for other callees).
pub(super) fn number<'a>(env: &Env, callee: &Callee, args: &'a [Operand]) -> Option<&'a Operand> {
    let (.., at) = env.formatters.iter().find(|p| p.0 == *callee)?;
    args.get(*at)
}

/// Block `bi` ends in a float formatter call whose number `x` (with fact `f`) prints as an
/// integer: call the integer formatter with `x as i64` instead. Returns whether it did.
pub(super) fn as_integer(env: &Env, func: &mut Function, bi: usize, f: Fact) -> bool {
    let Terminator::Call { callee, args, .. } = &func.blocks[bi].term else {
        return false;
    };
    let Some((_, int, prints_neg_zero, pos)) = env.formatters.iter().find(|p| p.0 == *callee)
    else {
        return false;
    };
    let whole = f.integral && !f.nan && f.lo > -TWO_53 && f.hi < TWO_53;
    if !whole || (*prints_neg_zero && f.neg_zero) {
        return false;
    }
    let pos = *pos;
    let Some(x) = args.get(pos).cloned() else {
        return false;
    };
    func.locals.push(LocalDecl::new(Ty::I64, None));
    let t = Local(func.locals.len() as u32 - 1);
    let at = func.term_loc(bi);
    push_stmt(
        func,
        bi,
        Stmt::Assign(Place::local(t), Rvalue::Cast(x, Ty::I64)),
        at,
    );
    if let Terminator::Call { callee, args, .. } = &mut func.blocks[bi].term {
        *callee = int.clone();
        args[pos] = Operand::Copy(Place::local(t));
    }
    true
}
