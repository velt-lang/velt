//! After a shared instance is lowered: each pseudo function id becomes a load of its
//! dictionary entry, and a call of it an indirect call; each call of another shared instance
//! gets that instance's dictionary, an entry of this one, as its last argument.

use super::super::{ice, Cx};
use super::Entry;
use crate::vir::{
    AggLayout, BasicBlock, BinOp, Callee, Const, FuncId, Function, LocalDecl, Operand, Place, Proj,
    Rvalue, SrcLoc, Stmt, Terminator, Ty,
};

impl Cx<'_> {
    /// Rewrite shared instance `inst`'s function `func` (module docs).
    pub(in crate::lower) fn shape_rewrite(&mut self, inst: FuncId, func: &mut Function) {
        let Some(dict) = self.shapes.insts[&inst].param else {
            ice("a shared instance without its dictionary parameter")
        };
        let blocks = std::mem::take(&mut func.blocks);
        let mut locs = std::mem::take(&mut func.locs);
        let mut out = Vec::with_capacity(blocks.len());
        for (b, block) in blocks.into_iter().enumerate() {
            let mut bl = locs.get_mut(b).map(std::mem::take).unwrap_or_default();
            let mut stmts = vec![];
            let mut new_locs = vec![];
            let has_locs = !bl.is_empty();
            for (i, mut s) in block.stmts.into_iter().enumerate() {
                let at = bl.get(i).copied().flatten();
                let mut pre = vec![];
                self.stmt_funcs(&mut s, &mut |cx, f| {
                    cx.load_entry(inst, dict, f, func, &mut pre)
                });
                push_all(&mut stmts, &mut new_locs, pre, at);
                stmts.push(s);
                new_locs.push(at);
            }
            let at = bl.pop().flatten();
            let mut term = block.term;
            let mut pre = vec![];
            self.term_funcs(inst, dict, &mut term, func, &mut pre);
            push_all(&mut stmts, &mut new_locs, pre, at);
            new_locs.push(at);
            out.push(BasicBlock { stmts, term });
            if has_locs {
                locs[b] = new_locs;
            }
        }
        func.blocks = out;
        func.locs = locs;
        if let Some(bad) = (self.statics.iter())
            .flat_map(|s| &s.relocs)
            .find(|(_, c)| matches!(c, Const::Func(f) if self.pseudo_of(*f).is_some()))
        {
            ice(format_args!(
                "a static refers to code at a placeholder type: {bad:?}"
            ));
        }
    }

    /// Calls `each` on every pseudo function id the operands of `s` use, replacing it by
    /// the operand `each` returns.
    fn stmt_funcs(&mut self, s: &mut Stmt, each: &mut dyn FnMut(&mut Self, FuncId) -> Operand) {
        let mut ops: Vec<&mut Operand> = vec![];
        match s {
            Stmt::Assign(_, rv) => rvalue_ops(rv, &mut ops),
            Stmt::MemCopy { dst, src, .. } => ops.extend([dst, src]),
            Stmt::MemCopyDyn { dst, src, len, .. } => ops.extend([dst, src, len]),
            Stmt::MemSet { dst, byte, len } => ops.extend([dst, byte, len]),
            Stmt::Nop => {}
        }
        for op in ops {
            if let Operand::Const(Const::Func(f), _) = op {
                if self.pseudo_of(*f).is_some() {
                    *op = each(self, *f);
                }
            }
        }
    }

    /// The terminator's operands, its callee, and the dictionary of a shared callee.
    fn term_funcs(
        &mut self,
        inst: FuncId,
        dict: crate::vir::Local,
        term: &mut Terminator,
        func: &mut Function,
        pre: &mut Vec<Stmt>,
    ) {
        let mut ops: Vec<&mut Operand> = vec![];
        match term {
            Terminator::Branch { cond, .. } => ops.push(cond),
            Terminator::Switch { value, .. } => ops.push(value),
            Terminator::Return(v) => ops.push(v),
            Terminator::Call { args, .. } => ops.extend(args.iter_mut()),
            Terminator::Goto(_) | Terminator::Unreachable => {}
        }
        for op in ops {
            if let Operand::Const(Const::Func(f), _) = op {
                if self.pseudo_of(*f).is_some() {
                    *op = self.load_entry(inst, dict, *f, func, pre);
                }
            }
        }
        let Terminator::Call {
            callee, args, dest, ..
        } = term
        else {
            return;
        };
        let Callee::Func(f) = *callee else {
            return;
        };
        if self.pseudo_of(f).is_some() {
            let target = self.load_entry(inst, dict, f, func, pre);
            let params = args.iter().map(|a| self.operand_ty(func, a)).collect();
            let ret = dest.as_ref().map_or(Ty::Unit, |d| self.place_ty(func, d));
            *callee = Callee::Ptr {
                target,
                params,
                ret,
            };
        } else if self.shapes.insts.contains_key(&f) {
            let i = self.entry_index(inst, Entry::Dict(f));
            let d = load(func, dict, i, pre);
            self.with_dict(f, args, d);
        }
    }

    /// The dictionary entry pseudo id `f` stands for, loaded into a new local.
    fn load_entry(
        &mut self,
        inst: FuncId,
        dict: crate::vir::Local,
        f: FuncId,
        func: &mut Function,
        pre: &mut Vec<Stmt>,
    ) -> Operand {
        let Some((owner, i)) = self.pseudo_of(f) else {
            ice("not a pseudo id")
        };
        if owner != inst {
            ice("a pseudo id of another shared instance");
        }
        load(func, dict, i, pre)
    }

    fn operand_ty(&self, func: &Function, op: &Operand) -> Ty {
        match op {
            Operand::Const(_, t) => *t,
            Operand::Copy(p) => self.place_ty(func, p),
        }
    }

    fn place_ty(&self, func: &Function, p: &Place) -> Ty {
        let mut t = func.locals[p.local.0 as usize].ty;
        for pr in &p.proj {
            t = match (pr, t) {
                (Proj::Field(n), Ty::Agg(a)) => field_ty(&self.aggs[a.0 as usize], *n),
                (Proj::Deref(to), _) => *to,
                (Proj::Cast(a), _) => Ty::Agg(*a),
                _ => ice("a field of a value that is not an aggregate"),
            };
        }
        t
    }
}

