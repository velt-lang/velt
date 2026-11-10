//! Numbers printed as integers. std's float formatters pair up with twins for whole numbers: a
//! call of the hook `numberWrite` (`console.log(x)`), `numberText`, `numberPush`, `floatText` or
//! `floatPush` of a whole `x` below 2^53 becomes a call of its twin with `x as i64`: `intWrite`,
//! `intText` or `intPush`, or for `numberWrite` in a program without `intWrite`,
//! `wholeNumberWrite`. No conversion to `f64` once `x` is an integer, and no test in the
//! formatter.
//! `console.log` prints `-0` as `-0`, so for `numberWrite` `x` must not be `-0` either
//! (`` `${-0}` `` is `0`, which the integer prints). A call switches only to a twin the program
//! has ([`pairs`]): the text hooks call theirs, so a program that calls one has both, but
//! `numberWrite` holds no integer writer (a printed float would make sema check the integer
//! printing). Its whole numbers have a writer of their own, `wholeNumberWrite`, which it calls:
//! `console.log` of a whole `number` takes `intWrite` where something else in the program, such
//! as printing an integer type, has it, and `wholeNumberWrite` otherwise. The
//! runtime's float printers have no integer twin left: the runtime prints no integer (#2197).

use velt_vir::vir::Place;
use velt_vir::vir::{Callee, FuncId, Function, Local, LocalDecl, Operand};
use velt_vir::vir::{Rvalue, Stmt, Terminator, Ty};

use super::fact::{Fact, TWO_53};
use super::Env;
use crate::srclocs::push_stmt;

/// A float formatter and the twin a call of a whole number turns into.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Formatter {
    /// The float formatter.
    float: Callee,
    /// Its twin.
    twin: Callee,
    /// Whether the float formatter prints `-0` as `-0`, so that `x` must not be `-0` either.
    neg_zero: bool,
    /// The index of the number among the arguments.
    at: usize,
}

/// The float formatters' hooks and their twins, which take `x as i64`: (float one, twin, whether
/// the float one prints `-0` as `-0`, the index of the number among the arguments), each taking
/// the number at the same place. A float formatter takes its first twin the program has.
const HOOK_PAIRS: [(&str, &str, bool, usize); 6] = [
    (
        velt_common::std_hook!(number_write),
        velt_common::std_hook!(int_write),
        true,
        1,
    ),
    (
        velt_common::std_hook!(number_write),
        velt_common::std_hook!(whole_number_write),
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

/// The formatters of `funcs`, each float one with the first twin of it that `funcs` has.
pub(super) fn pairs(funcs: &[Function]) -> Vec<Formatter> {
    let mut out: Vec<Formatter> = vec![];
    // A hook has one function per lock mode (`_L…` after its symbol); each pairs with the
    // twin of its mode.
    for (float, twin, neg_zero, at) in HOOK_PAIRS {
        let (fb, tb) = (
            velt_vir::mangle::mangle(float),
            velt_vir::mangle::mangle(twin),
        );
        for (fi, f) in funcs.iter().enumerate() {
            let Some(rest) = f.symbol.strip_prefix(fb.as_str()) else {
                continue;
            };
            if !(rest.is_empty() || rest.starts_with("_L")) {
                continue;
            }
            let float = Callee::Func(FuncId(fi as u32));
            if out.iter().any(|p| p.float == float) {
                continue;
            }
            let twin = format!("{tb}{rest}");
            if let Some(ti) = funcs.iter().position(|g| g.symbol == twin) {
                out.push(Formatter {
                    float,
                    twin: Callee::Func(FuncId(ti as u32)),
                    neg_zero,
                    at,
                });
            }
        }
    }
    out
}

/// The number a call of the float formatter `callee` prints (`None` for other callees).
pub(super) fn number<'a>(env: &Env, callee: &Callee, args: &'a [Operand]) -> Option<&'a Operand> {
    let p = env.formatters.iter().find(|p| p.float == *callee)?;
    args.get(p.at)
}

/// Block `bi` ends in a float formatter call whose number `x` (with fact `f`) is a whole number
/// below 2^53 (and not `-0` where that prints apart): call its twin with `x as i64` instead.
/// Returns whether it did.
pub(super) fn as_integer(env: &Env, func: &mut Function, bi: usize, f: Fact) -> bool {
    let Terminator::Call { callee, args, .. } = &func.blocks[bi].term else {
        return false;
    };
    let Some(p) = env.formatters.iter().find(|p| p.float == *callee) else {
        return false;
    };
    let whole = f.integral && !f.nan && f.lo > -TWO_53 && f.hi < TWO_53;
    if !whole || (p.neg_zero && f.neg_zero) {
        return false;
    }
    let Some(x) = args.get(p.at).cloned() else {
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
        *callee = p.twin.clone();
        args[p.at] = Operand::Copy(Place::local(t));
    }
    true
}
