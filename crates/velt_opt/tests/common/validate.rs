//! Strict VIR checker for tests: ids in range, operand/place typing, signatures, and
//! "every local is assigned before it is read" (a must-analysis over reachable blocks).
//! Also runs `velt_vir::verify`, so tests keep checking the real contract as it grows.

use velt_vir::vir::*;

/// Check the program; `Err` lists every problem found.
pub fn validate(p: &Program) -> Result<(), Vec<String>> {
    // Unit-test programs are fragments without the `velt_main` entry the full verifier demands.
    let mut errs: Vec<String> = velt_vir::verify(p)
        .err()
        .unwrap_or_default()
        .into_iter()
        .filter(|e| !e.contains("missing exported entry `velt_main`"))
        .collect();
    for (i, f) in p.funcs.iter().enumerate() {
        let mut ctx = Ctx { p, f, errs: vec![] };
        ctx.function();
        errs.extend(
            ctx.errs
                .into_iter()
                .map(|e| format!("fn#{i} {}: {e}", f.symbol)),
        );
    }
    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs)
    }
}

/// `validate` that panics with the program dump on failure.
pub fn assert_valid(p: &Program) {
    if let Err(errs) = validate(p) {
        panic!("invalid VIR:\n  {}\n{p}", errs.join("\n  "));
    }
}

struct Ctx<'a> {
    p: &'a Program,
    f: &'a Function,
    errs: Vec<String>,
}

