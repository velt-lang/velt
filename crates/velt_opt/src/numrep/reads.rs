//! The reads of narrowing candidates, for `narrow`'s two read rules: whether a read can tell
//! `-0` from 0, and whether it gains from an integer (otherwise narrowing a value that is only
//! converted from an integer and used as a double would add a conversion).

use velt_vir::vir::{BinOp, Function, Local, Operand, Place, Rvalue, Stmt, Terminator, Ty};

use super::fact::Fact;
use super::flow::{is_comparison, Flow, State};
use super::Env;

/// Whether a read gains from an integer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum IntUse {
    No,
    Yes,
    /// When this local is narrowed too (the other side of a comparison, or the local the read
    /// defines).
    With(Local),
}

/// Where a candidate is read, and how.
pub(super) struct Read {
    pub local: Local,
    pub at: (usize, usize),
    /// The read cannot tell `-0` from 0 whatever happens.
    pub blind: bool,
    /// ... or it cannot when this local is narrowed (the read is an operand of its definition).
    pub into: Option<Local>,
    pub int_use: IntUse,
    /// The read converts the value to a 64-bit integer (an index).
    pub to_wide: bool,
}

/// Collects the reads of candidates (`cand`) in a function.
pub(super) struct Reads<'c> {
    pub cand: &'c [bool],
    pub list: Vec<Read>,
}

impl Reads<'_> {
    fn push(
        &mut self,
        op: &Operand,
        at: (usize, usize),
        blind: bool,
        into: Option<Local>,
        int_use: IntUse,
    ) {
        if let Operand::Copy(p) = op {
            if p.proj.is_empty() && self.cand[p.local.0 as usize] {
                self.list.push(Read {
                    local: p.local,
                    at,
                    blind,
                    into,
                    int_use,
                    to_wide: false,
                });
            }
        }
    }

    /// The reads in `d = rv`, at a point with facts `st`.
    #[allow(clippy::too_many_arguments)] // the facts at the point are all needed
    pub fn assignment(
        &mut self,
        flow: &Flow,
        func: &Function,
        st: &State,
        rv: &Rvalue,
        d: &Place,
        at: (usize, usize),
    ) {
        let fact = |op: &Operand| flow.operand(st, func, op).unwrap_or(Fact::top(Ty::F64));
        let into = (d.proj.is_empty() && self.cand[d.local.0 as usize]).then_some(d.local);
        let def_use = into.map_or(IntUse::No, IntUse::With);
        match rv {
            Rvalue::Binary(op, a, b) if is_comparison(*op) => {
                self.push(a, at, true, into, compared_with(b));
                self.push(b, at, true, into, compared_with(a));
            }
            Rvalue::Cast(a, to) if to.is_int() => {
                let n = self.list.len();
                self.push(a, at, true, into, IntUse::Yes);
                if let Some(r) = self.list.get_mut(n) {
                    r.to_wide = matches!(to, Ty::I64 | Ty::U64);
                }
            }
            Rvalue::Binary(BinOp::Rem, a, b) => {
                self.push(a, at, false, into, def_use);
                self.push(b, at, true, into, def_use);
            }
            Rvalue::Binary(BinOp::Add, a, b) => {
                self.push(a, at, !fact(b).neg_zero, into, def_use);
                self.push(b, at, !fact(a).neg_zero, into, def_use);
            }
            Rvalue::Binary(BinOp::Sub, a, b) => {
                self.push(a, at, !fact(b).may_be_zero(), into, def_use);
                self.push(b, at, !fact(a).neg_zero, into, def_use);
            }
            _ => {
                crate::visit::rvalue_operands(rv, &mut |op| self.push(op, at, false, into, def_use))
            }
        }
    }

    /// Reads in a statement other than an assignment can all see `-0` (`MemSet`, copies).
    pub fn statement(&mut self, s: &Stmt, at: (usize, usize)) {
        crate::visit::stmt_operands(s, &mut |op| self.push(op, at, false, None, IntUse::No));
    }

    /// Reads in a terminator: ToInt32 and `__floatIndex` take integers and cannot see `-0`;
    /// other calls and returns can.
    pub fn terminator(&mut self, env: &Env, t: &Terminator, at: (usize, usize)) {
        let int = match t {
            Terminator::Call { callee, .. } => {
                env.is_to_int32(callee) || env.is_float_index(callee)
            }
            _ => false,
        };
        let int_use = if int { IntUse::Yes } else { IntUse::No };
        crate::visit::term_operands(t, &mut |op| self.push(op, at, int, None, int_use));
    }
}

/// What a comparison with `other` gains from an integer: everything when `other` is a whole
/// constant, or when it is narrowed too.
fn compared_with(other: &Operand) -> IntUse {
    match other {
        Operand::Const(velt_vir::vir::Const::Float(x), _) if Fact::float(*x).exact_int() => {
            IntUse::Yes
        }
        Operand::Copy(p) if p.proj.is_empty() => IntUse::With(p.local),
        _ => IntUse::No,
    }
}
