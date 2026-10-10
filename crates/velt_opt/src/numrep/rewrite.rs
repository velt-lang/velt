//! Applying a `Plan`: each narrowed local gets an integer twin that its definitions compute
//! with integer operations; comparisons of two narrowed values compare integers; ToInt32 and
//! integer conversions of a narrowed value take its integer directly; every other read
//! converts the twin back to the original type (exactly: the values are whole and within
//! ±2^53, and never `-0` where that read could tell).

use velt_vir::vir::{
    BinOp, Const, Function, Local, LocalDecl, Operand, Place, Rvalue, Stmt, Terminator, Ty, UnOp,
};

use super::fact::Fact;
use super::flow::is_comparison;
use super::narrow::Plan;
use super::Env;
use crate::constfold::normalize;
use crate::srclocs::{push_stmt, rewrite_stmts};
use crate::visit::{stmt_operands_mut, term_operands_mut};

/// Rewrite `func` per `plan`; returns whether anything changed.
pub(super) fn apply(env: &Env, func: &mut Function, plan: &Plan) -> bool {
    if plan.to.iter().all(Option::is_none) {
        return false;
    }
    let mut map: Vec<Option<(Local, Ty)>> = vec![None; func.locals.len()];
    for (i, to) in plan.to.iter().enumerate() {
        if let Some(t) = *to {
            map[i] = Some((Local(func.locals.len() as u32), t));
            let name = func.locals[i].name.clone();
            func.locals.push(LocalDecl::new(t, name));
        }
    }
    let mut rw = Rewriter {
        plan,
        tys: func.locals.iter().map(|l| l.ty).collect(),
        base: func.locals.len(),
        temps: vec![],
        map,
    };
    for bi in 0..func.blocks.len() {
        let mut si = 0;
        rewrite_stmts(func, bi, |s, out| {
            rw.stmt(s, (bi, si), out);
            si += 1;
        });
        rw.terminator(env, func, bi);
    }
    func.locals
        .extend(rw.temps.into_iter().map(|ty| LocalDecl::new(ty, None)));
    true
}

struct Rewriter<'p> {
    plan: &'p Plan,
    /// Per old local: its integer twin and the twin's type.
    map: Vec<Option<(Local, Ty)>>,
    /// Types of the locals before temporaries.
    tys: Vec<Ty>,
    /// Index of the first temporary.
    base: usize,
    temps: Vec<Ty>,
}

