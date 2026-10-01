//! Pre-pass over a function body:
//! - decides which droppable locals need a runtime `Bool` drop flag: those whose ownership state
//!   changes (a move, or re-initialization of a local that may be moved/uninitialized) inside a
//!   conditional region — an `if` branch, loop, `&&`/`||` rhs, ternary branch, match arm or
//!   `try` part — nested deeper than the local's declaration. All other state changes happen in
//!   straight-line code of the declaring block, so lowering tracks them statically;
//! - collects pattern bindings that bind by reference (`UseMode::Borrow`/`BorrowMut`): they hold a
//!   pointer to the matched part instead of a copy.

use velt_sema::hir::{self, LocalId, PassMode, UseMode};

pub(super) struct FlagScan<'h> {
    hir: &'h hir::Program,
    depth: u32,
    decl: Vec<u32>,
    moved: Vec<bool>,
    uninit: Vec<bool>,
    cond_move: Vec<bool>,
    cond_assign: Vec<bool>,
    /// Moved out of in part (a field or option payload moved out of the local).
    part_moved: Vec<bool>,
    /// A field moved out of the local inside a conditional region.
    cond_part: Vec<bool>,
    ref_bindings: Vec<LocalId>,
}

/// Result of the pre-pass.
pub(super) struct Scan {
    /// Per HIR local: does it need a drop flag (if it is droppable at all)?
    pub flagged: Vec<bool>,
    pub ref_bindings: Vec<LocalId>,
    /// Per HIR local: is it moved out of, wholly or in part, anywhere in the body?
    pub consumed: Vec<bool>,
    /// Per HIR local: are fields moved out of it zeroed in place instead of being tracked
    /// statically (it has a drop flag, or a field is moved in a conditional region)? See
    /// `LInfo::zero_parts`.
    pub zero_parts: Vec<bool>,
}

impl<'h> FlagScan<'h> {
    pub(super) fn run(hir: &'h hir::Program, body: &hir::Body) -> Scan {
        let n = body.locals.len();
        let mut s = FlagScan {
            hir,
            depth: 0,
            decl: vec![0; n],
            moved: vec![false; n],
            uninit: vec![false; n],
            cond_move: vec![false; n],
            cond_assign: vec![false; n],
            part_moved: vec![false; n],
            cond_part: vec![false; n],
            ref_bindings: vec![],
        };
        s.block(&body.block);
        let flagged: Vec<bool> = (0..n)
            .map(|i| s.cond_move[i] || (s.cond_assign[i] && (s.moved[i] || s.uninit[i])))
            .collect();
        let consumed = (0..n).map(|i| s.moved[i] || s.part_moved[i]).collect();
        let zero_parts = (0..n).map(|i| flagged[i] || s.cond_part[i]).collect();
        Scan {
            flagged,
            ref_bindings: s.ref_bindings,
            consumed,
            zero_parts,
        }
    }

    fn nested(&mut self, f: impl FnOnce(&mut Self)) {
        self.depth += 1;
        f(self);
        self.depth -= 1;
    }

    fn block(&mut self, b: &hir::Block) {
        for s in &b.stmts {
            self.stmt(s);
        }
        if let Some(v) = &b.value {
            self.expr(v);
        }
    }

    fn move_local(&mut self, id: LocalId) {
        let i = id.0 as usize;
        self.moved[i] = true;
        self.cond_move[i] |= self.depth > self.decl[i];
    }

    /// Record the locals a pattern declares (at the current depth).
    fn pat(&mut self, p: &hir::Pat) {
        use hir::PatKind as P;
        match &p.kind {
            P::Binding(id, mode) => {
                self.decl[id.0 as usize] = self.depth;
                if matches!(mode, UseMode::Borrow | UseMode::BorrowMut) {
                    self.ref_bindings.push(*id);
                }
            }
            P::Variant { args: ps, .. } | P::Tuple(ps) | P::Or(ps) => {
                ps.iter().for_each(|q| self.pat(q))
            }
            P::Adt { fields } => fields.iter().for_each(|(_, q)| self.pat(q)),
            P::Array { elems, rest } => {
                elems.iter().for_each(|q| self.pat(q));
                if let Some(r) = rest {
                    self.decl[r.0 as usize] = self.depth;
                }
            }
            P::Some(q) => self.pat(q),
            P::Wildcard | P::Lit(_) | P::None => {}
        }
    }