impl Ctx<'_> {
    fn err(&mut self, e: String) {
        self.errs.push(e);
    }

    fn function(&mut self) {
        let f = self.f;
        if f.blocks.is_empty() {
            return self.err("no blocks".into());
        }
        for (i, &param) in f.params.iter().enumerate() {
            if !param.is_scalar() || f.locals.get(i).map(|l| l.ty) != Some(param) {
                self.err(format!(
                    "param {i} ({param:?}) invalid or not matching its local"
                ));
            }
        }
        if matches!(f.ret, Ty::Agg(_)) {
            self.err("aggregate return".into());
        }
        for (bi, b) in f.blocks.iter().enumerate() {
            for s in &b.stmts {
                self.stmt(s)
                    .unwrap_or_else(|e| self.err(format!("bb{bi} {s:?}: {e}")));
            }
            self.term(&b.term)
                .unwrap_or_else(|e| self.err(format!("bb{bi} {:?}: {e}", b.term)));
        }
        if self.errs.is_empty() {
            self.definite_assignment();
        }
    }

    fn place_ty(&self, pl: &Place) -> Result<Ty, String> {
        let mut ty = self
            .f
            .locals
            .get(pl.local.0 as usize)
            .ok_or(format!("unknown local _{}", pl.local.0))?
            .ty;
        for proj in &pl.proj {
            ty = match (proj, ty) {
                (Proj::Field(n), Ty::Agg(id)) => {
                    let agg = self.p.aggs.get(id.0 as usize).ok_or("unknown agg")?;
                    agg.fields.get(*n as usize).ok_or("unknown field")?.0
                }
                (Proj::Deref(t), Ty::Ptr) => *t,
                (Proj::Cast(id), Ty::Agg(_)) if (id.0 as usize) < self.p.aggs.len() => Ty::Agg(*id),
                _ => return Err(format!("bad projection {proj:?} on {ty:?}")),
            };
        }
        Ok(ty)
    }

    fn operand_ty(&self, op: &Operand) -> Result<Ty, String> {
        match op {
            Operand::Copy(pl) => self.place_ty(pl),
            Operand::Const(c, ty) => {
                let ok = match c {
                    Const::Int(_) => ty.is_scalar(),
                    Const::Float(_) => ty.is_float(),
                    Const::Bool(_) => !ty.is_float() && ty.is_scalar(),
                    Const::Unit => *ty == Ty::Unit,
                    Const::Static(id) => *ty == Ty::Ptr && (id.0 as usize) < self.p.statics.len(),
                    Const::Func(id) => *ty == Ty::Ptr && (id.0 as usize) < self.p.funcs.len(),
                    Const::Extern(id) => *ty == Ty::Ptr && (id.0 as usize) < self.p.externs.len(),
                };
                ok.then_some(*ty)
                    .ok_or(format!("bad constant {c:?} of type {ty:?}"))
            }
        }
    }

    fn rvalue_ty(&self, rv: &Rvalue) -> Result<Ty, String> {
        Ok(match rv {
            Rvalue::Use(a) => self.operand_ty(a)?,
            Rvalue::Unary(UnOp::Not, a) => expect(self.operand_ty(a)?, Ty::Bool)?,
            Rvalue::Unary(_, a) => self.operand_ty(a)?,
            Rvalue::Binary(op, a, b) => self.binary_ty(*op, a, b)?,
            Rvalue::Cast(a, to) => {
                self.operand_ty(a)?;
                *to
            }
            Rvalue::AddrOf(pl) => {
                self.place_ty(pl)?;
                Ty::Ptr
            }
            Rvalue::Aggregate(id, ops) => {
                let agg = self.p.aggs.get(id.0 as usize).ok_or("unknown agg")?;
                if agg.fields.len() != ops.len() {
                    return Err("aggregate operand count".into());
                }
                for (op, (fty, _)) in ops.iter().zip(&agg.fields) {
                    expect(self.operand_ty(op)?, *fty)?;
                }
                Ty::Agg(*id)
            }
        })
    }

    fn binary_ty(&self, op: BinOp, a: &Operand, b: &Operand) -> Result<Ty, String> {
        let (ta, tb) = (self.operand_ty(a)?, self.operand_ty(b)?);
        match op {
            BinOp::PtrAdd if ta == Ty::Ptr && tb.is_int() => Ok(Ty::Ptr),
            BinOp::Shl | BinOp::Shr | BinOp::UShr if ta.is_int() && tb.is_int() => Ok(ta),
            _ if ta != tb || !ta.is_scalar() => Err(format!("{op:?} on {ta:?}, {tb:?}")),
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => Ok(Ty::Bool),
            _ => Ok(ta),
        }
    }

    fn stmt(&self, s: &Stmt) -> Result<(), String> {
        match s {
            Stmt::Assign(pl, rv) => {
                let pt = self.place_ty(pl)?;
                expect(self.rvalue_ty(rv)?, pt).map(|_| ())
            }
            Stmt::MemCopy { dst, src, .. } => {
                expect(self.operand_ty(dst)?, Ty::Ptr)?;
                expect(self.operand_ty(src)?, Ty::Ptr).map(|_| ())
            }
            Stmt::MemCopyDyn { dst, src, len, .. } => {
                expect(self.operand_ty(dst)?, Ty::Ptr)?;
                expect(self.operand_ty(src)?, Ty::Ptr)?;
                expect(self.operand_ty(len)?, Ty::U64).map(|_| ())
            }
            Stmt::MemSet { dst, byte, len } => {
                expect(self.operand_ty(dst)?, Ty::Ptr)?;
                expect(self.operand_ty(byte)?, Ty::U8)?;
                expect(self.operand_ty(len)?, Ty::U64).map(|_| ())
            }
            Stmt::Nop => Ok(()),
        }
    }

    fn block_ok(&self, b: BlockId) -> Result<(), String> {
        ((b.0 as usize) < self.f.blocks.len())
            .then_some(())
            .ok_or(format!("unknown block bb{}", b.0))
    }

    fn term(&self, t: &Terminator) -> Result<(), String> {
        match t {
            Terminator::Goto(b) => self.block_ok(*b),
            Terminator::Branch { cond, then, els } => {
                expect(self.operand_ty(cond)?, Ty::Bool)?;
                self.block_ok(*then)?;
                self.block_ok(*els)
            }
            Terminator::Switch {
                value,
                cases,
                default,
            } => {
                let ty = self.operand_ty(value)?;
                if !ty.is_int() && ty != Ty::Bool {
                    return Err("switch on non-int".into());
                }
                for (_, b) in cases {
                    self.block_ok(*b)?;
                }
                self.block_ok(*default)
            }
            Terminator::Return(op) => expect(self.operand_ty(op)?, self.f.ret).map(|_| ()),
            Terminator::Call {
                callee,
                args,
                dest,
                next,
            } => self.call(callee, args, dest, *next),
            Terminator::Unreachable => Ok(()),
        }
    }

    fn call(
        &self,
        callee: &Callee,
        args: &[Operand],
        dest: &Option<Place>,
        next: BlockId,
    ) -> Result<(), String> {
        let (params, ret) = match callee {
            Callee::Func(id) => {
                let f = self.p.funcs.get(id.0 as usize).ok_or("unknown func")?;
                (f.params.clone(), f.ret)
            }
            Callee::Extern(id) => {
                let e = self.p.externs.get(id.0 as usize).ok_or("unknown extern")?;
                (e.params.clone(), e.ret)
            }
            Callee::Ptr {
                target,
                params,
                ret,
            } => {
                expect(self.operand_ty(target)?, Ty::Ptr)?;
                (params.clone(), *ret)
            }
        };
        let tys = args
            .iter()
            .map(|a| self.operand_ty(a))
            .collect::<Result<Vec<_>, _>>()?;
        if tys != params {
            return Err(format!("args {tys:?} vs params {params:?}"));
        }
        if let Some(d) = dest {
            let dt = self.place_ty(d)?;
            if dt != ret && dt != Ty::Unit {
                return Err(format!("dest {dt:?} vs ret {ret:?}"));
            }
        }
        self.block_ok(next)
    }

    /// Forward must-analysis: which locals are definitely assigned at each block entry.
    fn definite_assignment(&mut self) {
        let f = self.f;
        let n = f.locals.len();
        let mut init = vec![false; n];
        for (i, l) in f.locals.iter().enumerate() {
            init[i] = i < f.params.len() || l.ty == Ty::Unit;
        }
        let mut entry: Vec<Option<Vec<bool>>> = vec![None; f.blocks.len()];
        entry[0] = Some(init);
        let mut work = vec![0usize];
        let mut errs = vec![];
        while let Some(b) = work.pop() {
            let mut st = entry[b].clone().expect("reached");
            let block = &f.blocks[b];
            for s in &block.stmts {
                reads_of_stmt(s, &mut |l| check_read(&st, l, b, &mut errs));
                writes_of_stmt(s, &mut |l| st[l] = true);
            }
            reads_of_term(&block.term, &mut |l| check_read(&st, l, b, &mut errs));
            if let Terminator::Call { dest: Some(d), .. } = &block.term {
                if !d.proj.iter().any(|x| matches!(x, Proj::Deref(_))) {
                    st[d.local.0 as usize] = true;
                }
            }
            for s in successors(&block.term) {
                let s = s.0 as usize;
                let changed = match &mut entry[s] {
                    None => {
                        entry[s] = Some(st.clone());
                        true
                    }
                    Some(e) => {
                        let mut c = false;
                        for (x, y) in e.iter_mut().zip(&st) {
                            if *x && !*y {
                                *x = false;
                                c = true;
                            }
                        }
                        c
                    }
                };
                if changed {
                    work.push(s);
                }
            }
        }
        errs.sort();
        errs.dedup();
        self.errs.extend(errs);
    }
}