fn field_ty(l: &AggLayout, n: u32) -> Ty {
    l.fields
        .get(n as usize)
        .map_or_else(|| ice("no such field"), |f| f.0)
}

/// `*(dict + 8 * i)` into a new local of `func`, by statements appended to `pre`.
fn load(func: &mut Function, dict: crate::vir::Local, i: u32, pre: &mut Vec<Stmt>) -> Operand {
    let addr = crate::vir::Local(func.locals.len() as u32);
    func.locals.push(LocalDecl::new(Ty::Ptr, None));
    let val = crate::vir::Local(func.locals.len() as u32);
    func.locals.push(LocalDecl::new(Ty::Ptr, None));
    let off = Operand::Const(Const::Int(8 * i128::from(i)), Ty::I64);
    let base = Operand::Copy(Place::local(dict));
    pre.push(Stmt::Assign(
        Place::local(addr),
        Rvalue::Binary(BinOp::PtrAdd, base, off),
    ));
    let slot = Place {
        local: addr,
        proj: vec![Proj::Deref(Ty::Ptr)],
    };
    pre.push(Stmt::Assign(
        Place::local(val),
        Rvalue::Use(Operand::Copy(slot)),
    ));
    Operand::Copy(Place::local(val))
}

fn push_all(
    stmts: &mut Vec<Stmt>,
    locs: &mut Vec<Option<SrcLoc>>,
    pre: Vec<Stmt>,
    at: Option<SrcLoc>,
) {
    for s in pre {
        stmts.push(s);
        locs.push(at);
    }
}

fn rvalue_ops<'a>(rv: &'a mut Rvalue, ops: &mut Vec<&'a mut Operand>) {
    match rv {
        Rvalue::Use(a) | Rvalue::Unary(_, a) | Rvalue::Cast(a, _) => ops.push(a),
        Rvalue::Binary(_, a, b) => ops.extend([a, b]),
        Rvalue::Aggregate(_, xs) | Rvalue::Intrinsic(_, xs) => ops.extend(xs.iter_mut()),
        Rvalue::AddrOf(_) => {}
    }
}
