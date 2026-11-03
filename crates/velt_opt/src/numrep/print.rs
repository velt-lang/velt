//! Numbers printed as integers. `velt_rt_write_f64(s, x)` (`console.log(x)`) of a whole `x`
//! below 2^53 prints its integer digits, so it becomes `velt_rt_write_i64` of `x as i64`: no
//! conversion to `f64` and no test in the runtime once `x` is an integer. `console.log` prints
//! `-0` as `-0`, so `x` must not be `-0` either. std's own formatters in Velt pair up the same
//! way: a call of the hook `numberText`, `numberPush`, `floatText` or `floatPush` becomes one of
//! `intText` or `intPush` (`` `${-0}` `` is `0`, which the integer prints). The runtime's float
//! printers (`velt_rt_str_from_f64`, `velt_rt_strbuf_push_f64`) have no integer twin left: the
//! runtime prints no integer for a template or `String()` (#2197), and prints a whole float's
//! digits itself.

use velt_vir::vir::Place;
use velt_vir::vir::{Callee, ExternFn, ExternId, FuncId, Function, Local, LocalDecl, Operand};
use velt_vir::vir::{Rvalue, Stmt, Terminator, Ty};

use super::fact::{Fact, TWO_53};
use super::Env;
use crate::srclocs::push_stmt;

/// (float formatter, integer formatter, whether the float one prints `-0` as `-0`, the
/// formatters' parameters with the number's as `I64`).
const PAIRS: [(&str, &str, bool, [Ty; 2]); 1] = [(
    "velt_rt_write_f64",
    "velt_rt_write_i64",
    true,
    [Ty::U32, Ty::I64],
)];

/// Declares the integer formatter of each float formatter the program calls.
pub(crate) fn declare(externs: &mut Vec<ExternFn>) {
    for (float, int, _, params) in PAIRS {
        let used = externs.iter().any(|e| e.symbol == float);
        if used && !externs.iter().any(|e| e.symbol == int) {
            externs.push(ExternFn {
                symbol: int.to_string(),
                params: params.to_vec(),
                ret: Ty::Unit,
                noreturn: false,
                host: None,
            });
        }
    }
}

/// The float formatters' hooks and their integer twins: (float one, integer one), each taking
/// the number where the externs above take it.
const HOOK_PAIRS: [(&str, &str); 4] = [
    (
        velt_common::std_hook!(number_push),
        velt_common::std_hook!(int_push),
    ),
    (
        velt_common::std_hook!(number_text),
        velt_common::std_hook!(int_text),
    ),
    (
        velt_common::std_hook!(float_push),
        velt_common::std_hook!(int_push),
    ),
    (
        velt_common::std_hook!(float_text),
        velt_common::std_hook!(int_text),
    ),
];

/// The formatters of `externs` and `funcs`: (float one, integer one, prints `-0`, the index of
/// the number among the arguments).
pub(super) fn pairs(
    externs: &[ExternFn],
    funcs: &[Function],
) -> Vec<(Callee, Callee, bool, usize)> {
    let id = |s: &str| {
        externs
            .iter()
            .position(|e| e.symbol == s)
            .map(|i| ExternId(i as u32))
    };
    let mut out: Vec<_> = PAIRS
        .iter()
        .filter_map(|(f, i, neg_zero, p)| {
            let at = usize::from(p[0] != Ty::I64);
            Some((
                Callee::Extern(id(f)?),
                Callee::Extern(id(i)?),
                *neg_zero,
                at,
            ))
        })
        .collect();
    // A hook has one function per lock mode (`_L…` after its symbol); each pairs with the
    // integer hook of its mode.
    for (float, int) in HOOK_PAIRS {
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
                let at = usize::from(float == HOOK_PAIRS[0].0);
                out.push((
                    Callee::Func(FuncId(fi as u32)),
                    Callee::Func(FuncId(ii as u32)),
                    false,
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
    if !whole || (*prints_neg_zero && f.neg_zero) || args.len() != 2 {
        return false;
    }
    let pos = *pos;
    let x = args[pos].clone();
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