fn expect(have: Ty, want: Ty) -> Result<Ty, String> {
    (have == want)
        .then_some(have)
        .ok_or(format!("type {have:?}, expected {want:?}"))
}

fn check_read(st: &[bool], l: usize, b: usize, errs: &mut Vec<String>) {
    if !st[l] {
        errs.push(format!("bb{b}: _{l} may be read before assignment"));
    }
}

fn operand_reads(op: &Operand, f: &mut impl FnMut(usize)) {
    if let Operand::Copy(p) = op {
        f(p.local.0 as usize);
    }
}

fn reads_of_stmt(s: &Stmt, f: &mut impl FnMut(usize)) {
    match s {
        Stmt::Assign(pl, rv) => {
            match rv {
                Rvalue::Use(a) | Rvalue::Unary(_, a) | Rvalue::Cast(a, _) => operand_reads(a, f),
                Rvalue::Binary(_, a, b) => {
                    operand_reads(a, f);
                    operand_reads(b, f);
                }
                Rvalue::Aggregate(_, ops) => ops.iter().for_each(|o| operand_reads(o, f)),
                Rvalue::AddrOf(p) if p.proj.iter().any(|x| matches!(x, Proj::Deref(_))) => {
                    f(p.local.0 as usize)
                }
                Rvalue::AddrOf(_) => {}
            }
            if pl.proj.iter().any(|x| matches!(x, Proj::Deref(_))) {
                f(pl.local.0 as usize);
            }
        }
        Stmt::MemCopy { dst, src, .. } => {
            operand_reads(dst, f);
            operand_reads(src, f);
        }
        Stmt::MemCopyDyn { dst, src, len, .. } => {
            [dst, src, len]
                .into_iter()
                .for_each(|o| operand_reads(o, f));
        }
        Stmt::MemSet { dst, byte, len } => {
            [dst, byte, len]
                .into_iter()
                .for_each(|o| operand_reads(o, f));
        }
        Stmt::Nop => {}
    }
}

fn writes_of_stmt(s: &Stmt, f: &mut impl FnMut(usize)) {
    if let Stmt::Assign(pl, rv) = s {
        if let Rvalue::AddrOf(p) = rv {
            if !p.proj.iter().any(|x| matches!(x, Proj::Deref(_))) {
                f(p.local.0 as usize);
            }
        }
        if !pl.proj.iter().any(|x| matches!(x, Proj::Deref(_))) {
            f(pl.local.0 as usize);
        }
    }
}

fn reads_of_term(t: &Terminator, f: &mut impl FnMut(usize)) {
    match t {
        Terminator::Branch { cond: a, .. }
        | Terminator::Switch { value: a, .. }
        | Terminator::Return(a) => operand_reads(a, f),
        Terminator::Call {
            callee, args, dest, ..
        } => {
            if let Callee::Ptr { target, .. } = callee {
                operand_reads(target, f);
            }
            args.iter().for_each(|a| operand_reads(a, f));
            if let Some(d) = dest {
                if d.proj.iter().any(|x| matches!(x, Proj::Deref(_))) {
                    f(d.local.0 as usize);
                }
            }
        }
        Terminator::Goto(_) | Terminator::Unreachable => {}
    }
}

fn successors(t: &Terminator) -> Vec<BlockId> {
    match t {
        Terminator::Goto(b) => vec![*b],
        Terminator::Branch { then, els, .. } => vec![*then, *els],
        Terminator::Switch { cases, default, .. } => {
            cases.iter().map(|c| c.1).chain([*default]).collect()
        }
        Terminator::Call { next, .. } => vec![*next],
        _ => vec![],
    }
}