    fn stmt(&mut self, s: &hir::Stmt) {
        use hir::StmtKind as S;
        match &s.kind {
            S::Let { local, init } => {
                if let Some(e) = init {
                    self.expr(e);
                }
                self.decl[local.0 as usize] = self.depth;
                self.uninit[local.0 as usize] |= init.is_none();
            }
            S::LetPat { pat, init } => {
                self.expr(init);
                self.pat(pat);
            }
            S::Expr(e) | S::Return(Some(e)) => self.expr(e),
            S::If { cond, then, els } => {
                self.expr(cond);
                self.nested(|s| {
                    s.block(then);
                    if let Some(b) = els {
                        s.block(b);
                    }
                });
            }
            S::While {
                cond, body, step, ..
            } => self.nested(|s| {
                s.expr(cond);
                s.block(body);
                if let Some(e) = step {
                    s.expr(e);
                }
            }),
            S::ForOf {
                binding,
                iter,
                body,
                ..
            } => {
                self.expr(iter);
                self.nested(|s| {
                    s.pat(binding);
                    s.block(body);
                });
            }
            S::Try {
                body,
                catch,
                finally,
            } => self.nested(|s| {
                s.block(body);
                if let Some((local, handler)) = catch {
                    if let Some(l) = local {
                        s.decl[l.0 as usize] = s.depth;
                    }
                    s.block(handler);
                }
                if let Some(f) = finally {
                    s.block(f);
                }
            }),
            S::Block(b) => self.block(b),
            S::Return(None) | S::Break(_) | S::Continue(_) => {}
        }
    }

    fn exprs(&mut self, es: &[hir::Expr]) {
        es.iter().for_each(|e| self.expr(e));
    }

    fn expr(&mut self, e: &hir::Expr) {
        use hir::ExprKind as K;
        match &e.kind {
            K::Local(id, UseMode::Move) => self.move_local(*id),
            K::Field {
                base,
                mode: UseMode::Move,
                ..
            } => {
                if let K::Local(id, _) = base.kind {
                    let i = id.0 as usize;
                    self.part_moved[i] = true;
                    self.cond_part[i] |= self.depth > self.decl[i];
                } else if let Some(id) = super::place::member_root(base) {
                    // A field of a union member: the union value is consumed.
                    self.move_local(id);
                } else if let Some(id) = super::place::part_root(base) {
                    self.part_moved[id.0 as usize] = true;
                }
                self.expr(base);
            }
            // An option / union payload is all a value owns: moving it moves the whole local.
            K::UnwrapSome(base, UseMode::Move)
            | K::UnwrapVariant {
                expr: base,
                mode: UseMode::Move,
                ..
            } => {
                if let Some(id) = super::place::unwrap_root(base) {
                    self.move_local(id);
                } else if let Some(id) = super::place::part_root(base) {
                    self.part_moved[id.0 as usize] = true;
                }
                self.expr(base);
            }
            K::Assign { place, value } => {
                self.expr(value);
                match &place.kind {
                    K::Local(id, _) => {
                        let i = id.0 as usize;
                        self.cond_assign[i] |= self.depth > self.decl[i];
                    }
                    _ => self.expr(place),
                }
            }
            K::CompoundAssign { place, value, .. } => {
                self.expr(value);
                if !matches!(place.kind, K::Local(..)) {
                    self.expr(place);
                }
            }
            K::Unary { expr, .. }
            | K::Cast(expr)
            | K::Await(expr)
            | K::WrapSome(expr)
            | K::UnwrapSome(expr, _)
            | K::UnwrapVariant { expr, .. }
            | K::Upcast(expr)
            | K::ToDyn { expr, .. }
            | K::Throw(expr)
            | K::Field { base: expr, .. } => self.expr(expr),
            K::Binary { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            K::Index { base, index, .. } => {
                self.expr(base);
                self.expr(index);
            }
            K::Logical { lhs, rhs, .. } => {
                self.expr(lhs);
                self.nested(|s| s.expr(rhs));
            }
            K::Call { callee, args } => {
                if let hir::Callee::Indirect(c) = callee {
                    self.expr(c);
                }
                self.exprs(args);
            }
            K::If { cond, then, els } => {
                self.expr(cond);
                self.nested(|s| {
                    s.expr(then);
                    s.expr(els);
                });
            }
            K::Block(b) => self.block(b),
            K::AdtLit { fields: es, .. }
            | K::Variant { args: es, .. }
            | K::ArrayLit(es)
            | K::Tuple(es)
            | K::New { args: es, .. } => self.exprs(es),
            K::Match { scrutinee, arms } => {
                self.expr(scrutinee);
                self.nested(|s| {
                    for arm in arms {
                        s.pat(&arm.pat);
                        if let Some(g) = &arm.guard {
                            s.expr(g);
                        }
                        s.expr(&arm.body);
                    }
                });
            }
            K::Closure(def) => self.closure(*def),
            K::Lit(_) | K::Local(..) | K::Global(_) | K::FnRef(..) => {}
        }
    }

    /// Owned captures move the captured local into the closure environment.
    fn closure(&mut self, def: hir::DefId) {
        if let hir::Def::Fn(f) = self.hir.def(def) {
            for c in &f.captures {
                if c.mode == PassMode::Owned {
                    self.move_local(c.outer);
                }
            }
        }
    }
}