impl Rewriter<'_> {
    fn temp(&mut self, ty: Ty) -> Local {
        self.temps.push(ty);
        Local((self.base + self.temps.len() - 1) as u32)
    }

    /// The twin of a narrowed local read by `op`.
    fn twin(&self, op: &Operand) -> Option<(Local, Ty)> {
        match op {
            Operand::Copy(p) if p.proj.is_empty() => self.map.get(p.local.0 as usize).copied()?,
            _ => None,
        }
    }

    fn ty_of(&self, op: &Operand) -> Ty {
        match op {
            Operand::Const(_, t) => *t,
            Operand::Copy(p) if p.proj.is_empty() => self.tys[p.local.0 as usize],
            Operand::Copy(p) => match p.proj.last() {
                Some(velt_vir::vir::Proj::Deref(t)) => *t,
                _ => Ty::Unit,
            },
        }
    }

    /// `v` converted to `t` into a temporary (pushed to `out`), or as is when it has type `t`.
    fn conv_to(&mut self, v: Operand, from: Ty, t: Ty, out: &mut Vec<Stmt>) -> Operand {
        if from == t {
            return v;
        }
        let tmp = self.temp(t);
        out.push(Stmt::Assign(Place::local(tmp), Rvalue::Cast(v, t)));
        Operand::Copy(Place::local(tmp))
    }

    /// The whole number `op` (with facts `f` at the point) as an integer of type `t`.
    fn int_operand(&mut self, op: &Operand, f: Fact, t: Ty, out: &mut Vec<Stmt>) -> Operand {
        if let Some((tw, tt)) = self.twin(op) {
            return self.conv_to(Operand::Copy(Place::local(tw)), tt, t, out);
        }
        match op {
            Operand::Const(Const::Float(x), _) => {
                Operand::Const(Const::Int(normalize(*x as i128, t)), t)
            }
            Operand::Const(Const::Int(v), _) => Operand::Const(Const::Int(normalize(*v, t)), t),
            _ => {
                let from = self.ty_of(op);
                if from.is_float() && !f.fits(t) {
                    // Saturation would differ: convert through `i64` (exact) and wrap.
                    let wide = self.conv_to(op.clone(), from, Ty::I64, out);
                    return self.conv_to(wide, Ty::I64, t, out);
                }
                self.conv_to(op.clone(), from, t, out)
            }
        }
    }

    fn stmt(&mut self, s: Stmt, at: (usize, usize), out: &mut Vec<Stmt>) {
        match s {
            Stmt::Assign(d, rv) if d.proj.is_empty() && self.map[d.local.0 as usize].is_some() => {
                self.definition(d.local, rv, at, out)
            }
            Stmt::Assign(d, Rvalue::Binary(op, a, b))
                if is_comparison(op) && self.int_comparable(&a, &b) =>
            {
                let t = if self.int32_like(&a) && self.int32_like(&b) {
                    Ty::I32
                } else {
                    Ty::I64
                };
                let top = Fact::top(Ty::F64);
                let x = self.int_operand(&a, top, t, out);
                let y = self.int_operand(&b, top, t, out);
                out.push(Stmt::Assign(d, Rvalue::Binary(op, x, y)));
            }
            Stmt::Assign(d, Rvalue::Cast(a, to)) if to.is_int() && self.twin_fits(&a, to) => {
                let (tw, tt) = self.twin(&a).expect("ICE: numrep twin_fits without a twin");
                let v = self.conv_to(Operand::Copy(Place::local(tw)), tt, to, out);
                out.push(Stmt::Assign(d, Rvalue::Use(v)));
            }
            mut s => {
                stmt_operands_mut(&mut s, &mut |op| self.widen_read(op, out));
                out.push(s);
            }
        }
    }

    /// `a` and `b` (doubles) can be compared as integers: narrowed locals or whole constants
    /// within ±2^53, at least one of them narrowed.
    fn int_comparable(&self, a: &Operand, b: &Operand) -> bool {
        let avail = |op: &Operand| {
            self.twin(op).is_some()
                || matches!(op, Operand::Const(Const::Float(x), Ty::F64) if Fact::float(*x).exact_int())
        };
        self.ty_of(a) == Ty::F64
            && (self.twin(a).is_some() || self.twin(b).is_some())
            && avail(a)
            && avail(b)
    }

    fn int32_like(&self, op: &Operand) -> bool {
        match self.twin(op) {
            Some((_, t)) => t == Ty::I32,
            None => {
                matches!(op, Operand::Const(Const::Float(x), _) if Fact::float(*x).fits(Ty::I32))
            }
        }
    }

    /// `a` is a narrowed local whose every value fits the integer type `to`.
    fn twin_fits(&self, a: &Operand, to: Ty) -> bool {
        match a {
            Operand::Copy(p) if self.twin(a).is_some() => {
                self.plan.range[p.local.0 as usize].fits(to)
            }
            _ => false,
        }
    }

    /// A definition of the narrowed local `l` as an integer computation.
    fn definition(&mut self, l: Local, rv: Rvalue, at: (usize, usize), out: &mut Vec<Stmt>) {
        let (tw, t) =
            self.map[l.0 as usize].expect("ICE: numrep definition of an unnarrowed local");
        let dst = Place::local(tw);
        let Some(ops) = self.plan.operands.get(&at).cloned() else {
            return self.fallback(l, dst, t, rv, out);
        };
        let new = match rv {
            Rvalue::Use(a) | Rvalue::Cast(a, _) => {
                Rvalue::Use(self.int_operand(&a, ops[0], t, out))
            }
            Rvalue::Unary(UnOp::Neg, a) => {
                Rvalue::Unary(UnOp::Neg, self.int_operand(&a, ops[0], t, out))
            }
            Rvalue::Binary(op @ (BinOp::Add | BinOp::Sub | BinOp::Mul), a, b) => {
                let x = self.int_operand(&a, ops[0], t, out);
                let y = self.int_operand(&b, ops[1], t, out);
                Rvalue::Binary(op, x, y)
            }
            Rvalue::Binary(BinOp::Rem, a, b) => {
                // A remainder is exact only on the operands' own values: compute it in a type
                // that holds them.
                let w = if ops[0].fits(Ty::I32) && ops[1].fits(Ty::I32) {
                    Ty::I32
                } else {
                    Ty::I64
                };
                let x = self.int_operand(&a, ops[0], w, out);
                let y = self.int_operand(&b, ops[1], w, out);
                if w == t {
                    Rvalue::Binary(BinOp::Rem, x, y)
                } else {
                    let r = self.temp(w);
                    out.push(Stmt::Assign(
                        Place::local(r),
                        Rvalue::Binary(BinOp::Rem, x, y),
                    ));
                    Rvalue::Cast(Operand::Copy(Place::local(r)), t)
                }
            }
            rv => return self.fallback(l, dst, t, rv, out),
        };
        out.push(Stmt::Assign(dst, new));
    }

    /// Compute `rv` as before and convert it (code the facts did not reach: never run).
    fn fallback(&mut self, l: Local, dst: Place, t: Ty, mut rv: Rvalue, out: &mut Vec<Stmt>) {
        crate::visit::rvalue_operands_mut(&mut rv, &mut |op| self.widen_read(op, out));
        let tmp = self.temp(self.tys[l.0 as usize]);
        out.push(Stmt::Assign(Place::local(tmp), rv));
        out.push(Stmt::Assign(
            dst,
            Rvalue::Cast(Operand::Copy(Place::local(tmp)), t),
        ));
    }

    /// A read of a narrowed local becomes a read of its twin converted back.
    fn widen_read(&mut self, op: &mut Operand, out: &mut Vec<Stmt>) {
        let Some((tw, tt)) = self.twin(op) else {
            return;
        };
        let Operand::Copy(p) = op else { return };
        let ty = self.tys[p.local.0 as usize];
        *op = self.conv_to(Operand::Copy(Place::local(tw)), tt, ty, out);
    }

    /// ToInt32 of a narrowed local truncates its twin; other terminators read converted values.
    fn terminator(&mut self, env: &Env, func: &mut Function, bi: usize) {
        let mut term = std::mem::replace(&mut func.blocks[bi].term, Terminator::Unreachable);
        let mut stmts = vec![];
        if let Terminator::Call {
            callee,
            args,
            dest,
            next,
        } = &term
        {
            if let (true, [a]) = (env.is_to_int32(callee), args.as_slice()) {
                if let Some((tw, tt)) = self.twin(a) {
                    let v = self.conv_to(Operand::Copy(Place::local(tw)), tt, Ty::I32, &mut stmts);
                    if let Some(d) = dest {
                        stmts.push(Stmt::Assign(d.clone(), Rvalue::Use(v)));
                    }
                    term = Terminator::Goto(*next);
                }
            }
        }
        term_operands_mut(&mut term, &mut |op| self.widen_read(op, &mut stmts));
        for s in stmts {
            push_stmt(func, bi, s, None);
        }
        func.blocks[bi].term = term;
    }
}
